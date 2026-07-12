//! Libraries (Knowledge Bases) — the intersection invariant and its security
//! tests. The retrieval allow-list
//! is `attached(context) ∩ can_read(user)` — never one side alone. Skips when
//! DATABASE_URL is unset.

use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::db;
use fosnie_backend::kb;

async fn pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    db::connect(&url, 5).await.ok()
}

fn ctx(user_id: Uuid, role: PlatformRole) -> AuthContext {
    AuthContext { user_id: Some(user_id), email: None, display_name: None, role, break_glass: false, mfa_enroll_only: false }
}

async fn mk_user(pool: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'T', $2, 'user')")
        .bind(id)
        .bind(format!("{id}@example.com"))
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn mk_project(pool: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, name, owner_user_id) VALUES ($1, 'P', $2)")
        .bind(id)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn mk_chat(pool: &PgPool, owner: Uuid, project: Option<Uuid>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO chats (id, project_id, owner_user_id, title) VALUES ($1, $2, $3, 'C')")
        .bind(id)
        .bind(project)
        .bind(owner)
        .execute(pool)
        .await
        .unwrap();
    id
}

/// A ready, standalone KB owned by `owner` (visibility 'shared' so it never
/// counts as a project default).
async fn mk_kb(pool: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO knowledge_bases \
           (id, name, owner_id, visibility, embedding_model_id, embedding_dimension, status) \
         VALUES ($1, 'KB', $2, 'shared', 'test-model', 1024, 'ready')",
    )
    .bind(id)
    .bind(owner)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn grant_kb(pool: &PgPool, kb_id: Uuid, principal: Uuid, perm: &str) {
    sqlx::query(
        "INSERT INTO kb_access_grants (id, kb_id, principal_type, principal_id, permission, granted_by) \
         VALUES ($1, $2, 'user', $3, ($4::text)::kb_permission, $3) \
         ON CONFLICT (kb_id, principal_type, principal_id) DO UPDATE SET permission = EXCLUDED.permission",
    )
    .bind(Uuid::now_v7())
    .bind(kb_id)
    .bind(principal)
    .bind(perm)
    .execute(pool)
    .await
    .unwrap();
}

async fn attach_project(pool: &PgPool, project: Uuid, kb_id: Uuid) {
    sqlx::query("INSERT INTO project_kb_links (project_id, kb_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(project)
        .bind(kb_id)
        .execute(pool)
        .await
        .unwrap();
}

/// The ethical wall: a project member WITHOUT a grant on an attached KB gets
/// ZERO of that KB (intersection, not just "attached").
#[tokio::test]
async fn negative_leak_attached_but_not_granted() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let owner = mk_user(&pool).await; // owns the project + the restricted KB
    let member = mk_user(&pool).await; // project member without a grant on KB-2
    let project = mk_project(&pool, owner).await;
    let kb2 = mk_kb(&pool, owner).await; // member has NO grant here
    attach_project(&pool, project, kb2).await;
    let chat = mk_chat(&pool, member, Some(project)).await;

    // Owner (can read their KB) sees it; member (no grant) sees nothing.
    let owner_allow = kb::retrieval_allowlist(&pool, &ctx(owner, PlatformRole::User), chat, Some(project), None).await.unwrap();
    assert!(owner_allow.contains(&kb2), "owner should retrieve their attached KB");
    let member_allow = kb::retrieval_allowlist(&pool, &ctx(member, PlatformRole::User), chat, Some(project), None).await.unwrap();
    assert!(!member_allow.contains(&kb2), "member without a grant must get ZERO of the attached KB");
}

/// No cross-matter bleed: a KB attached only to Project 1 never surfaces when
/// querying in Project 2, even for someone who can read it.
#[tokio::test]
async fn cross_matter_isolation() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let user = mk_user(&pool).await;
    let p1 = mk_project(&pool, user).await;
    let p2 = mk_project(&pool, user).await;
    let kb_a = mk_kb(&pool, user).await; // user owns ⇒ can read
    attach_project(&pool, p1, kb_a).await; // attached to P1 only

    let chat_in_p2 = mk_chat(&pool, user, Some(p2)).await;
    let allow = kb::retrieval_allowlist(&pool, &ctx(user, PlatformRole::User), chat_in_p2, Some(p2), None).await.unwrap();
    assert!(!allow.contains(&kb_a), "a KB attached to another matter must not bleed into this one");

    // Sanity: in P1 it IS in scope.
    let chat_in_p1 = mk_chat(&pool, user, Some(p1)).await;
    let allow1 = kb::retrieval_allowlist(&pool, &ctx(user, PlatformRole::User), chat_in_p1, Some(p1), None).await.unwrap();
    assert!(allow1.contains(&kb_a));
}

/// Grant/revoke take effect on the very next query — no re-index, no stale ACL.
#[tokio::test]
async fn revocation_is_immediate() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let owner = mk_user(&pool).await;
    let other = mk_user(&pool).await;
    let project = mk_project(&pool, owner).await;
    let kb_id = mk_kb(&pool, owner).await;
    attach_project(&pool, project, kb_id).await;
    let chat = mk_chat(&pool, other, Some(project)).await;
    let other_ctx = ctx(other, PlatformRole::User);

    // No grant yet → excluded.
    assert!(!kb::retrieval_allowlist(&pool, &other_ctx, chat, Some(project), None).await.unwrap().contains(&kb_id));
    // Grant read → next query includes it.
    grant_kb(&pool, kb_id, other, "read").await;
    assert!(kb::retrieval_allowlist(&pool, &other_ctx, chat, Some(project), None).await.unwrap().contains(&kb_id));
    // Revoke → next query excludes it (no re-index).
    sqlx::query("DELETE FROM kb_access_grants WHERE kb_id = $1 AND principal_id = $2")
        .bind(kb_id).bind(other).execute(&pool).await.unwrap();
    assert!(!kb::retrieval_allowlist(&pool, &other_ctx, chat, Some(project), None).await.unwrap().contains(&kb_id));
}

/// Fail-closed: a chat with nothing attached resolves to an EMPTY allow-list
/// (the caller then skips retrieval — never "search everything").
#[tokio::test]
async fn fail_closed_empty_allowlist() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let user = mk_user(&pool).await;
    // Readable KB that exists but is attached to NOTHING.
    let _orphan = mk_kb(&pool, user).await;
    let chat = mk_chat(&pool, user, None).await; // ad-hoc, no project, no attaches
    let allow = kb::retrieval_allowlist(&pool, &ctx(user, PlatformRole::User), chat, None, None).await.unwrap();
    assert!(allow.is_empty(), "no attaches ⇒ empty allow-list (readable-but-unattached KB must NOT leak in)");
}

/// The intersection both ways: readable-and-attached included; attached-not-
/// readable excluded; readable-not-attached excluded. Plus ad-hoc chat attach.
#[tokio::test]
async fn intersection_both_sides() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let owner = mk_user(&pool).await;
    let user = mk_user(&pool).await;
    let user_ctx = ctx(user, PlatformRole::User);

    let kb_attached_readable = mk_kb(&pool, owner).await;
    let kb_attached_unreadable = mk_kb(&pool, owner).await;
    let kb_readable_unattached = mk_kb(&pool, owner).await;
    grant_kb(&pool, kb_attached_readable, user, "read").await;
    grant_kb(&pool, kb_readable_unattached, user, "read").await;

    // Ad-hoc chat: attach two of them directly to the chat.
    let chat = mk_chat(&pool, user, None).await;
    sqlx::query("INSERT INTO chat_kb_links (chat_id, kb_id) VALUES ($1, $2), ($1, $3)")
        .bind(chat).bind(kb_attached_readable).bind(kb_attached_unreadable)
        .execute(&pool).await.unwrap();

    let allow = kb::retrieval_allowlist(&pool, &user_ctx, chat, None, None).await.unwrap();
    assert!(allow.contains(&kb_attached_readable), "attached ∧ readable ⇒ in");
    assert!(!allow.contains(&kb_attached_unreadable), "attached ∧ NOT readable ⇒ out (the leak guard)");
    assert!(!allow.contains(&kb_readable_unattached), "readable ∧ NOT attached ⇒ out (the cross-matter guard)");
}

/// can_read / can_manage: owner reads+manages; a read grant reads but does not
/// manage; manage implies read.
#[tokio::test]
async fn read_and_manage_predicates() {
    let Some(pool) = pool().await else { eprintln!("skipping: DATABASE_URL unset"); return };

    let owner = mk_user(&pool).await;
    let reader = mk_user(&pool).await;
    let manager = mk_user(&pool).await;
    let kb_id = mk_kb(&pool, owner).await;
    grant_kb(&pool, kb_id, reader, "read").await;
    grant_kb(&pool, kb_id, manager, "manage").await;

    let r = ctx(reader, PlatformRole::User);
    let m = ctx(manager, PlatformRole::User);
    let o = ctx(owner, PlatformRole::User);

    assert!(kb::can_read(&pool, &o, kb_id).await.unwrap() && kb::can_manage(&pool, &o, kb_id).await.unwrap());
    assert!(kb::can_read(&pool, &r, kb_id).await.unwrap());
    assert!(!kb::can_manage(&pool, &r, kb_id).await.unwrap(), "read grant must NOT confer manage");
    assert!(kb::can_read(&pool, &m, kb_id).await.unwrap() && kb::can_manage(&pool, &m, kb_id).await.unwrap());

    // A stranger has neither.
    let stranger = ctx(mk_user(&pool).await, PlatformRole::User);
    assert!(!kb::can_read(&pool, &stranger, kb_id).await.unwrap());
}
