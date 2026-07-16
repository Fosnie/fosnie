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

//! Native MCP tool support (FEATURE B1). The platform is an MCP **client/host**:
//! admin-registered, allow-listed, sandboxed, fully-audited, zero-egress. Tools
//! discovered from approved servers ride the SAME rails as native tools — mapped to
//! the OpenAI tool schema, **namespaced** `slug__tool`, appended to the per-turn
//! tool defs, dispatched through the same loop, results normalised into the same
//! envelope. "Is MCP" is metadata, not a separate code path.
//!
//! Submodules: `client` (the rmcp transport boundary + trait), `manager` (the live
//! connection registry on AppState), `pin` (rug-pull fingerprints), `validate`
//! (private-endpoint check), `oauth_policy` (SSRF policy for discovered OAuth
//! authorisation-server endpoints).

pub mod client;
pub mod manager;
pub mod oauth_flow;
pub mod oauth_policy;
pub mod oauth_store;
pub mod pin;
pub mod validate;

pub use manager::McpManager;

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditEvent, AuditOutcome};
use crate::auth::rbac::{Permission, ResourceType};
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::integrations::{self, ConnectorKind};
use crate::state::AppState;

/// The namespace delimiter between a server slug and a tool name. A server slug may
/// not contain it (DB CHECK in `0060_mcp_servers.sql`), so the first `__` splits.
pub const NS_DELIM: &str = "__";

pub fn namespaced(slug: &str, tool: &str) -> String {
    format!("{slug}{NS_DELIM}{tool}")
}
pub fn is_namespaced(name: &str) -> bool {
    name.contains(NS_DELIM)
}
pub fn split(name: &str) -> Option<(&str, &str)> {
    name.split_once(NS_DELIM)
}

/// One discovered MCP tool (catalog entry). `side_effecting` drives HITL gating
/// (unknown ⇒ true). Serialised into `mcp_servers.tools_catalog`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCatalogEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub schema: Value,
    #[serde(default = "default_true")]
    pub side_effecting: bool,
}
fn default_true() -> bool {
    true
}

fn parse_catalog(v: Option<Value>) -> Vec<ToolCatalogEntry> {
    v.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// The pinned catalogue of the server `slug`, or `None` if no such server exists. Used at
/// config time to validate an agent's MCP tool grants against the catalogue (validate on
/// write; a stored grant is tolerated on read even if the tool later vanishes).
pub async fn server_catalogue(
    pg: &sqlx::PgPool,
    slug: &str,
) -> Result<Option<Vec<ToolCatalogEntry>>> {
    let row = sqlx::query!("SELECT tools_catalog FROM mcp_servers WHERE slug = $1", slug)
        .fetch_optional(pg)
        .await?;
    Ok(row.map(|r| parse_catalog(r.tools_catalog)))
}

/// What an agent is granted on one MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantScope {
    /// Every tool in the server's pinned catalogue, including ones added later. A server
    /// that adds a tool is quarantined until an admin re-approves the changed catalogue,
    /// so growth is human-mediated — `All` never silently widens to an unreviewed tool.
    All,
    /// A specific, explicit set of tool names. Each must be present in the pinned
    /// catalogue at read time to actually grant.
    Tools(HashSet<String>),
}

/// An agent's MCP grants, parsed from its `tools` list: one entry per server slug.
///
/// Grammar (`slug` cannot contain the `__` delimiter, per the DB CHECK, so the first
/// `__` splits unambiguously):
/// - `slug__*` grants [`GrantScope::All`] — the whole (pinned) catalogue.
/// - `slug__toolname` grants that one tool.
/// - Both present ⇒ `All` wins, regardless of order.
/// - A tool literally named `*` is indistinguishable from the wildcard and is treated as
///   the wildcard by design (asserted in the unit tests).
///
/// Crucially, [`GrantScope::All`] does NOT mean "any string": [`McpGrants::permits`]
/// resolves it against the pinned catalogue, so the catalogue is the authority for both
/// grant shapes. A malicious server honouring an unadvertised name is refused because the
/// name is not in the catalogue.
#[derive(Debug, Clone, Default)]
pub struct McpGrants(HashMap<String, GrantScope>);

impl McpGrants {
    /// Whether `tool` on `slug` is granted, given the server's pinned `catalogue`. `All`
    /// still requires catalogue membership; an explicit tool requires both membership and
    /// an explicit grant. A stored grant whose tool has since vanished from the catalogue
    /// simply does not grant (tolerated on read — catalogues drift, configs are long-lived).
    pub fn permits(&self, slug: &str, tool: &str, catalogue: &[ToolCatalogEntry]) -> bool {
        let in_catalogue = catalogue.iter().any(|e| e.name == tool);
        match self.0.get(slug) {
            None => false,
            Some(GrantScope::All) => in_catalogue,
            Some(GrantScope::Tools(set)) => in_catalogue && set.contains(tool),
        }
    }

    /// Whether the agent has any grant on `slug` (server-grain pre-filter, before the more
    /// expensive per-server RBAC + catalogue work).
    pub fn grants_server(&self, slug: &str) -> bool {
        self.0.contains_key(slug)
    }

    /// The server slugs this agent holds any grant on.
    pub fn slugs(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Parse an agent's `tools` list into per-server [`McpGrants`]. Non-namespaced entries
/// (native + custom tools) are ignored. See [`McpGrants`] for the grammar.
pub fn parse_grants(agent_tools: &[String]) -> McpGrants {
    let mut map: HashMap<String, GrantScope> = HashMap::new();
    for t in agent_tools {
        let Some((slug, tool)) = split(t) else { continue };
        if tool == "*" {
            map.insert(slug.to_string(), GrantScope::All);
        } else {
            match map.entry(slug.to_string()).or_insert_with(|| GrantScope::Tools(HashSet::new())) {
                GrantScope::All => {} // wildcard already won; keep it
                GrantScope::Tools(set) => {
                    set.insert(tool.to_string());
                }
            }
        }
    }
    McpGrants(map)
}

/// The namespaced OpenAI tool defs the caller may use this turn: from servers that
/// are enabled + active + not quarantined + RBAC-readable by the caller AND assigned to
/// the active agent (`allowed`). Empty when the feature is off, the connector is dormant
/// (zero-egress default), or the agent has no MCP servers assigned.
pub async fn session_tool_defs(
    state: &AppState,
    ctx: &AuthContext,
    grants: &McpGrants,
) -> Vec<Value> {
    if !state.boot.features.mcp || grants.is_empty() {
        return Vec::new();
    }
    if !integrations::is_enabled(&state.pg, ConnectorKind::Mcp).await.unwrap_or(false) {
        return Vec::new();
    }
    let rows = match sqlx::query!(
        r#"SELECT id, slug, auth_type, tools_catalog FROM mcp_servers WHERE status = 'active' AND enabled"#
    )
    .fetch_all(&state.pg)
    .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut defs = Vec::new();
    for r in rows {
        if !grants.grants_server(&r.slug) {
            continue; // not assigned to this agent
        }
        if !state.rbac.can(&state.pg, ctx, ResourceType::McpServer, r.id, Permission::Read)
            .await
            .unwrap_or(false)
        {
            continue;
        }
        // OAuth servers need a usable connection (this user's own, or the service
        // connection for an unattended run). Without one, omit the server's tools silently
        // (the UI surfaces a Connect prompt); do not error the turn.
        if r.auth_type == "oauth" && !has_active_oauth_connection(state, ctx.user_id, r.id).await {
            continue;
        }
        let catalogue = parse_catalog(r.tools_catalog);
        for t in &catalogue {
            if !grants.permits(&r.slug, &t.name, &catalogue) {
                continue; // granted the server, but not this specific tool
            }
            let schema = if t.schema.is_null() {
                json!({ "type": "object", "properties": {} })
            } else {
                t.schema.clone()
            };
            defs.push(json!({
                "type": "function",
                "function": {
                    "name": namespaced(&r.slug, &t.name),
                    "description": t.description,
                    "parameters": schema,
                }
            }));
        }
    }
    defs
}

/// Whether there is an active OAuth connection usable by a caller with the given
/// `user_id` to `server_id`. `Some(uid)` looks for that user's own connection; `None`
/// (an unattended run) falls back to the service connection (`user_id IS NULL`), matching
/// [`resolve_oauth_connection`], so the defs offered and the connection actually resolved
/// at dispatch agree.
async fn has_active_oauth_connection(state: &AppState, user_id: Option<Uuid>, server_id: Uuid) -> bool {
    let found = match user_id {
        Some(uid) => sqlx::query_scalar!(
            r#"SELECT 1 FROM mcp_oauth_connections
                 WHERE mcp_server_id = $1 AND user_id = $2 AND status = 'active'"#,
            server_id,
            uid
        )
        .fetch_optional(&state.pg)
        .await,
        None => sqlx::query_scalar!(
            r#"SELECT 1 FROM mcp_oauth_connections
                 WHERE mcp_server_id = $1 AND user_id IS NULL AND status = 'active'"#,
            server_id
        )
        .fetch_optional(&state.pg)
        .await,
    };
    matches!(found, Ok(Some(_)))
}

/// Resolve which OAuth connection a caller's tool call should run under, ensuring the
/// live connection exists (building it lazily on a cache miss). Returns the connection id.
/// For the durable-resume / unattended paths `user_id` may be `None`, in which case the
/// service connection (`user_id IS NULL`) is used; there is deliberately no fall-back to
/// an arbitrary user's token.
async fn resolve_oauth_connection(
    state: &AppState,
    server_id: Uuid,
    slug: &str,
    url: &str,
    user_id: Option<Uuid>,
) -> Result<Uuid> {
    let found: Option<Uuid> = match user_id {
        Some(uid) => sqlx::query_scalar!(
            r#"SELECT id FROM mcp_oauth_connections
                 WHERE mcp_server_id = $1 AND user_id = $2 AND status = 'active'"#,
            server_id,
            uid
        )
        .fetch_optional(&state.pg)
        .await?,
        None => sqlx::query_scalar!(
            r#"SELECT id FROM mcp_oauth_connections
                 WHERE mcp_server_id = $1 AND user_id IS NULL AND status = 'active'"#,
            server_id
        )
        .fetch_optional(&state.pg)
        .await?,
    };
    let connection_id = found
        .ok_or_else(|| AppError::Validation(format!("no active OAuth connection for MCP server '{slug}'")))?;

    if !state.mcp.is_connected(slug, Some(connection_id)).await {
        let client = oauth_flow::load_oauth_client_row(&state.pg, server_id)
            .await?
            .ok_or_else(|| AppError::Validation(format!("MCP server '{slug}' has no approved OAuth client")))?;
        let conn = oauth_flow::connect_oauth_conn(state, url, &client, connection_id).await?;
        state.mcp.insert_conn(slug, Some(connection_id), conn).await;
    }
    Ok(connection_id)
}

/// True iff a tool-call error is an OAuth reauthorisation signal (expired/revoked token
/// that could not be refreshed, or an insufficient-scope 403).
fn is_reauth_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("authorization required") || m.contains("insufficient scope")
}

/// Proof that a specific MCP tool call passed every authorisation gate. Its fields are
/// private and there is no public constructor, so a call cannot reach the transport
/// (`McpManager::call_tool` / `list_tools`) without first obtaining one from
/// [`authorize_call`] or [`authorize_system_call`] — bypass is a compile error.
pub struct AuthorizedCall {
    server_id: Uuid,
    slug: String,
    tool: String,
    connection_id: Option<Uuid>,
}

impl AuthorizedCall {
    pub(in crate::mcp) fn slug(&self) -> &str {
        &self.slug
    }
    pub(in crate::mcp) fn tool(&self) -> &str {
        &self.tool
    }
    pub(in crate::mcp) fn connection_id(&self) -> Option<Uuid> {
        self.connection_id
    }
}

/// The outcome of authorising an MCP call. Three outcomes, all pre-existing in the
/// original code and all preserved so behaviour is unchanged:
/// - `Allowed` — proceed with the witnessed call.
/// - `Recoverable` — return the string to the model as `Ok("error: …")` so it can recover
///   (unavailable server, missing connection, ungranted or off-catalogue tool).
/// - `Denied` — a hard error (egress refused, RBAC denied, unknown server) → `Err`.
pub enum CallDecision {
    Allowed(AuthorizedCall),
    Recoverable(String),
    Denied(AppError),
}

/// Authorise one namespaced MCP tool call for an agent turn. The single place every
/// pre-call gate lives: egress → server exists → active → RBAC → grant (resolved against
/// the pinned catalogue) → a usable connection. A recoverable refusal is audited as a
/// failed `mcp.call` carrying a `denied` marker (`server` | `grant` | `catalogue`) so a
/// SOC can see a model reaching for a tool it was never offered — the tell for injection.
pub async fn authorize_call(
    state: &AppState,
    ctx: &AuthContext,
    grants: &McpGrants,
    chat_id: Uuid,
    slug: &str,
    tool: &str,
) -> CallDecision {
    // Zero-egress choke-point (dormant ⇒ refuse + audit `integration.blocked`).
    if let Err(e) = integrations::guard_egress(state, ctx, ConnectorKind::Mcp).await {
        return CallDecision::Denied(e);
    }
    let server = match sqlx::query!(
        "SELECT id, status, auth_type, url, tools_catalog FROM mcp_servers WHERE slug = $1",
        slug
    )
    .fetch_optional(&state.pg)
    .await
    {
        Ok(Some(s)) => s,
        Ok(None) => {
            return CallDecision::Denied(AppError::Validation(format!("unknown MCP server '{slug}'")))
        }
        Err(e) => return CallDecision::Denied(e.into()),
    };
    if server.status != "active" {
        audit_denied(state, ctx, chat_id, server.id, slug, tool, "server").await;
        return CallDecision::Recoverable(format!(
            "error: MCP server '{slug}' is {} (unavailable)",
            server.status
        ));
    }
    if let Err(e) =
        state.rbac.require(&state.pg, ctx, ResourceType::McpServer, server.id, Permission::Read).await
    {
        audit_denied(state, ctx, chat_id, server.id, slug, tool, "rbac").await;
        return CallDecision::Denied(e);
    }
    // Grant + pinned-catalogue check. The model may name any string (our own text-tool
    // fallback can even manufacture one from a remote result), so this is where an
    // ungranted, cross-server, or off-catalogue tool is stopped.
    let catalogue = parse_catalog(server.tools_catalog);
    if !grants.permits(slug, tool, &catalogue) {
        let marker = if catalogue.iter().any(|e| e.name == tool) { "grant" } else { "catalogue" };
        audit_denied(state, ctx, chat_id, server.id, slug, tool, marker).await;
        return CallDecision::Recoverable(format!(
            "error: tool '{tool}' is not available to this agent"
        ));
    }
    // OAuth servers run under the caller's connection (their own, or the service
    // connection for an unattended run); resolve it and connect lazily. No connection is a
    // recoverable error, not a broken turn.
    let connection_id = if server.auth_type == "oauth" {
        let url = server.url.clone().unwrap_or_default();
        match resolve_oauth_connection(state, server.id, slug, &url, ctx.user_id).await {
            Ok(cid) => Some(cid),
            Err(_) => {
                audit_denied(state, ctx, chat_id, server.id, slug, tool, "connection").await;
                return CallDecision::Recoverable(format!(
                    "error: you are not connected to MCP server '{slug}'. Connect it under \
                     Profile then Connections, then try again."
                ));
            }
        }
    } else {
        None
    };
    CallDecision::Allowed(AuthorizedCall {
        server_id: server.id,
        slug: slug.to_string(),
        tool: tool.to_string(),
        connection_id,
    })
}

/// Why a system path (no agent, no user) is touching the transport. The deliberate,
/// enumerable hole in the authorisation wall — kept small and easy to review.
pub enum SystemPurpose {
    /// The periodic health sweep, reading a server's live catalogue to diff against the
    /// pin. Requires the server to be `active`.
    HealthSweep,
    /// Admin approval of an OAuth server: read the catalogue from the just-connected
    /// designated connection. Does NOT require `active` (approval is what makes it active).
    Approve { connection_id: Uuid },
}

/// The single system entry point to the transport for callers with no `AuthContext` and
/// no agent grant. Resolves the connection to use and audits every use. It deliberately
/// does NOT check an agent grant (there is none) and, for `Approve`, does NOT require the
/// server to be active.
pub async fn authorize_system_call(
    state: &AppState,
    slug: &str,
    purpose: SystemPurpose,
) -> Result<AuthorizedCall> {
    let server = sqlx::query!("SELECT id, status, auth_type FROM mcp_servers WHERE slug = $1", slug)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation(format!("unknown MCP server '{slug}'")))?;
    let connection_id = match &purpose {
        SystemPurpose::Approve { connection_id } => Some(*connection_id),
        SystemPurpose::HealthSweep => {
            if server.status != "active" {
                return Err(AppError::Validation(format!("MCP server '{slug}' is not active")));
            }
            if server.auth_type == "oauth" {
                let cid = sqlx::query_scalar!(
                    r#"SELECT id FROM mcp_oauth_connections
                         WHERE mcp_server_id = $1 AND is_catalog_source AND status = 'active'"#,
                    server.id
                )
                .fetch_optional(&state.pg)
                .await?
                .ok_or_else(|| {
                    AppError::Validation(format!("no catalogue-source connection for '{slug}'"))
                })?;
                Some(cid)
            } else {
                None
            }
        }
    };
    let purpose_label = match purpose {
        SystemPurpose::HealthSweep => "health_sweep",
        SystemPurpose::Approve { .. } => "approve",
    };
    let mut ev = AuditEvent::action("mcp.system_call", "system");
    ev.resource_type = Some("mcp_server".into());
    ev.resource_id = Some(server.id);
    ev.payload = Some(json!({ "server": slug, "purpose": purpose_label }));
    let _ = audit::append(&state.pg, &ev).await;
    Ok(AuthorizedCall {
        server_id: server.id,
        slug: slug.to_string(),
        tool: String::new(),
        connection_id,
    })
}

/// The approval path's transport touch, owned by `mcp` so the HTTP layer never holds a raw
/// connection: authorise (system) → connect the designated OAuth connection → register it
/// → read + pin the catalogue. Returns the discovered catalogue.
pub async fn approve_oauth_catalog(
    state: &AppState,
    server_id: Uuid,
    slug: &str,
    url: &str,
    connection_id: Uuid,
) -> Result<Vec<ToolCatalogEntry>> {
    let call = authorize_system_call(state, slug, SystemPurpose::Approve { connection_id }).await?;
    let client = oauth_flow::load_oauth_client_row(&state.pg, server_id)
        .await?
        .ok_or_else(|| AppError::Validation("configure an OAuth client before approving".into()))?;
    let conn = oauth_flow::connect_oauth_conn(state, url, &client, connection_id).await?;
    state.mcp.insert_conn(slug, Some(connection_id), conn).await;
    let catalog = state.mcp.list_tools(&call).await?;
    record_catalog(&state.pg, server_id, &catalog).await?;
    Ok(catalog)
}

/// Audit a recoverable authorisation refusal as a failed `mcp.call` with a `denied`
/// marker, so injection attempts (a model calling something it was never offered) are
/// visible to a SOC.
async fn audit_denied(
    state: &AppState,
    ctx: &AuthContext,
    chat_id: Uuid,
    server_id: Uuid,
    slug: &str,
    tool: &str,
    marker: &str,
) {
    let mut ev = AuditEvent::action("mcp.call", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("mcp_server".into());
    ev.resource_id = Some(server_id);
    ev.outcome = AuditOutcome::Failure;
    ev.payload = Some(json!({ "chat_id": chat_id, "server": slug, "tool": tool, "denied": marker }));
    let _ = audit::append(&state.pg, &ev).await;
}

/// Whether a namespaced MCP tool is side-effecting (⇒ HITL). Unknown ⇒ true (safe).
pub async fn is_side_effecting(state: &AppState, slug: &str, tool: &str) -> bool {
    let Ok(Some(row)) =
        sqlx::query!("SELECT tools_catalog FROM mcp_servers WHERE slug = $1", slug)
            .fetch_optional(&state.pg)
            .await
    else {
        return true;
    };
    parse_catalog(row.tools_catalog)
        .iter()
        .find(|t| t.name == tool)
        .map(|t| t.side_effecting)
        .unwrap_or(true)
}

/// Dispatch a namespaced MCP tool call: authorise (one seam) → call via the manager →
/// normalise → audit. Errors come back as `"error: …"` so the model can recover, exactly
/// like native tools. `grants` are the calling agent's, threaded from the turn. `durable`
/// marks the durable/unattended resume of an already-approved call (audited as such); the
/// authorisation gates are identical either way, so a since-revoked grant, RBAC entitlement,
/// or quarantined server refuses the resume rather than executing on stale state.
pub async fn dispatch(
    state: &AppState,
    ctx: &AuthContext,
    grants: &McpGrants,
    chat_id: Uuid,
    name: &str,
    args: &Value,
    durable: bool,
) -> Result<String> {
    let (slug, tool) =
        split(name).ok_or_else(|| AppError::Validation("not a namespaced MCP tool".into()))?;
    let call = match authorize_call(state, ctx, grants, chat_id, slug, tool).await {
        CallDecision::Allowed(c) => c,
        CallDecision::Recoverable(msg) => return Ok(msg),
        CallDecision::Denied(e) => return Err(e),
    };

    let started = OffsetDateTime::now_utc();
    let result = state.mcp.call_tool(&call, args.clone()).await;
    let ms = (OffsetDateTime::now_utc() - started).whole_milliseconds();

    // Map an OAuth reauth signal to a durable state change + a recoverable message.
    if let (Some(cid), Err(e)) = (call.connection_id, &result) {
        if is_reauth_error(&e.to_string()) {
            let _ = sqlx::query!(
                "UPDATE mcp_oauth_connections SET status = 'reauth_required' WHERE id = $1",
                cid
            )
            .execute(&state.pg)
            .await;
            state.mcp.disconnect(slug, Some(cid)).await;
            let mut ev = AuditEvent::action("mcp.oauth.reauth_required", ctx.role.as_str());
            ev.actor_user_id = ctx.user_id;
            ev.resource_type = Some("mcp_server".into());
            ev.resource_id = Some(call.server_id);
            ev.payload = Some(json!({ "server": slug, "tool": tool }));
            let _ = audit::append(&state.pg, &ev).await;
            let body = format!("error: {e}");
            audit_call(state, ctx, chat_id, call.server_id, slug, tool, args, &body, ms, AuditOutcome::Failure, durable).await;
            return Ok(format!(
                "error: your connection to MCP server '{slug}' needs re-authorisation. Reconnect \
                 it under Profile then Connections, then try again."
            ));
        }
    }

    let (outcome, body) = match &result {
        Ok(s) => (AuditOutcome::Success, s.clone()),
        Err(e) => (AuditOutcome::Failure, format!("error: {e}")),
    };
    audit_call(state, ctx, chat_id, call.server_id, slug, tool, args, &body, ms, outcome, durable).await;
    Ok(match result {
        Ok(s) => s,
        Err(e) => format!("error: {e}"),
    })
}

#[allow(clippy::too_many_arguments)]
async fn audit_call(
    state: &AppState,
    ctx: &AuthContext,
    chat_id: Uuid,
    server_id: Uuid,
    slug: &str,
    tool: &str,
    args: &Value,
    result: &str,
    ms: i128,
    outcome: AuditOutcome,
    durable: bool,
) {
    // Hash args + result rather than storing raw text in the chain (A2 hygiene: no
    // raw PII in audit_events). Identity/server/tool/latency are recorded in clear.
    let args_hash = hex::encode(Sha256::digest(serde_json::to_vec(args).unwrap_or_default()));
    let result_hash = hex::encode(Sha256::digest(result.as_bytes()));
    let mut ev = AuditEvent::action("mcp.call", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("mcp_server".into());
    ev.resource_id = Some(server_id);
    ev.outcome = outcome;
    let mut payload = json!({
        "chat_id": chat_id, "server": slug, "tool": tool,
        "args_hash": args_hash, "result_hash": result_hash,
        "result_bytes": result.len(), "latency_ms": ms,
    });
    if durable {
        // The durable/unattended resume of a call a human already approved.
        payload["approved"] = json!(true);
        payload["durable"] = json!(true);
    }
    ev.payload = Some(payload);
    let _ = audit::append(&state.pg, &ev).await;
}

/// Persist a discovered catalog + pin its fingerprints, marking the server `active`.
/// Called on admin approval (and re-approval after a reviewed catalog change).
pub async fn record_catalog(
    pg: &sqlx::PgPool,
    server_id: Uuid,
    catalog: &[ToolCatalogEntry],
) -> Result<()> {
    let cat_json = serde_json::to_value(catalog)
        .map_err(|e| AppError::Other(anyhow::anyhow!("serialise catalog: {e}")))?;
    let pins_json = serde_json::to_value(pin::fingerprints(catalog))
        .map_err(|e| AppError::Other(anyhow::anyhow!("serialise pins: {e}")))?;
    sqlx::query!(
        "UPDATE mcp_servers SET tools_catalog = $2, pinned_tools = $3, status = 'active', \
         last_health_at = now(), updated_at = now() WHERE id = $1",
        server_id,
        cat_json,
        pins_json,
    )
    .execute(pg)
    .await?;
    Ok(())
}

/// Quarantine a server (rug-pull detected or admin action): drop the connection,
/// mark it, and raise an anomaly-flagged audit alert.
pub async fn quarantine(state: &AppState, server_id: Uuid, slug: &str, reason: &str) {
    let _ = sqlx::query!(
        "UPDATE mcp_servers SET status = 'quarantined', updated_at = now() WHERE id = $1",
        server_id
    )
    .execute(&state.pg)
    .await;
    state.mcp.disconnect_all(slug).await;
    let mut ev = AuditEvent::action("mcp.quarantined", "system");
    ev.resource_type = Some("mcp_server".into());
    ev.resource_id = Some(server_id);
    ev.risk_anomaly_flag = true;
    ev.payload = Some(json!({ "server": slug, "reason": reason }));
    let _ = audit::append(&state.pg, &ev).await;
}

/// Periodic supervisor sweep (driven by `TaskType::McpHealth`): for each active,
/// connected server — ping + refresh, diff the live catalog against the pinned set
/// (rug-pull → quarantine), or mark unreachable. Returns servers checked.
pub async fn health_sweep(state: &AppState) -> Result<u64> {
    if !state.boot.features.mcp {
        return Ok(0);
    }
    let rows = sqlx::query!(
        r#"SELECT id, slug, auth_type, url, pinned_tools FROM mcp_servers WHERE status = 'active' AND enabled"#
    )
    .fetch_all(&state.pg)
    .await?;
    let mut checked = 0u64;
    for r in rows {
        // The single audited system entry point: resolves the catalogue-source connection
        // (for an OAuth server the catalogue is pinned against ONE designated connection,
        // so users' legitimately-differing tool sets are never read as a rug-pull) and
        // refuses when the server is not active or has no catalogue source. A missing/dead
        // source is not an attack — skip, never quarantine.
        let call = match authorize_system_call(state, &r.slug, SystemPurpose::HealthSweep).await {
            Ok(c) => c,
            Err(_) => continue, // not active / no catalogue source → nothing to sweep
        };
        let conn_id = call.connection_id();

        if !state.mcp.is_connected(&r.slug, conn_id).await {
            if let Some(cid) = conn_id {
                // Lazily rebuild the catalogue-source connection; a build failure means
                // the source token died → unreachable, not quarantined.
                let url = r.url.clone().unwrap_or_default();
                let built = match oauth_flow::load_oauth_client_row(&state.pg, r.id).await {
                    Ok(Some(client)) => oauth_flow::connect_oauth_conn(state, &url, &client, cid).await.ok(),
                    _ => None,
                };
                match built {
                    Some(conn) => state.mcp.insert_conn(&r.slug, Some(cid), conn).await,
                    None => {
                        let _ = sqlx::query!(
                            "UPDATE mcp_servers SET status = 'unreachable', last_health_at = now() WHERE id = $1",
                            r.id
                        )
                        .execute(&state.pg)
                        .await;
                        checked += 1;
                        continue;
                    }
                }
            } else {
                continue; // admin re-approval reconnects
            }
        }

        match state.mcp.list_tools(&call).await {
            Ok(live) => {
                if let Some(pins) = r.pinned_tools.as_ref().and_then(|v| v.as_object()) {
                    if let Some(reason) = pin::diff(pins, &live) {
                        quarantine(state, r.id, &r.slug, &reason).await;
                        checked += 1;
                        continue;
                    }
                }
                let _ = sqlx::query!(
                    "UPDATE mcp_servers SET last_health_at = now() WHERE id = $1",
                    r.id
                )
                .execute(&state.pg)
                .await;
            }
            Err(_) => {
                let _ = sqlx::query!(
                    "UPDATE mcp_servers SET status = 'unreachable', last_health_at = now() WHERE id = $1",
                    r.id
                )
                .execute(&state.pg)
                .await;
                state.mcp.disconnect_all(&r.slug).await;
            }
        }
        checked += 1;
    }
    Ok(checked)
}

/// Compile-time proof that the transport is sealed (these must NOT compile).
///
/// `AuthorizedCall` has private fields and no public constructor, so it cannot be forged
/// outside `mcp` — and `McpManager::call_tool` cannot be reached without one:
/// ```compile_fail
/// let _c = fosnie_backend::mcp::AuthorizedCall {
///     server_id: todo!(), slug: todo!(), tool: todo!(), connection_id: todo!(),
/// };
/// ```
///
/// The `client` transport module is sealed to `mcp`:
/// ```compile_fail
/// use fosnie_backend::mcp::client::connect;
/// ```
///
/// And `connect_oauth_conn`, which returns a raw connection, is no longer public:
/// ```compile_fail
/// use fosnie_backend::mcp::oauth_flow::connect_oauth_conn;
/// ```
#[cfg(doc)]
pub struct TransportSealProof;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::client::FakeConn;
    use std::sync::Arc;

    fn entry(name: &str, side_effecting: bool) -> ToolCatalogEntry {
        ToolCatalogEntry {
            name: name.into(),
            description: String::new(),
            schema: json!({ "type": "object", "properties": {} }),
            side_effecting,
        }
    }
    fn catalogue(names: &[&str]) -> Vec<ToolCatalogEntry> {
        names.iter().map(|n| entry(n, false)).collect()
    }

    // ── Grant grammar ─────────────────────────────────────────────────────────
    #[test]
    fn wildcard_grants_the_whole_catalogue() {
        let g = parse_grants(&["files__*".into()]);
        let c = catalogue(&["read", "write"]);
        assert!(g.grants_server("files"));
        assert!(g.permits("files", "read", &c));
        assert!(g.permits("files", "write", &c));
    }

    #[test]
    fn explicit_tools_grant_only_those() {
        // Hole A: `files__read` must not confer `files__delete`.
        let g = parse_grants(&["files__read".into(), "files__list".into()]);
        let c = catalogue(&["read", "list", "delete"]);
        assert!(g.permits("files", "read", &c));
        assert!(g.permits("files", "list", &c));
        assert!(!g.permits("files", "delete", &c));
    }

    #[test]
    fn wildcard_wins_over_explicit_in_either_order() {
        let c = catalogue(&["read", "delete"]);
        let a = parse_grants(&["files__read".into(), "files__*".into()]);
        let b = parse_grants(&["files__*".into(), "files__read".into()]);
        assert!(a.permits("files", "delete", &c));
        assert!(b.permits("files", "delete", &c));
    }

    #[test]
    fn all_refuses_an_off_catalogue_tool() {
        // `All` is bounded by the pinned catalogue, NOT "any string" — a malicious server
        // honouring an unadvertised name is still refused.
        let g = parse_grants(&["files__*".into()]);
        let c = catalogue(&["read"]);
        assert!(!g.permits("files", "delete_everything", &c));
    }

    #[test]
    fn explicit_refuses_an_off_catalogue_tool() {
        let g = parse_grants(&["files__ghost".into()]);
        let c = catalogue(&["read"]);
        assert!(!g.permits("files", "ghost", &c));
    }

    #[test]
    fn ungranted_server_is_denied() {
        // Hole B cross-server: a grant on `files` confers nothing on `other`.
        let g = parse_grants(&["files__*".into()]);
        let c = catalogue(&["read"]);
        assert!(!g.grants_server("other"));
        assert!(!g.permits("other", "read", &c));
    }

    #[test]
    fn non_namespaced_entries_are_ignored() {
        let g = parse_grants(&["web_search".into(), "generate_artefact".into()]);
        assert!(g.is_empty());
    }

    #[test]
    fn a_tool_literally_named_star_is_the_wildcard() {
        // Deliberate: `*` as a tool name is indistinguishable from the wildcard.
        let g = parse_grants(&["files__*".into()]);
        assert!(matches!(g.0.get("files"), Some(GrantScope::All)));
    }

    // ── Manager discovery + dispatch through the witness ──────────────────────
    fn witness(slug: &str, tool: &str) -> AuthorizedCall {
        AuthorizedCall {
            server_id: Uuid::nil(),
            slug: slug.into(),
            tool: tool.into(),
            connection_id: None,
        }
    }

    #[tokio::test]
    async fn manager_two_servers_discovery_and_dispatch() {
        let mgr = McpManager::new();
        mgr.insert_conn("files", None, Arc::new(FakeConn { catalog: vec![entry("read_file", false)] }))
            .await;
        mgr.insert_conn("db", None, Arc::new(FakeConn { catalog: vec![entry("query", true)] })).await;

        assert_eq!(mgr.list_tools(&witness("files", "")).await.unwrap().len(), 1);
        assert_eq!(mgr.list_tools(&witness("db", "")).await.unwrap().len(), 1);
        assert!(mgr.is_connected("files", None).await && mgr.is_connected("db", None).await);

        let r = mgr.call_tool(&witness("files", "read_file"), json!({ "path": "/x" })).await.unwrap();
        assert!(r.contains("read_file"));
        let r2 = mgr.call_tool(&witness("db", "query"), json!({ "sql": "select 1" })).await.unwrap();
        assert!(r2.contains("query"));

        assert!(mgr.call_tool(&witness("files", "nope"), json!({})).await.is_err());
        mgr.disconnect("files", None).await;
        assert!(!mgr.is_connected("files", None).await);
    }

    // ── DB-backed authorisation matrix ────────────────────────────────────────
    // These drive the real seam (authorize_call / session_tool_defs / dispatch /
    // authorize_system_call) against seeded rows. They skip when DATABASE_URL is
    // unset. Each builds its own AppState (Core defaults: real rbac + manager).
    use crate::auth::PlatformRole;
    use crate::state::AppState;

    async fn authz_state() -> Option<(sqlx::PgPool, AppState)> {
        let db_url = std::env::var("DATABASE_URL").ok()?;
        let redis_url =
            std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
        let pg = crate::db::connect(&db_url, 5).await.ok()?;
        let redis = crate::cache::create_pool(&redis_url).ok()?;
        let boot = crate::config::BootConfig {
            database_url: db_url,
            redis_url,
            ..Default::default()
        };
        let state = AppState::new(pg.clone(), redis, std::sync::Arc::new(boot));
        // The MCP connector must be enabled or every call is refused at the egress gate.
        let _ = crate::config::runtime::set(
            &pg,
            "integration.mcp.enabled",
            "true",
            crate::config::runtime::ConfigValueType::Bool,
            "deployment",
            None,
            "system",
        )
        .await;
        Some((pg, state))
    }

    fn mcp_ctx(uid: Uuid) -> AuthContext {
        AuthContext {
            user_id: Some(uid),
            email: None,
            display_name: None,
            role: PlatformRole::User,
            break_glass: false,
            mfa_enroll_only: false,
        }
    }

    async fn seed_server(
        pg: &sqlx::PgPool,
        slug: &str,
        auth_type: &str,
        status: &str,
        tools: &[&str],
    ) -> Uuid {
        let id = Uuid::now_v7();
        let cat = Value::Array(
            tools
                .iter()
                .map(|t| json!({ "name": t, "description": "", "schema": {}, "side_effecting": false }))
                .collect(),
        );
        sqlx::query(
            "INSERT INTO mcp_servers (id, slug, name, transport, url, status, enabled, auth_type, tools_catalog) \
             VALUES ($1,$2,$2,'http','https://example.test/mcp',$3,true,$4,$5)",
        )
        .bind(id)
        .bind(slug)
        .bind(status)
        .bind(auth_type)
        .bind(cat)
        .execute(pg)
        .await
        .unwrap();
        id
    }

    async fn grant_read(pg: &sqlx::PgPool, server_id: Uuid, uid: Uuid) {
        sqlx::query(
            "INSERT INTO access_grants (id, resource_type, resource_id, principal_type, principal_id, permission) \
             VALUES ($1,'mcp_server'::grant_resource_type,$2,'user'::principal_type,$3,'read'::permission) \
             ON CONFLICT DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(server_id)
        .bind(uid)
        .execute(pg)
        .await
        .unwrap();
    }

    /// Count failed `mcp.call` audit rows carrying a specific `denied` marker.
    async fn call_denials(pg: &sqlx::PgPool, slug: &str, tool: &str, marker: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action_type='mcp.call' \
             AND payload->>'server'=$1 AND payload->>'tool'=$2 AND payload->>'denied'=$3",
        )
        .bind(slug)
        .bind(tool)
        .bind(marker)
        .fetch_one(pg)
        .await
        .unwrap()
    }

    async fn cleanup_server(pg: &sqlx::PgPool, id: Uuid) {
        let _ = sqlx::query("DELETE FROM access_grants WHERE resource_id=$1").bind(id).execute(pg).await;
        let _ = sqlx::query("DELETE FROM mcp_servers WHERE id=$1").bind(id).execute(pg).await;
    }

    /// Hole A: a `slug__read_file` grant offers exactly that tool from the server's
    /// catalogue, never its siblings. `slug__delete_everything` must be absent.
    #[tokio::test]
    async fn hole_a_session_defs_offer_only_the_granted_tool() {
        let Some((pg, state)) = authz_state().await else {
            eprintln!("skip: DATABASE_URL unset");
            return;
        };
        let uid = Uuid::now_v7();
        let slug = format!("ha{}", Uuid::now_v7().simple());
        let sid = seed_server(&pg, &slug, "none", "active", &["read_file", "delete_everything"]).await;
        grant_read(&pg, sid, uid).await;

        let grants = parse_grants(&[format!("{slug}__read_file")]);
        let defs = session_tool_defs(&state, &mcp_ctx(uid), &grants).await;
        let names: Vec<String> = defs
            .iter()
            .filter_map(|d| d["function"]["name"].as_str().map(String::from))
            .collect();
        assert!(names.contains(&format!("{slug}__read_file")), "granted tool must be offered: {names:?}");
        assert!(
            !names.contains(&format!("{slug}__delete_everything")),
            "an ungranted sibling tool must NOT be offered: {names:?}"
        );
        cleanup_server(&pg, sid).await;
    }

    /// Hole B (cross-tool): a grant on one tool confers nothing on another tool of the
    /// same server. Refused recoverably, audited denied=grant.
    #[tokio::test]
    async fn hole_b_cross_tool_call_refused_and_audited() {
        let Some((pg, state)) = authz_state().await else {
            return;
        };
        let uid = Uuid::now_v7();
        let slug = format!("hb{}", Uuid::now_v7().simple());
        let sid = seed_server(&pg, &slug, "none", "active", &["read_file", "delete_everything"]).await;
        grant_read(&pg, sid, uid).await;

        let grants = parse_grants(&[format!("{slug}__read_file")]);
        let before = call_denials(&pg, &slug, "delete_everything", "grant").await;
        let d = authorize_call(&state, &mcp_ctx(uid), &grants, Uuid::now_v7(), &slug, "delete_everything").await;
        assert!(matches!(d, CallDecision::Recoverable(_)), "cross-tool call must be refused recoverably");
        assert!(
            call_denials(&pg, &slug, "delete_everything", "grant").await > before,
            "the refusal must be audited denied=grant"
        );
        cleanup_server(&pg, sid).await;
    }

    /// Hole B (cross-server): a grant on server A confers nothing on server B, even
    /// though the user can RBAC-read B. The bigger blast radius the original framing
    /// of the bug would have missed.
    #[tokio::test]
    async fn hole_b_cross_server_call_refused() {
        let Some((pg, state)) = authz_state().await else {
            return;
        };
        let uid = Uuid::now_v7();
        let a = format!("hba{}", Uuid::now_v7().simple());
        let b = format!("hbb{}", Uuid::now_v7().simple());
        let sa = seed_server(&pg, &a, "none", "active", &["read_file"]).await;
        let sb = seed_server(&pg, &b, "none", "active", &["read_file"]).await;
        grant_read(&pg, sa, uid).await;
        grant_read(&pg, sb, uid).await; // user CAN read B; only the grant is missing

        // Agent is granted server A only.
        let grants = parse_grants(&[format!("{a}__*")]);
        let d = authorize_call(&state, &mcp_ctx(uid), &grants, Uuid::now_v7(), &b, "read_file").await;
        assert!(matches!(d, CallDecision::Recoverable(_)), "a never-granted server must be refused");
        assert!(
            call_denials(&pg, &b, "read_file", "grant").await > 0,
            "the cross-server refusal must be audited"
        );
        cleanup_server(&pg, sa).await;
        cleanup_server(&pg, sb).await;
    }

    /// Hole B (off-catalogue): a wildcard grant is still bounded by the pinned
    /// catalogue. A name the server never advertised is refused, audited denied=catalogue.
    #[tokio::test]
    async fn hole_b_off_catalogue_call_refused() {
        let Some((pg, state)) = authz_state().await else {
            return;
        };
        let uid = Uuid::now_v7();
        let slug = format!("hbo{}", Uuid::now_v7().simple());
        let sid = seed_server(&pg, &slug, "none", "active", &["read_file"]).await;
        grant_read(&pg, sid, uid).await;

        let grants = parse_grants(&[format!("{slug}__*")]); // wildcard
        let before = call_denials(&pg, &slug, "ghost_tool", "catalogue").await;
        let d = authorize_call(&state, &mcp_ctx(uid), &grants, Uuid::now_v7(), &slug, "ghost_tool").await;
        assert!(matches!(d, CallDecision::Recoverable(_)), "an off-catalogue tool must be refused");
        assert!(
            call_denials(&pg, &slug, "ghost_tool", "catalogue").await > before,
            "the refusal must be audited denied=catalogue"
        );
        cleanup_server(&pg, sid).await;
    }

    /// Hole C, the one that matters: a durable resume of an approved call against a
    /// server that has since been QUARANTINED is refused at the status gate and
    /// audited, rather than executing against rug-pulled state. On the pre-fix code
    /// the resume path did not pass through `authorize_call` at all (it resolved the
    /// connection directly with no status predicate and rebuilt the dropped
    /// connection), so the call executed; this cannot be demonstrated against the
    /// pre-fix binary from here, so the refusal + audit is asserted directly.
    #[tokio::test]
    async fn hole_c_quarantined_server_refuses_durable_resume() {
        let Some((pg, state)) = authz_state().await else {
            return;
        };
        // A real, loadable user (execute_pending rebuilds its context) + a throwaway agent.
        let uid: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM users WHERE deactivated_at IS NULL LIMIT 1")
                .fetch_optional(&pg)
                .await
                .unwrap();
        let Some(uid) = uid else {
            eprintln!("skip: no seeded user");
            return;
        };
        let slug = format!("hc{}", Uuid::now_v7().simple());
        let sid = seed_server(&pg, &slug, "oauth", "active", &["read"]).await;
        grant_read(&pg, sid, uid).await;

        let agent_id = Uuid::now_v7();
        sqlx::query("INSERT INTO agents (id,name,system_prompt,params,modes) VALUES ($1,'authz test','x','{}'::jsonb,'{}'::text[])")
            .bind(agent_id).execute(&pg).await.unwrap();
        sqlx::query("INSERT INTO agent_tools (agent_id, tool_name) VALUES ($1,$2)")
            .bind(agent_id).bind(format!("{slug}__*")).execute(&pg).await.unwrap();

        let run_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO agent_runs (id, agent_id, acting_user_id, chat_id, turn_id, status, pending_tool, pending_args) \
             VALUES ($1,$2,$3,$4,$5,'approved'::agent_run_status,$6,'{}'::jsonb)",
        )
        .bind(run_id).bind(agent_id).bind(uid).bind(Uuid::now_v7()).bind(Uuid::now_v7())
        .bind(format!("{slug}__read"))
        .execute(&pg).await.unwrap();

        // The rug-pull: quarantine the server AFTER the call was approved.
        quarantine(&state, sid, &slug, "test rug-pull").await;

        let before = call_denials(&pg, &slug, "read", "server").await;
        crate::agent::execute_pending(&state, run_id).await.unwrap();
        assert!(
            call_denials(&pg, &slug, "read", "server").await > before,
            "a quarantined server must refuse the durable resume at the status gate, audited denied=server"
        );

        let _ = sqlx::query("DELETE FROM agent_runs WHERE id=$1").bind(run_id).execute(&pg).await;
        let _ = sqlx::query("DELETE FROM agent_tools WHERE agent_id=$1").bind(agent_id).execute(&pg).await;
        let _ = sqlx::query("DELETE FROM agents WHERE id=$1").bind(agent_id).execute(&pg).await;
        cleanup_server(&pg, sid).await;
    }

    /// Hole D: the custom-tool durable resume enforces the agent grant. A pending
    /// custom call whose tool the agent does not hold is refused and audited, rather
    /// than executed on the grant-blind lookup the pre-fix resume used.
    #[tokio::test]
    async fn hole_d_custom_resume_refused_when_ungranted() {
        let Some((pg, state)) = authz_state().await else {
            return;
        };
        let uid: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM users WHERE deactivated_at IS NULL LIMIT 1")
                .fetch_optional(&pg)
                .await
                .unwrap();
        let Some(uid) = uid else {
            return;
        };

        let agent_id = Uuid::now_v7();
        sqlx::query("INSERT INTO agents (id,name,system_prompt,params,modes) VALUES ($1,'authz test','x','{}'::jsonb,'{}'::text[])")
            .bind(agent_id).execute(&pg).await.unwrap();
        // Note: NO agent_tools row — the agent does not hold this custom tool.

        let run_id = Uuid::now_v7();
        let tool = format!("ghost_custom_{}", Uuid::now_v7().simple());
        sqlx::query(
            "INSERT INTO agent_runs (id, agent_id, acting_user_id, chat_id, turn_id, status, pending_tool, pending_args) \
             VALUES ($1,$2,$3,$4,$5,'approved'::agent_run_status,$6,'{}'::jsonb)",
        )
        .bind(run_id).bind(agent_id).bind(uid).bind(Uuid::now_v7()).bind(Uuid::now_v7())
        .bind(&tool)
        .execute(&pg).await.unwrap();

        crate::agent::execute_pending(&state, run_id).await.unwrap();

        let denied: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action_type='tool.resume_denied' \
             AND resource_id=$1 AND payload->>'reason'='tool not granted to agent'",
        )
        .bind(run_id)
        .fetch_one(&pg)
        .await
        .unwrap();
        assert!(denied > 0, "an ungranted custom resume must be refused and audited");

        let _ = sqlx::query("DELETE FROM agent_runs WHERE id=$1").bind(run_id).execute(&pg).await;
        let _ = sqlx::query("DELETE FROM agents WHERE id=$1").bind(agent_id).execute(&pg).await;
    }

    /// The system seam: the health sweep authorises an active server and refuses a
    /// non-active one (it must not sweep a quarantined server).
    #[tokio::test]
    async fn system_seam_health_sweep_gates_on_active_status() {
        let Some((pg, state)) = authz_state().await else {
            return;
        };
        let slug = format!("hs{}", Uuid::now_v7().simple());
        let sid = seed_server(&pg, &slug, "none", "active", &["read"]).await;

        assert!(
            authorize_system_call(&state, &slug, SystemPurpose::HealthSweep).await.is_ok(),
            "health sweep must authorise an active server"
        );
        quarantine(&state, sid, &slug, "test").await;
        assert!(
            authorize_system_call(&state, &slug, SystemPurpose::HealthSweep).await.is_err(),
            "health sweep must refuse a quarantined server"
        );
        cleanup_server(&pg, sid).await;
    }
}
