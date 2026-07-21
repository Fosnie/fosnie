//! Per-user BYOK end-to-end. No Keycloak: a header-driven
//! fake `AuthProvider` yields a chosen user, so the test can act as user A or B.
//! Needs Postgres (:5433) + Redis; skips if DATABASE_URL is unset.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::request::Parts;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::AppError;
use fosnie_backend::ext::AuthProvider;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::{cache, db, http};

/// Authenticates as whoever the `X-Test-User` header names (a uuid). Stands in
/// for a real provider so the test can switch identities per request.
struct HeaderAuthProvider;

#[async_trait]
impl AuthProvider for HeaderAuthProvider {
    async fn authenticate(&self, parts: &mut Parts, _state: &AppState) -> Result<AuthContext, AppError> {
        let uid = parts
            .headers
            .get("x-test-user")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| AppError::Unauthorized("no test user".into()))?;
        Ok(AuthContext { user_id: Some(uid), email: None, display_name: None, role: PlatformRole::User, break_glass: false, mfa_enroll_only: false })
    }
}

async fn mk_user(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'byok', $2, 'user')")
        .bind(id)
        .bind(format!("byok-{}@local.test", id.simple()))
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn setup() -> Option<(AppState, sqlx::PgPool, u16)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url = std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into();
    // A real 32-byte key so provider API keys can be encrypted.
    boot.message_encryption_key = base64_key();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_auth(Arc::new(HeaderAuthProvider))
        .build();
    let app = http::router(state.clone(), None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((state, pg, port))
}

fn base64_key() -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode([9u8; 32])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn byok_gate_isolation_and_revert() {
    let Some((state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let a = mk_user(&pg).await;
    let b = mk_user(&pg).await;
    let get = |role_user: Uuid| {
        api.get(format!("{base}/api/me/providers")).header("x-test-user", role_user.to_string()).send()
    };

    // Force BYOK off for this case. The boot default is ON for public Core, so we
    // set an explicit `false` runtime override rather than clearing it.
    sqlx::query("INSERT INTO config_settings (key, value, value_type, scope) VALUES ('providers.user_byok_enabled','false','bool','global') ON CONFLICT (key) DO UPDATE SET value='false'")
        .execute(&pg).await.unwrap();

    // OFF: GET works (read-only), a BYOK write (create) is refused.
    let cfg: serde_json::Value = get(a).await.unwrap().json().await.unwrap();
    assert_eq!(cfg["user_byok_enabled"], false);
    let create_off = api
        .post(format!("{base}/api/me/providers/llm"))
        .header("x-test-user", a.to_string())
        .json(&serde_json::json!({ "label": "A", "base_url": "https://a.example/v1", "enabled": true }))
        .send().await.unwrap();
    assert_eq!(create_off.status(), 403, "BYOK off → create refused");

    // Enable BYOK via the runtime override.
    sqlx::query("INSERT INTO config_settings (key, value, value_type, scope) VALUES ('providers.user_byok_enabled','true','bool','global') ON CONFLICT (key) DO UPDATE SET value='true'")
        .execute(&pg).await.unwrap();

    // A creates its own named LLM provider + key. The first row at a scope becomes
    // its default, so it resolves immediately.
    let create_on = api
        .post(format!("{base}/api/me/providers/llm"))
        .header("x-test-user", a.to_string())
        .json(&serde_json::json!({ "label": "A", "base_url": "https://a.example/v1", "model": "claude-a", "api_key": "sk-a", "enabled": true }))
        .send().await.unwrap();
    assert_eq!(create_on.status(), 200);
    let a_row_id = create_on.json::<serde_json::Value>().await.unwrap()["id"].as_str().unwrap().to_string();

    // A's GET shows source=user + masked key.
    let av: serde_json::Value = get(a).await.unwrap().json().await.unwrap();
    let a_llm = av["providers"].as_array().unwrap().iter().find(|p| p["role"] == "llm").unwrap();
    assert_eq!(a_llm["source"], "user");
    assert_eq!(a_llm["api_key_set"], true);
    assert_eq!(a_llm["base_url"], "https://a.example/v1");
    assert!(a_llm.get("api_key").is_none(), "key never returned");

    // Resolver isolation: A's overrides carry A's provider; B (no row) gets none.
    let a_ov = fosnie_backend::ml::provider_overrides(&state, Some(a)).await;
    assert_eq!(a_ov.get("llm_base_url").and_then(|v| v.as_str()), Some("https://a.example/v1"));
    assert_eq!(a_ov.get("llm_api_key").and_then(|v| v.as_str()), Some("sk-a"));
    let b_ov = fosnie_backend::ml::provider_overrides(&state, Some(b)).await;
    assert!(b_ov.is_empty(), "user B has no row and no deployment row ⇒ ML default");

    // B's GET never shows A's row.
    let bv: serde_json::Value = get(b).await.unwrap().json().await.unwrap();
    let b_llm = bv["providers"].as_array().unwrap().iter().find(|p| p["role"] == "llm").unwrap();
    assert_eq!(b_llm["source"], "default");
    assert_eq!(b_llm["api_key_set"], false);
    assert!(b_llm["base_url"].is_null());

    // A removes its row → reverts to default.
    let del = api.delete(format!("{base}/api/me/providers/llm/{a_row_id}")).header("x-test-user", a.to_string()).send().await.unwrap();
    assert_eq!(del.status(), 200);
    let av2: serde_json::Value = get(a).await.unwrap().json().await.unwrap();
    let a_llm2 = av2["providers"].as_array().unwrap().iter().find(|p| p["role"] == "llm").unwrap();
    assert_eq!(a_llm2["source"], "default");
    assert!(fosnie_backend::ml::provider_overrides(&state, Some(a)).await.is_empty());

    // Cleanup.
    let _ = sqlx::query("DELETE FROM provider_configs WHERE scope='user' AND scope_id = ANY($1)").bind(vec![a, b]).execute(&pg).await;
    let _ = sqlx::query("DELETE FROM config_settings WHERE key = 'providers.user_byok_enabled'").execute(&pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = ANY($1)").bind(vec![a, b]).execute(&pg).await;
}
