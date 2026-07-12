//! Extension-surface proof for the `ProviderRegistry` seam.
//!
//! A `FakeProviderRegistry` injected via `AppStateBuilder::with_providers` returns
//! a fixed provider for the `llm` role; `ml::provider_overrides` must then build
//! that provider into the override map the backend ships in the ML request body.
//! Proves the slot is consumed and the `pub` surface suffices for a future
//! `fosnie-enterprise` org-policy wrapper. No live ML/DB needed.

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

fn state_with(providers: Option<Arc<dyn ProviderRegistry>>) -> AppState {
    let pg = PgPoolOptions::new().connect_lazy("postgres://localhost/pai_test").expect("lazy pg pool");
    let redis = fosnie_backend::cache::create_pool("redis://localhost:6379").expect("redis pool");
    let mut b = AppStateBuilder::new(pg, redis, Arc::new(BootConfig::default()));
    if let Some(p) = providers {
        b = b.with_providers(p);
    }
    b.build()
}

#[tokio::test]
async fn injected_provider_is_built_into_overrides() {
    let state = state_with(Some(Arc::new(FakeProviderRegistry)));
    let map = fosnie_backend::ml::provider_overrides(&state, None).await;
    assert_eq!(map.get("llm_base_url").and_then(|v| v.as_str()), Some("https://api.anthropic.example/v1"));
    assert_eq!(map.get("llm_model").and_then(|v| v.as_str()), Some("claude-test"));
    assert_eq!(map.get("llm_api_key").and_then(|v| v.as_str()), Some("sk-secret"));
    // Roles the fake doesn't configure are absent ⇒ ML keeps its defaults.
    assert!(!map.contains_key("embed_base_url"));
}

#[tokio::test]
async fn default_registry_yields_empty_overrides() {
    // No override → Core DbProviderRegistry. With no rows (the lazy pool yields no
    // results / errors, both swallowed) the map is empty ⇒ behaviour-identical.
    let state = state_with(None);
    let map = fosnie_backend::ml::provider_overrides(&state, None).await;
    assert!(map.is_empty(), "no provider rows ⇒ no overrides ⇒ ML uses its .env defaults");
}
