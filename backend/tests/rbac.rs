//! Flat AccessGrants, no inheritance. Skips when
//! DATABASE_URL is unset.

use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::auth::rbac::{self, Permission, PrincipalType, ResourceType};
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::db;

async fn pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    db::connect(&url, 5).await.ok()
}

fn ctx(user_id: Uuid, role: PlatformRole) -> AuthContext {
    AuthContext {
        user_id: Some(user_id),
        email: None,
        display_name: None,
        role,
        break_glass: false, mfa_enroll_only: false,
    }
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

#[tokio::test]
async fn admin_override_user_grant_group_grant_and_no_inheritance() {
    let Some(pool) = pool().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };

    let admin = mk_user(&pool, "client_admin").await;
    let normal = mk_user(&pool, "user").await;
    let admin_ctx = ctx(admin, PlatformRole::ClientAdmin);
    let user_ctx = ctx(normal, PlatformRole::User);

    let res_a = Uuid::now_v7();
    let res_b = Uuid::now_v7();

    // Admin level overrides everything.
    assert!(rbac::can(&pool, &admin_ctx, ResourceType::Project, res_a, Permission::Delete)
        .await
        .unwrap());

    // Plain user has nothing to start.
    assert!(!rbac::can(&pool, &user_ctx, ResourceType::Project, res_a, Permission::Read)
        .await
        .unwrap());

    // Grant read on A to the user.
    rbac::grant(&pool, &admin_ctx, ResourceType::Project, res_a, PrincipalType::User, normal, Permission::Read)
        .await
        .unwrap();
    assert!(rbac::can(&pool, &user_ctx, ResourceType::Project, res_a, Permission::Read)
        .await
        .unwrap());

    // No inheritance across permissions: read does not imply write.
    assert!(!rbac::can(&pool, &user_ctx, ResourceType::Project, res_a, Permission::Write)
        .await
        .unwrap());
    // No inheritance across resources: a grant on A says nothing about B.
    assert!(!rbac::can(&pool, &user_ctx, ResourceType::Project, res_b, Permission::Read)
        .await
        .unwrap());

    // Group grant flows to members.
    let group = Uuid::now_v7();
    sqlx::query("INSERT INTO groups (id, name, created_by) VALUES ($1, $2, $3)")
        .bind(group)
        .bind("team")
        .bind(admin)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO group_members (group_id, user_id) VALUES ($1, $2)")
        .bind(group)
        .bind(normal)
        .execute(&pool)
        .await
        .unwrap();
    rbac::grant(&pool, &admin_ctx, ResourceType::Project, res_b, PrincipalType::Group, group, Permission::Write)
        .await
        .unwrap();
    assert!(rbac::can(&pool, &user_ctx, ResourceType::Project, res_b, Permission::Write)
        .await
        .unwrap());

    // A non-admin (power_user) may not grant in this build.
    let pu = ctx(mk_user(&pool, "power_user").await, PlatformRole::PowerUser);
    assert!(rbac::grant(&pool, &pu, ResourceType::Project, res_a, PrincipalType::User, normal, Permission::Delete)
        .await
        .is_err());
}
