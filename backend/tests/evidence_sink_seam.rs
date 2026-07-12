//! Extension-surface proof for the `EvidenceSink` seam.
//!
//! A `FakeEvidenceSink` (returns a fixed hash, or `None`) injected via
//! `AppStateBuilder::with_evidence` is consumed by the chat path
//! (`state.evidence.capture`), proving the slot is wired: Enterprise registers
//! the real sink here and the Core default becomes a no-op.
//! No DB: the fake touches nothing.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;

use fosnie_backend::audit::EvidenceInput;
use fosnie_backend::config::BootConfig;
use fosnie_backend::ext::EvidenceSink;
use fosnie_backend::state::{AppState, AppStateBuilder};

struct FakeEvidenceSink {
    hash: Option<String>,
}

#[async_trait]
impl EvidenceSink for FakeEvidenceSink {
    async fn capture(&self, _state: &AppState, _input: EvidenceInput) -> Option<String> {
        self.hash.clone()
    }
}

fn state_with(hash: Option<String>) -> AppState {
    let pg = PgPoolOptions::new().connect_lazy("postgres://localhost/pai_test").expect("lazy pg pool");
    let redis = fosnie_backend::cache::create_pool("redis://localhost:6379").expect("redis pool");
    AppStateBuilder::new(pg, redis, Arc::new(BootConfig::default()))
        .with_evidence(Arc::new(FakeEvidenceSink { hash }))
        .build()
}

#[tokio::test]
async fn injected_sink_hash_is_consumed() {
    let state = state_with(Some("deadbeef".into()));
    let out = state.evidence.capture(&state, EvidenceInput::default()).await;
    assert_eq!(out.as_deref(), Some("deadbeef"), "the chat path consumed the injected sink");
}

#[tokio::test]
async fn injected_sink_none_is_consumed() {
    let state = state_with(None);
    let out = state.evidence.capture(&state, EvidenceInput::default()).await;
    assert_eq!(out, None, "a disabled/failed sink yields None (chain unbound, graceful)");
}
