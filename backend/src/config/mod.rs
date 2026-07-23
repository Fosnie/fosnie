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

//! Configuration.
//!
//! Three layers, of which the platform owns only the lower two:
//!   * **deployment layer** — external services' versions/flags (not this
//!     module);
//!   * **boot-time** — this module: endpoints, secrets, ports, paths, read once
//!     at startup from defaults → optional TOML file → `PAI__` env vars;
//!   * **runtime-mutable** — [`runtime`]: typed validated rows in
//!     `config_settings`, edited from the UI, every change audited.
//!
//! Derived parameters such as `max_model_len` are *learned* from vLLM, never
//! stored — they are not represented here.

pub mod runtime;

use std::collections::HashMap;

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

/// Boot-time configuration. Built once at startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootConfig {
    pub server: ServerConfig,
    /// Postgres DSN, e.g. `postgres://pai:pai@localhost:5433/pai`.
    pub database_url: String,
    /// Redis URL, e.g. `redis://localhost:6379`.
    pub redis_url: String,
    /// Postgres connection-pool tuning. Optional — defaults suit the local
    /// container; lower `max_connections` for a small managed tier.
    #[serde(default)]
    pub db: DbConfig,
    pub storage: StorageConfig,
    pub scheduler: SchedulerConfig,
    pub ml: MlConfig,
    /// Layer [1] fallback system prompt when a chat has no Agent configured.
    pub default_system_prompt: String,
    /// Optional 32-byte hex seed for Ed25519 audit signing. Empty = unsigned
    /// (hash-chain only). Set in deployment for non-repudiation.
    pub audit_signing_key: String,
    /// Optional base64-encoded 32-byte AES-256-GCM key for at-rest encryption of
    /// direct-message bodies. Empty = DMs stored in plaintext (dev default).
    #[serde(default)]
    pub message_encryption_key: String,
    /// When true (and a key is set), also encrypt **group/project** team-chat message
    /// bodies at rest, not just DMs. Trade-off: encrypted messages are excluded from
    /// full-text search (ciphertext is not searchable). Default off — group/project
    /// chats stay plaintext + searchable.
    #[serde(default)]
    pub encrypt_group_messages: bool,
    /// Token budget before chat history is compacted. 0 = learn from vLLM
    /// (`/model-info` max_model_len); a small value forces early compaction.
    pub max_context_tokens: i64,
    /// Authentication mode + local-login knobs. Default is local email/password
    /// (the open-source build); `keycloak` is opt-in via `PAI__AUTH__MODE=keycloak`.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Provider-registry policy. Off by default — only the
    /// deployment provider is effective; turn on for hosted-B2C BYOK.
    #[serde(default)]
    pub providers: ProvidersConfig,
    /// Keycloak settings — used only when `auth.mode = keycloak`.
    pub keycloak: KeycloakConfig,
    pub log_level: String,
    /// Host capabilities this deployment can run. Off by default — a macOS host
    /// leaves `code_interpreter` off (no Firecracker); a Linux+KVM host opts in.
    pub features: FeaturesConfig,
    /// Per-tool-type timeout overrides in seconds, keyed by tool type
    /// (`"rag"`, `"web"`, `"document_read"`, `"artefact"`, `"code"`, `"system"`,
    /// `"memory"`, `"dms"`). Empty = code defaults. A slower inference profile
    /// (e.g. llama.cpp on macOS) widens these via its config file.
    #[serde(default)]
    pub tool_timeout_secs: HashMap<String, u64>,
    /// Firecracker code-interpreter VM settings (used only when the feature is on).
    #[serde(default)]
    pub code_interpreter_vm: CodeInterpreterConfig,
    /// Code-interpreter backend selection + gVisor (runsc) settings. Picks between
    /// the Firecracker microVM (needs KVM) and the gVisor sandbox (KVM-less), or
    /// `auto` to prefer whichever the host supports.
    #[serde(default)]
    pub code_interpreter: CodeInterpreterBackendConfig,
    /// Live / streaming-voice engine endpoints (used only when the feature is on).
    #[serde(default)]
    pub voice_live: VoiceLiveConfig,
    /// Metrics + logging knobs.
    #[serde(default)]
    pub observability: ObservabilityConfig,
    /// Default lifetime (seconds) of a break-glass super-admin grant minted by
    /// the `breakglass` CLI subcommand when `--ttl` is not given. 30 minutes.
    #[serde(default = "default_breakglass_ttl_secs")]
    pub breakglass_default_ttl_secs: u64,
    /// Hard server-side ceiling on a break-glass grant's TTL. `issue` rejects
    /// anything longer — break-glass is ephemeral by design, so a long-lived grant
    /// (which would recreate standing privilege) is not allowed. 1 hour.
    #[serde(default = "default_breakglass_max_ttl_secs")]
    pub breakglass_max_ttl_secs: u64,
}

fn default_breakglass_ttl_secs() -> u64 {
    1800
}

fn default_breakglass_max_ttl_secs() -> u64 {
    3600
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Bearer token gating `GET /metrics`. **Fail-closed:** empty (default) means
    /// the endpoint is *disabled* (404) — system telemetry is never readable out
    /// of the box. Set a high-entropy token to enable the Prometheus scrape; it is
    /// then required (`Authorization: Bearer <token>` or `?token=`) and compared in
    /// constant time. Keep the scrape internal (loopback); never proxy `/metrics`.
    #[serde(default)]
    pub metrics_token: String,
    /// Log output format: `"text"` (default) or `"json"` for structured logs.
    #[serde(default)]
    pub log_format: String,
    /// Accept client-side error reports at `POST /api/telemetry` (logged +
    /// metered intra-perimeter; never forwarded outward). Default on; set false
    /// to mute ingestion entirely on a privacy-conservative deployment.
    #[serde(default = "default_true")]
    pub client_telemetry: bool,
}

// Manual Default (not derived): `client_telemetry` defaults *on*, which the
// derived `bool::default()` (false) would get wrong. Mirrors the serde default.
impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self { metrics_token: String::new(), log_format: String::new(), client_telemetry: true }
    }
}

/// Postgres connection-pool tuning. Independent of the engine — the platform is
/// Postgres-coupled (audit advisory locks / sequences / partitions / append-only
/// triggers); flexibility is the **host** (`DATABASE_URL` may point at a local
/// container or any managed Postgres).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbConfig {
    /// Max pooled connections for the main application pool. Default 10. Lower it
    /// to fit a small managed tier's connection cap (`PAI__DB__MAX_CONNECTIONS`).
    #[serde(default = "default_db_max_connections")]
    pub max_connections: u32,
}

fn default_db_max_connections() -> u32 {
    10
}

impl Default for DbConfig {
    fn default() -> Self {
        Self { max_connections: default_db_max_connections() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturesConfig {
    /// The code-interpreter tool (Firecracker microVM, Linux+KVM only). The tool
    /// is never advertised or dispatchable while this is false.
    pub code_interpreter: bool,
    /// Working in a folder on a paired machine. A presence capability, not an
    /// egress one: the work happens on the user's own computer, at their own
    /// request, after they have connected a folder there and agreed to each
    /// change. Default **on**, because the gates that matter are the folder
    /// itself and the approval in front of the person — this switch is for an
    /// administrator who wants the whole family off the instance regardless.
    #[serde(default = "default_true")]
    pub desktop_execution: bool,
    /// Voice (dictation + read-aloud). Requires STT/TTS engines configured on the
    /// ML service; the voice endpoints/frames are refused while this is false.
    #[serde(default)]
    pub voice: bool,
    /// Fleet kill-switch for action-taking agent runs. When false, no agent run
    /// may take its next action (every step + the approval gate refuse). Default
    /// on; flip off to emergency-stop all runs.
    #[serde(default = "default_true")]
    pub agents_enabled: bool,
    /// Event-driven workflow engine. When
    /// false, the dispatcher is a no-op and no domain event fires a workflow.
    /// Off by default — explicit opt-in.
    #[serde(default)]
    pub workflows: bool,
    /// Groundedness verification. When
    /// false, no post-stream faithfulness check runs and no score is shown. Needs
    /// a verifier engine configured on the ML service (`VERIFY_*`). Off by default;
    /// the spec ships it on for the legal profile via deployment config.
    #[serde(default)]
    pub groundedness: bool,
    /// Live / streaming voice — the real-time cascade (streaming STT → LLM →
    /// streaming TTS) with barge-in (mode 3). When false the
    /// live-voice WS frames are refused. Off by default; needs `voice` on too and
    /// the streaming engines configured under `[voice_live]`. Absent engines
    /// degrade (batch STT per utterance / silence gate / batch TTS per clause), so
    /// the loop still runs without them.
    #[serde(default)]
    pub voice_live: bool,
    /// Native MCP tool support (FEATURE B1).
    /// A Core presence capability (like `voice`/`messaging`, NOT an edition gate): when
    /// false the MCP client is invisible and no server is dispatched. Default **on** so
    /// admins can register/approve servers out of the box — this is the *capability*, not
    /// *egress*. Egress stays zero-by-default: `session_tool_defs` and server approval both
    /// still require `integration.mcp.enabled` (super-admin), and each server stays
    /// `enabled = false` until approved. Surfaced via `whoami.capabilities.mcp`.
    #[serde(default = "default_true")]
    pub mcp: bool,
    /// Team messaging — group/project chats and direct messages (Core
    /// collaboration table-stakes). A presence capability like `voice`/`mcp`
    /// (NOT an edition gate): when false the messaging endpoints/WS frames are
    /// refused and the Teams/DM nav disappears. Default **on** (preserves current
    /// behaviour); an admin can toggle it off at runtime
    /// (`config_settings["features.messaging"]`). Surfaced via
    /// `whoami.capabilities.messaging`.
    #[serde(default = "default_true")]
    pub messaging: bool,
    /// The OpenAI-compatible programmatic surface (`/v1`) and the platform API
    /// keys that authenticate it. A presence capability like `messaging`/`mcp`
    /// (NOT an edition gate): when false both `/v1` and key management answer
    /// 404, so an instance that never wants a machine door simply has none.
    /// Default **on**: the surface is inert until a user mints a key, and a key
    /// only ever carries that user's existing rights. Toggleable at runtime
    /// (`config_settings["features.public_api"]`) as a kill-switch, and
    /// restrictable per user group. Surfaced via `whoami.capabilities.public_api`.
    #[serde(default = "default_true")]
    pub public_api: bool,
    /// Edition capability — white-label branding (replace product identity: logo,
    /// name, colours). Off by default in Core (the branding section is hidden and
    /// the write endpoints 403); a `fosnie-enterprise` edition/licence resolver flips
    /// it on. Surfaced to the SPA via `whoami.capabilities.white_label`.
    ///
    /// Reserved edition-capability keys (gated by this same mechanism later, not
    /// yet wired): `custom_roles`.
    #[serde(default)]
    pub white_label: bool,
    /// Edition capability — compliance/audit surface: evidence-pack + audit export,
    /// signed checkpoints, GDPR crypto-shred erasure, and legal holds. Off by default
    /// in Core (the Audit/Holds sections are hidden and the write/exec endpoints 403);
    /// base audit *reads* (the admin Overview's recent-events widget) stay Core.
    /// Surfaced via `whoami.capabilities.compliance_audit`.
    #[serde(default)]
    pub compliance_audit: bool,
    /// Edition capability — the moderation subsystem (Admin Moderation section +
    /// the `/moderation` review queue). Off by default in Core (section/route hidden,
    /// every moderation endpoint 403). Surfaced via `whoami.capabilities.moderation`.
    #[serde(default)]
    pub moderation: bool,
    /// Edition capability — chat message Review & Approve sign-off. Off by default in
    /// Core (the chat badge/button is hidden and `submit_review` 403s). Surfaced via
    /// `whoami.capabilities.message_review`.
    #[serde(default)]
    pub message_review: bool,
    /// Edition capability — data-owner approval for adding members to access-bearing
    /// groups (Решение #6). Off by default in Core (the add applies directly via
    /// `DirectAddPolicy` and the approval-inbox endpoints 403). Surfaced via
    /// `whoami.capabilities.data_owner_approval`.
    #[serde(default)]
    pub data_owner_approval: bool,
    /// Edition capability — federated SSO (SAML/OIDC identity brokering) and SCIM 2.0
    /// provisioning. Off by default in Core (the Identity admin section is hidden and
    /// the `/api/admin/sso/*` endpoints 403; the SCIM server is not compiled into a
    /// Core binary at all). A `fosnie-enterprise` edition/licence resolver flips it on.
    /// Surfaced via `whoami.capabilities.federated_sso`.
    #[serde(default)]
    pub federated_sso: bool,
    /// Edition capability — custom roles, delegated (scoped) admin and ABAC
    /// policies. Off by default in Core (the Roles &
    /// Access admin section is hidden and the `/api/admin/rbac|abac/*` endpoints
    /// 403; a Core binary resolves every permission as `is_admin()`). A
    /// `fosnie-enterprise` edition/licence resolver flips it on. Surfaced via
    /// `whoami.capabilities.custom_rbac`.
    #[serde(default)]
    pub custom_rbac: bool,
    /// Edition capability — the DMS/mail connectors (iManage/NetDocuments/Outlook/
    /// Gmail): per-user OAuth connections, import, continuous sync, write-back.
    /// Off by default in Core (the Profile Connections
    /// tab + Admin Connectors section are hidden and the `/api/connectors/*` +
    /// `/api/admin/connectors/*` endpoints 403; a Core binary's connector registry
    /// returns dormant `NotBuilt`/`None`). A `fosnie-enterprise` edition/licence
    /// resolver flips it on. Surfaced via `whoami.capabilities.enterprise_connectors`.
    #[serde(default)]
    pub enterprise_connectors: bool,
}

fn default_true() -> bool {
    true
}

/// Firecracker code-interpreter settings (used only when
/// `features.code_interpreter` is on and the host is Linux+KVM). Paths point at
/// the versioned rootfs/kernel/snapshot deployment artefacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeInterpreterConfig {
    /// `firecracker` (or `jailer`) binary.
    pub firecracker_bin: String,
    /// Uncompressed guest kernel image (vmlinux).
    pub kernel_image: String,
    /// Base rootfs image (ext4) — fixed: python + pandas/openpyxl/matplotlib/numpy.
    pub rootfs_image: String,
    /// Directory holding the pre-warmed VM snapshot (mem + state) to restore.
    pub snapshot_dir: String,
    /// Directory for per-execution Firecracker API Unix sockets.
    pub socket_dir: String,
    /// Guest vsock context id + port the in-guest agent listens on.
    pub vsock_cid: u32,
    pub vsock_port: u32,
    pub vcpus: u32,
    pub mem_mb: u32,
    /// Hard wall-clock limit per execution (seconds).
    pub wall_secs: u64,
    /// Warm-pool size (v1 restores per-exec; reserved for pool tuning — Pass-2).
    pub pool_size: u32,
    /// Cap on total bytes returned (stdout + output files) before truncation.
    pub max_output_bytes: u64,
}

impl Default for CodeInterpreterConfig {
    fn default() -> Self {
        Self {
            firecracker_bin: "firecracker".into(),
            kernel_image: "/opt/pai/firecracker/vmlinux".into(),
            rootfs_image: "/opt/pai/firecracker/rootfs.ext4".into(),
            snapshot_dir: "/opt/pai/firecracker/snapshot".into(),
            socket_dir: "/run/pai/firecracker".into(),
            vsock_cid: 3,
            vsock_port: 5005,
            vcpus: 2,
            mem_mb: 512,
            wall_secs: 30,
            pool_size: 1,
            max_output_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Code-interpreter backend selection + gVisor settings. The Firecracker microVM
/// (`[code_interpreter_vm]`) stays the strongest tier for KVM hosts; gVisor
/// (`runsc`, systrap platform — no KVM needed) covers KVM-less hosts (Verda-class
/// VM guests, ordinary Docker/VM boxes). Both are Linux-only and network-less.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeInterpreterBackendConfig {
    /// `auto` (default: KVM → Firecracker, else runsc → gVisor), `firecracker`,
    /// `gvisor`, or `off`.
    #[serde(default = "default_ci_backend")]
    pub backend: String,
    /// gVisor runtime binary (resolved on `PATH` or absolute).
    #[serde(default = "default_runsc_bin")]
    pub runsc_bin: String,
    /// OCI rootfs **directory** for gVisor — the same package set as the
    /// Firecracker ext4 rootfs, emitted as a plain dir by `build-rootfs.sh`.
    #[serde(default = "default_gvisor_rootfs")]
    pub gvisor_rootfs: String,
    /// runsc state/root directory (per-sandbox runtime state).
    #[serde(default = "default_gvisor_state_dir")]
    pub gvisor_state_dir: String,
    /// Whether gVisor skips cgroup setup (`runsc --ignore-cgroups`):
    /// `auto` (default — ignore when rootless OR the host cgroup hierarchy is not
    /// writable, e.g. root inside a container without `--cgroupns=host`),
    /// `always`, or `never`. `always` is the in-container fix; `never` forces real
    /// cgroup resource enforcement.
    #[serde(default = "default_ignore_cgroups")]
    pub ignore_cgroups: String,
}

fn default_ci_backend() -> String {
    "auto".into()
}
fn default_runsc_bin() -> String {
    "runsc".into()
}
fn default_gvisor_rootfs() -> String {
    "/opt/pai/firecracker/rootfs".into()
}
fn default_gvisor_state_dir() -> String {
    "/run/pai/gvisor".into()
}
fn default_ignore_cgroups() -> String {
    "auto".into()
}

impl Default for CodeInterpreterBackendConfig {
    fn default() -> Self {
        Self {
            backend: default_ci_backend(),
            runsc_bin: default_runsc_bin(),
            gvisor_rootfs: default_gvisor_rootfs(),
            gvisor_state_dir: default_gvisor_state_dir(),
            ignore_cgroups: default_ignore_cgroups(),
        }
    }
}

/// Live / streaming-voice engine endpoints (used only when `features.voice_live`
/// is on). Operator-pinned at boot — the streaming-STT engine, the turn-detection
/// sidecar, and the streaming-TTS engine are external, in-perimeter services. Any
/// absent engine degrades (batch STT per utterance / silence-threshold gate /
/// batch TTS per clause) so the loop still runs. The tunable dials (silence
/// threshold, PTT default, AEC-required) live in the super-admin runtime knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceLiveConfig {
    /// Streaming-STT transport: `"none"` (batch fallback) or `"websocket"`.
    pub stt_stream_kind: String,
    /// WebSocket URL of the streaming-STT engine (sherpa-onnx / NeMo), `ws://` on
    /// the controlled LAN. Empty → batch fallback regardless of `stt_stream_kind`.
    pub stt_stream_url: String,
    /// PCM sample rate (Hz) sent to the STT engine and the turn detector.
    pub stt_sample_rate: u32,
    /// HTTP base URL of the turn-detection sidecar (Silero VAD + Smart-Turn).
    /// Empty → the silence-threshold gate is used instead.
    pub turn_detector_url: String,
    /// Stream TTS audio out per clause (kokoro chunked `/v1/audio/speech`). False →
    /// per-clause batch synthesis (still fast first-audio).
    pub tts_stream: bool,
    /// Base URL of the streaming-TTS engine when `tts_stream` is on. Empty → the
    /// platform's ML service (batch `/speech`) is used.
    pub tts_stream_url: String,
}

impl Default for VoiceLiveConfig {
    fn default() -> Self {
        Self {
            stt_stream_kind: "none".into(),
            stt_stream_url: String::new(),
            stt_sample_rate: 16_000,
            turn_detector_url: String::new(),
            tts_stream: false,
            tts_stream_url: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Directory of the built React SPA bundle; served as a fallback if present.
    pub static_dir: String,
    /// Public base URL the browser reaches the platform at — the OIDC redirect
    /// base for the login flow (e.g. `http://localhost:8088`).
    pub public_url: String,
    /// Origins permitted to open the WebSocket (anti cross-site WS hijacking).
    /// Empty (default) ⇒ the origin of `public_url` is the sole allowed origin.
    /// Each entry is a scheme://host[:port] with no path.
    #[serde(default)]
    pub allowed_ws_origins: Vec<String>,
    /// Origins the desktop client presents. They are cross-origin to this server
    /// by construction (the client serves its own shell from a local scheme), so
    /// they must be permitted for both the WebSocket upgrade and ordinary
    /// cross-origin requests to the native surface. The default covers the
    /// standard desktop shell; it is overridable so a repackaged client using a
    /// different local scheme can be admitted, and an operator who runs no
    /// desktop clients can empty it to switch the allowance off entirely.
    #[serde(default = "default_desktop_origins")]
    pub desktop_origins: Vec<String>,
}

fn default_desktop_origins() -> Vec<String> {
    ["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost"]
        .into_iter()
        .map(String::from)
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub branding_dir: String,
    pub artefacts_dir: String,
    /// Where uploaded Project Knowledge documents are written before ingestion.
    pub documents_dir: String,
    /// Version-pinned legal-workspace documents (`<doc_id>/<version_id>.<ext>`).
    pub workspace_dir: String,
    /// Skill folders (`<id>/SKILL.md`) — slot-[2] instruction modules. The
    /// *runtime* store: where seeded + user-authored skills are copied/written and
    /// where `read_skill` reads from.
    pub skills_dir: String,
    /// The in-repo default-skill *library* (`<slug>/SKILL.md`) shipped with the
    /// release. The boot seeder walks it and copies each skill into `skills_dir`.
    /// Read-only source — never written to, never created. Resolved against a few
    /// candidates (this value → `./skills` → `../skills`) so it works whether the
    /// process runs from the repo root or `backend/`.
    #[serde(default = "default_skills_library_dir")]
    pub skills_library_dir: String,
    /// Prompt templates (`<id>.md`) — `/`-invoked user-message templates.
    pub prompts_dir: String,
    /// Built async export files (`<export_id>.<ext>`) — served via download link.
    pub exports_dir: String,
    /// Persisted group/DM message attachments (`<id>__<filename>`).
    #[serde(default = "default_message_attachments_dir")]
    pub message_attachments_dir: String,
    /// Per-user avatar images (`<user_id>`), set on the profile page.
    #[serde(default = "default_avatars_dir")]
    pub avatars_dir: String,
    /// Persisted per-turn chat (LLM) attachments (`<id>__<filename>`) — rendered
    /// under the user message and in the docs rail.
    #[serde(default = "default_chat_attachments_dir")]
    pub chat_attachments_dir: String,
}

fn default_message_attachments_dir() -> String {
    "./data/message-attachments".into()
}

fn default_chat_attachments_dir() -> String {
    "./data/chat-attachments".into()
}

fn default_avatars_dir() -> String {
    "./data/avatars".into()
}

fn default_skills_library_dir() -> String {
    "../skills".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    /// Worker threads for the background tokio runtime (isolated from the hot path).
    pub worker_threads: usize,
    /// How often the durable task poller wakes, in seconds.
    pub poll_interval_secs: u64,
    /// Maximum tasks claimed per poll.
    pub batch_size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MlConfig {
    /// Base URL of the Python ML/RAG service (the platform's LLM client).
    pub base_url: String,
    /// Shared secret sent as `X-PAI-ML-Key` on every ML call. The ML service
    /// rejects requests without it. Empty (default) = unauthenticated, for
    /// localhost dev; set `PAI__ML__SHARED_SECRET` in production.
    #[serde(default)]
    pub shared_secret: String,
}

/// Which authentication provider the platform uses. Not composite — exactly one
/// (simultaneous local + SSO is a future enhancement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Core email/password login (Argon2id + Redis sessions). The default.
    Local,
    /// Keycloak OIDC (the pre-2b behaviour). Requires `keycloak.*` configured.
    Keycloak,
}

/// Authentication configuration. `mode` selects the `AuthProvider`; the other
/// knobs apply to local login only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_auth_mode")]
    pub mode: AuthMode,
    /// Lifetime of a local-login session in Redis, seconds (default 7 days).
    #[serde(default = "default_session_ttl_secs")]
    pub session_ttl_secs: u64,
    /// Minimum acceptable local password length at registration (default 10).
    #[serde(default = "default_password_min_len")]
    pub password_min_len: usize,
}

fn default_auth_mode() -> AuthMode {
    AuthMode::Local
}

fn default_session_ttl_secs() -> u64 {
    604_800
}

fn default_password_min_len() -> usize {
    10
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: default_auth_mode(),
            session_ttl_secs: default_session_ttl_secs(),
            password_min_len: default_password_min_len(),
        }
    }
}

/// Provider-registry policy. `user_byok_enabled` gates per-user BYOK writes:
/// off ⇒ only the deployment provider is effective and
/// `PUT /api/me/providers` is refused; on ⇒ users may store their own keys. A
/// runtime override (`config_settings["providers.user_byok_enabled"]`) lets a
/// host toggle it without a restart. Default **on** for public Core (BYOK is the
/// product point — individuals bring their own key); an admin can switch it off
/// so everyone goes through the deployment key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvidersConfig {
    #[serde(default = "default_true")]
    pub user_byok_enabled: bool,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self { user_byok_enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeycloakConfig {
    /// Base server URL, e.g. `http://localhost:8081`. The issuer is
    /// `{url}/realms/{realm}`.
    pub url: String,
    pub realm: String,
    pub client_id: String,
    pub client_secret: String,
}

impl KeycloakConfig {
    /// OIDC issuer URL (`{url}/realms/{realm}`).
    pub fn issuer(&self) -> String {
        format!("{}/realms/{}", self.url.trim_end_matches('/'), self.realm)
    }

    /// Whether Keycloak is configured (url + realm present).
    pub fn is_configured(&self) -> bool {
        !self.url.is_empty() && !self.realm.is_empty()
    }
}

impl Default for BootConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
                static_dir: "../frontend/dist".into(),
                public_url: "http://localhost:8080".into(),
                allowed_ws_origins: Vec::new(),
                desktop_origins: default_desktop_origins(),
            },
            database_url: String::new(),
            redis_url: "redis://localhost:6379".into(),
            db: DbConfig::default(),
            storage: StorageConfig {
                branding_dir: "./data/branding".into(),
                artefacts_dir: "./data/artefacts".into(),
                documents_dir: "./data/documents".into(),
                workspace_dir: "./data/workspace".into(),
                skills_dir: "./data/skills".into(),
                skills_library_dir: default_skills_library_dir(),
                prompts_dir: "./data/prompts".into(),
                exports_dir: "./data/exports".into(),
                message_attachments_dir: default_message_attachments_dir(),
                avatars_dir: default_avatars_dir(),
                chat_attachments_dir: default_chat_attachments_dir(),
            },
            scheduler: SchedulerConfig {
                worker_threads: 2,
                poll_interval_secs: 5,
                batch_size: 10,
            },
            ml: MlConfig {
                base_url: "http://localhost:8090".into(),
                shared_secret: String::new(),
            },
            default_system_prompt: "You are a helpful assistant.".into(),
            audit_signing_key: String::new(),
            message_encryption_key: String::new(),
            encrypt_group_messages: false,
            max_context_tokens: 0,
            auth: AuthConfig::default(),
            providers: ProvidersConfig::default(),
            keycloak: KeycloakConfig {
                url: String::new(),
                realm: String::new(),
                client_id: "fosnie".into(),
                client_secret: String::new(),
            },
            log_level: "info".into(),
            features: FeaturesConfig { code_interpreter: false, desktop_execution: true, voice: false, agents_enabled: true, workflows: false, groundedness: false, voice_live: false, mcp: true, messaging: true, public_api: true, white_label: false, compliance_audit: false, moderation: false, message_review: false, data_owner_approval: false, federated_sso: false, custom_rbac: false, enterprise_connectors: false },
            tool_timeout_secs: HashMap::new(),
            code_interpreter_vm: CodeInterpreterConfig::default(),
            code_interpreter: CodeInterpreterBackendConfig::default(),
            voice_live: VoiceLiveConfig::default(),
            observability: ObservabilityConfig::default(),
            breakglass_default_ttl_secs: default_breakglass_ttl_secs(),
            breakglass_max_ttl_secs: default_breakglass_max_ttl_secs(),
        }
    }
}

impl BootConfig {
    /// Build config from defaults → optional TOML file → `PAI__` env vars.
    /// The file path comes from `PAI_CONFIG_FILE` (default `config.toml`);
    /// a missing file is not an error. Env vars use `__` to nest, e.g.
    /// `PAI__SERVER__PORT=9000`, `PAI__DATABASE_URL=...`.
    pub fn load() -> Result<Self, Box<figment::Error>> {
        let file = std::env::var("PAI_CONFIG_FILE").unwrap_or_else(|_| "config.toml".into());
        Figment::new()
            .merge(Serialized::defaults(BootConfig::default()))
            .merge(Toml::file(file))
            .merge(Env::prefixed("PAI__").split("__"))
            .extract()
            .map_err(Box::new)
    }

    /// Reject obviously-broken config before anything depends on it.
    pub fn validate(&self) -> Result<(), String> {
        if self.database_url.is_empty() {
            return Err("database_url is required (set PAI__DATABASE_URL)".into());
        }
        if !self.database_url.starts_with("postgres://")
            && !self.database_url.starts_with("postgresql://")
        {
            return Err("database_url must be a postgres:// DSN".into());
        }
        if self.redis_url.is_empty() {
            return Err("redis_url is required".into());
        }
        if self.server.port == 0 {
            return Err("server.port must be non-zero".into());
        }
        if self.scheduler.batch_size <= 0 {
            return Err("scheduler.batch_size must be positive".into());
        }
        if self.ml.base_url.is_empty() {
            return Err("ml.base_url is required".into());
        }
        // On a public host the browser-facing URL and the token issuer MUST be
        // https — otherwise the session cookie's `Secure` flag goes off and Bearer
        // tokens/cookies travel in cleartext. Localhost (dev) is exempt. Catches the
        // `http://chat.example` deployment typo at boot rather than in production.
        require_secure_url("server.public_url", &self.server.public_url)?;
        require_secure_url("keycloak.url", &self.keycloak.url)?;
        // A configured-but-unparseable message key must fail CLOSED: silently
        // storing confidential direct messages in plaintext while the operator
        // believes encryption is on is worse than refusing to boot. (An empty key
        // is the deliberate "encryption disabled" dev default and is allowed.)
        if !self.message_encryption_key.trim().is_empty()
            && crate::crypto::parse_key(&self.message_encryption_key).is_none()
        {
            return Err("message_encryption_key is set but is not a valid base64-encoded 32-byte \
                        key — refusing to boot rather than silently storing direct messages in \
                        plaintext (unset it to deliberately disable at-rest encryption)"
                .into());
        }
        Ok(())
    }

    /// Non-fatal hardening advisories logged at boot — distinct from [`validate`],
    /// which rejects outright. These catch config that is *technically* bootable but
    /// weakens the security posture, so they surface in the operator's logs instead
    /// of breaking an existing deployment.
    pub fn hardening_warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        let host = self.server.host.trim();
        if !matches!(host, "127.0.0.1" | "::1" | "localhost") {
            w.push(format!(
                "server.host = `{host}` binds a non-loopback interface — the app is then \
                 reachable on every address this machine holds, bypassing the reverse \
                 proxy/tunnel. Prefer `127.0.0.1` and let the proxy reach it on loopback, \
                 unless public binding is genuinely intended."
            ));
        }
        // On a public deployment the internal ML service MUST require its shared
        // secret — otherwise anything that can reach :8090 reads documents and drives
        // the model unauthenticated (the Rust client always sends the key; an empty
        // secret disables the check on the ML side).
        let public = !self.server.public_url.is_empty() && !host_is_local(&self.server.public_url);
        if public && self.ml.shared_secret.trim().is_empty() {
            w.push(
                "ml.shared_secret is empty on a public deployment — the ML service (:8090) \
                 runs UNAUTHENTICATED. Set PAI__ML__SHARED_SECRET so direct calls are rejected."
                    .into(),
            );
        }
        w
    }
}

/// True when the URL's host is loopback (dev), so plain http is acceptable there.
fn host_is_local(url: &str) -> bool {
    let after = url.split("://").nth(1).unwrap_or(url);
    let authority = after.split('/').next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or(authority); // strip userinfo
    let host = match hostport.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest), // [::1]
        None => hostport.split(':').next().unwrap_or(hostport),
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1") || host.ends_with(".localhost")
}

/// Reject a non-https URL on a non-loopback host (empty = unset, allowed).
fn require_secure_url(label: &str, url: &str) -> Result<(), String> {
    if url.is_empty() || host_is_local(url) {
        return Ok(());
    }
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err(format!(
            "{label} must be https:// on a public host (got `{url}`) — plaintext would \
             expose session cookies and bearer tokens"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    // `load()` reads process-global env; serialise the env-mutating test(s).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn load_layers_env_over_file_over_default() {
        let _guard = ENV_LOCK.lock().unwrap();

        // A partial TOML file: overrides log_level and server.port only.
        let path = std::env::temp_dir().join(format!("pai_cfg_{}.toml", std::process::id()));
        {
            let mut f = std::fs::File::create(&path).unwrap();
            write!(f, "log_level = \"debug\"\n\n[server]\nport = 7000\n").unwrap();
        }

        // SAFETY: edition 2021; serialised by ENV_LOCK, cleaned up below.
        std::env::set_var("PAI_CONFIG_FILE", &path);
        std::env::set_var("PAI__SERVER__PORT", "9000"); // env overrides the file

        let cfg = BootConfig::load().expect("load");

        // env beats file:
        assert_eq!(cfg.server.port, 9000);
        // file beats default:
        assert_eq!(cfg.log_level, "debug");
        // default holds where neither file nor env set it:
        assert_eq!(cfg.ml.base_url, "http://localhost:8090");

        std::env::remove_var("PAI_CONFIG_FILE");
        std::env::remove_var("PAI__SERVER__PORT");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn byok_defaults_on_for_public_core() {
        // BYOK is the product point for public Core; an admin can switch it off.
        assert!(ProvidersConfig::default().user_byok_enabled);
    }

    #[test]
    fn messaging_defaults_on() {
        // Teams + DMs are Core collaboration table-stakes; on unless toggled off.
        assert!(BootConfig::default().features.messaging);
    }

    #[test]
    fn public_api_defaults_on() {
        // The programmatic surface is inert until a user mints a key, so it ships
        // present rather than requiring an .env edit before a first integration.
        assert!(BootConfig::default().features.public_api);
    }

    #[test]
    fn host_is_local_detects_loopback_only() {
        assert!(host_is_local("http://localhost:8080"));
        assert!(host_is_local("http://127.0.0.1:5173/path"));
        assert!(host_is_local("http://[::1]:8080"));
        assert!(host_is_local("https://app.localhost"));
        assert!(!host_is_local("http://chat.example.com"));
        assert!(!host_is_local("https://chat.example.com"));
        // A host that merely *contains* "localhost" must not pass as loopback.
        assert!(!host_is_local("http://localhost.evil.com"));
    }

    #[test]
    fn require_secure_url_enforces_https_off_loopback() {
        assert!(require_secure_url("x", "").is_ok()); // unset = allowed
        assert!(require_secure_url("x", "http://localhost:8080").is_ok()); // dev
        assert!(require_secure_url("x", "https://chat.example").is_ok()); // prod https
        assert!(require_secure_url("x", "http://chat.example").is_err()); // prod http → reject
        assert!(require_secure_url("x", "HTTP://Chat.Example").is_err()); // case-insensitive
    }

    fn minimal_valid() -> BootConfig {
        let mut c = BootConfig::default();
        c.database_url = "postgres://localhost/pai".into();
        c.redis_url = "redis://localhost:6379".into();
        c.ml.base_url = "http://localhost:8090".into();
        c
    }

    #[test]
    fn invalid_message_key_fails_closed_but_empty_is_allowed() {
        use base64::Engine as _;
        let mut c = minimal_valid();
        // Empty = encryption deliberately disabled (dev) → allowed.
        c.message_encryption_key = String::new();
        assert!(c.validate().is_ok());
        // Configured-but-unparseable → refuse to boot (no silent plaintext fallback).
        c.message_encryption_key = "not-a-valid-key".into();
        assert!(c.validate().is_err());
        // A valid base64 32-byte key → allowed.
        c.message_encryption_key = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn hardening_flags_non_loopback_bind() {
        let mut c = minimal_valid();
        c.server.host = "0.0.0.0".into();
        assert!(c.hardening_warnings().iter().any(|w| w.contains("non-loopback")));
        c.server.host = "127.0.0.1".into();
        assert!(!c.hardening_warnings().iter().any(|w| w.contains("non-loopback")));
    }

    #[test]
    fn linux_profile_example_loads_voice_live() {
        let _guard = ENV_LOCK.lock().unwrap();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/deploy/config.linux.example.toml");
        std::env::set_var("PAI_CONFIG_FILE", path);
        let cfg = BootConfig::load().expect("load linux profile");
        std::env::remove_var("PAI_CONFIG_FILE");

        // The Linux profile turns live voice on and pins the streaming engines.
        assert!(cfg.features.voice, "batch voice on");
        assert!(cfg.features.voice_live, "live voice on");
        assert_eq!(cfg.voice_live.stt_stream_kind, "websocket");
        assert_eq!(cfg.voice_live.stt_sample_rate, 16_000);
        assert!(cfg.voice_live.tts_stream);
        assert!(!cfg.voice_live.turn_detector_url.is_empty());
    }

    #[test]
    fn macos_profile_example_loads() {
        let _guard = ENV_LOCK.lock().unwrap();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/deploy/config.macos.example.toml");
        std::env::set_var("PAI_CONFIG_FILE", path);
        let cfg = BootConfig::load().expect("load macos profile");
        std::env::remove_var("PAI_CONFIG_FILE");

        // The macOS profile disables code-interpreter and widens RAG timeout.
        assert!(!cfg.features.code_interpreter, "code_interpreter off on macOS");
        assert_eq!(cfg.tool_timeout_secs.get("rag"), Some(&240));
    }
}
