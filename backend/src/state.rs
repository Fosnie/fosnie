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

//! Shared application state handed to HTTP handlers and the scheduler.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use deadpool_redis::Pool as RedisPool;
use sqlx::PgPool;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::config::BootConfig;
use crate::ws::hub::Hub;

/// Process-local set of tabular-review ids requested to stop. The REST cancel
/// handler inserts an id; the background generator checks + clears it between
/// cells. Process-local is fine (single process); a multi-process split would
/// move this to Redis alongside the WS session state.
#[derive(Clone, Default)]
pub struct Cancellations(Arc<Mutex<HashSet<Uuid>>>);

impl Cancellations {
    pub fn request(&self, id: Uuid) {
        self.0.lock().unwrap().insert(id);
    }

    /// True if `id` was marked for cancellation; clears the flag.
    pub fn take(&self, id: Uuid) -> bool {
        self.0.lock().unwrap().remove(&id)
    }
}

/// In-process waiters for interactive agent-run approvals. The run awaits a
/// `oneshot`; the approve/reject REST handler delivers the decision here. The
/// durable `agent_runs` row + the atomic CAS in `agent::decide` are the source of
/// truth — this map is only the fast path for a live socket (a dropped waiter
/// falls back to the `agent_resume` durable task).
#[derive(Clone, Default)]
pub struct Approvals(Arc<Mutex<HashMap<Uuid, oneshot::Sender<bool>>>>);

impl Approvals {
    /// Register a waiter for `run_id`; await the returned receiver for the decision.
    pub fn register(&self, run_id: Uuid) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.0.lock().unwrap().insert(run_id, tx);
        rx
    }

    /// Deliver a decision to a live waiter. True if one was present (fast path),
    /// false if the socket dropped (caller falls back to the durable resume task).
    pub fn resolve(&self, run_id: Uuid, approve: bool) -> bool {
        if let Some(tx) = self.0.lock().unwrap().remove(&run_id) {
            tx.send(approve).is_ok()
        } else {
            false
        }
    }

    /// Drop a waiter (timeout / run finished) without delivering.
    pub fn forget(&self, run_id: Uuid) {
        self.0.lock().unwrap().remove(&run_id);
    }
}

/// Process-local registry of live-voice sessions, keyed by socket id. A live
/// session spans many `voice.audio.chunk` frames, so — unlike the per-frame batch
/// voice handlers — it must outlive a single frame. The socket's disconnect path
/// (and `voice.stream.end`) removes it; dropping/`shutdown`ing the `Session` tears
/// down its STT/TTS tasks. One live session per socket.
#[derive(Clone, Default)]
pub struct VoiceSessions(Arc<Mutex<HashMap<Uuid, Arc<crate::voice::Session>>>>);

impl VoiceSessions {
    pub fn insert(&self, socket_id: Uuid, session: Arc<crate::voice::Session>) {
        self.0.lock().unwrap().insert(socket_id, session);
    }
    pub fn get(&self, socket_id: Uuid) -> Option<Arc<crate::voice::Session>> {
        self.0.lock().unwrap().get(&socket_id).cloned()
    }
    pub fn remove(&self, socket_id: Uuid) -> Option<Arc<crate::voice::Session>> {
        self.0.lock().unwrap().remove(&socket_id)
    }
}

/// Per-socket streaming-**dictation** sessions (composer mic; STT-only). Separate
/// from [`VoiceSessions`] — the two are mutually exclusive per socket in the UI.
#[derive(Clone, Default)]
pub struct DictationSessions(Arc<Mutex<HashMap<Uuid, Arc<crate::voice::DictationSession>>>>);

impl DictationSessions {
    pub fn insert(&self, socket_id: Uuid, session: Arc<crate::voice::DictationSession>) {
        self.0.lock().unwrap().insert(socket_id, session);
    }
    pub fn get(&self, socket_id: Uuid) -> Option<Arc<crate::voice::DictationSession>> {
        self.0.lock().unwrap().get(&socket_id).cloned()
    }
    pub fn remove(&self, socket_id: Uuid) -> Option<Arc<crate::voice::DictationSession>> {
        self.0.lock().unwrap().remove(&socket_id)
    }
}

/// Cheap to clone (pools are `Arc` internally; `boot` and `hub` wrap `Arc`;
/// `reqwest::Client` is an `Arc` handle to a shared connection pool).
#[derive(Clone)]
pub struct AppState {
    pub pg: PgPool,
    pub redis: RedisPool,
    pub boot: Arc<BootConfig>,
    /// HTTP client to the Python ML service (the LLM client).
    pub http: reqwest::Client,
    /// Process-local WebSocket registry (fan-out + per-turn cancel).
    pub hub: Hub,
    /// Feature-gate resolver (extension seam). Core default is
    /// [`crate::ext::HostFeatureResolver`]; a private `fosnie-enterprise` crate can
    /// inject its own via [`AppStateBuilder::with_features`].
    pub features: Arc<dyn crate::ext::FeatureResolver>,
    /// Request → [`crate::auth::AuthContext`] resolver (extension seam). Core
    /// default is [`crate::auth::keycloak::KeycloakAuthProvider`]; a private
    /// `fosnie-enterprise` crate (or `LocalAuth`) injects its own via
    /// [`AppStateBuilder::with_auth`].
    pub auth: Arc<dyn crate::ext::AuthProvider>,
    /// Access-control policy (extension seam). Core default is
    /// [`crate::auth::rbac::FlatRbacPolicy`]; a private `fosnie-enterprise` crate
    /// (custom roles / delegated admin / ABAC) injects its own via
    /// [`AppStateBuilder::with_rbac`].
    pub rbac: Arc<dyn crate::ext::RbacPolicy>,
    /// Provider registry (extension seam). Core default is
    /// [`crate::providers::DbProviderRegistry`] (runtime provider config in
    /// `provider_configs`); a private `fosnie-enterprise` crate can wrap it with org
    /// policy via [`AppStateBuilder::with_providers`].
    pub providers: Arc<dyn crate::ext::ProviderRegistry>,
    /// Data-source (DMS) connector registry (extension seam). Core default is
    /// [`crate::integrations::dms::DefaultConnectorRegistry`] (dormant `NotBuilt`);
    /// a private `fosnie-enterprise` crate injects real adapters via
    /// [`AppStateBuilder::with_connectors`].
    pub connectors: Arc<dyn crate::ext::ConnectorRegistry>,
    /// Post-turn moderation hook (extension seam). Core default is
    /// [`crate::moderation::CoreModerationHook`] (OFF by default); a private
    /// `fosnie-enterprise` crate injects its accountability subsystem via
    /// [`AppStateBuilder::with_moderation`].
    pub moderation: Arc<dyn crate::ext::ModerationHook>,
    /// Hot-path evidence capture (extension seam). Core default is
    /// [`crate::audit::evidence::CoreEvidenceSink`] (writes `interaction_evidence`);
    /// a private `fosnie-enterprise` crate injects its own via
    /// [`AppStateBuilder::with_evidence`].
    pub evidence: Arc<dyn crate::ext::EvidenceSink>,
    /// Scheduler job registrar (extension seam). Core default is
    /// [`crate::scheduler::CoreJobs`] (host periodic jobs + task handlers); a private
    /// `fosnie-enterprise` crate injects its own via [`AppStateBuilder::with_jobs`].
    pub jobs: Arc<dyn crate::ext::JobRegistrar>,
    /// Hold-gated record retention policy (extension seam). Core default is
    /// [`crate::scheduler::CoreRetentionPolicy`] (legal-hold gate + evidence prune);
    /// a private `fosnie-enterprise` crate injects its own via
    /// [`AppStateBuilder::with_retention`].
    pub retention: Arc<dyn crate::ext::RetentionPolicy>,
    /// Group-membership add policy (extension seam). Core default registers
    /// [`crate::http::group_requests::OwnerApprovalPolicy`] (data-owner approval,
    /// behaviour-identical today); a private `fosnie-enterprise` crate injects its own
    /// via [`AppStateBuilder::with_group_policy`] (a Core-only build's default is
    /// `DirectAddPolicy`).
    pub group_policy: Arc<dyn crate::ext::GroupMembershipPolicy>,
    /// New-user seat gate (extension seam). Core default is
    /// [`crate::ext::UnlimitedSeats`] (no cap); a private `fosnie-enterprise` crate
    /// injects a licence-aware policy via [`AppStateBuilder::with_seats`].
    pub seats: Arc<dyn crate::ext::SeatPolicy>,
    /// Non-Core async-export-kind builders (extension seam). Empty in Core; a private
    /// `fosnie-enterprise` crate registers the `audit` evidence export via
    /// [`AppStateBuilder::with_export_kinds`]. Consulted by
    /// [`crate::http::export::run_export`] for kinds not handled inline.
    pub export_kinds: Arc<crate::http::export::ExportRegistry>,
    /// Tabular reviews requested to stop mid-generation.
    pub cancellations: Cancellations,
    /// In-process waiters for interactive agent-run approvals.
    pub approvals: Approvals,
    /// Live-voice sessions, keyed by socket id.
    pub voice: VoiceSessions,
    pub dictation: DictationSessions,
    /// Live MCP connections (one per approved server), keyed by slug (FEATURE B1).
    pub mcp: crate::mcp::McpManager,
    /// Parsed at-rest DM encryption key (None = encryption disabled).
    pub message_key: Option<[u8; 32]>,
    /// Hot-path audit appends are enqueued here for the writer task, off the
    /// request await path (optimisation audit, L6). Atomic appends bypass this.
    pub audit_tx: mpsc::Sender<crate::audit::AuditEvent>,
    /// The writer task's handle — taken once at shutdown (after the last state
    /// clone drops) so the queue drains before the runtime exits (re-audit R4b).
    pub audit_writer: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl AppState {
    /// Build an `AppState` with Core defaults for every extension seam. Thin
    /// wrapper over [`AppStateBuilder`] so existing call-sites stay unchanged; a
    /// private `fosnie-enterprise` crate uses the builder directly to inject overrides.
    pub fn new(pg: PgPool, redis: RedisPool, boot: Arc<BootConfig>) -> Self {
        AppStateBuilder::new(pg, redis, boot).build()
    }
}

/// Assembles an [`AppState`], letting an external crate substitute extension-seam
/// implementations before construction. `connect`/`migrate` stay in `main`/`run`
/// — the builder only wires already-open pools and the seam slots, so the boot
/// sequence is not duplicated. Each `with_*` setter overrides one seam; an
/// un-set seam falls back to its Core default in [`build`](Self::build).
pub struct AppStateBuilder {
    pg: PgPool,
    redis: RedisPool,
    boot: Arc<BootConfig>,
    features: Option<Arc<dyn crate::ext::FeatureResolver>>,
    auth: Option<Arc<dyn crate::ext::AuthProvider>>,
    rbac: Option<Arc<dyn crate::ext::RbacPolicy>>,
    providers: Option<Arc<dyn crate::ext::ProviderRegistry>>,
    connectors: Option<Arc<dyn crate::ext::ConnectorRegistry>>,
    moderation: Option<Arc<dyn crate::ext::ModerationHook>>,
    evidence: Option<Arc<dyn crate::ext::EvidenceSink>>,
    jobs: Option<Arc<dyn crate::ext::JobRegistrar>>,
    retention: Option<Arc<dyn crate::ext::RetentionPolicy>>,
    group_policy: Option<Arc<dyn crate::ext::GroupMembershipPolicy>>,
    seats: Option<Arc<dyn crate::ext::SeatPolicy>>,
    export_kinds: Option<crate::http::export::ExportRegistry>,
}

impl AppStateBuilder {
    pub fn new(pg: PgPool, redis: RedisPool, boot: Arc<BootConfig>) -> Self {
        Self { pg, redis, boot, features: None, auth: None, rbac: None, providers: None, connectors: None, moderation: None, evidence: None, jobs: None, retention: None, group_policy: None, seats: None, export_kinds: None }
    }

    /// Override the [`FeatureResolver`](crate::ext::FeatureResolver) seam.
    pub fn with_features(mut self, r: Arc<dyn crate::ext::FeatureResolver>) -> Self {
        self.features = Some(r);
        self
    }

    /// Override the [`AuthProvider`](crate::ext::AuthProvider) seam.
    pub fn with_auth(mut self, p: Arc<dyn crate::ext::AuthProvider>) -> Self {
        self.auth = Some(p);
        self
    }

    /// Override the [`RbacPolicy`](crate::ext::RbacPolicy) seam.
    pub fn with_rbac(mut self, p: Arc<dyn crate::ext::RbacPolicy>) -> Self {
        self.rbac = Some(p);
        self
    }

    /// Override the [`ProviderRegistry`](crate::ext::ProviderRegistry) seam.
    pub fn with_providers(mut self, p: Arc<dyn crate::ext::ProviderRegistry>) -> Self {
        self.providers = Some(p);
        self
    }

    /// Override the [`ConnectorRegistry`](crate::ext::ConnectorRegistry) seam.
    pub fn with_connectors(mut self, c: Arc<dyn crate::ext::ConnectorRegistry>) -> Self {
        self.connectors = Some(c);
        self
    }

    /// Override the [`ModerationHook`](crate::ext::ModerationHook) seam.
    pub fn with_moderation(mut self, m: Arc<dyn crate::ext::ModerationHook>) -> Self {
        self.moderation = Some(m);
        self
    }

    /// Override the [`EvidenceSink`](crate::ext::EvidenceSink) seam.
    pub fn with_evidence(mut self, e: Arc<dyn crate::ext::EvidenceSink>) -> Self {
        self.evidence = Some(e);
        self
    }

    /// Override the [`JobRegistrar`](crate::ext::JobRegistrar) seam.
    pub fn with_jobs(mut self, j: Arc<dyn crate::ext::JobRegistrar>) -> Self {
        self.jobs = Some(j);
        self
    }

    /// Override the [`RetentionPolicy`](crate::ext::RetentionPolicy) seam.
    pub fn with_retention(mut self, r: Arc<dyn crate::ext::RetentionPolicy>) -> Self {
        self.retention = Some(r);
        self
    }

    /// Override the [`GroupMembershipPolicy`](crate::ext::GroupMembershipPolicy) seam.
    pub fn with_group_policy(mut self, p: Arc<dyn crate::ext::GroupMembershipPolicy>) -> Self {
        self.group_policy = Some(p);
        self
    }

    /// Override the [`SeatPolicy`](crate::ext::SeatPolicy) seam.
    pub fn with_seats(mut self, p: Arc<dyn crate::ext::SeatPolicy>) -> Self {
        self.seats = Some(p);
        self
    }

    /// Register the non-Core export kinds (e.g. Enterprise `audit`).
    pub fn with_export_kinds(mut self, r: crate::http::export::ExportRegistry) -> Self {
        self.export_kinds = Some(r);
        self
    }

    pub fn build(self) -> AppState {
        let AppStateBuilder { pg, redis, boot, features, auth, rbac, providers, connectors, moderation, evidence, jobs, retention, group_policy, seats, export_kinds } = self;
        let http = build_ml_client(&boot.ml.shared_secret);
        let (audit_tx, audit_writer) = crate::audit::spawn_writer(pg.clone());
        // The DEK is resolved by the KeyProvider seam at boot (`install_key_provider`);
        // if that has not run (Core builds/tests that skip it), fall back to the legacy
        // config key. `message_key` is the active DEK bytes for the single-key call
        // sites; multi-key reads (post-rotation) go through the global keyring.
        crate::crypto::ensure_keyring_from_legacy(&boot.message_encryption_key);
        let message_key = crate::crypto::keyring().active_key();
        if boot.message_encryption_key.trim().is_empty() && message_key.is_none() {
            tracing::warn!("message_encryption_key is empty — direct messages are stored in plaintext");
        } else if !boot.message_encryption_key.trim().is_empty() && message_key.is_none() {
            tracing::warn!("message_encryption_key is set but not a valid base64 32-byte key — DMs stored in plaintext");
        }
        AppState {
            pg,
            redis,
            boot,
            http,
            hub: Hub::new(),
            features: features.unwrap_or_else(|| Arc::new(crate::ext::HostFeatureResolver)),
            auth: auth.unwrap_or_else(|| Arc::new(crate::auth::keycloak::KeycloakAuthProvider)),
            rbac: rbac.unwrap_or_else(|| Arc::new(crate::auth::rbac::FlatRbacPolicy)),
            providers: providers
                .unwrap_or_else(|| Arc::new(crate::providers::DbProviderRegistry::new(message_key))),
            connectors: connectors
                .unwrap_or_else(|| Arc::new(crate::integrations::dms::DefaultConnectorRegistry)),
            moderation: moderation.unwrap_or_else(|| Arc::new(crate::ext::NoopModerationHook)),
            evidence: evidence.unwrap_or_else(|| Arc::new(crate::ext::NoopEvidenceSink)),
            jobs: jobs.unwrap_or_else(|| Arc::new(crate::scheduler::CoreJobs)),
            retention: retention.unwrap_or_else(|| Arc::new(crate::ext::NoopRetentionPolicy)),
            group_policy: group_policy.unwrap_or_else(|| Arc::new(crate::ext::DirectAddPolicy)),
            seats: seats.unwrap_or_else(|| Arc::new(crate::ext::UnlimitedSeats)),
            export_kinds: Arc::new(export_kinds.unwrap_or_default()),
            cancellations: Cancellations::default(),
            approvals: Approvals::default(),
            voice: VoiceSessions::default(),
            dictation: DictationSessions::default(),
            mcp: crate::mcp::McpManager::new(),
            message_key,
            audit_tx,
            audit_writer: Arc::new(Mutex::new(audit_writer)),
        }
    }
}

/// The ML HTTP client carries the shared secret as a default `X-PAI-ML-Key`
/// header on every request (the ML service rejects calls without it). `state.http`
/// only ever targets the ML service, so a default header cannot leak elsewhere.
/// An empty secret (dev) yields a plain client — the ML side then runs open.
pub(crate) fn build_ml_client(secret: &str) -> reqwest::Client {
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    if secret.is_empty() {
        return reqwest::Client::new();
    }
    match HeaderValue::from_str(secret) {
        Ok(mut value) => {
            value.set_sensitive(true);
            let mut headers = HeaderMap::new();
            headers.insert(HeaderName::from_static("x-pai-ml-key"), value);
            reqwest::Client::builder()
                .default_headers(headers)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        }
        Err(_) => {
            tracing::warn!("ml.shared_secret contains invalid header characters; ML calls will be unauthenticated");
            reqwest::Client::new()
        }
    }
}
