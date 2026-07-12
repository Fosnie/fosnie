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
//! (private-endpoint check).

pub mod client;
pub mod manager;
pub mod pin;
pub mod validate;

pub use manager::McpManager;

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

/// The set of server slugs an agent is allowed to use, derived from its `tools` list:
/// any entry that is namespaced (`slug__*` for a whole server, or `slug__tool`) grants
/// that server. Server-level grain — the tool part is not enforced here. An agent with
/// no namespaced entries gets NO MCP tools (so e.g. General Assistant sees none unless
/// a server is explicitly assigned to it).
pub fn allowed_slugs(agent_tools: &[String]) -> std::collections::HashSet<String> {
    agent_tools
        .iter()
        .filter_map(|t| split(t).map(|(slug, _)| slug.to_string()))
        .collect()
}

/// The namespaced OpenAI tool defs the caller may use this turn: from servers that
/// are enabled + active + not quarantined + RBAC-readable by the caller AND assigned to
/// the active agent (`allowed`). Empty when the feature is off, the connector is dormant
/// (zero-egress default), or the agent has no MCP servers assigned.
pub async fn session_tool_defs(
    state: &AppState,
    ctx: &AuthContext,
    allowed: &std::collections::HashSet<String>,
) -> Vec<Value> {
    if !state.boot.features.mcp || allowed.is_empty() {
        return Vec::new();
    }
    if !integrations::is_enabled(&state.pg, ConnectorKind::Mcp).await.unwrap_or(false) {
        return Vec::new();
    }
    let rows = match sqlx::query!(
        r#"SELECT id, slug, tools_catalog FROM mcp_servers WHERE status = 'active' AND enabled"#
    )
    .fetch_all(&state.pg)
    .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut defs = Vec::new();
    for r in rows {
        if !allowed.contains(&r.slug) {
            continue; // not assigned to this agent
        }
        if !state.rbac.can(&state.pg, ctx, ResourceType::McpServer, r.id, Permission::Read)
            .await
            .unwrap_or(false)
        {
            continue;
        }
        for t in parse_catalog(r.tools_catalog) {
            let schema = if t.schema.is_null() {
                json!({ "type": "object", "properties": {} })
            } else {
                t.schema
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

/// Is at least one MCP server in scope for this caller (so the turn must run as a
/// gated agent run)? Cheap pre-check mirroring `session_tool_defs`' gating.
pub async fn any_in_scope(
    state: &AppState,
    ctx: &AuthContext,
    allowed: &std::collections::HashSet<String>,
) -> bool {
    !session_tool_defs(state, ctx, allowed).await.is_empty()
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

/// Dispatch a namespaced MCP tool call: egress gate → resolve server → RBAC →
/// call via the manager → normalise → audit. Errors come back as `"error: …"` so
/// the model can recover, exactly like native tools.
pub async fn dispatch(
    state: &AppState,
    ctx: &AuthContext,
    chat_id: Uuid,
    name: &str,
    args: &Value,
) -> Result<String> {
    let (slug, tool) =
        split(name).ok_or_else(|| AppError::Validation("not a namespaced MCP tool".into()))?;
    // Zero-egress choke-point (dormant ⇒ refuse + audit `integration.blocked`).
    integrations::guard_egress(state, ctx, ConnectorKind::Mcp).await?;

    let server = sqlx::query!("SELECT id, status FROM mcp_servers WHERE slug = $1", slug)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation(format!("unknown MCP server '{slug}'")))?;
    if server.status != "active" {
        return Ok(format!("error: MCP server '{slug}' is {} (unavailable)", server.status));
    }
    state.rbac.require(&state.pg, ctx, ResourceType::McpServer, server.id, Permission::Read).await?;

    let started = OffsetDateTime::now_utc();
    let result = state.mcp.call_tool(slug, tool, args.clone()).await;
    let ms = (OffsetDateTime::now_utc() - started).whole_milliseconds();
    let (outcome, body) = match &result {
        Ok(s) => (AuditOutcome::Success, s.clone()),
        Err(e) => (AuditOutcome::Failure, format!("error: {e}")),
    };
    audit_call(state, ctx, chat_id, server.id, slug, tool, args, &body, ms, outcome).await;
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
    ev.payload = Some(json!({
        "chat_id": chat_id, "server": slug, "tool": tool,
        "args_hash": args_hash, "result_hash": result_hash,
        "result_bytes": result.len(), "latency_ms": ms,
    }));
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
    state.mcp.disconnect(slug).await;
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
        r#"SELECT id, slug, pinned_tools FROM mcp_servers WHERE status = 'active' AND enabled"#
    )
    .fetch_all(&state.pg)
    .await?;
    let mut checked = 0u64;
    for r in rows {
        if !state.mcp.is_connected(&r.slug).await {
            continue; // admin re-approval reconnects
        }
        match state.mcp.list_tools(&r.slug).await {
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
                state.mcp.disconnect(&r.slug).await;
            }
        }
        checked += 1;
    }
    Ok(checked)
}
