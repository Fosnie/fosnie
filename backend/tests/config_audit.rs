//! Runtime config: validate-on-write and audit-on-change.
//! Skips when `DATABASE_URL` is unset.

use sqlx::PgPool;

use fosnie_backend::audit::verify::verify_chain;
use fosnie_backend::config::runtime::{self, ConfigValueType};
use fosnie_backend::db;

async fn pool_from_env() -> Option<PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    db::connect(&url, 5).await.ok()
}

#[tokio::test]
async fn set_writes_config_and_emits_audit() {
    let Some(pool) = pool_from_env().await else {
        eprintln!("skipping set_writes_config_and_emits_audit: DATABASE_URL unset");
        return;
    };

    let key = format!("test.rag.top_k.{}", uuid::Uuid::now_v7());
    runtime::set(&pool, &key, "12", ConfigValueType::Int, "global", None, "client_admin")
        .await
        .expect("set");

    let entry = runtime::get(&pool, &key).await.expect("get").expect("present");
    assert_eq!(entry.value, "12");
    assert_eq!(entry.value_type, ConfigValueType::Int);

    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'config.changed' AND payload->>'key' = $1",
    )
    .bind(&key)
    .fetch_one(&pool)
    .await
    .expect("count audit");
    assert_eq!(count, 1, "config change must emit exactly one audit event");

    assert!(verify_chain(&pool).await.expect("verify").ok);
}

#[tokio::test]
async fn set_rejects_invalid_value() {
    let Some(pool) = pool_from_env().await else {
        eprintln!("skipping set_rejects_invalid_value: DATABASE_URL unset");
        return;
    };

    let result = runtime::set(
        &pool,
        "test.invalid.int",
        "not-an-integer",
        ConfigValueType::Int,
        "global",
        None,
        "client_admin",
    )
    .await;
    assert!(result.is_err(), "invalid value must be rejected on write");
}
