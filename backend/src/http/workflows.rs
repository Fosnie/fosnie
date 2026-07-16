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

//! Workflows REST: power-user CRUD for
//! event-driven workflows (WHEN trigger [IF condition] THEN action) + run history.
//! Dispatch/execution live on the engine ([`crate::workflows`]) + the background
//! scheduler; this is the management surface. Workflows are **owned and scoped**:
//! only a power-user (or admin) authors them, created **disabled** (explicit enable).

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::{AuthContext, PlatformRole};
use crate::db;
use crate::error::{AppError, Result};
use crate::events;
use crate::state::AppState;

fn rfc3339(t: Option<OffsetDateTime>) -> Option<String> {
    t.and_then(|t| t.format(&Rfc3339).ok())
}

/// A subscribable trigger. `emitted` marks the events the platform actually
/// emits today; the rest are reserved names — a workflow may subscribe but stays
/// inert until they are wired. This slice is the **single source of truth** for the
/// backend validator and the `GET /api/workflows/triggers` UI dropdown (D4).
#[derive(Serialize)]
pub struct TriggerDef {
    pub name: &'static str,
    pub description: &'static str,
    pub emitted: bool,
}

const TRIGGER_CATALOGUE: &[TriggerDef] = &[
    // Documents.
    TriggerDef { name: events::DOCUMENT_INGESTED, description: "A document finished ingesting into a knowledge base (ready for RAG). Also fires for KB-destined connector imports.", emitted: true },
    TriggerDef { name: events::DOCUMENT_IMPORTED, description: "A document arrived from an external connector (mail/DMS import).", emitted: true },
    TriggerDef { name: events::DOCUMENT_DELETED, description: "A workspace document was deleted.", emitted: true },
    TriggerDef { name: "document.ingest_failed", description: "A document failed to ingest.", emitted: false },
    TriggerDef { name: "document.version_created", description: "A new version of a tracked document was created.", emitted: false },
    // Knowledge bases.
    TriggerDef { name: "kb.attached_to_project", description: "A knowledge base was attached to a project.", emitted: false },
    TriggerDef { name: "kb.detached", description: "A knowledge base was detached from a project.", emitted: false },
    TriggerDef { name: "kb.grant_added", description: "A knowledge-base access grant was added.", emitted: false },
    TriggerDef { name: "kb.grant_revoked", description: "A knowledge-base access grant was revoked.", emitted: false },
    // Membership & directory.
    TriggerDef { name: events::PROJECT_MEMBER_ADDED, description: "A user was added to a group (legacy name; see group.member_added).", emitted: true },
    TriggerDef { name: events::GROUP_MEMBER_ADDED, description: "A user was added to a group (admin action, SCIM or IdP sync).", emitted: true },
    TriggerDef { name: events::GROUP_MEMBER_REMOVED, description: "A user was removed from a group.", emitted: true },
    TriggerDef { name: events::CHAT_MEMBER_ADDED, description: "A user was added to a group chat.", emitted: true },
    TriggerDef { name: events::DIRECTORY_USER_PROVISIONED, description: "A directory user was provisioned (SCIM or manual create).", emitted: true },
    TriggerDef { name: events::DIRECTORY_USER_DEACTIVATED, description: "A directory user was deactivated.", emitted: true },
    TriggerDef { name: events::ACCOUNT_ARCHIVED, description: "A user archived their own account.", emitted: true },
    TriggerDef { name: "project.member_removed", description: "A user was removed from a project.", emitted: false },
    TriggerDef { name: "user.role_changed", description: "A user's platform role changed.", emitted: false },
    TriggerDef { name: "chat.created", description: "A group chat was created.", emitted: false },
    // Agent runs & review.
    TriggerDef { name: "agent_run.completed", description: "An agent run completed.", emitted: false },
    TriggerDef { name: "agent_run.awaiting_approval", description: "An agent run is awaiting human approval.", emitted: false },
    TriggerDef { name: "feedback.submitted", description: "Message feedback was submitted.", emitted: false },
    TriggerDef { name: "tabular_review.completed", description: "A tabular review completed.", emitted: false },
    // Compliance.
    TriggerDef { name: "legal_hold.set", description: "A legal hold was set.", emitted: false },
    TriggerDef { name: "legal_hold.cleared", description: "A legal hold was cleared.", emitted: false },
    TriggerDef { name: "export.produced", description: "An export was produced.", emitted: false },
    // Advanced: react to workflow-caused events (requires trigger_on_system_events).
    TriggerDef { name: events::WORKFLOW_MESSAGE_POSTED, description: "A workflow posted a message (advanced — requires trigger_on_system_events).", emitted: true },
];

const MAX_COALESCE_SECS: i32 = 86_400;
const MAX_RUNS_PER_WINDOW: i32 = 10_000;

/// Only a power-user (or admin) may author workflows.
fn require_lead(ctx: &AuthContext) -> Result<()> {
    if matches!(
        ctx.role,
        PlatformRole::PowerUser | PlatformRole::ClientAdmin | PlatformRole::SuperAdmin
    ) {
        Ok(())
    } else {
        Err(AppError::Forbidden("requires power user or admin".into()))
    }
}

/// The authoring gate (D3): a power-user/admin (`require_lead`) **or** any caller
/// holding the `workflows.manage` permission. Additive — Core's default policy
/// treats the permission as `is_admin`, so power-users keep access unchanged and
/// an Enterprise delegated-admin role can be granted authoring without the fixed
/// lead roles.
async fn require_manage(state: &AppState, ctx: &AuthContext) -> Result<()> {
    if require_lead(ctx).is_ok() {
        return Ok(());
    }
    state
        .rbac
        .require_permission(&state.pg, ctx, crate::auth::permissions::WORKFLOWS_MANAGE)
        .await
}

/// Owner or admin may manage a workflow; returns the owner.
async fn require_owner(state: &AppState, ctx: &AuthContext, id: Uuid) -> Result<Uuid> {
    let owner: Option<Uuid> =
        sqlx::query_scalar!("SELECT owner_id FROM workflows WHERE id = $1", id)
            .fetch_optional(&state.pg)
            .await?;
    let owner = owner.ok_or_else(|| AppError::Validation("workflow not found".into()))?;
    if ctx.user_id == Some(owner) || ctx.is_admin() {
        Ok(owner)
    } else {
        Err(AppError::Forbidden("not your workflow".into()))
    }
}

/// Shared field validation for create/update.
fn validate_trigger(t: &str) -> Result<()> {
    if !TRIGGER_CATALOGUE.iter().any(|d| d.name == t) {
        return Err(AppError::Validation(format!("unknown trigger_event_type {t:?}")));
    }
    Ok(())
}

fn validate_action(action_type: &str, cfg: &Value) -> Result<()> {
    match action_type {
        "system_action" => {
            if cfg.get("kind").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                return Err(AppError::Validation("system_action needs action_config.kind".into()));
            }
        }
        "agent_run" => {
            if cfg.get("prompt").and_then(|v| v.as_str()).unwrap_or("").trim().is_empty() {
                return Err(AppError::Validation("agent_run needs action_config.prompt".into()));
            }
        }
        other => return Err(AppError::Validation(format!("action_type must be agent_run|system_action, got {other:?}"))),
    }
    Ok(())
}

/// A condition must be null or a JSON object (a safe declarative filter).
fn validate_condition(c: &Option<Value>) -> Result<()> {
    if let Some(v) = c {
        if !v.is_null() && !v.is_object() {
            return Err(AppError::Validation("condition must be a JSON object or null".into()));
        }
    }
    Ok(())
}

fn validate_bounds(coalesce: i32, max_runs: i32) -> Result<()> {
    if !(0..=MAX_COALESCE_SECS).contains(&coalesce) {
        return Err(AppError::Validation(format!("coalesce_window_secs must be 0..={MAX_COALESCE_SECS}")));
    }
    if !(0..=MAX_RUNS_PER_WINDOW).contains(&max_runs) {
        return Err(AppError::Validation(format!("max_runs_per_window must be 0..={MAX_RUNS_PER_WINDOW}")));
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct CreateWorkflow {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    pub trigger_event_type: String,
    #[serde(default)]
    pub trigger_scope: Option<Value>,
    #[serde(default)]
    pub trigger_on_system_events: bool,
    #[serde(default)]
    pub condition: Option<Value>,
    #[serde(default)]
    pub coalesce_window_secs: i32,
    pub action_type: String,
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    #[serde(default)]
    pub action_config: Option<Value>,
    #[serde(default = "default_rate")]
    pub max_runs_per_window: i32,
}

fn default_rate() -> i32 {
    60
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

pub async fn create_workflow(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateWorkflow>,
) -> Result<Json<CreatedId>> {
    require_manage(&state, &ctx).await?;
    let owner = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    if body.name.trim().is_empty() {
        return Err(AppError::Validation("name must not be empty".into()));
    }
    validate_trigger(&body.trigger_event_type)?;
    let action_config = body.action_config.clone().unwrap_or_else(|| serde_json::json!({}));
    validate_action(&body.action_type, &action_config)?;
    validate_condition(&body.condition)?;
    validate_bounds(body.coalesce_window_secs, body.max_runs_per_window)?;
    let trigger_scope = body.trigger_scope.clone().unwrap_or_else(|| serde_json::json!({}));

    let id = db::new_id();
    sqlx::query!(
        "INSERT INTO workflows \
            (id, name, description, owner_id, project_id, trigger_event_type, trigger_scope, \
             trigger_on_system_events, condition, coalesce_window_secs, action_type, agent_id, \
             action_config, max_runs_per_window) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        id, body.name, body.description, owner, body.project_id, body.trigger_event_type,
        trigger_scope, body.trigger_on_system_events, body.condition, body.coalesce_window_secs,
        body.action_type, body.agent_id, action_config, body.max_runs_per_window,
    )
    .execute(&state.pg)
    .await?;
    audit_workflow(&state, &ctx, "workflow.created", id).await;
    Ok(Json(CreatedId { id }))
}

#[derive(Serialize)]
pub struct WorkflowOut {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: Uuid,
    pub owner_name: String,
    pub project_id: Option<Uuid>,
    pub enabled: bool,
    pub trigger_event_type: String,
    pub trigger_scope: Value,
    pub trigger_on_system_events: bool,
    pub condition: Option<Value>,
    pub coalesce_window_secs: i32,
    pub action_type: String,
    pub agent_id: Option<Uuid>,
    pub action_config: Value,
    pub max_runs_per_window: i32,
    pub version: i32,
}

pub async fn list_workflows(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<WorkflowOut>>> {
    require_manage(&state, &ctx).await?;
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    // Owner sees theirs; an admin sees all.
    let admin = ctx.is_admin();
    let rows = sqlx::query!(
        r#"SELECT w.id, w.name, w.description, w.owner_id, u.display_name AS owner_name,
                  w.project_id, w.enabled, w.trigger_event_type, w.trigger_scope,
                  w.trigger_on_system_events, w.condition, w.coalesce_window_secs, w.action_type,
                  w.agent_id, w.action_config, w.max_runs_per_window, w.version
           FROM workflows w
           JOIN users u ON u.id = w.owner_id
           WHERE ($1 OR w.owner_id = $2)
           ORDER BY w.created_at DESC"#,
        admin,
        uid,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(rows.into_iter().map(|r| WorkflowOut {
        id: r.id, name: r.name, description: r.description, owner_id: r.owner_id, owner_name: r.owner_name,
        project_id: r.project_id,
        enabled: r.enabled, trigger_event_type: r.trigger_event_type, trigger_scope: r.trigger_scope,
        trigger_on_system_events: r.trigger_on_system_events, condition: r.condition,
        coalesce_window_secs: r.coalesce_window_secs, action_type: r.action_type, agent_id: r.agent_id,
        action_config: r.action_config, max_runs_per_window: r.max_runs_per_window, version: r.version,
    }).collect()))
}

pub async fn get_workflow(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkflowOut>> {
    require_owner(&state, &ctx, id).await?;
    let r = sqlx::query!(
        r#"SELECT w.id, w.name, w.description, w.owner_id, u.display_name AS owner_name,
                  w.project_id, w.enabled, w.trigger_event_type, w.trigger_scope,
                  w.trigger_on_system_events, w.condition, w.coalesce_window_secs, w.action_type,
                  w.agent_id, w.action_config, w.max_runs_per_window, w.version
           FROM workflows w
           JOIN users u ON u.id = w.owner_id
           WHERE w.id = $1"#,
        id
    )
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(WorkflowOut {
        id: r.id, name: r.name, description: r.description, owner_id: r.owner_id, owner_name: r.owner_name,
        project_id: r.project_id,
        enabled: r.enabled, trigger_event_type: r.trigger_event_type, trigger_scope: r.trigger_scope,
        trigger_on_system_events: r.trigger_on_system_events, condition: r.condition,
        coalesce_window_secs: r.coalesce_window_secs, action_type: r.action_type, agent_id: r.agent_id,
        action_config: r.action_config, max_runs_per_window: r.max_runs_per_window, version: r.version,
    }))
}

#[derive(Deserialize)]
pub struct UpdateWorkflow {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// The explicit enable/disable toggle.
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub trigger_on_system_events: Option<bool>,
    #[serde(default)]
    pub condition: Option<Value>,
    #[serde(default)]
    pub action_config: Option<Value>,
    #[serde(default)]
    pub coalesce_window_secs: Option<i32>,
    #[serde(default)]
    pub max_runs_per_window: Option<i32>,
}

pub async fn update_workflow(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateWorkflow>,
) -> Result<Json<serde_json::Value>> {
    require_owner(&state, &ctx, id).await?;
    validate_condition(&body.condition)?;
    if let Some(c) = body.coalesce_window_secs {
        validate_bounds(c, body.max_runs_per_window.unwrap_or(60))?;
    } else if let Some(m) = body.max_runs_per_window {
        validate_bounds(0, m)?;
    }
    // COALESCE: absent field = unchanged. `version` bumps on every edit.
    sqlx::query!(
        "UPDATE workflows SET \
            name = COALESCE($2, name), \
            description = COALESCE($3, description), \
            enabled = COALESCE($4, enabled), \
            trigger_on_system_events = COALESCE($5, trigger_on_system_events), \
            condition = COALESCE($6, condition), \
            action_config = COALESCE($7, action_config), \
            coalesce_window_secs = COALESCE($8, coalesce_window_secs), \
            max_runs_per_window = COALESCE($9, max_runs_per_window), \
            version = version + 1, \
            updated_at = now() \
         WHERE id = $1",
        id, body.name, body.description, body.enabled, body.trigger_on_system_events,
        body.condition, body.action_config, body.coalesce_window_secs, body.max_runs_per_window,
    )
    .execute(&state.pg)
    .await?;
    audit_workflow(&state, &ctx, "workflow.updated", id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn delete_workflow(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_owner(&state, &ctx, id).await?;
    sqlx::query!("DELETE FROM workflows WHERE id = $1", id).execute(&state.pg).await?;
    audit_workflow(&state, &ctx, "workflow.deleted", id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize)]
pub struct RunOut {
    pub id: Uuid,
    pub status: String,
    pub depth: i32,
    pub event_count: i32,
    pub outcome: Option<Value>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub created_at: Option<String>,
}

/// Per-workflow run history: the observability + dead-letter surface.
pub async fn list_runs(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<RunOut>>> {
    require_owner(&state, &ctx, id).await?;
    let rows = sqlx::query!(
        r#"SELECT id, status::text AS "status!", depth,
                  COALESCE(array_length(trigger_event_ids, 1), 0) AS "event_count!",
                  outcome, error, started_at, finished_at, created_at
           FROM workflow_runs WHERE workflow_id = $1 ORDER BY created_at DESC LIMIT 100"#,
        id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(rows.into_iter().map(|r| RunOut {
        id: r.id, status: r.status, depth: r.depth, event_count: r.event_count,
        outcome: r.outcome, error: r.error,
        started_at: rfc3339(r.started_at), finished_at: rfc3339(r.finished_at),
        created_at: rfc3339(Some(r.created_at)),
    }).collect()))
}

/// The trigger catalogue (D4) — the single source for the create-form dropdown.
/// Gated like authoring so a caller who can't create workflows can't enumerate the
/// catalogue either. `emitted` lets the UI flag reserved-but-inert triggers.
pub async fn list_triggers(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<&'static [TriggerDef]>> {
    require_manage(&state, &ctx).await?;
    Ok(Json(TRIGGER_CATALOGUE))
}

async fn audit_workflow(state: &AppState, ctx: &AuthContext, action: &str, id: Uuid) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("workflow".into());
    ev.resource_id = Some(id);
    let _ = audit::append(&state.pg, &ev).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every domain-event constant the platform emits must appear in
    /// the trigger catalogue and be flagged `emitted` — so the UI dropdown (which is
    /// driven solely by this catalogue) can offer them, and nothing emits an event no
    /// workflow could ever subscribe to.
    #[test]
    fn emitted_constants_are_catalogued() {
        for name in [
            events::DOCUMENT_INGESTED,
            events::DOCUMENT_IMPORTED,
            events::DOCUMENT_DELETED,
            events::PROJECT_MEMBER_ADDED,
            events::GROUP_MEMBER_ADDED,
            events::GROUP_MEMBER_REMOVED,
            events::CHAT_MEMBER_ADDED,
            events::DIRECTORY_USER_PROVISIONED,
            events::DIRECTORY_USER_DEACTIVATED,
            events::ACCOUNT_ARCHIVED,
            events::WORKFLOW_MESSAGE_POSTED,
        ] {
            let def = TRIGGER_CATALOGUE.iter().find(|d| d.name == name);
            assert!(def.is_some(), "emitted event {name:?} missing from TRIGGER_CATALOGUE");
            assert!(def.unwrap().emitted, "event {name:?} present but not flagged emitted");
        }
    }

    #[test]
    fn catalogue_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for d in TRIGGER_CATALOGUE {
            assert!(seen.insert(d.name), "duplicate trigger in catalogue: {}", d.name);
        }
    }
}
