//! Bearer-JWT validation + role normalisation against a live Keycloak — the
//! pinning gate. Skips when Keycloak or the DB is unreachable.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{auth, cache, db, http};

const ISSUER: &str = "http://localhost:8081/realms/fosnie";

async fn mint_token(username: &str, password: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{ISSUER}/protocol/openid-connect/token"))
        .form(&[
            ("grant_type", "password"),
            ("client_id", "fosnie"),
            ("client_secret", "fosnie-secret"),
            ("username", username),
            ("password", password),
            ("scope", "openid profile email"),
        ])
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    json["access_token"].as_str().map(|s| s.to_string())
}

async fn state() -> Option<AppState> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;

    let mut boot = BootConfig::default();
    boot.keycloak.url = "http://localhost:8081".into();
    boot.keycloak.realm = "fosnie".into();
    boot.keycloak.client_id = "fosnie".into();
    boot.server.static_dir = "___no_spa___".into();
    Some(AppState::new(pg, redis, Arc::new(boot)))
}

// Multi-thread runtime: the Keycloak crate's OIDC discovery runs on a spawned
// task that must start before the first `poll_ready` (matches how `main` runs).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whoami_rejects_anon_and_normalises_admin() {
    let Some(state) = state().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let Some(token) = mint_token("alice", "alice").await else {
        eprintln!("skipping: Keycloak unreachable");
        return;
    };

    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance, state.boot.keycloak.client_id.clone());
    let app = http::router(state, Some(kc), None, None, None);

    // No credential → 401.
    let anon = app
        .clone()
        .oneshot(Request::builder().uri("/api/whoami").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);

    // Valid admin token → 200 and role normalised to client_admin.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/whoami")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["role"], "client_admin", "Keycloak 'admin' normalises to client_admin");
}
