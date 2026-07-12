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

//! Agent-run approval + trajectory.
//! The invoking user (or an admin) approves/rejects a run paused on a gated
//! action, and can read the run's audited trajectory.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::scheduler::{self, TaskType};
use crate::state::AppState;

// --- Listing (trajectory rail + approval inbox) ------------------------------

#[derive(Deserialize)]
pub struct RunsQuery {
    pub chat_id: Uuid,
}

#[derive(Serialize)]
pub struct RunSummary {
    pub id: Uuid,
    pub status: String,
    pub step_count: i32,
    pub agent_id: Option<Uuid>,
    pub pending_tool: Option<String>,
    pub created_epoch: i64,
    pub finished_epoch: Option<i64>,
}

/// GET /api/agent-runs?chat_id={id} — the chat's agent runs (newest first), for
/// the trajectory rail. Visible to anyone who can read the chat.
pub async fn list_runs(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<RunsQuery>,
) -> Result<Json<Vec<RunSummary>>> {
    crate::http::export::require_chat_read(&state, &ctx, q.chat_id).await?;
    let rows = sqlx::query!(
        r#"SELECT id, status::text AS "status!", step_count, agent_id, pending_tool,
                  extract(epoch from created_at)::bigint AS "created_epoch!",
                  extract(epoch from finished_at)::bigint AS finished_epoch
           FROM agent_runs WHERE chat_id = $1 ORDER BY created_at DESC LIMIT 50"#,
        q.chat_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| RunSummary {
                id: r.id,
                status: r.status,
                step_count: r.step_count,
                agent_id: r.agent_id,
                pending_tool: r.pending_tool,
                created_epoch: r.created_epoch,
                finished_epoch: r.finished_epoch,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct PendingApproval {
    pub run_id: Uuid,
    pub tool: Option<String>,
    pub summary: String,
    pub context: String,
    pub created_epoch: i64,
}

/// GET /api/agent-runs/pending — the CALLER's runs awaiting approval (the durable,
/// on-login notification for unattended runs an offline owner missed).
pub async fn list_pending(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<PendingApproval>>> {
    let Some(uid) = ctx.user_id else { return Ok(Json(vec![])) };
    let rows = sqlx::query!(
        r#"SELECT ar.id, ar.pending_tool, ar.pending_args, ar.automation_id, ar.chat_id,
                  extract(epoch from ar.created_at)::bigint AS "created_epoch!",
                  au.name AS "automation_name?", c.title AS "chat_title?"
           FROM agent_runs ar
           LEFT JOIN automations au ON au.id = ar.automation_id
           LEFT JOIN chats c ON c.id = ar.chat_id
           WHERE ar.acting_user_id = $1 AND ar.status = 'awaiting_approval'
           ORDER BY ar.created_at DESC LIMIT 50"#,
        uid
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| {
                let summary = r
                    .pending_args
                    .as_ref()
                    .and_then(|a| a.get("title"))
                    .and_then(|t| t.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| "Pending action".into());
                let context = r.automation_name.or(r.chat_title).unwrap_or_else(|| "Chat".into());
                PendingApproval { run_id: r.id, tool: r.pending_tool, summary, context, created_epoch: r.created_epoch }
            })
            .collect(),
    ))
}

/// Only the run's acting user (or an admin) may decide / inspect it.
async fn require_run_actor(state: &AppState, ctx: &AuthContext, run_id: Uuid) -> Result<()> {
    let owner: Option<Uuid> =
        sqlx::query_scalar!("SELECT acting_user_id FROM agent_runs WHERE id = $1", run_id)
            .fetch_optional(&state.pg)
            .await?
            .flatten();
    let Some(owner) = owner else {
        return Err(AppError::Validation("agent run not found".into()));
    };
    if ctx.is_admin() || ctx.user_id == Some(owner) {
        Ok(())
    } else {
        Err(AppError::Forbidden("not your agent run".into()))
    }
}

/// POST /api/agent-runs/{id}/approve — approve the pending gated action. Atomic
/// single-winner: if the CAS doesn't move the run, it was already decided (409).
/// Hands off to a live interactive waiter, else enqueues a durable resume.
pub async fn approve_run(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(run_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_run_actor(&state, &ctx, run_id).await?;
    if !crate::agent::decide(&state, run_id, true).await? {
        return Err(AppError::Conflict("agent run is no longer awaiting approval".into()));
    }
    crate::agent::audit_run(&state, ctx.user_id, ctx.role.as_str(), "agent.approved", run_id, json!({})).await;
    // Fast path: a live turn is awaiting → it executes. Else durable resume task.
    if !state.approvals.resolve(run_id, true) {
        scheduler::enqueue(&state.pg, TaskType::AgentResume, json!({ "run_id": run_id })).await?;
    }
    Ok(Json(json!({ "ok": true, "status": "approved" })))
}

/// POST /api/agent-runs/{id}/reject — reject the pending action; the run stops.
pub async fn reject_run(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(run_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_run_actor(&state, &ctx, run_id).await?;
    if !crate::agent::decide(&state, run_id, false).await? {
        return Err(AppError::Conflict("agent run is no longer awaiting approval".into()));
    }
    crate::agent::audit_run(&state, ctx.user_id, ctx.role.as_str(), "agent.rejected", run_id, json!({})).await;
    state.approvals.resolve(run_id, false); // unblock a waiter (no generation)
    crate::agent::finish(&state, run_id, "rejected").await;
    Ok(Json(json!({ "ok": true, "status": "rejected" })))
}

/// POST /api/agent-runs/{id}/cancel — stop a RUNNING agent-run (unlike `reject`,
/// which only resolves an `awaiting_approval` gate). Drops the kill-token and
/// marks the run `cancelled`; a long background run (e.g. Deep Research) polls
/// the token and aborts promptly, discarding any in-flight result. Actor-gated.
pub async fn cancel_run(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(run_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_run_actor(&state, &ctx, run_id).await?;
    crate::agent::kill(&state, run_id).await;
    crate::agent::audit_run(&state, ctx.user_id, ctx.role.as_str(), "agent.cancelled", run_id, json!({})).await;
    state.approvals.resolve(run_id, false); // unblock any waiter
    Ok(Json(json!({ "ok": true, "status": "cancelled" })))
}

#[derive(Serialize)]
pub struct RunEvent {
    pub action: String,
    pub occurred_epoch: Option<i64>,
    pub payload: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct RunOut {
    pub id: Uuid,
    pub status: String,
    pub step_count: i32,
    pub agent_id: Option<Uuid>,
    pub chat_id: Option<Uuid>,
    pub pending_tool: Option<String>,
    pub created_epoch: i64,
    pub finished_epoch: Option<i64>,
    /// The audited trajectory — every tool/agent event tagged with this run_id.
    pub events: Vec<RunEvent>,
}

/// GET /api/agent-runs/{id} — status + the audited trajectory.
pub async fn get_run(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(run_id): Path<Uuid>,
) -> Result<Json<RunOut>> {
    require_run_actor(&state, &ctx, run_id).await?;
    let r = sqlx::query!(
        r#"SELECT status::text AS "status!", step_count, agent_id, chat_id, pending_tool,
                  extract(epoch from created_at)::bigint AS "created_epoch!",
                  extract(epoch from finished_at)::bigint AS finished_epoch
           FROM agent_runs WHERE id = $1"#,
        run_id
    )
    .fetch_one(&state.pg)
    .await?;

    let events = sqlx::query!(
        r#"SELECT action_type,
                  extract(epoch from occurred_at)::bigint AS occurred_epoch,
                  payload
           FROM audit_events WHERE payload->>'run_id' = $1 ORDER BY seq"#,
        run_id.to_string()
    )
    .fetch_all(&state.pg)
    .await?
    .into_iter()
    .map(|e| RunEvent { action: e.action_type, occurred_epoch: e.occurred_epoch, payload: e.payload })
    .collect();

    Ok(Json(RunOut {
        id: run_id,
        status: r.status,
        step_count: r.step_count,
        agent_id: r.agent_id,
        chat_id: r.chat_id,
        pending_tool: r.pending_tool,
        created_epoch: r.created_epoch,
        finished_epoch: r.finished_epoch,
        events,
    }))
}
