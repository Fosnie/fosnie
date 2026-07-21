//! Platform API keys and the authentication of the OpenAI-compatible surface.
//! No Keycloak: a header-driven fake `AuthProvider` yields a chosen user for the
//! key-management routes, while `/v1` authenticates by key as it does in
//! production. Needs Postgres (:5433) + Redis; skips if DATABASE_URL is unset.

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

/// Authenticates as whoever `X-Test-User` names. `X-Test-Enrol-Only` marks the
/// session as one that exists only to finish setting up a second factor.
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
        let enrol_only = parts.headers.get("x-test-enrol-only").is_some();
        Ok(AuthContext {
            user_id: Some(uid),
            email: None,
            display_name: None,
            role: PlatformRole::User,
            break_glass: false,
            mfa_enroll_only: enrol_only,
        })
    }
}

pub async fn mk_user(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'apikey', $2, 'user')")
        .bind(id)
        .bind(format!("apikey-{}@local.test", id.simple()))
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn setup() -> Option<(AppState, sqlx::PgPool, u16)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
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

/// `features.public_api` is a deployment-wide row, so the case that switches it
/// off would otherwise pull it out from under the cases running beside it. The
/// switch-off case takes this exclusively; everything else shares it.
static SURFACE: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

/// The surface is on by default; make that explicit so a leftover runtime row
/// from another case cannot decide this one.
async fn set_public_api(pg: &sqlx::PgPool, on: bool) {
    let v = if on { "true" } else { "false" };
    sqlx::query(
        "INSERT INTO config_settings (key, value, value_type, scope) \
         VALUES ('features.public_api', $1, 'bool', 'global') \
         ON CONFLICT (key) DO UPDATE SET value = $1",
    )
    .bind(v)
    .execute(pg)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_lifecycle_and_v1_authentication() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    set_public_api(&pg, true).await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;

    // --- Minting -------------------------------------------------------------
    let created: serde_json::Value = api
        .post(format!("{base}/api/me/api-keys"))
        .header("x-test-user", user.to_string())
        .json(&serde_json::json!({ "name": "integration" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = created["token"].as_str().expect("token returned once").to_string();
    assert!(token.starts_with("sk-fosnie-"), "recognisable prefix: {token}");
    assert!(created["display_prefix"].as_str().is_some_and(|p| token.starts_with(p)));

    // Listing never carries the secret back.
    let listed: serde_json::Value = api
        .get(format!("{base}/api/me/api-keys"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert!(listed[0].get("token").is_none(), "the secret is never listed");

    // --- Authenticating ------------------------------------------------------
    let models = |bearer: Option<String>| {
        let mut r = api.get(format!("{base}/v1/models"));
        if let Some(b) = bearer {
            r = r.header("authorization", format!("Bearer {b}"));
        }
        r.send()
    };

    let ok = models(Some(token.clone())).await.unwrap();
    assert_eq!(ok.status(), 200, "a valid key is accepted");

    for (label, presented) in [
        ("no key", None),
        ("nonsense", Some("not-a-key".to_string())),
        ("well-formed but unknown", Some("sk-fosnie-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string())),
    ] {
        let res = models(presented).await.unwrap();
        assert_eq!(res.status(), 401, "{label} is refused");
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(
            body["error"]["type"], "authentication_error",
            "{label} is refused in the shape clients parse"
        );
        assert!(body["error"]["message"].as_str().is_some());
    }

    // --- Expiry --------------------------------------------------------------
    let expired: serde_json::Value = api
        .post(format!("{base}/api/me/api-keys"))
        .header("x-test-user", user.to_string())
        .json(&serde_json::json!({ "name": "expired", "expires_in_days": 1 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let expired_token = expired["token"].as_str().unwrap().to_string();
    assert_eq!(models(Some(expired_token.clone())).await.unwrap().status(), 200);
    sqlx::query("UPDATE api_keys SET expires_at = now() - interval '1 hour' WHERE id = $1")
        .bind(Uuid::parse_str(expired["id"].as_str().unwrap()).unwrap())
        .execute(&pg)
        .await
        .unwrap();
    assert_eq!(
        models(Some(expired_token)).await.unwrap().status(),
        401,
        "an expired key stops authenticating"
    );

    // --- Revocation ----------------------------------------------------------
    let id = created["id"].as_str().unwrap();
    let del = api
        .delete(format!("{base}/api/me/api-keys/{id}"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
    assert_eq!(
        models(Some(token)).await.unwrap().status(),
        401,
        "a revoked key stops authenticating"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_users_key_cannot_be_revoked() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    set_public_api(&pg, true).await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let owner = mk_user(&pg).await;
    let other = mk_user(&pg).await;

    let created: serde_json::Value = api
        .post(format!("{base}/api/me/api-keys"))
        .header("x-test-user", owner.to_string())
        .json(&serde_json::json!({ "name": "owned" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = created["token"].as_str().unwrap().to_string();
    let id = created["id"].as_str().unwrap();

    // Answered as though it were not there, and it still works.
    let del = api
        .delete(format!("{base}/api/me/api-keys/{id}"))
        .header("x-test-user", other.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
    let still = api
        .get(format!("{base}/v1/models"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(still.status(), 200, "someone else's revoke did nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrol_only_session_cannot_mint_a_key() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    set_public_api(&pg, true).await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;

    let res = api
        .post(format!("{base}/api/me/api-keys"))
        .header("x-test-user", user.to_string())
        .header("x-test-enrol-only", "1")
        .json(&serde_json::json!({ "name": "premature" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        403,
        "a long-lived credential cannot be minted from a session that has not finished enrolling"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switched_off_the_surface_and_its_keys_disappear() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _exclusive = SURFACE.write().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;

    set_public_api(&pg, true).await;
    let created: serde_json::Value = api
        .post(format!("{base}/api/me/api-keys"))
        .header("x-test-user", user.to_string())
        .json(&serde_json::json!({ "name": "before" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = created["token"].as_str().unwrap().to_string();

    set_public_api(&pg, false).await;

    // 404 rather than 403: the feature is absent, not forbidden. And with a
    // valid key presented, so this proves the check runs ahead of authentication
    // rather than merely coinciding with a refusal.
    let v1 = api
        .get(format!("{base}/v1/models"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(v1.status(), 404, "the programmatic surface is gone");

    let mgmt = api
        .get(format!("{base}/api/me/api-keys"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(mgmt.status(), 404, "so is key management");

    // Leave the instance as it ships.
    set_public_api(&pg, true).await;
}
