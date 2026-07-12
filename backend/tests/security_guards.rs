//! Authorization-guard regression tests for the security hardening passes.
//! Proves the cross-team boundaries hold: project-scoped access (owner short-
//! circuit + grants), memory-fact moderation, and prompt scope visibility.
//! Exercises the exact functions the handlers call. Skips when DATABASE_URL is
//! unset (same convention as tests/rbac.rs).

use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::auth::rbac::{self, Permission, PrincipalType, ResourceType};
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::db;
use fosnie_backend::http::{memory, prompts};

async fn pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    db::connect(&url, 5).await.ok()
}

fn ctx(user_id: Uuid, role: PlatformRole) -> AuthContext {
    AuthContext { user_id: Some(user_id), email: None, display_name: None, role, break_glass: false, mfa_enroll_only: false }
}

async fn mk_user(pool: &PgPool, role: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, $2, $3, $4::platform_role)")
        .bind(id)
        .bind("T")
        .bind(format!("{id}@example.com"))
        .bind(role)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn mk_project(pool: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name, owner_user_id) VALUES ($1, $2, $3)")
        .bind(id)
        .bind("P")
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn mk_user_fact(pool: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO memory_facts (id, scope, owner_user_id, content, created_by) VALUES ($1, ($2::text)::mem_scope, $3, $4, $3)")
        .bind(id).bind("user").bind(owner).bind("c")
        .execute(pool).await.unwrap();
    id
}

async fn mk_project_fact(pool: &PgPool, project: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO memory_facts (id, scope, owner_project_id, content) VALUES ($1, ($2::text)::mem_scope, $3, $4)")
        .bind(id).bind("project").bind(project).bind("c")
        .execute(pool).await.unwrap();
    id
}

async fn mk_prompt(pool: &PgPool, scope: &str, project_id: Option<Uuid>, created_by: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO prompts (id, name, content, scope, project_id, created_by) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(id).bind("pr").bind("body").bind(scope).bind(project_id).bind(created_by)
        .execute(pool).await.unwrap();
    id
}

// ── project access: owner short-circuit + admin + grant (no inheritance) ──────

#[tokio::test]
async fn project_can_owner_admin_and_outsider() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let owner = mk_user(&pool, "user").await;
    let outsider = mk_user(&pool, "user").await;
    let admin = mk_user(&pool, "client_admin").await;
    let project = mk_project(&pool, owner).await;

    // Owner passes via the short-circuit even though no grant row exists.
    assert!(rbac::project_can(&pool, &ctx(owner, PlatformRole::User), project, Permission::Write).await.unwrap());
    // Admin always passes.
    assert!(rbac::project_can(&pool, &ctx(admin, PlatformRole::ClientAdmin), project, Permission::Delete).await.unwrap());
    // Outsider has nothing.
    assert!(!rbac::project_can(&pool, &ctx(outsider, PlatformRole::User), project, Permission::Read).await.unwrap());
    // require_project surfaces the same as a 403.
    assert!(rbac::require_project(&pool, &ctx(outsider, PlatformRole::User), project, Permission::Read).await.is_err());
    assert!(rbac::require_project(&pool, &ctx(owner, PlatformRole::User), project, Permission::Write).await.is_ok());
}

#[tokio::test]
async fn project_read_grant_does_not_imply_write() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let owner = mk_user(&pool, "user").await;
    let reader = mk_user(&pool, "user").await;
    let admin_ctx = ctx(mk_user(&pool, "client_admin").await, PlatformRole::ClientAdmin);
    let project = mk_project(&pool, owner).await;

    rbac::grant(&pool, &admin_ctx, ResourceType::Project, project, PrincipalType::User, reader, Permission::Read).await.unwrap();
    let reader_ctx = ctx(reader, PlatformRole::User);
    assert!(rbac::project_can(&pool, &reader_ctx, project, Permission::Read).await.unwrap());
    assert!(!rbac::project_can(&pool, &reader_ctx, project, Permission::Write).await.unwrap());
}

// Escalation mechanism the group gate (slice I) defends: a group grant reaches
// its members; removing membership revokes it.
#[tokio::test]
async fn group_membership_confers_and_revokes_access() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let owner = mk_user(&pool, "user").await;
    let member = mk_user(&pool, "power_user").await;
    let admin_ctx = ctx(mk_user(&pool, "client_admin").await, PlatformRole::ClientAdmin);
    let project = mk_project(&pool, owner).await;

    let group = Uuid::now_v7();
    sqlx::query("INSERT INTO groups (id, name, created_by) VALUES ($1, $2, $3)")
        .bind(group).bind("team").bind(owner).execute(&pool).await.unwrap();
    rbac::grant(&pool, &admin_ctx, ResourceType::Project, project, PrincipalType::Group, group, Permission::Read).await.unwrap();

    let member_ctx = ctx(member, PlatformRole::PowerUser);
    // Not yet a member → no access (a power_user is NOT an admin).
    assert!(!rbac::project_can(&pool, &member_ctx, project, Permission::Read).await.unwrap());
    // Join the group → inherits the grant.
    sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES ($1, $2)")
        .bind(group).bind(member).execute(&pool).await.unwrap();
    assert!(rbac::project_can(&pool, &member_ctx, project, Permission::Read).await.unwrap());
    // Leave → revoked.
    sqlx::query("DELETE FROM group_members WHERE group_id = $1 AND user_id = $2")
        .bind(group).bind(member).execute(&pool).await.unwrap();
    assert!(!rbac::project_can(&pool, &member_ctx, project, Permission::Read).await.unwrap());
}

// ── memory fact moderation (fetch_owned) ─────────────────────────────────────

#[tokio::test]
async fn memory_user_fact_owner_only() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let owner = mk_user(&pool, "user").await;
    let other = mk_user(&pool, "power_user").await; // power_user is not privileged here
    let admin = mk_user(&pool, "client_admin").await;
    let fact = mk_user_fact(&pool, owner).await;

    assert!(memory::fetch_owned(&pool, &ctx(owner, PlatformRole::User), fact).await.is_ok());
    assert!(memory::fetch_owned(&pool, &ctx(other, PlatformRole::PowerUser), fact).await.is_err());
    assert!(memory::fetch_owned(&pool, &ctx(admin, PlatformRole::ClientAdmin), fact).await.is_ok());
}

#[tokio::test]
async fn memory_project_fact_requires_project_write() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let owner = mk_user(&pool, "user").await;
    let outsider = mk_user(&pool, "power_user").await;
    let writer = mk_user(&pool, "user").await;
    let admin_ctx = ctx(mk_user(&pool, "client_admin").await, PlatformRole::ClientAdmin);
    let project = mk_project(&pool, owner).await;
    let fact = mk_project_fact(&pool, project).await;

    // Outsider (even a power_user) cannot moderate another project's fact.
    assert!(memory::fetch_owned(&pool, &ctx(outsider, PlatformRole::PowerUser), fact).await.is_err());
    // Project owner can.
    assert!(memory::fetch_owned(&pool, &ctx(owner, PlatformRole::User), fact).await.is_ok());
    // A user granted write on the project can.
    rbac::grant(&pool, &admin_ctx, ResourceType::Project, project, PrincipalType::User, writer, Permission::Write).await.unwrap();
    assert!(memory::fetch_owned(&pool, &ctx(writer, PlatformRole::User), fact).await.is_ok());
}

// ── prompt scope visibility (load_authorized) ────────────────────────────────

#[tokio::test]
async fn prompt_personal_creator_only() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let creator = mk_user(&pool, "user").await;
    let other = mk_user(&pool, "power_user").await;
    let admin = mk_user(&pool, "client_admin").await;
    let p = mk_prompt(&pool, "personal", None, creator).await;

    assert!(prompts::load_authorized(&pool, &ctx(creator, PlatformRole::User), p).await.is_ok());
    assert!(prompts::load_authorized(&pool, &ctx(other, PlatformRole::PowerUser), p).await.is_err());
    assert!(prompts::load_authorized(&pool, &ctx(admin, PlatformRole::ClientAdmin), p).await.is_ok());
}

#[tokio::test]
async fn prompt_project_scope_is_read_gated() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let owner = mk_user(&pool, "user").await;
    let outsider = mk_user(&pool, "user").await;
    let project = mk_project(&pool, owner).await;
    let p = mk_prompt(&pool, "project", Some(project), owner).await;

    assert!(prompts::load_authorized(&pool, &ctx(owner, PlatformRole::User), p).await.is_ok());
    assert!(prompts::load_authorized(&pool, &ctx(outsider, PlatformRole::User), p).await.is_err());
}

#[tokio::test]
async fn prompt_global_is_open() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let creator = mk_user(&pool, "power_user").await;
    let anyone = mk_user(&pool, "user").await;
    let p = mk_prompt(&pool, "global", None, creator).await;

    assert!(prompts::load_authorized(&pool, &ctx(anyone, PlatformRole::User), p).await.is_ok());
}

// ── pure role gate: power_user is not an admin ───────────────────────────────

#[tokio::test]
async fn is_admin_matrix() {
    assert!(!ctx(Uuid::now_v7(), PlatformRole::User).is_admin());
    assert!(!ctx(Uuid::now_v7(), PlatformRole::PowerUser).is_admin());
    assert!(ctx(Uuid::now_v7(), PlatformRole::ClientAdmin).is_admin());
    assert!(ctx(Uuid::now_v7(), PlatformRole::SuperAdmin).is_admin());
}
