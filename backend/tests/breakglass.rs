//! Ephemeral super-admin grant store. Skips when DATABASE_URL is unset.

use std::sync::Arc;

use fosnie_backend::audit::verify::verify_chain;
use fosnie_backend::auth::breakglass;
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{cache, db};

async fn state() -> Option<AppState> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    Some(AppState::new(pg, redis, Arc::new(BootConfig::default())))
}

#[tokio::test]
async fn issue_validate_revoke_and_audit() {
    let Some(st) = state().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };

    let grant = breakglass::issue(&st, 60, "servicing", "unit test").await.unwrap();
    assert!(breakglass::validate(&st, &grant).await.unwrap(), "active after issue");

    breakglass::revoke(&st, &grant).await.unwrap();
    assert!(!breakglass::validate(&st, &grant).await.unwrap(), "inactive after revoke");

    // issue + revoke both audited (by grant fingerprint — the secret is never
    // stored); chain stays valid.
    let fp = &grant[..8];
    let issued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'breakglass.issued' AND payload->>'grant_fp' = $1",
    )
    .bind(fp)
    .fetch_one(&st.pg)
    .await
    .unwrap();
    assert_eq!(issued, 1);
    assert!(verify_chain(&st.pg).await.unwrap().ok);
}

#[tokio::test]
async fn list_active_reports_issued_grant() {
    let Some(st) = state().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };

    let grant = breakglass::issue(&st, 120, "listing", "unit test").await.unwrap();
    let active = breakglass::list_active(&st).await.unwrap();
    let found = active.iter().find(|g| g.grant_id == grant).expect("issued grant is listed");
    assert!(found.ttl_secs > 0 && found.ttl_secs <= 120, "ttl within bound");
    assert_eq!(found.label.as_deref(), Some("listing"));

    breakglass::revoke(&st, &grant).await.unwrap();
    let after = breakglass::list_active(&st).await.unwrap();
    assert!(!after.iter().any(|g| g.grant_id == grant), "revoked grant drops from the list");
}

#[tokio::test]
async fn grant_expires_on_ttl() {
    let Some(st) = state().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };

    let grant = breakglass::issue(&st, 1, "ttl", "unit test").await.unwrap();
    assert!(breakglass::validate(&st, &grant).await.unwrap());
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert!(
        !breakglass::validate(&st, &grant).await.unwrap(),
        "grant must auto-revoke on TTL"
    );
}
