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

//! Agent CRUD. The Agent is the named LLM configuration
//! (system prompt, params, tools, Project-Knowledge scope) a chat runs under.
//! Create is power-user/admin; everyone may list.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Owner-or-admin guard for managing (edit/delete/rollback) an agent. Personal-only:
/// a non-admin may manage only the agents they created; seeded/shared agents
/// (`created_by IS NULL`) and other people's are read-only. Admins manage anything.
async fn require_manage_agent(state: &AppState, ctx: &AuthContext, id: Uuid) -> Result<()> {
    if ctx.is_admin() {
        return Ok(());
    }
    let me = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let created_by = sqlx::query_scalar!(
        "SELECT created_by FROM agents WHERE id = $1 AND archived_at IS NULL",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("agent not found".into()))?;
    if created_by == Some(me) {
        Ok(())
    } else {
        Err(AppError::Forbidden("only the agent's owner or an admin may manage it".into()))
    }
}

/// Validate an Agent's tool whitelist against the closed set of grantable tools,
/// at config time. Every entry must resolve to one of: a native tool in the
/// platform's closed registry; an MCP grant (`slug__*` / `slug__tool`) whose
/// server and tool are in the server's pinned catalogue; or a custom tool that
/// exists. An unknown name is refused here rather than silently ignored at dispatch.
async fn validate_toolset(pg: &sqlx::PgPool, tools: &[String]) -> Result<()> {
    let mut catalogues: std::collections::HashMap<String, Option<Vec<crate::mcp::ToolCatalogEntry>>> =
        std::collections::HashMap::new();
    for t in tools {
        // A namespaced entry (`slug__*` / `slug__tool`) grants an MCP server's tools to the
        // agent (FEATURE B1). `slug__*` = the whole (pinned) catalogue; `slug__tool` = one
        // tool, which must be in that catalogue. Validate the grant here at write time
        // (unknown server, or a tool absent from the catalogue, is refused); a stored grant
        // is tolerated on read even if the tool later vanishes. Which servers are actually
        // in scope for a given turn is still decided by the per-turn authorisation seam
        // (active + enabled + RBAC + grant + connection).
        if crate::mcp::is_namespaced(t) {
            let Some((slug, tool)) = crate::mcp::split(t) else { continue };
            if !catalogues.contains_key(slug) {
                let cat = crate::mcp::server_catalogue(pg, slug).await?;
                catalogues.insert(slug.to_string(), cat);
            }
            let Some(catalogue) = catalogues.get(slug).and_then(|c| c.as_ref()) else {
                return Err(AppError::Validation(format!(
                    "unknown MCP server '{slug}' in tool grant '{t}'"
                )));
            };
            if tool != "*" && !catalogue.iter().any(|e| e.name == tool) {
                return Err(AppError::Validation(format!(
                    "tool '{tool}' is not in MCP server '{slug}' catalogue (grant '{t}')"
                )));
            }
            continue;
        }
        // Native tool in the closed registry.
        if crate::tools::ALL.contains(&t.as_str()) {
            continue;
        }
        // Otherwise a custom HTTP/script tool the agent selects by name: valid iff a
        // row exists. Its enabled + approved-version gate is re-applied at read time
        // by `load_enabled_custom`, so a stored grant to a later-disabled tool
        // degrades quietly rather than breaking the Agent save.
        if crate::tools::custom::exists_by_name(pg, t).await? {
            continue;
        }
        return Err(AppError::Validation(format!(
            "unknown tool '{t}' — not in the closed registry"
        )));
    }
    Ok(())
}

/// The workmodes an agent can be made available in.
const VALID_MODES: [&str; 3] = ["general", "legal", "research"];

/// An agent's `modes` must be a non-empty subset of the known workmodes; each mode
/// gates whether the agent appears in that workmode's picker.
fn validate_modes(modes: &[String]) -> Result<()> {
    if modes.is_empty() {
        return Err(AppError::Validation(
            "an agent must be available in at least one workmode".into(),
        ));
    }
    for m in modes {
        if !VALID_MODES.contains(&m.as_str()) {
            return Err(AppError::Validation(format!(
                "mode must be one of general/legal/research (got '{m}')"
            )));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct CreateAgent {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub system_prompt: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub project_knowledge_ids: Vec<Uuid>,
    /// Optional sector tag (`general`|`legal`); legacy, unused for filtering.
    #[serde(default)]
    pub sector: Option<String>,
    /// Workmodes this agent is available in (non-empty subset of
    /// general/legal/research). Drives picker filtering.
    #[serde(default)]
    pub modes: Vec<String>,
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

pub async fn create_agent(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateAgent>,
) -> Result<Json<CreatedId>> {
    // Personal-only: any authenticated user may create an agent — it becomes THEIRS
    // (created_by), and only the owner (or an admin) can later edit/delete it.
    let me = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    validate_toolset(&state.pg, &body.tools).await?;
    validate_modes(&body.modes)?;
    let id = db::new_id();
    let params = body.params.unwrap_or_else(|| serde_json::json!({}));

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO agents (id, name, description, system_prompt, params, created_by, sector, modes) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        id,
        body.name,
        body.description,
        body.system_prompt,
        params,
        me,
        body.sector,
        &body.modes,
    )
    .execute(&mut *tx)
    .await?;

    for tool in &body.tools {
        sqlx::query!(
            "INSERT INTO agent_tools (agent_id, tool_name) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            id,
            tool
        )
        .execute(&mut *tx)
        .await?;
    }
    for pk in &body.project_knowledge_ids {
        sqlx::query!(
            "INSERT INTO agent_project_knowledge (agent_id, project_knowledge_id) \
             VALUES ($1, $2) ON CONFLICT DO NOTHING",
            id,
            pk
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    // Record the initial version (best-effort — the Agent already exists).
    if let Err(e) = snapshot_version(&state.pg, id, "created", ctx.user_id).await {
        tracing::warn!(error = %e, %id, "agent v1 snapshot failed");
    }
    Ok(Json(CreatedId { id }))
}

/// Snapshot the Agent's current configuration into `agent_versions` as the next
/// version. Reads the live row + tool-set + Project-Knowledge scope (post-commit)
/// so the snapshot reflects exactly what was saved.
async fn snapshot_version(
    pool: &sqlx::PgPool,
    agent_id: Uuid,
    source: &str,
    created_by: Option<Uuid>,
) -> Result<i32> {
    let core = sqlx::query!(
        "SELECT name, description, system_prompt, params FROM agents WHERE id = $1",
        agent_id
    )
    .fetch_one(pool)
    .await?;
    let tools: Vec<String> =
        sqlx::query_scalar!("SELECT tool_name FROM agent_tools WHERE agent_id = $1", agent_id)
            .fetch_all(pool)
            .await?;
    let pks: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT project_knowledge_id FROM agent_project_knowledge WHERE agent_id = $1",
        agent_id
    )
    .fetch_all(pool)
    .await?;
    let next: i32 = sqlx::query_scalar!(
        "SELECT COALESCE(MAX(version_number), 0) + 1 FROM agent_versions WHERE agent_id = $1",
        agent_id
    )
    .fetch_one(pool)
    .await?
    .unwrap_or(1);
    let pk_strs: Vec<String> = pks.iter().map(|u| u.to_string()).collect();
    sqlx::query!(
        "INSERT INTO agent_versions \
           (id, agent_id, version_number, source, name, description, system_prompt, params, tools, project_knowledge_ids, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        db::new_id(),
        agent_id,
        next,
        source,
        core.name,
        core.description,
        core.system_prompt,
        core.params,
        serde_json::json!(tools),
        serde_json::json!(pk_strs),
        created_by,
    )
    .execute(pool)
    .await?;
    Ok(next)
}

#[derive(Serialize)]
pub struct AgentSummary {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub tools: Vec<String>,
    pub sector: Option<String>,
    /// Workmodes this agent appears in (general/legal/research).
    pub modes: Vec<String>,
    /// May the caller edit/delete this agent? Owner (created_by) or admin only;
    /// seeded/shared agents are read-only for non-admins.
    pub can_manage: bool,
}

pub async fn list_agents(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<AgentSummary>>> {
    let is_admin = ctx.is_admin();
    let me = ctx.user_id;
    // Visibility (personal-only): a non-admin sees the shared/seeded pool
    // (created_by IS NULL) + their own; others' personal agents are hidden. Admin: all.
    // Tools fetched in the same query via a LATERAL aggregate — no per-agent
    // round-trip (avoids the N+1 re-audit).
    let rows = sqlx::query!(
        r#"SELECT a.id, a.name, a.description, a.sector, a.modes, a.created_by,
                  COALESCE(t.tools, ARRAY[]::text[]) AS "tools!"
           FROM agents a
           LEFT JOIN LATERAL (
               SELECT array_agg(tool_name ORDER BY tool_name) AS tools
               FROM agent_tools WHERE agent_id = a.id
           ) t ON true
           WHERE a.archived_at IS NULL AND ($1 OR a.created_by IS NULL OR a.created_by = $2)
           ORDER BY a.created_at DESC"#,
        is_admin,
        me,
    )
    .fetch_all(&state.pg)
    .await?;

    let out = rows
        .into_iter()
        .map(|r| AgentSummary {
            id: r.id,
            name: r.name,
            description: r.description,
            tools: r.tools,
            sector: r.sector,
            modes: r.modes,
            can_manage: is_admin || (r.created_by.is_some() && r.created_by == me),
        })
        .collect();
    Ok(Json(out))
}

// --- Detail / update / delete ------------------------------------------------

#[derive(Serialize)]
pub struct SkillRef {
    pub id: Uuid,
    pub name: String,
}

#[derive(Serialize)]
pub struct AgentDetail {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub system_prompt: String,
    pub params: serde_json::Value,
    pub tools: Vec<String>,
    pub skills: Vec<SkillRef>,
    pub project_knowledge_ids: Vec<Uuid>,
    pub sector: Option<String>,
    /// Workmodes this agent appears in (general/legal/research).
    pub modes: Vec<String>,
    /// May the caller edit/delete this agent? (owner or admin; shared/seeded = false)
    pub can_manage: bool,
}

/// Full agent config — system prompt, params, tools, attached Skills. Any
/// authenticated user may read it (needed to run / inspect an Agent).
pub async fn get_agent(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AgentDetail>> {
    let row = sqlx::query!(
        "SELECT name, description, system_prompt, params, sector, modes, created_by FROM agents WHERE id = $1 AND archived_at IS NULL",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("agent not found".into()))?;

    let tools: Vec<String> =
        sqlx::query_scalar!("SELECT tool_name FROM agent_tools WHERE agent_id = $1", id)
            .fetch_all(&state.pg)
            .await?;
    let skills = sqlx::query!(
        "SELECT s.id, s.name FROM agent_skills a JOIN skills s ON s.id = a.skill_id \
         WHERE a.agent_id = $1 ORDER BY s.name",
        id
    )
    .fetch_all(&state.pg)
    .await?
    .into_iter()
    .map(|r| SkillRef { id: r.id, name: r.name })
    .collect();
    let project_knowledge_ids: Vec<Uuid> = sqlx::query_scalar!(
        "SELECT project_knowledge_id FROM agent_project_knowledge WHERE agent_id = $1",
        id
    )
    .fetch_all(&state.pg)
    .await?;

    let can_manage = ctx.is_admin() || (row.created_by.is_some() && row.created_by == ctx.user_id);
    Ok(Json(AgentDetail {
        id,
        name: row.name,
        description: row.description,
        system_prompt: row.system_prompt,
        params: row.params,
        tools,
        skills,
        project_knowledge_ids,
        sector: row.sector,
        modes: row.modes,
        can_manage,
    }))
}

#[derive(Deserialize)]
pub struct UpdateAgent {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    /// When present, replaces the agent's whole tool set.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// When present, replaces the agent's attached Project-Knowledge bases.
    #[serde(default)]
    pub project_knowledge_ids: Option<Vec<Uuid>>,
    /// When present, updates the sector tag (`general`|`legal`); legacy.
    #[serde(default)]
    pub sector: Option<String>,
    /// When present, replaces the agent's workmode availability set.
    #[serde(default)]
    pub modes: Option<Vec<String>>,
}

pub async fn update_agent(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateAgent>,
) -> Result<Json<serde_json::Value>> {
    require_manage_agent(&state, &ctx, id).await?;
    if let Some(tools) = &body.tools {
        validate_toolset(&state.pg, tools).await?;
    }
    if let Some(modes) = &body.modes {
        validate_modes(modes)?;
    }

    let mut tx = state.pg.begin().await?;
    let n = sqlx::query!(
        "UPDATE agents SET \
           name = COALESCE($2, name), \
           description = COALESCE($3, description), \
           system_prompt = COALESCE($4, system_prompt), \
           params = COALESCE($5, params), \
           sector = COALESCE($6, sector), \
           modes = COALESCE($7, modes) \
         WHERE id = $1 AND archived_at IS NULL",
        id,
        body.name,
        body.description,
        body.system_prompt,
        body.params,
        body.sector,
        body.modes.as_deref(),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::Validation("agent not found".into()));
    }

    if let Some(tools) = &body.tools {
        sqlx::query!("DELETE FROM agent_tools WHERE agent_id = $1", id)
            .execute(&mut *tx)
            .await?;
        for tool in tools {
            sqlx::query!(
                "INSERT INTO agent_tools (agent_id, tool_name) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                id,
                tool
            )
            .execute(&mut *tx)
            .await?;
        }
    }

    if let Some(pks) = &body.project_knowledge_ids {
        sqlx::query!("DELETE FROM agent_project_knowledge WHERE agent_id = $1", id)
            .execute(&mut *tx)
            .await?;
        for pk in pks {
            sqlx::query!(
                "INSERT INTO agent_project_knowledge (agent_id, project_knowledge_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                id,
                pk
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;

    audit_agent(&state, &ctx, "agent.updated", id).await;
    if let Err(e) = snapshot_version(&state.pg, id, "updated", ctx.user_id).await {
        tracing::warn!(error = %e, %id, "agent version snapshot failed");
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Soft delete — archive the Agent. Chats keep their `agent_id`; the row stays
/// but drops out of the list (which filters `archived_at IS NULL`).
pub async fn delete_agent(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_manage_agent(&state, &ctx, id).await?;
    let n = sqlx::query!(
        "UPDATE agents SET archived_at = now() WHERE id = $1 AND archived_at IS NULL",
        id
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::Validation("agent not found".into()));
    }
    audit_agent(&state, &ctx, "agent.deleted", id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn audit_agent(state: &AppState, ctx: &AuthContext, action: &str, agent_id: Uuid) {
    let mut event = AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("agent".into());
    event.resource_id = Some(agent_id);
    let _ = audit::append(&state.pg, &event).await;
}

// --- Version history ---------------------------------------------------------

#[derive(Serialize)]
pub struct AgentVersionSummary {
    pub version_number: i32,
    pub source: String,
    pub created_at: String,
    pub created_by: Option<Uuid>,
}

/// List an Agent's version history (newest first). Any authenticated user may
/// read it (needed to audit which configuration produced an answer).
pub async fn list_agent_versions(
    State(state): State<AppState>,
    AuthUser(_ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AgentVersionSummary>>> {
    let rows = sqlx::query!(
        "SELECT version_number, source, created_at, created_by \
         FROM agent_versions WHERE agent_id = $1 ORDER BY version_number DESC",
        id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| AgentVersionSummary {
                version_number: r.version_number,
                source: r.source,
                created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
                created_by: r.created_by,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct AgentVersionDetail {
    pub version_number: i32,
    pub source: String,
    pub name: String,
    pub description: Option<String>,
    pub system_prompt: String,
    pub params: serde_json::Value,
    pub tools: serde_json::Value,
    pub project_knowledge_ids: serde_json::Value,
    pub created_at: String,
}

/// Full snapshot of one Agent version (for viewing / diffing before a rollback).
pub async fn get_agent_version(
    State(state): State<AppState>,
    AuthUser(_ctx): AuthUser,
    Path((id, vnum)): Path<(Uuid, i32)>,
) -> Result<Json<AgentVersionDetail>> {
    let r = sqlx::query!(
        "SELECT version_number, source, name, description, system_prompt, params, tools, \
                project_knowledge_ids, created_at \
         FROM agent_versions WHERE agent_id = $1 AND version_number = $2",
        id, vnum
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("agent version not found".into()))?;
    Ok(Json(AgentVersionDetail {
        version_number: r.version_number,
        source: r.source,
        name: r.name,
        description: r.description,
        system_prompt: r.system_prompt,
        params: r.params,
        tools: r.tools,
        project_knowledge_ids: r.project_knowledge_ids,
        created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
    }))
}

/// Restore an Agent to a prior version: apply that snapshot to the live Agent
/// (core fields + tool-set + Project-Knowledge scope) and record the result as a
/// new `rollback` version. Power-user/admin only; audited.
pub async fn rollback_agent_version(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((id, vnum)): Path<(Uuid, i32)>,
) -> Result<Json<serde_json::Value>> {
    require_manage_agent(&state, &ctx, id).await?;
    let v = sqlx::query!(
        "SELECT name, description, system_prompt, params, tools, project_knowledge_ids \
         FROM agent_versions WHERE agent_id = $1 AND version_number = $2",
        id, vnum
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("agent version not found".into()))?;

    let tools: Vec<String> = serde_json::from_value(v.tools).unwrap_or_default();
    // Restore is a write: re-validate the snapshot's tool-set so a rollback cannot
    // reintroduce a grant naming a now-unknown server or an off-catalogue tool.
    validate_toolset(&state.pg, &tools).await?;
    let pk_strs: Vec<String> = serde_json::from_value(v.project_knowledge_ids).unwrap_or_default();
    let pks: Vec<Uuid> = pk_strs.iter().filter_map(|s| Uuid::parse_str(s).ok()).collect();

    let mut tx = state.pg.begin().await?;
    let n = sqlx::query!(
        "UPDATE agents SET name = $2, description = $3, system_prompt = $4, params = $5 \
         WHERE id = $1 AND archived_at IS NULL",
        id, v.name, v.description, v.system_prompt, v.params,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::Validation("agent not found".into()));
    }
    sqlx::query!("DELETE FROM agent_tools WHERE agent_id = $1", id).execute(&mut *tx).await?;
    for t in &tools {
        sqlx::query!(
            "INSERT INTO agent_tools (agent_id, tool_name) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            id, t
        )
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query!("DELETE FROM agent_project_knowledge WHERE agent_id = $1", id)
        .execute(&mut *tx)
        .await?;
    for pk in &pks {
        sqlx::query!(
            "INSERT INTO agent_project_knowledge (agent_id, project_knowledge_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            id, pk
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let new_version = snapshot_version(&state.pg, id, "rollback", ctx.user_id).await?;
    let mut ev = AuditEvent::action("agent.rolled_back", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("agent".into());
    ev.resource_id = Some(id);
    ev.payload = Some(serde_json::json!({ "restored_from": vnum, "new_version": new_version }));
    let _ = audit::append(&state.pg, &ev).await;
    Ok(Json(serde_json::json!({ "ok": true, "version": new_version })))
}
