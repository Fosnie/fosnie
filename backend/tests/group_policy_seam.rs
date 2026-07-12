//! Extension-surface proof for the `GroupMembershipPolicy` seam + the
//! `data_owner_approval` edition capability.
//!
//! Swap-test (no DB): a `FakeGroupMembershipPolicy` injected via
//! `AppStateBuilder::with_group_policy` is consumed by `state.group_policy.gate_add`;
//! `DirectAddPolicy` (the Core default) returns `Direct`.
//!
//! The DB-backed approval-inbox capability gating lives in the Enterprise test suite
//! (`enterprise/tests/group_requests_gating.rs`) now that the endpoints moved there.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::AppError;
use fosnie_backend::ext::{AddOutcome, DirectAddPolicy, GroupMembershipPolicy};
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::cache;
use sqlx::postgres::PgPoolOptions;

struct FakeGroupMembershipPolicy {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl GroupMembershipPolicy for FakeGroupMembershipPolicy {
    async fn gate_add(&self, _state: &AppState, _ctx: &AuthContext, _group: Uuid, _target: Uuid) -> Result<AddOutcome, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(AddOutcome::Pending(Uuid::nil()))
    }
}

fn lazy_state(policy: Arc<dyn GroupMembershipPolicy>) -> AppState {
    let pg = PgPoolOptions::new().connect_lazy("postgres://localhost/pai_test").expect("lazy pg pool");
    let redis = cache::create_pool("redis://localhost:6379").expect("redis pool");
    AppStateBuilder::new(pg, redis, Arc::new(BootConfig::default()))
        .with_group_policy(policy)
        .build()
}

fn ctx() -> AuthContext {
    AuthContext { user_id: Some(Uuid::now_v7()), email: None, display_name: None, role: PlatformRole::User, break_glass: false, mfa_enroll_only: false }
}

#[tokio::test]
async fn injected_policy_is_consumed() {
    let calls = Arc::new(AtomicU32::new(0));
    let state = lazy_state(Arc::new(FakeGroupMembershipPolicy { calls: calls.clone() }));
    let out = state.group_policy.gate_add(&state, &ctx(), Uuid::now_v7(), Uuid::now_v7()).await.expect("gate_add");
    assert!(matches!(out, AddOutcome::Pending(_)), "the add path consumed the injected policy");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn direct_add_policy_adds_directly() {
    let state = lazy_state(Arc::new(DirectAddPolicy));
    let out = DirectAddPolicy.gate_add(&state, &ctx(), Uuid::now_v7(), Uuid::now_v7()).await.expect("gate_add");
    assert!(matches!(out, AddOutcome::Direct), "Core default adds directly (no approval)");
}
