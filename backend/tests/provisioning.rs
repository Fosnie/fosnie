//! User auto-provisioning. Skips when DATABASE_URL is unset.

use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::audit::verify::verify_chain;
use fosnie_backend::auth::provisioning::{self, ProvisionClaims};
use fosnie_backend::auth::PlatformRole;
use fosnie_backend::config::runtime::{self, ConfigValueType};
use fosnie_backend::db;

async fn pool() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    db::connect(&url, 5).await.ok()
}

#[tokio::test]
async fn first_login_creates_then_subsequent_updates() {
    let Some(pool) = pool().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };

    let sub = Uuid::now_v7();
    let email = format!("u-{sub}@example.com");

    provisioning::upsert_from_claims(
        &pool,
        &ProvisionClaims {
            sub,
            email: email.clone(),
            display_name: "Test User".into(),
            role: PlatformRole::PowerUser,
            groups: vec![],
        },
    )
    .await
    .expect("first upsert");

    let role: String = sqlx::query_scalar("SELECT role::text FROM users WHERE id = $1")
        .bind(sub)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(role, "power_user");

    let provisioned: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'user.provisioned' AND resource_id = $1",
    )
    .bind(sub)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(provisioned, 1, "first sight emits exactly one provisioned event");

    // Second upsert updates identity, emits no new provisioned event.
    provisioning::upsert_from_claims(
        &pool,
        &ProvisionClaims {
            sub,
            email,
            display_name: "Renamed".into(),
            role: PlatformRole::PowerUser,
            groups: vec![],
        },
    )
    .await
    .expect("second upsert");

    let still_one: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'user.provisioned' AND resource_id = $1",
    )
    .bind(sub)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(still_one, 1);

    let name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(sub)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Renamed");

    assert!(verify_chain(&pool).await.unwrap().ok);
}

#[tokio::test]
async fn jit_group_sync_create_then_remove() {
    let Some(pool) = pool().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };

    // Enable JIT group sync in `create` mode (global runtime config).
    runtime::set(&pool, "identity.jit_group_sync", "create", ConfigValueType::String, "global", None, "system")
        .await
        .unwrap();

    let sub = Uuid::now_v7();
    let email = format!("jit-{sub}@example.com");
    let gname = format!("Eng-{}", sub.simple());

    // First login with a groups claim → the group is minted (managed_by='idp') and
    // the user joins it (source='idp').
    provisioning::upsert_from_claims(
        &pool,
        &ProvisionClaims { sub, email: email.clone(), display_name: "JIT".into(), role: PlatformRole::User, groups: vec![gname.clone()] },
    )
    .await
    .expect("jit create upsert");

    let gid: Uuid = sqlx::query_scalar("SELECT id FROM groups WHERE name = $1 AND managed_by = 'idp'")
        .bind(&gname)
        .fetch_one(&pool)
        .await
        .expect("group minted");
    let member: i64 = sqlx::query_scalar("SELECT count(*) FROM group_members WHERE group_id = $1 AND user_id = $2 AND source = 'idp'")
        .bind(gid)
        .bind(sub)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(member, 1, "user joined the JIT group");
    let added: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type = 'identity.jit.member_added' AND resource_id = $1")
        .bind(gid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(added, 1, "membership add audited");

    // Second login WITHOUT the group in the claim → the idp membership is removed.
    provisioning::upsert_from_claims(
        &pool,
        &ProvisionClaims { sub, email, display_name: "JIT".into(), role: PlatformRole::User, groups: vec![] },
    )
    .await
    .expect("jit remove upsert");

    let still: i64 = sqlx::query_scalar("SELECT count(*) FROM group_members WHERE group_id = $1 AND user_id = $2")
        .bind(gid)
        .bind(sub)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(still, 0, "membership removed when dropped from the claim");
    let removed: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type = 'identity.jit.member_removed' AND resource_id = $1")
        .bind(gid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(removed, 1, "membership removal audited");

    assert!(verify_chain(&pool).await.unwrap().ok);

    // Reset the global knob so other tests see the default (off).
    runtime::set(&pool, "identity.jit_group_sync", "off", ConfigValueType::String, "global", None, "system")
        .await
        .unwrap();
    let _ = sqlx::query("DELETE FROM group_members WHERE group_id = $1").bind(gid).execute(&pool).await;
    let _ = sqlx::query("DELETE FROM groups WHERE id = $1").bind(gid).execute(&pool).await;
}
