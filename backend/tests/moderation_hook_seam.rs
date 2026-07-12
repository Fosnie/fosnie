//! Extension-surface proof for the `ModerationHook` seam.
//!
//! A `FakeModerationHook` (counts calls) injected via
//! `AppStateBuilder::with_moderation` is invoked by the post-turn path
//! (`state.moderation.on_turn_completed`), proving the slot is consumed: Enterprise
//! registers its accountability subsystem here and the Core
//! default becomes a no-op. No DB: the fake touches nothing.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use fosnie_backend::config::BootConfig;
use fosnie_backend::ext::ModerationHook;
use fosnie_backend::state::{AppState, AppStateBuilder};

struct FakeModerationHook {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl ModerationHook for FakeModerationHook {
    async fn on_turn_completed(
        &self,
        _state: &AppState,
        _user_id: Uuid,
        _chat_id: Uuid,
        _message_id: Uuid,
        _project_id: Option<Uuid>,
        _prompt: String,
    ) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn injected_hook_is_invoked_post_turn() {
    let calls = Arc::new(AtomicU32::new(0));
    let pg = PgPoolOptions::new().connect_lazy("postgres://localhost/pai_test").expect("lazy pg pool");
    let redis = fosnie_backend::cache::create_pool("redis://localhost:6379").expect("redis pool");
    let state = AppStateBuilder::new(pg, redis, Arc::new(BootConfig::default()))
        .with_moderation(Arc::new(FakeModerationHook { calls: calls.clone() }))
        .build();

    state
        .moderation
        .on_turn_completed(&state, Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7(), None, "hello".into())
        .await;

    assert_eq!(calls.load(Ordering::SeqCst), 1, "the post-turn path consumed the injected hook");
}
