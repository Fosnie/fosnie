// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Scheduler job registry (open-core seam).
//!
//! The [`JobRegistry`] collects the scheduler's periodic (cron) jobs and the
//! handlers for durable task kinds that are *not* hard-coded in
//! [`super::handle`]. It is populated at boot by the [`crate::ext::JobRegistrar`]
//! seam: the Core default [`CoreJobs`] registers the host jobs (byte-identical to
//! the previous hard-wired set); a private `fosnie-enterprise` crate can register
//! checkpoint minting + evidence/hold retention instead, and the Core default
//! omits them. Core references no enterprise symbol.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;

use super::TaskType;
use crate::state::AppState;

/// A boxed future returned by a periodic job body.
pub type BoxFut = Pin<Box<dyn Future<Output = ()> + Send>>;
/// A periodic job body — invoked on each cron tick with a fresh `AppState` clone.
pub type PeriodicFn = Arc<dyn Fn(AppState) -> BoxFut + Send + Sync>;

/// Handles one durable task kind dispatched by string key (the `task_type` text).
/// Used for kinds not known to the Core [`super::handle`] match — Enterprise
/// registers its kinds (e.g. `audit_checkpoint`) here.
#[async_trait]
pub trait TaskHandler: Send + Sync {
    async fn handle(&self, state: &AppState, payload: &serde_json::Value) -> Result<(), String>;
}

/// A registered periodic job: a 6-field cron expression + its body.
pub struct PeriodicSpec {
    pub cron: String,
    pub run: PeriodicFn,
}

/// The scheduler's registry of periodic jobs + extra task handlers, assembled at
/// boot from the [`crate::ext::JobRegistrar`] seam.
#[derive(Default)]
pub struct JobRegistry {
    pub periodic: Vec<PeriodicSpec>,
    handlers: HashMap<String, Arc<dyn TaskHandler>>,
}

impl JobRegistry {
    /// Register a periodic (cron) job. `cron` is a 6-field expression
    /// (`sec min hour dom mon dow`).
    pub fn register_periodic_job(&mut self, cron: &str, run: PeriodicFn) {
        self.periodic.push(PeriodicSpec { cron: cron.to_string(), run });
    }

    /// Register a handler for a durable task kind keyed by its `task_type` string.
    pub fn register_task_handler(&mut self, key: &str, handler: Arc<dyn TaskHandler>) {
        self.handlers.insert(key.to_string(), handler);
    }

    /// The registered handler for `key`, if any.
    pub fn task_handler(&self, key: &str) -> Option<&Arc<dyn TaskHandler>> {
        self.handlers.get(key)
    }
}

/// Periodic body that enqueues a typed Core task onto the durable queue.
fn periodic_enqueue(tt: TaskType, reason: &'static str) -> PeriodicFn {
    Arc::new(move |state: AppState| {
        Box::pin(async move {
            match super::enqueue(&state.pg, tt, serde_json::json!({ "reason": reason })).await {
                Ok(id) => tracing::info!(%id, task = tt.as_key(), "enqueued periodic task"),
                Err(e) => tracing::error!(error = %e, task = tt.as_key(), "failed to enqueue periodic task"),
            }
        })
    })
}

/// The Core [`ext::JobRegistrar`](crate::ext::JobRegistrar): registers only the
/// genuinely-Core periodic jobs (audit-partition retention, MCP health, artefact
/// cleanup). The Enterprise edition registers checkpoint minting + moderation/
/// evidence retention through its own registrar via
/// [`crate::state::AppStateBuilder::with_jobs`].
pub struct CoreJobs;

impl crate::ext::JobRegistrar for CoreJobs {
    fn register(&self, reg: &mut JobRegistry) {
        // Daily audit retention (audit_events partition-drop). 03:00.
        reg.register_periodic_job("0 0 3 * * *", periodic_enqueue(TaskType::AuditRetention, "daily"));
        // MCP connection-manager sweep (FEATURE B1) — every minute.
        reg.register_periodic_job("0 * * * * *", periodic_enqueue(TaskType::McpHealth, "periodic"));
        // Daily orphaned-artefact sweep. 03:30.
        reg.register_periodic_job("0 30 3 * * *", periodic_enqueue(TaskType::ArtefactCleanup, "daily"));
        // Daily sweep of aged API conversations. 03:45. A no-op unless a
        // retention period has been configured.
        reg.register_periodic_job("0 45 3 * * *", periodic_enqueue(TaskType::ApiChatCleanup, "daily"));
    }
}
