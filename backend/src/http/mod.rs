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

//! HTTP transport: the axum router and its layers.
//!
//! Route classes:
//!   * public — `/health*`;
//!   * protected — Bearer-JWT behind the Keycloak validation layer (`/api/*`);
//!   * break-glass — `/api/admin/*`, gated by the `X-Break-Glass` header, NOT
//!     behind the Keycloak layer so it works even if Keycloak is down.
//! Browser login is keycloak-js (PKCE) + Bearer JWT — no server-side OIDC flow.
//!
//! CORS denies cross-origin by default — the SPA is served same-origin.

pub mod agents;
pub mod announcements;
pub mod api_keys;
pub mod artefacts;
pub mod auth;
pub mod automations;
pub mod chat_attachments;
pub mod chats;
pub mod devices;
pub mod agent_runs;
pub mod config_admin;
pub mod documents;
pub mod profile;
pub mod export;
pub mod feedback;
pub mod groundedness;
pub mod groundedness_admin;
pub mod health;
pub mod mcp_admin;
pub mod mcp_oauth;
pub mod integrations;
pub mod kb;
pub mod memory;
pub mod message_attachments;
pub mod messaging;
pub mod power;
pub mod projects;
pub mod providers;
pub mod prompts;
pub mod research;
pub mod research_templates;
pub mod skills;
pub mod superadmin;
pub mod tabular;
pub mod telemetry;
pub mod tools;
pub mod users_admin;
pub mod v1;
pub mod voice;
pub mod voice_admin;
pub mod workflows;

use std::path::Path;

use std::time::Instant;

use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::json;
use tower::ServiceExt;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::auth::breakglass::SuperAdmin;
use crate::auth::device::MaybeDevice;
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Whether a request carries one of our platform tokens rather than an identity
/// provider's JWT. Decided on the prefix alone — never on a successful lookup —
/// so an invalid token is still refused inside the extractor and the two routing
/// branches behind the JWT layer answer identically.
fn is_platform_token(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|raw| {
            raw.strip_prefix("Bearer ")
                .or_else(|| raw.strip_prefix("bearer "))
                .unwrap_or(raw)
                .trim()
                .starts_with(crate::auth::api_key::TOKEN_PREFIX)
        })
        .unwrap_or(false)
}

/// Edition-capability gate. An Enterprise-only `feature` resolved
/// through the [`crate::ext::FeatureResolver`] seam (host flag ∧ group-restrict);
/// off ⇒ 403. The single mechanism behind every edition gate (white-label,
/// compliance/audit, moderation, message-review) — never rely on the SPA hiding
/// the UI. `label` names the feature in the 403 message.
pub async fn require_capability(
    state: &AppState,
    ctx: &AuthContext,
    feature: &str,
    label: &str,
) -> Result<()> {
    if state.features.enabled_for(state, ctx, feature).await {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!("{label} is an Enterprise feature")))
    }
}

/// Build the application router. `kc_layer` / `ws_layer` are present only when
/// Keycloak is configured. Browser login is keycloak-js (PKCE) + Bearer JWT — the
/// platform exposes no server-side OIDC login flow.
pub fn router(
    state: AppState,
    kc_layer: Option<crate::auth::keycloak::KcLayer>,
    ws_layer: Option<crate::auth::keycloak::KcLayer>,
    extra: Option<Router<AppState>>,
    extra_public: Option<Router<AppState>>,
) -> Router {
    let static_dir = state.boot.server.static_dir.clone();
    // Precompute the CSP once from config so the deployment's own Keycloak origin
    // (not a hardcoded demo domain) is what the auth flow is allowed to reach.
    let csp_value = HeaderValue::from_str(&build_csp(&state.boot))
        .unwrap_or_else(|_| HeaderValue::from_static("default-src 'self'"));

    // Public (health + Prometheus scrape + branding theme read at SPA boot).
    let public = Router::new()
        .route("/health", get(health::liveness))
        .route("/health/ready", get(health::readiness))
        .route("/metrics", get(metrics_endpoint))
        .route("/api/branding/theme", get(config_admin::get_theme))
        // Local auth: public — these establish/clear the
        // session and report the login mode, so they sit ahead of any auth gate.
        .route("/api/auth/config", get(auth::config))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        // Second factor: the verify step is public — it exchanges a
        // login pending token (issued by /login when MFA is on) for a session, so
        // it must sit ahead of the auth gate like /login itself.
        .route("/api/auth/mfa/verify", post(auth::mfa_verify))
        // Client-side error telemetry: public (errors happen when auth is down),
        // bounded by a small body limit + a per-IP rate limit in the handler.
        .route(
            "/api/telemetry",
            post(telemetry::report).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        // MCP OAuth callback: public — identity is reconstructed from the parked flow
        // record keyed on `state`, not from a session, so it sits ahead of the auth gate
        // (like /login). Rate-limited inside the handler; never returns a 500.
        .route("/api/mcp/oauth/callback", get(mcp_oauth::callback))
        // Desktop pairing: public by necessity — the client has no credential
        // yet. The short-lived, single-use code minted from an authenticated
        // browser session is the whole authority. Per-IP rate limited in the
        // handler.
        .route("/api/device/pair", post(devices::pair_device))
        .with_state(state.clone());

    // Edition public-route extension: merge a private edition's *public* routes
    // (e.g. a SCIM 2.0 server, gated by its own bearer token — not the
    // Keycloak/session layer) into the public router, ahead of any auth layer.
    // Core passes `None` → no such endpoints exist. The merged router carries its
    // own authentication inside its handlers/layers.
    let public = match extra_public {
        Some(routes) => public.merge(routes.with_state(state.clone())),
        None => public,
    };

    // Break-glass (independent of Keycloak). Connector *activation* is a
    // sensitive super-admin action, so it is gated by the X-Break-Glass header
    // here rather than the Bearer/Keycloak layer — viewing connectors stays a
    // client-admin (Bearer) route below.
    let breakglass = Router::new()
        .route("/api/admin/ping", get(admin_ping))
        .route("/api/admin/breakglass/grants", get(breakglass_grants))
        .route("/api/admin/integrations/{kind}", axum::routing::put(integrations::set_enabled))
        // Super-admin panel (ephemeral break-glass): session + dynamic tuning knobs,
        // cross-user chat viewing, and account deactivate / GDPR erasure.
        .route("/api/admin/super/session", get(superadmin::session))
        .route("/api/admin/super/integrations", get(integrations::list_all_super))
        .route("/api/admin/super/config", get(superadmin::list_config))
        .route(
            "/api/admin/super/config/{key}",
            axum::routing::put(superadmin::set_config).delete(superadmin::reset_config),
        )
        .route("/api/admin/super/users", get(superadmin::list_users))
        .route("/api/admin/super/users/{id}/chats", get(superadmin::user_chats))
        .route("/api/admin/super/users/{id}/deactivate", post(superadmin::deactivate_user))
        .route("/api/admin/super/users/{id}/data", axum::routing::delete(superadmin::erase_user))
        .route("/api/admin/super/chats/{id}/messages", get(superadmin::chat_messages))
        .with_state(state.clone());

    let mut app = public.merge(breakglass);

    // Protected API. Routes are mounted regardless of auth mode: in keycloak mode
    // the Bearer-JWT validation layer is applied below; in local mode the
    // `AuthUser` extractor authenticates via the session cookie (no middleware).
    {
        let mut protected = Router::new()
            .route("/api/whoami", get(whoami))
            .route("/api/notices", get(announcements::get_notices))
            .route("/api/ws-ticket", post(ws_ticket))
            .route("/api/projects", post(projects::create_project).get(projects::list_projects))
            .route("/api/projects/{id}", delete(projects::delete_project))
            .route(
                "/api/chat-attachments",
                post(chat_attachments::upload_attachment)
                    .layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
            )
            .route("/api/chat-attachments/{id}", get(chat_attachments::download))
            .route("/api/chats", get(chats::list_chats))
            .route(
                "/api/chats/{id}",
                axum::routing::patch(chats::rename_chat).delete(chats::delete_chat),
            )
            .route("/api/chats/{id}/messages", get(chats::list_messages))
            .route("/api/chats/{id}/share", post(chats::share_chat))
            // Shared-chats governance: list / revoke the caller's own shares.
            .route("/api/chat-shares", get(chats::list_chat_shares))
            .route(
                "/api/chat-shares/{chat_id}/{group_chat_id}",
                delete(chats::revoke_chat_share),
            )
            .route("/api/users", get(users_admin::list_directory))
            // Self-service profile: own name + avatar, plus any user's avatar bytes.
            .route(
                "/api/me/profile",
                get(profile::get_profile).patch(profile::update_profile),
            )
            .route(
                "/api/me/avatar",
                post(profile::upload_avatar)
                    .delete(profile::delete_avatar)
                    .layer(DefaultBodyLimit::max(2 * 1024 * 1024)),
            )
            .route("/api/users/{id}/avatar", get(profile::get_avatar))
            // Platform API keys: minted from the browser, used by external apps.
            .route(
                "/api/me/api-keys",
                post(api_keys::create_key).get(api_keys::list_keys),
            )
            .route("/api/me/api-keys/{id}", delete(api_keys::revoke_key))
            .route(
                "/api/admin/users/{id}/api-keys",
                get(api_keys::admin_list_keys),
            )
            .route(
                "/api/admin/users/{id}/api-keys/{key_id}",
                delete(api_keys::admin_revoke_key),
            )
            // Connected devices: a paired desktop client and its token. Distinct
            // from the API keys above — a device is minted by pairing, not from
            // this screen, and lists beside keys rather than among them.
            .route("/api/me/devices/pairing-code", post(devices::create_pairing_code))
            .route("/api/me/devices", get(devices::list_devices))
            .route("/api/me/devices/{id}", delete(devices::revoke_device))
            .route("/api/admin/users/{id}/devices", get(devices::admin_list_devices))
            .route(
                "/api/admin/users/{id}/devices/{device_id}",
                delete(devices::admin_revoke_device),
            )
            // Self-serve account deletion (soft-archive; emits `account.archived`).
            .route("/api/me/account", axum::routing::delete(profile::delete_account))
            // Agent runs: list (per chat) + pending-approval inbox + approve/reject + trajectory.
            .route("/api/agent-runs", get(agent_runs::list_runs))
            .route("/api/agent-runs/pending", get(agent_runs::list_pending))
            .route("/api/agent-runs/{id}/approve", post(agent_runs::approve_run))
            .route("/api/agent-runs/{id}/reject", post(agent_runs::reject_run))
            .route("/api/agent-runs/{id}/cancel", post(agent_runs::cancel_run))
            .route("/api/agent-runs/{id}", get(agent_runs::get_run))
            .route("/api/projects/{id}/knowledge", post(projects::create_knowledge))
            .route(
                "/api/projects/{id}/documents",
                post(projects::upload_document)
                    .get(projects::list_documents)
                    .layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
            )
            .route("/api/agents", post(agents::create_agent).get(agents::list_agents))
            .route(
                "/api/agents/{id}",
                get(agents::get_agent)
                    .patch(agents::update_agent)
                    .delete(agents::delete_agent),
            )
            .route(
                "/api/agents/{id}/skills/{skill_id}",
                post(skills::attach_skill).delete(skills::detach_skill),
            )
            .route("/api/agents/{id}/versions", get(agents::list_agent_versions))
            .route("/api/agents/{id}/versions/{vnum}", get(agents::get_agent_version))
            .route("/api/agents/{id}/versions/{vnum}/rollback", post(agents::rollback_agent_version))
            .route("/api/knowledge-docs/{id}/source", get(projects::knowledge_doc_source))
            .route("/api/project-knowledge", get(projects::list_project_knowledge))
            // Libraries (standalone Knowledge Bases).
            .route("/api/kb", post(kb::create_kb).get(kb::list_kb))
            .route(
                "/api/kb/{id}",
                get(kb::get_kb).patch(kb::patch_kb).delete(kb::delete_kb),
            )
            .route(
                "/api/kb/{id}/documents",
                post(kb::upload_kb_document).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
            )
            .route("/api/kb/{id}/documents/{doc}", delete(kb::delete_kb_document))
            .route("/api/kb/{id}/grants", get(kb::list_grants).put(kb::put_grant))
            .route("/api/kb/{id}/grants/{grant}", delete(kb::delete_grant))
            .route("/api/kb/{id}/promote", post(kb::promote_kb))
            .route(
                "/api/projects/{id}/kb-links",
                get(kb::list_project_links).post(kb::attach_project),
            )
            .route("/api/projects/{id}/kb-links/{kb}", delete(kb::detach_project))
            .route(
                "/api/chats/{id}/kb-links",
                get(kb::list_chat_links).post(kb::attach_chat),
            )
            .route("/api/chats/{id}/kb-links/{kb}", delete(kb::detach_chat))
            .route("/api/skills", post(skills::create_skill).get(skills::list_skills))
            .route(
                "/api/skills/{id}",
                get(skills::get_skill)
                    .patch(skills::update_skill)
                    .delete(skills::delete_skill),
            )
            .route("/api/skills/{id}/test", post(skills::test_skill))
            .route("/api/skills/{id}/enabled", post(skills::set_skill_enabled))
            .route("/api/prompts", post(prompts::create_prompt).get(prompts::list_prompts))
            .route("/api/prompts/{id}", get(prompts::get_prompt))
            .route("/api/prompts/{id}/render", post(prompts::render_prompt))
            .route("/api/memory", post(memory::create_fact).get(memory::list_facts))
            .route(
                "/api/memory/{id}",
                axum::routing::patch(memory::update_fact).delete(memory::delete_fact),
            )
            .route(
                "/api/projects/{id}/workspace/documents",
                post(documents::upload_workspace_document)
                    .get(documents::list_workspace_documents)
                    .layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
            )
            .route(
                "/api/documents/{id}",
                get(documents::get_document).delete(documents::delete_document),
            )
            .route("/api/documents/{id}/versions/{vid}/download", get(documents::download_version))
            .route("/api/documents/{id}/versions/{vid}/text", get(documents::version_text))
            .route("/api/documents/{id}/versions/{vid}/pdf", post(documents::render_version_pdf))
            .route("/api/documents/{id}/edits", get(documents::list_edits))
            .route("/api/documents/{id}/edits/accept-all", post(documents::accept_all_edits))
            .route("/api/documents/{id}/edits/reject-all", post(documents::reject_all_edits))
            .route("/api/documents/{id}/edits/{w_id}/accept", post(documents::accept_edit))
            .route("/api/documents/{id}/edits/{w_id}/reject", post(documents::reject_edit))
            .route("/api/tabular-reviews", post(tabular::create_review))
            .route("/api/projects/{id}/tabular-reviews", get(tabular::list_reviews))
            .route("/api/tabular-reviews/{id}", get(tabular::get_review))
            .route("/api/tabular-reviews/{id}/run", post(tabular::run_review))
            .route("/api/tabular-reviews/{id}/cancel", post(tabular::cancel_review))
            .route("/api/tabular-reviews/{id}/rerun-errors", post(tabular::rerun_errors))
            .route(
                "/api/tabular-reviews/{id}/cells/{document_id}/{column_key}/rerun",
                post(tabular::rerun_cell),
            )
            .route("/api/tabular-reviews/{id}/export", get(tabular::export_review))
            .route("/api/tabular-reviews/{id}/chat", post(tabular::create_review_chat))
            .route("/api/verify-draft", post(groundedness::start))
            .route("/api/verification-runs", get(groundedness::latest_for_target))
            .route("/api/verification-runs/{id}", get(groundedness::get_run))
            .route("/api/verification-runs/{id}/report", get(groundedness::export_report))
            .route("/api/verification-runs/{id}/repair", post(groundedness::repair))
            .route("/api/chats/{id}/artefacts", get(artefacts::list_artefacts))
            .route("/api/artefacts/{id}/download", get(artefacts::download_artefact))
            .route("/api/artefacts/{id}/convert", post(artefacts::convert_artefact))
            .route("/api/artefacts/{id}/create-page", post(artefacts::create_page))
            .route("/api/research/prepare", post(research::prepare))
            .route("/api/research/start", post(research::start))
            .route(
                "/api/research/templates",
                post(research_templates::create_template).get(research_templates::list_templates),
            )
            .route(
                "/api/research/templates/{id}",
                axum::routing::get(research_templates::get_template)
                    .patch(research_templates::update_template)
                    .delete(research_templates::archive_template),
            )
            .route("/api/chats/{id}/export", get(export::export_chat))
            .route("/api/admin/projects/{id}/export", get(export::export_project_db))
            .route("/api/exports", post(export::create_export).get(export::list_exports))
            .route("/api/exports/{id}", get(export::get_export))
            .route("/api/exports/{id}/download", get(export::download_export))
            .route("/api/memory/recall", get(memory::recall_facts))
            .route(
                "/api/admin/announcements",
                get(announcements::list_announcements).post(announcements::create_announcement),
            )
            .route(
                "/api/admin/announcements/{id}",
                axum::routing::put(announcements::update_announcement)
                    .delete(announcements::delete_announcement),
            )
            .route(
                "/api/admin/welcome",
                get(announcements::get_welcome).put(announcements::set_welcome),
            )
            .route("/api/admin/config", get(config_admin::list_config))
            .route("/api/admin/config/{key}", axum::routing::put(config_admin::set_config))
            .route("/api/admin/providers", get(providers::list_providers))
            // Multi-LLM: named deployment llm CRUD (static `llm` wins over `{role}`).
            .route("/api/admin/providers/llm", post(providers::create_admin_llm))
            .route("/api/admin/providers/llm/test", post(providers::test_admin_llm))
            .route(
                "/api/admin/providers/llm/{id}",
                axum::routing::put(providers::update_admin_llm).delete(providers::delete_admin_llm),
            )
            .route("/api/admin/providers/llm/{id}/default", axum::routing::put(providers::set_admin_llm_default))
            .route("/api/admin/providers/{role}", axum::routing::put(providers::set_provider))
            .route("/api/admin/providers/{role}/test", post(providers::test_provider))
            .route("/api/admin/voice-live", get(voice_admin::get).put(voice_admin::set))
            .route("/api/admin/embedding-index", get(providers::embedding_index_status))
            .route("/api/admin/embedding-index/reindex", post(providers::reindex_embeddings))
            .route("/api/me/providers", get(providers::list_my_providers))
            // Multi-LLM: personal (BYOK) named llm CRUD + the composer selection list.
            .route("/api/me/providers/llm", post(providers::create_my_llm))
            .route("/api/me/providers/llm/test", post(providers::test_my_llm))
            .route(
                "/api/me/providers/llm/{id}",
                axum::routing::put(providers::update_my_llm).delete(providers::delete_my_llm),
            )
            .route("/api/me/llm-providers", get(providers::list_llm_providers))
            .route("/api/me/chats/{chat_id}/llm-provider", axum::routing::put(providers::set_chat_llm_provider))
            .route(
                "/api/me/providers/{role}",
                axum::routing::put(providers::set_my_provider).delete(providers::delete_my_provider),
            )
            .route("/api/me/providers/{role}/test", post(providers::test_my_provider))
            .route("/api/config/branding", get(config_admin::list_branding))
            .route("/api/branding/{kind}", get(config_admin::get_branding))
            .route(
                "/api/messages/{id}/feedback",
                post(feedback::submit_feedback)
                    .get(feedback::get_feedback)
                    .delete(feedback::delete_feedback),
            )
            .route("/api/agents/{id}/feedback/summary", get(feedback::agent_summary))
            .route("/api/admin/users", get(users_admin::list_users))
            .route("/api/admin/users/{id}/deactivate", post(users_admin::deactivate_user))
            .route("/api/admin/users/{id}/reactivate", post(users_admin::reactivate_user))
            .route("/api/admin/users/{id}/mfa/reset", post(users_admin::reset_user_mfa))
            .route("/api/admin/analytics", get(users_admin::usage_analytics))
            .route("/api/admin/groundedness", get(groundedness_admin::analytics))
            .route("/api/admin/feedback", get(feedback::list_feedback))
            .route("/api/admin/grants", post(users_admin::create_grant).get(users_admin::list_grants))
            .route("/api/admin/grants/{id}", axum::routing::delete(users_admin::revoke_grant))
            .route("/api/groups", post(users_admin::create_group).get(users_admin::list_groups))
            .route("/api/groups/{id}", get(users_admin::get_group).delete(users_admin::delete_group))
            .route("/api/groups/{id}/members", post(users_admin::add_group_member))
            .route("/api/groups/{id}/members/{user_id}", axum::routing::delete(users_admin::remove_group_member))
            .route("/api/groups/{id}/feature-flags", get(users_admin::list_group_flags))
            .route(
                "/api/groups/{id}/feature-flags/{feature}",
                axum::routing::put(users_admin::set_group_flag).delete(users_admin::clear_group_flag),
            )
            .route("/api/power/directory", get(power::power_directory))
            .route("/api/power/analytics", get(power::power_analytics))
            .route("/api/automations", post(automations::create_automation).get(automations::list_automations))
            .route("/api/automations/calendar", get(automations::calendar))
            .route(
                "/api/automations/{id}",
                get(automations::get_automation)
                    .patch(automations::update_automation)
                    .delete(automations::delete_automation),
            )
            .route("/api/automations/{id}/run", post(automations::run_now))
            .route("/api/automations/{id}/runs", get(automations::list_runs))
            .route(
                "/api/workflows",
                post(workflows::create_workflow).get(workflows::list_workflows),
            )
            .route("/api/workflows/triggers", get(workflows::list_triggers))
            .route(
                "/api/workflows/{id}",
                get(workflows::get_workflow)
                    .patch(workflows::update_workflow)
                    .delete(workflows::delete_workflow),
            )
            .route("/api/workflows/{id}/runs", get(workflows::list_runs))
            .route(
                "/api/voice/transcribe",
                post(voice::transcribe).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
            )
            .route("/api/voice/speech", post(voice::speak))
            .route("/api/integrations", get(integrations::list_enabled))
            .route("/api/admin/integrations", get(integrations::list_all))
            // Activation (PUT) is break-glass-gated above; viewing one connector
            // stays client-admin.
            .route("/api/admin/integrations/{kind}", get(integrations::get_one))
            // MCP server registry (FEATURE B1): client-admin CRUD; approval connects +
            // pins the catalog. Egress still needs integration.mcp.enabled (super-admin).
            .route("/api/admin/mcp-servers", get(mcp_admin::list).post(mcp_admin::register))
            .route(
                "/api/admin/mcp-servers/{id}",
                axum::routing::delete(mcp_admin::delete).patch(mcp_admin::patch_server),
            )
            .route("/api/admin/mcp-servers/{id}/approve", post(mcp_admin::approve))
            // One-click MCP connections (OAuth 2.1). Admin: discover + approve an issuer,
            // register a client (manual or DCR), designate the catalogue source.
            .route("/api/admin/mcp-servers/{id}/oauth/discover", post(mcp_oauth::discover))
            .route(
                "/api/admin/mcp-servers/{id}/oauth/client",
                axum::routing::put(mcp_oauth::put_client).delete(mcp_oauth::delete_client),
            )
            .route(
                "/api/admin/mcp-servers/{id}/oauth/catalog-source",
                axum::routing::put(mcp_oauth::set_catalog_source),
            )
            // User: connect / list / disconnect under one's own identity.
            .route("/api/me/mcp-connections", get(mcp_oauth::list_my_connections))
            .route(
                "/api/me/mcp-connections/{server_id}/connect",
                post(mcp_oauth::connect),
            )
            .route(
                "/api/me/mcp-connections/{server_id}",
                axum::routing::delete(mcp_oauth::disconnect),
            )
            // Tool catalogue: read for any authed user (agent
            // editor); native on/off + description overrides gated by tools.manage.
            .route("/api/tools/catalog", get(tools::catalog))
            .route(
                "/api/admin/tools/native/{name}",
                axum::routing::put(tools::put_native).delete(tools::reset_native),
            )
            // Custom tools: CRUD + approve/enable + test-run.
            .route("/api/admin/tools/custom", post(tools::create_custom))
            .route(
                "/api/admin/tools/custom/{id}",
                axum::routing::put(tools::update_custom).delete(tools::delete_custom),
            )
            .route("/api/admin/tools/custom/{id}/enable", post(tools::enable_custom))
            .route("/api/admin/tools/custom/{id}/disable", post(tools::disable_custom))
            .route("/api/admin/tools/custom/{id}/test-run", post(tools::test_run_custom))
            .route("/api/group-chats", post(messaging::create_chat).get(messaging::list_chats))
            .route("/api/group-chats/search", get(messaging::search_messages))
            .route("/api/group-chats/{id}", get(messaging::get_chat))
            .route("/api/group-chats/{id}/members", post(messaging::add_member))
            .route("/api/group-chats/{id}/members/{user_id}", axum::routing::delete(messaging::remove_member))
            .route(
                "/api/group-chats/{id}/messages",
                post(messaging::send_message).get(messaging::list_messages),
            )
            .route(
                "/api/group-chats/{id}/messages/{message_id}/reactions",
                post(messaging::toggle_reaction),
            )
            .route("/api/group-chats/{id}/notes", get(messaging::list_notes).post(messaging::create_note))
            .route(
                "/api/group-chats/{id}/notes/{note_id}",
                axum::routing::put(messaging::update_note).delete(messaging::delete_note),
            )
            .route("/api/dms/{user_id}", post(messaging::start_dm))
            .route("/api/message-attachments", post(message_attachments::upload))
            .route("/api/message-attachments/{id}", get(message_attachments::download))
            .route("/api/auth/password", post(auth::change_password))
            // Second factor management: protected — the caller must
            // already hold a session. `setup`/`confirm`/`status` are reachable by an
            // enrolment-only session (see the D6 allow-list in `local.rs`).
            .route("/api/auth/mfa/setup", post(auth::mfa_setup))
            .route("/api/auth/mfa/confirm", post(auth::mfa_confirm))
            .route("/api/auth/mfa/disable", post(auth::mfa_disable))
            .route("/api/auth/mfa/status", get(auth::mfa_status))
            .route("/api/auth/mfa/recovery/regenerate", post(auth::mfa_regenerate));
        // Enterprise route extension (open-core): merge the private edition's
        // protected routes (audit/holds/moderation/reviews/branding-write/group-requests)
        // BEFORE the auth layer + state, so they are gated and stated identically to
        // Core routes. Core passes `None` here → none of these endpoints exist.
        if let Some(extra) = extra {
            protected = protected.merge(extra);
        }
        // Keycloak mode: gate every protected route behind the Bearer-JWT layer.
        // Local mode (no layer): the AuthUser extractor reads the session cookie.
        if let Some(kc) = kc_layer {
            // A paired desktop client presents a platform token, not a JWT, so
            // the Bearer-validation layer would refuse it before the request
            // ever reached the extractor that knows how to read it. Requests
            // bearing a platform token are therefore routed to an identical,
            // unlayered copy of these routes, where `AuthUser` authenticates
            // them via the device fallback. The choice is made on the token's
            // prefix alone — never on a successful lookup — so an invalid token
            // still fails inside the extractor and both branches answer alike.
            let bypass: Router = protected
                .clone()
                .route_layer(axum::middleware::from_fn(http_metrics))
                .with_state(state.clone());
            protected = protected.layer(kc).layer(axum::middleware::from_fn(
                move |req: Request, next: Next| {
                    let bypass = bypass.clone();
                    async move {
                        if is_platform_token(req.headers()) {
                            // The router service is infallible, so the `Err` arm
                            // is uninhabited and the match stays exhaustive.
                            match bypass.oneshot(req).await {
                                Ok(resp) => resp,
                            }
                        } else {
                            next.run(req).await
                        }
                    }
                },
            ));
        }
        // route_layer → runs after routing, so MatchedPath (low-cardinality
        // route label) is available to the metrics middleware.
        let protected = protected
            .route_layer(axum::middleware::from_fn(http_metrics))
            .with_state(state.clone());
        app = app.merge(protected);
    }

    // WebSocket transport. In keycloak mode the Pass-mode layer injects the token
    // if present (without blocking) so a `?resume=` reconnect is handled in-handler.
    // In local mode there is no layer — the WsAuth extractor authenticates via the
    // single-use `?ticket=` (minted over the cookie-authenticated HTTP path).
    {
        let mut ws_routes = Router::new().route("/ws", get(crate::ws::ws_handler));
        if let Some(ws_layer) = ws_layer {
            ws_routes = ws_routes.layer(ws_layer);
        }
        app = app.merge(ws_routes.with_state(state.clone()));
    }

    if Path::new(&static_dir).is_dir() {
        // Serve real asset files from disk; for any unmatched path (a client-side
        // route like /studio/automations, or a hard reload / deep link) fall back
        // to index.html so the SPA can route it. ServeDir keeps a 404 status on the
        // not-found response even though it serves index.html — the `spa_ok_status`
        // middleware below flips that to 200 (a hard reload / deep link is a valid
        // page, not a Not-Found; a 404 breaks reloads and trips monitoring).
        let index = format!("{static_dir}/index.html");
        let serve_dir = ServeDir::new(&static_dir).not_found_service(ServeFile::new(index));
        app = app.fallback_service(serve_dir);
    }

    let csp_v1 = csp_value.clone();
    let app = app
        .layer(axum::middleware::from_fn(spa_ok_status))
        .layer(axum::middleware::from_fn(move |req, next| {
            security_headers(csp_value.clone(), req, next)
        }))
        .layer(TraceLayer::new_for_http())
        .layer(desktop_cors(&state.boot.server.desktop_origins));

    // The OpenAI-compatible surface is merged AFTER the global stack, so it
    // carries its own. Two reasons it cannot simply live inside:
    //  * the global `CorsLayer` denies cross-origin and answers preflight itself
    //    without calling any inner service, so a permissive layer mounted under
    //    it could never run — and `/v1` needs the option of allowing browser
    //    tooling (see the module's CORS middleware);
    //  * `spa_ok_status` and the SPA fallback belong to the app; an unmatched
    //    `/v1` path must 404 as JSON, never as the index page.
    // It keeps `security_headers` (HSTS in particular belongs on an API surface)
    // and tracing.
    let v1 = v1::router(&state)
        .route_layer(axum::middleware::from_fn(http_metrics))
        .layer(axum::middleware::from_fn_with_state(state.clone(), v1::cors))
        .with_state(state.clone())
        .layer(axum::middleware::from_fn(move |req, next| {
            security_headers(csp_v1.clone(), req, next)
        }))
        .layer(TraceLayer::new_for_http());

    app.merge(v1)
}

/// Normalise the SPA deep-link/hard-reload status. The static fallback serves
/// index.html for unknown client-side routes but keeps ServeDir's 404 status; a
/// browser navigation to `/studio/automations` (or a reload of any SPA route) is a
/// real page, so an HTML 404 on a non-API GET is rewritten to 200. API/WS/ops
/// routes are untouched, so genuine JSON 404s stay 404.
async fn spa_ok_status(req: Request, next: Next) -> Response {
    let p = req.uri().path();
    let is_navigation = req.method() == axum::http::Method::GET
        && !p.starts_with("/api")
        && !p.starts_with("/v1")
        && !p.starts_with("/ws")
        && !p.starts_with("/health")
        && !p.starts_with("/metrics");
    let mut res = next.run(req).await;
    if is_navigation && res.status() == StatusCode::NOT_FOUND {
        let is_html = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/html"));
        if is_html {
            *res.status_mut() = StatusCode::OK;
        }
    }
    res
}

/// Baseline security response headers on every response (defence-in-depth).
/// `nosniff` + `DENY` framing stop MIME-confusion and clickjacking; the CSP
/// confines the SPA to same-origin assets (and contains the docx-preview DOM
/// render to non-executing content). `connect-src 'self'` covers same-origin
/// `/api` + `/ws`; `'unsafe-inline'` styles are needed by the Tailwind/React UI.
/// `frame-src 'self' blob:` permits the `srcdoc` sandboxed preview of html
/// artefacts (the framed page carries its own strict injected CSP) and the
/// in-app PDF viewer. `blob:` is listed explicitly because a CSP source
/// expression matches on scheme/host/port and a `blob:` URL has scheme `blob`
/// with an empty host, so `'self'` never matches it (`img-src` lists it for the
/// same reason). Document bytes are fetched with the caller's credentials and
/// framed from an object URL — the download endpoint always answers
/// `Content-Disposition: attachment`, so it cannot be framed directly. A blob
/// URL inherits this origin, so only PDF bytes are ever framed and the frontend
/// builds the object URL with a forced `application/pdf` type; HTML artefacts
/// stay on the `srcdoc` + `sandbox` path and are never given a blob URL.
/// The `csp` value is precomputed from config in `router` so the deployment's
/// own auth origin is what the login flow is allowed to reach.
async fn security_headers(csp: HeaderValue, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    // HSTS: pin browsers to HTTPS for 2 years, defeating downgrade/sslstrip on a
    // public TLS domain. Safe to send unconditionally — browsers ignore it over
    // plain HTTP/localhost and honour it only over HTTPS. `includeSubDomains`/
    // `preload` are deliberately omitted (opt-in per the client's domain layout).
    h.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=63072000"),
    );
    h.insert(header::CONTENT_SECURITY_POLICY, csp);
    res
}

/// Build the CSP string from config. The auth flow (keycloak-js) needs
/// `connect-src` for the token/userinfo endpoints and `frame-src` for the
/// silent-check-sso iframe, so the configured Keycloak origin is spliced in; in
/// local-auth mode there is no external auth origin and the policy stays
/// same-origin only. A third-party deployment thus gets its own auth origin, not
/// a baked-in one.
/// The cross-origin policy for the native surface. The global stack otherwise
/// denies cross-origin and answers preflight itself, so a desktop client — which
/// serves its shell from its own local origin — has to be admitted here, at the
/// outer layer, or its preflight never reaches a handler.
///
/// Credentials are deliberately not allowed: a desktop client authenticates with
/// its own token in the `Authorization` header, never an ambient cookie, so
/// granting the origin cookie access would only widen what a compromised client
/// could reach. With no configured origins the layer is `CorsLayer::new()` —
/// the deny-cross-origin default, unchanged from before this existed.
fn desktop_cors(origins: &[String]) -> CorsLayer {
    let parsed: Vec<HeaderValue> =
        origins.iter().filter_map(|o| match HeaderValue::from_str(o.trim()) {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!(origin = %o, "ignoring unparseable desktop origin");
                None
            }
        })
        .collect();
    if parsed.is_empty() {
        return CorsLayer::new();
    }
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(parsed))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

fn build_csp(boot: &crate::config::BootConfig) -> String {
    build_csp_inner(
        &boot.server.public_url,
        &boot.keycloak.url,
        boot.keycloak.is_configured(),
    )
}

fn build_csp_inner(public_url: &str, keycloak_url: &str, keycloak_configured: bool) -> String {
    // Add the Keycloak origin only when keycloak auth is configured and it is a
    // distinct origin from where the SPA is served ('self' already covers it).
    let self_origin = crate::ws::origin_of(public_url);
    let auth = if keycloak_configured {
        crate::ws::origin_of(keycloak_url).filter(|o| Some(o) != self_origin.as_ref())
    } else {
        None
    };
    let auth = auth.map(|o| format!(" {o}")).unwrap_or_default();
    format!(
        "default-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; \
         script-src 'self' 'sha256-fHEyTuBQgebpyWZPwX0cILbtpEM+jmMNnAHGBZyIbvQ='; \
         worker-src 'self'; \
         connect-src 'self'{auth}; \
         object-src 'none'; base-uri 'self'; frame-ancestors 'none'; \
         frame-src 'self' blob:{auth}"
    )
}

#[cfg(test)]
mod csp_tests {
    use super::build_csp_inner;

    #[test]
    fn includes_configured_keycloak_origin() {
        let csp = build_csp_inner("https://chat.example.com", "https://auth.example.com/realms/x", true);
        assert!(csp.contains("connect-src 'self' https://auth.example.com;"), "{csp}");
        assert!(csp.contains("frame-src 'self' blob: https://auth.example.com"), "{csp}");
        assert!(!csp.contains("scottish-ai"), "{csp}");
    }

    #[test]
    fn local_auth_is_self_only() {
        let csp = build_csp_inner("http://localhost:8088", "", false);
        assert!(csp.contains("connect-src 'self';"), "{csp}");
        assert!(csp.trim_end().ends_with("frame-src 'self' blob:"), "{csp}");
        assert!(!csp.contains("https://"), "{csp}");
    }

    #[test]
    fn omits_keycloak_when_same_origin_as_spa() {
        let csp = build_csp_inner("https://app.example.com", "https://app.example.com/auth", true);
        assert!(csp.contains("connect-src 'self';"), "{csp}");
        assert!(csp.trim_end().ends_with("frame-src 'self' blob:"), "{csp}");
    }

    // The in-app document viewers frame PDF bytes from an object URL, which a
    // bare `frame-src 'self'` would refuse — the allowance must survive every
    // auth configuration, not just the local-auth one.
    #[test]
    fn frames_blob_urls_in_every_auth_mode() {
        for csp in [
            build_csp_inner("http://localhost:8088", "", false),
            build_csp_inner("https://app.example.com", "https://app.example.com/auth", true),
            build_csp_inner("https://chat.example.com", "https://auth.example.com/realms/x", true),
        ] {
            assert!(csp.contains("frame-src 'self' blob:"), "{csp}");
        }
    }
}

/// Identity of the Bearer-authenticated caller + the host capabilities the SPA
/// should expose (code-interpreter, voice).
async fn whoami(State(state): State<AppState>, AuthUser(ctx): AuthUser) -> impl IntoResponse {
    // Read the DB row for the authoritative (possibly user-customised) name and an
    // avatar cache-buster, so the sidebar/chat show the chosen name + image rather
    // than the raw Keycloak claim or a 404-flicker. Falls back to the token claim.
    let mut display_name = ctx.display_name.clone();
    let mut avatar_updated_at: Option<i64> = None;
    if let Some(uid) = ctx.user_id {
        if let Ok(Some(row)) = sqlx::query!(
            r#"SELECT display_name, extract(epoch from avatar_updated_at)::bigint AS "epoch?"
               FROM users WHERE id = $1"#,
            uid
        )
        .fetch_optional(&state.pg)
        .await
        {
            display_name = Some(row.display_name);
            avatar_updated_at = row.epoch;
        }
    }
    // Moderator is a distinct, assignment-granted role (orthogonal to the base role) —
    // gates the SPA Moderation tab. Never implied by admin.
    let is_moderator = match ctx.user_id {
        Some(uid) => state.moderation.is_moderator(&state, uid).await,
        None => false,
    };
    // Voice / live-voice / groundedness resolve through the FeatureResolver (host
    // boot flag OR the admin runtime toggle `features.*`), so a self-hoster can
    // enable them from the admin UI without a `.env` edit + restart.
    let voice_on = state.features.enabled_for(&state, &ctx, "voice").await;
    let voice_live = state.features.enabled_for(&state, &ctx, "voice_live").await;
    let groundedness_on = state.features.enabled_for(&state, &ctx, "groundedness").await;
    // Ground-or-cut repair: feature on AND the runtime knob enabled (default off).
    let groundedness_repair =
        groundedness_on && crate::ml::groundedness_repair_enabled(&state.pg).await;
    // Live voice: the host flag plus the client-relevant runtime dials (so the SPA
    // can default push-to-talk and require echo cancellation without a second call).
    let voice_live_opts = if voice_live {
        let k = crate::voice::VoiceKnobs::load(&state.pg).await;
        json!({
            "ptt_default": k.ptt_default,
            "aec_required": k.aec_required,
            "silence_threshold_ms": k.silence_threshold_ms,
        })
    } else {
        serde_json::Value::Null
    };
    // Streaming dictation: the composer mic streams live text-while-speaking when a
    // realtime STT engine is configured; otherwise the SPA keeps batch-on-pause.
    let dictation_streaming = voice_on && {
        crate::voice::VoiceLiveResolved::load(&state.pg, state.message_key, &state.boot.voice_live)
            .await
            .has_streaming_stt()
    };
    // Core presence capability: the OpenAI-compatible surface and its keys.
    // Resolved per user (a group can have it switched off), so the SPA hides the
    // Profile key management for exactly the people who cannot use it.
    let public_api = state.features.enabled_for(&state, &ctx, "public_api").await;
    // Edition capability: white-label branding, resolved through the
    // FeatureResolver seam (host flag ∧ group-restrict) so the SPA hides the
    // Enterprise-only branding section when off.
    let white_label = state.features.enabled_for(&state, &ctx, "white_label").await;
    // Edition capabilities: same FeatureResolver path — the SPA hides
    // the Audit/Holds/Moderation sections, the /moderation route, and the chat
    // Review & Approve sign-off when their flag is off.
    let compliance_audit = state.features.enabled_for(&state, &ctx, "compliance_audit").await;
    let moderation = state.features.enabled_for(&state, &ctx, "moderation").await;
    let message_review = state.features.enabled_for(&state, &ctx, "message_review").await;
    // Edition capability: data-owner approval for access-bearing group
    // adds — gates the approval-inbox endpoints + the sidebar Access-requests UI.
    let data_owner_approval = state.features.enabled_for(&state, &ctx, "data_owner_approval").await;
    // Edition capability (Enterprise SSO/SCIM): federated SSO brokering + SCIM
    // provisioning — gates the Admin Identity section + the `/api/admin/sso/*`
    // endpoints (the SCIM server is enterprise-binary-only). Off in Core.
    let federated_sso = state.features.enabled_for(&state, &ctx, "federated_sso").await;
    // Edition capability (custom RBAC): custom roles, delegated admin, ABAC — gates
    // the Admin Roles & Access section. Off in Core.
    let custom_rbac = state.features.enabled_for(&state, &ctx, "custom_rbac").await;
    // Edition capability (connectors): DMS/mail connectors — gates the Profile
    // Connections tab + the Admin Connectors section + the `/api/connectors/*`
    // endpoints. Off in Core.
    let enterprise_connectors = state.features.enabled_for(&state, &ctx, "enterprise_connectors").await;
    // Fine-grained permissions the caller effectively holds (resolved through the
    // RbacPolicy seam). Empty in Core → the SPA falls back to its `is_admin` gate;
    // an Enterprise policy returns the delegated set (scoped holdings marked
    // `perm:scoped`) so a delegated admin sees exactly their own sections.
    let permissions = state.rbac.resolved_permissions(&state.pg, &ctx).await.unwrap_or_default();
    // Presence capability (Core): team chats + DMs. Resolved through the
    // FeatureResolver seam so the runtime override + per-group restriction apply;
    // the SPA hides the Teams/DM nav and routes when off.
    let messaging = state.features.enabled_for(&state, &ctx, "messaging").await;
    // Workflows presence capability (Core): resolved through the same seam so the
    // admin runtime toggle (`features.workflows`) + per-group restriction apply.
    let workflows = state.features.enabled_for(&state, &ctx, "workflows").await;
    // Capability-aware reasoning control: resolve the effective
    // llm provider for this caller (per-user BYOK wins) and compute what reasoning the
    // model supports, so the SPA renders the right control (or hides it). An unset
    // provider (ML .env default) ⇒ a safe `toggle`.
    let (reasoning, vision, llm_configured) = {
        // The default llm provider (no chat context): the caller's own default, else
        // the deployment default (multi-LLM). The composer's LIVE reasoning control is
        // re-derived per selected provider from GET /api/me/llm-providers; this is the
        // fallback for a draft chat / single-provider deploy.
        let p = crate::providers::resolve_llm(&state.pg, state.message_key, ctx.user_id, None, None).await.ok().flatten();
        let (base_url, model, mode) = match &p {
            Some(p) => (p.base_url.as_deref(), p.model.as_deref(), p.reasoning_mode.as_deref()),
            None => (None, None, None),
        };
        // Vision (image input) capability of the same resolved llm — lets the SPA
        // hint that attached images are seen (vision) vs OCR'd to text (fallback).
        let vision = crate::vision::detect(base_url, model, None);
        // First-run onboarding signal: is an LLM provider wired up at all? Reuses the
        // already-resolved row (no extra query). None ⇒ the SPA shows the setup checklist.
        (crate::reasoning::detect(base_url, model, mode), vision, p.is_some())
    };
    Json(json!({
        "user_id": ctx.user_id,
        "email": ctx.email,
        "display_name": display_name,
        "role": ctx.role.as_str(),
        "break_glass": ctx.break_glass,
        // Enrolment-only local session: the SPA redirects into the
        // MFA setup wizard and hides everything else until the user enrols.
        "mfa_enroll_only": ctx.mfa_enroll_only,
        "is_moderator": is_moderator,
        "avatar_updated_at": avatar_updated_at,
        // First-run onboarding: false ⇒ no LLM provider configured yet (empty deploy).
        "llm_configured": llm_configured,
        "permissions": permissions,
        "capabilities": {
            "code_interpreter": state.boot.features.code_interpreter,
            "voice": voice_on,
            "voice_live": voice_live,
            "dictation_streaming": dictation_streaming,
            "workflows": workflows,
            "groundedness": groundedness_on,
            "groundedness_repair": groundedness_repair,
            "mcp": state.boot.features.mcp,
            "messaging": messaging,
            "public_api": public_api,
            "white_label": white_label,
            "compliance_audit": compliance_audit,
            "moderation": moderation,
            "message_review": message_review,
            "data_owner_approval": data_owner_approval,
            "federated_sso": federated_sso,
            "custom_rbac": custom_rbac,
            "enterprise_connectors": enterprise_connectors,
            "reasoning": reasoning,
            "vision": vision,
        },
        "voice_live_opts": voice_live_opts,
    }))
}

/// Constant-time byte equality — no early exit on a content mismatch, so a
/// timing side-channel can't recover the secret byte-by-byte. (Length is
/// allowed to short-circuit: a high-entropy token's length is not the secret.)
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Prometheus scrape endpoint. **Fail-closed:** with no `observability.metrics_token`
/// configured the endpoint is *disabled* (404 — reveals nothing), never served
/// open. When a token is set it is required (Bearer or `?token=`) and checked in
/// constant time. System telemetry is therefore unreadable by default; enabling
/// it is a deliberate, authenticated act. Keep the scrape internal (loopback) and
/// never proxy `/metrics` to the public gateway.
async fn metrics_endpoint(State(state): State<AppState>, req: Request) -> Response {
    let want = state.boot.observability.metrics_token.trim();
    // No token ⇒ metrics are off. Return 404 so the surface is invisible to a probe.
    if want.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    let query_tok = req.uri().query().and_then(|q| {
        q.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == "token").map(|(_, v)| v))
    });
    let want_b = want.as_bytes();
    let ok = bearer.is_some_and(|t| ct_eq(t.as_bytes(), want_b))
        || query_tok.is_some_and(|t| ct_eq(t.as_bytes(), want_b));
    if !ok {
        return (StatusCode::UNAUTHORIZED, "metrics token required").into_response();
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        crate::metrics::render(),
    )
        .into_response()
}

/// Per-request HTTP metrics: count by method/route/status + a latency histogram.
/// `MatchedPath` keeps the `route` label low-cardinality (pattern, not raw ids).
async fn http_metrics(req: Request, next: Next) -> Response {
    let method = req.method().as_str().to_owned();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned())
        .unwrap_or_else(|| "other".to_owned());
    let start = Instant::now();
    let res = next.run(req).await;
    let status = res.status().as_u16().to_string();
    let elapsed = start.elapsed().as_secs_f64();
    metrics::counter!("http_requests_total", "method" => method.clone(), "route" => route.clone(), "status" => status.clone()).increment(1);
    metrics::histogram!("http_request_duration_seconds", "method" => method, "route" => route, "status" => status).record(elapsed);
    res
}

/// Mint a single-use WebSocket connect ticket for the authenticated caller.
/// The browser calls this with its Bearer token in the `Authorization` header
/// (never a URL) and then opens `/ws?ticket=<t>`, so the JWT never appears in a
/// socket URL / access log. The ticket is short-lived and single-use.
///
/// `MaybeDevice` follows `AuthUser` so a device token's ticket carries the device
/// id forward: the socket opened with it inherits the desktop provenance.
async fn ws_ticket(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    MaybeDevice(device_id): MaybeDevice,
) -> impl IntoResponse {
    let Some(user_id) = ctx.user_id else {
        return (axum::http::StatusCode::FORBIDDEN, "no user").into_response();
    };
    if !crate::cache::rate_limit_ok(&state.redis, &format!("ws-ticket:{user_id}"), 30, 60).await {
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, "rate limited").into_response();
    }
    match crate::ws::session::issue_ticket(&state.redis, user_id, device_id).await {
        Ok(ticket) => Json(json!({ "ticket": ticket })).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "ws-ticket mint failed");
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "ticket mint failed").into_response()
        }
    }
}

/// Break-glass-gated probe — proves the ephemeral super-admin path.
async fn admin_ping(SuperAdmin(ctx): SuperAdmin) -> impl IntoResponse {
    Json(json!({ "ok": true, "role": ctx.role.as_str(), "break_glass": ctx.break_glass }))
}

/// Read-only list of active break-glass grants — for the admin UI's
/// "super-admin active" indicator. Gated by the break-glass extractor itself
/// (you present a valid grant to view the set). The mint/revoke lifecycle is the
/// `fosnie-backend breakglass` CLI (direct Redis), which works with the server down.
async fn breakglass_grants(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
) -> crate::error::Result<Json<Vec<serde_json::Value>>> {
    // Fingerprints only — never expose another live grant's full token over the API
    // (it would let one super-admin lift another's credential). The CLI shows full
    // tokens (host-only trust boundary).
    let grants = crate::auth::breakglass::list_active(&state).await?;
    Ok(Json(
        grants
            .into_iter()
            .map(|g| json!({
                "fp": g.grant_id.chars().take(8).collect::<String>(),
                "ttl_secs": g.ttl_secs,
                "label": g.label,
                "reason": g.reason,
            }))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::ct_eq;

    #[test]
    fn ct_eq_matches_only_identical_bytes() {
        assert!(ct_eq(b"s3cret-token", b"s3cret-token"));
        assert!(!ct_eq(b"s3cret-token", b"s3cret-tokeX")); // same length, last byte differs
        assert!(!ct_eq(b"s3cret-token", b"s3cret")); // length mismatch
        assert!(!ct_eq(b"", b"x"));
        assert!(ct_eq(b"", b"")); // empty==empty (the handler rejects empty tokens before calling)
    }
}
