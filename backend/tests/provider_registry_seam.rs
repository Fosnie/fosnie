//! Extension-surface proof for the `ProviderRegistry` seam.
//!
//! A `FakeProviderRegistry` injected via `AppStateBuilder::with_providers` returns
//! a fixed provider for the `llm` role; `ml::provider_overrides` must then build
//! that provider into the override map the backend ships in the ML request body.
//! Proves the slot is consumed and the `pub` surface suffices for a future
//! `fosnie-enterprise` org-policy wrapper. Requires the isolated routing test DB.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::config::BootConfig;
use fosnie_backend::error::Result;
use fosnie_backend::ext::{ProviderRegistry, ResolvedProvider};
use fosnie_backend::state::{AppState, AppStateBuilder};

/// Returns a fixed external provider for `llm`, nothing for other roles.
struct FakeProviderRegistry;

#[async_trait]
impl ProviderRegistry for FakeProviderRegistry {
    async fn resolve(&self, _pool: &PgPool, role: &str, _user_id: Option<Uuid>) -> Result<Option<ResolvedProvider>> {
        if role == "llm" {
            Ok(Some(ResolvedProvider {
                base_url: Some("https://api.anthropic.example/v1".into()),
                model: Some("claude-test".into()),
                api_key: Some("sk-secret".into()),
                enabled: true,
                reasoning_mode: None,
            }))
        } else {
            Ok(None)
        }
    }
}

fn state_with(providers: Option<Arc<dyn ProviderRegistry>>, unavailable: bool) -> AppState {
    let url = if unavailable { "postgres://routing_test:routing_test_only@127.0.0.1:1/routing_unavailable".into() }
        else { std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL").expect("set FOSNIE_ROUTING_TEST_DATABASE_URL to the isolated routing test DB") };
    let options: sqlx::postgres::PgConnectOptions = url.parse().unwrap();
    assert!(options.get_database().is_some_and(|db| db.starts_with("routing_")));
    let pg = PgPoolOptions::new().acquire_timeout(std::time::Duration::from_millis(300)).connect_lazy_with(options);
    let redis = fosnie_backend::cache::create_pool("redis://localhost:6379").expect("redis pool");
    let mut b = AppStateBuilder::new(pg, redis, Arc::new(BootConfig::default()));
    if let Some(p) = providers {
        b = b.with_providers(p);
    }
    b.build()
}

#[tokio::test]
async fn injected_provider_is_built_into_overrides() {
    let state = state_with(Some(Arc::new(FakeProviderRegistry)), false);
    let map = fosnie_backend::ml::provider_overrides(&state, None).await.unwrap();
    assert_eq!(map.get("llm_base_url").and_then(|v| v.as_str()), Some("https://api.anthropic.example/v1"));
    assert_eq!(map.get("llm_model").and_then(|v| v.as_str()), Some("claude-test"));
    assert_eq!(map.get("llm_api_key").and_then(|v| v.as_str()), Some("sk-secret"));
    // Roles the fake doesn't configure are absent ⇒ ML keeps its defaults.
    assert!(!map.contains_key("embed_base_url"));
}

#[tokio::test]
async fn r24_database_failure_never_becomes_environment_fallback() {
    let state = state_with(None, true);
    assert!(fosnie_backend::ml::provider_overrides(&state, None).await.is_err());
}

#[tokio::test]
async fn r32_explicit_selection_cannot_bypass_injected_registry() {
    let state = state_with(Some(Arc::new(FakeProviderRegistry)), true);
    assert!(fosnie_backend::providers::resolve_llm(&state, None, None, Some(Uuid::now_v7())).await.is_err());
}

#[test]
fn provider_debug_redacts_credentials() {
    let provider = ResolvedProvider { base_url: Some("https://secret-host.example".into()), model: Some("test".into()), api_key: Some("sk-secret".into()), enabled: true, reasoning_mode: None };
    let debug = format!("{provider:?}");
    assert!(!debug.contains("sk-secret"));
    assert!(!debug.contains("secret-host"));
}
