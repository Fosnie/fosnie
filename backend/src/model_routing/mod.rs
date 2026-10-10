// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0

//! Model routing is part of Core. Policy extensions can narrow the eligible pool;
//! no edition-specific implementation is required to select a model.

pub mod types;
pub mod target;
pub mod engine;
pub mod store;
pub mod budget;
pub mod classifier;
pub mod schema;
pub mod service;
pub mod fallback;
pub mod failure;
pub mod health;
pub mod learning;
pub mod quality;
pub mod lease;
pub mod dispatch;
pub mod fixed;
pub mod fixed_lease;
pub mod mixed;
pub mod transport;
pub mod run;
pub mod admission;
pub mod ingestion;
pub(crate) mod durable;
#[cfg(test)]
mod engine_tests;
#[cfg(test)]
mod lease_tests;
pub use types::*;
pub use target::{InferenceEnvelope, InferenceTarget, resolve_target};

#[async_trait::async_trait]
pub trait InferencePolicy: Send + Sync {
    /// Opt-in: avoid a judge used only to choose among a single known candidate.
    fn single_candidate_fast_path(&self) -> bool { false }
    /// Additional restrictions only. The caller independently enforces ownership,
    /// deployment restrictions and capabilities, then intersects this result.
    async fn constraints(
        &self,
        pool: &sqlx::PgPool,
        context: &RoutingContext,
    ) -> crate::Result<RoutingConstraints>;
}

#[derive(Default)]
pub struct CoreInferencePolicy;

#[async_trait::async_trait]
impl InferencePolicy for CoreInferencePolicy {
    async fn constraints(&self, _pool: &sqlx::PgPool, _context: &RoutingContext) -> crate::Result<RoutingConstraints> {
        Ok(RoutingConstraints::default())
    }
}

pub mod reindex;
