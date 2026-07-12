//! Extension-surface proof for the `AuthProvider` seam.
//!
//! Mirrors `feature_resolver_seam.rs`: a `FakeAuthProvider` defined in this
//! external test crate is injected via `AppStateBuilder::with_auth`, and the
//! `AuthUser` extractor is shown to return that fake `AuthContext` — proving the
//! slot is consumed and the `pub` surface suffices for a future `fosnie-enterprise`
//! (or `LocalAuth`) provider.
//!
//! No live Keycloak/DB: the fake ignores `parts`/`pg`. The default
//! `KeycloakAuthProvider` would instead 401 on the empty request extensions, so
//! a successful fake context is unambiguous evidence the slot won.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use fosnie_backend::auth::keycloak::AuthUser;
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::AppError;
use fosnie_backend::ext::AuthProvider;
use fosnie_backend::state::{AppState, AppStateBuilder};

/// A provider external to Core that returns a fixed identity regardless of the
/// request. Stands in for an Enterprise SAML / LocalAuth provider.
struct FakeAuthProvider(AuthContext);

#[async_trait]
impl AuthProvider for FakeAuthProvider {
    async fn authenticate(&self, _parts: &mut Parts, _state: &AppState) -> Result<AuthContext, AppError> {
        Ok(self.0.clone())
    }
}

fn state_with_auth(provider: Arc<dyn AuthProvider>) -> AppState {
    let pg = PgPoolOptions::new()
        .connect_lazy("postgres://localhost/pai_test")
        .expect("lazy pg pool");
    let redis = fosnie_backend::cache::create_pool("redis://localhost:6379").expect("redis pool");
    let boot = Arc::new(BootConfig::default());
    AppStateBuilder::new(pg, redis, boot).with_auth(provider).build()
}

fn empty_parts() -> Parts {
    axum::http::Request::builder()
        .uri("/")
        .body(())
        .expect("request")
        .into_parts()
        .0
}

#[tokio::test]
async fn injected_auth_provider_is_used_by_extractor() {
    let uid = Uuid::now_v7();
    let fake = AuthContext {
        user_id: Some(uid),
        email: Some("fake@example.test".into()),
        display_name: Some("Fake User".into()),
        role: PlatformRole::PowerUser,
        break_glass: false, mfa_enroll_only: false,
    };
    let state = state_with_auth(Arc::new(FakeAuthProvider(fake)));

    let mut parts = empty_parts();
    let AuthUser(ctx) = AuthUser::from_request_parts(&mut parts, &state)
        .await
        .expect("extractor must succeed via the injected provider");

    assert_eq!(ctx.user_id, Some(uid), "extractor returned the fake provider's context");
    assert_eq!(ctx.role, PlatformRole::PowerUser);
    assert_eq!(ctx.email.as_deref(), Some("fake@example.test"));
}

#[tokio::test]
async fn default_provider_rejects_unauthenticated_request() {
    // No override → Core KeycloakAuthProvider. With empty extensions (no token),
    // the request is rejected — confirming the success above came from the slot.
    let state = AppState::new(
        PgPoolOptions::new().connect_lazy("postgres://localhost/pai_test").unwrap(),
        fosnie_backend::cache::create_pool("redis://localhost:6379").unwrap(),
        Arc::new(BootConfig::default()),
    );
    let mut parts = empty_parts();
    let res = AuthUser::from_request_parts(&mut parts, &state).await;
    assert!(res.is_err(), "default provider must reject a request with no token");
}
