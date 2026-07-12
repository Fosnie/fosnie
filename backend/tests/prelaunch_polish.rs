//! Pre-launch polish (Core): self-serve account soft-archive + the `messaging`
//! presence-capability gate. No Keycloak: a header-driven fake `AuthProvider`
//! yields a chosen user. Needs Postgres + Redis; skips if DATABASE_URL is unset.

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

/// Authenticates as whoever the `X-Test-User` header names (a uuid). Note this
/// stand-in does NOT run `load_context`, so it deliberately bypasses the
/// deactivation check — the 401-on-deactivated path is covered by the real
/// providers' E2E (`users_admin`).
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
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'polish', $2, 'user')")
        .bind(id)
        .bind(format!("polish-{}@local.test", id.simple()))
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
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_auth(Arc::new(HeaderAuthProvider))
        .build();
    let app = http::router(state.clone(), None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((state, pg, port))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_self_archive() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let u = mk_user(&pg).await;

    let del = api
        .delete(format!("{base}/api/me/account"))
        .header("x-test-user", u.to_string())
        .send().await.unwrap();
    assert_eq!(del.status(), 200, "self-delete succeeds");

    // Row kept, deactivated + self-archived, PII anonymised/tombstoned.
    let row = sqlx::query!(
        r#"SELECT display_name, display_name_custom, email,
                  (deactivated_at IS NOT NULL) AS "deactivated!",
                  (self_archived_at IS NOT NULL) AS "self_archived!"
           FROM users WHERE id = $1"#,
        u
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(row.deactivated, "deactivated_at set");
    assert!(row.self_archived, "self_archived_at set");
    assert!(!row.display_name_custom);
    assert_eq!(row.display_name, "Deleted user");
    assert_eq!(row.email, format!("deleted-{}@deleted.invalid", u.simple()));

    // The crypto-shred hook event is emitted.
    let ev_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events WHERE event_type = 'account.archived' AND resource_id = $1",
    )
    .bind(u)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(ev_count, 1, "account.archived emitted");

    // Cleanup.
    let _ = sqlx::query("DELETE FROM events WHERE resource_id = $1").bind(u).execute(&pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(u).execute(&pg).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messaging_feature_gate() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let u = mk_user(&pg).await;
    let list = || {
        api.get(format!("{base}/api/group-chats")).header("x-test-user", u.to_string()).send()
    };

    // Default on (no override) → endpoint serves.
    let _ = sqlx::query("DELETE FROM config_settings WHERE key = 'features.messaging'").execute(&pg).await;
    assert_eq!(list().await.unwrap().status(), 200, "messaging on by default");

    // Toggle off via the runtime override → 403.
    sqlx::query("INSERT INTO config_settings (key, value, value_type, scope) VALUES ('features.messaging','false','bool','global') ON CONFLICT (key) DO UPDATE SET value='false'")
        .execute(&pg).await.unwrap();
    assert_eq!(list().await.unwrap().status(), 403, "messaging off → 403");

    // Back on → serves again.
    sqlx::query("UPDATE config_settings SET value='true' WHERE key = 'features.messaging'").execute(&pg).await.unwrap();
    assert_eq!(list().await.unwrap().status(), 200, "messaging back on");

    // Cleanup.
    let _ = sqlx::query("DELETE FROM config_settings WHERE key = 'features.messaging'").execute(&pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(u).execute(&pg).await;
}
