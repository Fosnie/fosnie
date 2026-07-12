//! Provider "Test connection" endpoints — the access
//! gates that need no ML service: admin-only on the deployment route, and the
//! BYOK gate (403 when `user_byok_enabled` is false) on the user route. The happy
//! path needs a live ML probe and is exercised manually / by the ML pytest.
//! No Keycloak: a header-driven fake AuthProvider. Skips if DATABASE_URL is unset.

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

/// Authenticates as the `X-Test-User` uuid, always role `user`.
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
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'ptest', $2, 'user')")
        .bind(id)
        .bind(format!("ptest-{}@local.test", id.simple()))
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn setup() -> Option<(sqlx::PgPool, u16)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url = std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_auth(Arc::new(HeaderAuthProvider))
        .build();
    let app = http::router(state.clone(), None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((pg, port))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_test_access_gates() {
    let Some((pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let u = mk_user(&pg).await;

    // Non-admin → deployment test route is admin-only → 403.
    let admin = api
        .post(format!("{base}/api/admin/providers/llm/test"))
        .header("x-test-user", u.to_string())
        .json(&serde_json::json!({ "enabled": true }))
        .send().await.unwrap();
    assert_eq!(admin.status(), 403, "deployment test is admin-only");

    // BYOK off → user test route → 403.
    sqlx::query("INSERT INTO config_settings (key, value, value_type, scope) VALUES ('providers.user_byok_enabled','false','bool','global') ON CONFLICT (key) DO UPDATE SET value='false'")
        .execute(&pg).await.unwrap();
    let off = api
        .post(format!("{base}/api/me/providers/llm/test"))
        .header("x-test-user", u.to_string())
        .json(&serde_json::json!({ "enabled": true }))
        .send().await.unwrap();
    assert_eq!(off.status(), 403, "user test refused when BYOK disabled");

    // Cleanup.
    let _ = sqlx::query("DELETE FROM config_settings WHERE key = 'providers.user_byok_enabled'").execute(&pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(u).execute(&pg).await;
}
