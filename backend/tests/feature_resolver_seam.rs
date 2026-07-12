//! Extension-surface proof for the `FeatureResolver` seam.
//!
//! Demonstrates two things a future private `fosnie-enterprise` crate relies on:
//!   1. The slot is actually consumed — a resolver injected via
//!      `AppStateBuilder::with_features` is what the gate path sees, NOT the Core
//!      `HostFeatureResolver` default.
//!   2. The `pub` surface is sufficient for an *external* crate — `FakeResolver`
//!      below is defined entirely outside `fosnie_backend` (this is a separate test
//!      crate) and implements the trait with no access to Core internals.
//!
//! Needs no database: the fake resolver short-circuits before any SQL, so the
//! pools are built lazily and never connect.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use fosnie_backend::config::BootConfig;
use fosnie_backend::ext::FeatureResolver;
use fosnie_backend::state::{AppState, AppStateBuilder};

/// A resolver external to Core that ignores host/group state and returns a fixed
/// answer. Mirrors how Enterprise would inject licence-aware logic.
struct FakeResolver(bool);

#[async_trait]
impl FeatureResolver for FakeResolver {
    async fn enabled_for_user(&self, _state: &AppState, _user_id: Option<Uuid>, _feature: &str) -> bool {
        self.0
    }
}

/// Build an `AppState` against lazy (never-connecting) pools and the given
/// resolver. Boot defaults leave every `features.*` flag OFF, so the Core
/// `HostFeatureResolver` would answer `false` for any feature — making a `true`
/// from the gate path unambiguous evidence the injected resolver won.
fn state_with(resolver: Option<Arc<dyn FeatureResolver>>) -> AppState {
    let pg = PgPoolOptions::new()
        .connect_lazy("postgres://localhost/pai_test")
        .expect("lazy pg pool");
    let redis = fosnie_backend::cache::create_pool("redis://localhost:6379").expect("redis pool");
    let boot = Arc::new(BootConfig::default()); // all feature flags default OFF
    let mut builder = AppStateBuilder::new(pg, redis, boot);
    if let Some(r) = resolver {
        builder = builder.with_features(r);
    }
    builder.build()
}

#[tokio::test]
async fn injected_resolver_overrides_host_default() {
    // Host default would say `false` (global voice OFF); the fake forces `true`.
    let state = state_with(Some(Arc::new(FakeResolver(true))));
    assert!(
        state.features.enabled_for_user(&state, Some(Uuid::now_v7()), "voice").await,
        "the gate path must see the injected resolver, not HostFeatureResolver"
    );

    // The free-function delegators route through the same slot.
    assert!(
        fosnie_backend::features::enabled_for_user(&state, Some(Uuid::now_v7()), "voice").await,
        "features::enabled_for_user must delegate to the slot"
    );
}

#[tokio::test]
async fn default_resolver_honours_host_ceiling() {
    // No override → Core default. Voice is OFF in boot defaults, so the gate is
    // closed — confirming the difference above is the resolver, not the config.
    let state = state_with(None);
    assert!(
        !state.features.enabled_for_user(&state, None, "voice").await,
        "default HostFeatureResolver respects the (off) host ceiling"
    );
}
