//! Integration tests for the health endpoints. Driven via `oneshot` (no socket
//! bind). Skips cleanly when `DATABASE_URL` is unset so a checkout without the
//! dev stack still passes.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{cache, db, http};

async fn state_from_env() -> Option<AppState> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;

    let mut boot = BootConfig {
        database_url: db_url,
        redis_url,
        ..BootConfig::default()
    };
    boot.server.static_dir = "___no_spa___".into(); // ensure no fallback service
    Some(AppState::new(pg, redis, Arc::new(boot)))
}

#[tokio::test]
async fn liveness_returns_200() {
    let Some(state) = state_from_env().await else {
        eprintln!("skipping liveness_returns_200: DATABASE_URL unset");
        return;
    };
    let app = http::router(state, None, None, None, None);
    let resp = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Swap-test for the edition public-route slot (open-core seam): a route mounted
/// via `extra_public` is reachable with no auth header, proving the slot merges
/// into the *public* router ahead of any auth layer. Enterprise mounts its SCIM
/// server here; Core passes `None` and the route does not exist.
#[tokio::test]
async fn public_slot_route_is_reachable_without_auth() {
    let Some(state) = state_from_env().await else {
        eprintln!("skipping public_slot_route_is_reachable_without_auth: DATABASE_URL unset");
        return;
    };
    let extra_public: Router<AppState> =
        Router::new().route("/__swaptest/public", get(|| async { "ok" }));
    let app = http::router(state, None, None, None, Some(extra_public));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/__swaptest/public")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn readiness_returns_200_when_deps_up() {
    let Some(state) = state_from_env().await else {
        eprintln!("skipping readiness_returns_200_when_deps_up: DATABASE_URL unset");
        return;
    };
    let app = http::router(state, None, None, None, None);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
