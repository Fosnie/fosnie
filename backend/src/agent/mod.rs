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

//! Action-taking agent runs.
//!
//! A *run* wraps a chat turn (or an unattended automation run) so side-effecting
//! MCP and custom tools can be **gated** behind human approval, **durably** (the
//! pending call is persisted and executed verbatim on approval, surviving a
//! crash), under a **per-run kill-token** that doubles as the run's identity.
//! Native tools never pause for approval; the run exists for them only to carry
//! the kill-token and the trajectory audit.
//!
//! Containment order (environment layer, not the prompt): the **approval gate**
//! pauses side-effecting MCP/custom calls; `tools::authorize_native_call` gates
//! every native call (grant, admin override, host capability, and
//! `tools::tool_permitted`'s constrained delegation to the invoking user's
//! permissions); zero-egress removes exfiltration. The hash-chain audit, keyed by
//! `run_id`, is the trajectory log.

use deadpool_redis::redis;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::error::{AppError, Result};
use crate::state::AppState;

/// What a durable resume actually did with the pending call.
///
/// Most resumes are terminal: the call ran (or was refused, which is still a
/// final answer), and the run is finished. A folder call is the exception — it
/// can only run while its machine is connected, so a resume that arrives with the
/// machine offline must not be marked done. It is *deferred*: parked again, to be
/// carried out the next time the owner approves or the machine reappears.
pub enum ResumeOutcome {
    Completed,
    Deferred,
}

fn token_key(run_id: Uuid) -> String {
    format!("pai:agentrun:{run_id}")
}

/// Start a run: insert the durable row + mint the Redis kill-token (TTL = the
/// run's wall-clock budget). Deleting the token = the run cannot take its next
/// action (a real per-run kill, not decorative identity).
#[allow(clippy::too_many_arguments)]
pub async fn start_run(
    state: &AppState,
    agent_id: Option<Uuid>,
    actor: Option<Uuid>,
    role: &str,
    chat_id: Option<Uuid>,
    turn_id: Uuid,
    project_id: Option<Uuid>,
    automation_id: Option<Uuid>,
    wall_clock_secs: u64,
) -> Result<Uuid> {
    let run_id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO agent_runs (id, agent_id, acting_user_id, chat_id, turn_id, project_id, automation_id, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'running')",
        run_id, agent_id, actor, chat_id, turn_id, project_id, automation_id,
    )
    .execute(&state.pg)
    .await?;
    let mut conn = state.redis.get().await?;
    redis::cmd("SET")
        .arg(token_key(run_id))
        .arg("1")
        .arg("EX")
        .arg(wall_clock_secs.max(1))
        .query_async::<()>(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis SET agentrun: {e}")))?;
    audit_run(state, actor, role, "agent.run.started", run_id, json!({})).await;
    Ok(run_id)
}

/// May the run still take an action? False if the fleet switch is off OR the
/// per-run kill-token is gone (expired / explicitly killed).
pub async fn alive(state: &AppState, run_id: Uuid) -> bool {
    if !state.boot.features.agents_enabled {
        return false;
    }
    let Ok(mut conn) = state.redis.get().await else { return false };
    let exists: i64 = redis::cmd("EXISTS")
        .arg(token_key(run_id))
        .query_async(&mut conn)
        .await
        .unwrap_or(0);
    exists > 0
}

/// Release a run's kill-token without touching its status. Used by the approval
/// TTL sweep, which claims the terminal status itself in one atomic update and
/// then only needs the token gone.
pub async fn release_token(state: &AppState, run_id: Uuid) {
    drop_token(state, run_id).await;
}

/// Drop the run's kill-token (used by `finish` to release the TTL key).
async fn drop_token(state: &AppState, run_id: Uuid) {
    if let Ok(mut conn) = state.redis.get().await {
        let _: std::result::Result<i64, _> =
            redis::cmd("DEL").arg(token_key(run_id)).query_async(&mut conn).await;
    }
}

/// Explicit per-run kill: drop the token (halts an active in-loop run at once) AND
/// flip the durable status to `cancelled` (defeats a later approval). The DB flag
/// is the authority for the deferred path, since the token's TTL = the wall-clock
/// budget and a legitimate long unattended approval may outlive it.
pub async fn kill(state: &AppState, run_id: Uuid) {
    drop_token(state, run_id).await;
    let _ = sqlx::query!(
        "UPDATE agent_runs SET status = 'cancelled', finished_at = now(), updated_at = now() \
         WHERE id = $1 AND status IN ('running', 'awaiting_approval', 'approved')",
        run_id,
    )
    .execute(&state.pg)
    .await;
}

/// Pause at a gated action: persist the EXACT pending call (executed verbatim on
/// approval) and flip to `awaiting_approval`.
pub async fn request_approval(
    state: &AppState,
    run_id: Uuid,
    actor: Option<Uuid>,
    role: &str,
    tool: &str,
    args: &Value,
    step: i32,
) -> Result<()> {
    sqlx::query!(
        "UPDATE agent_runs SET status = 'awaiting_approval', pending_tool = $2, \
         pending_args = $3, pending_step = $4, updated_at = now() WHERE id = $1",
        run_id, tool, args, step,
    )
    .execute(&state.pg)
    .await?;
    audit_run(state, actor, role, "agent.approval_requested", run_id, json!({ "tool": tool })).await;
    Ok(())
}

/// Atomic single-winner decision — defeats the in-process-oneshot vs durable
/// `agent_resume` double-resume race. Returns true iff THIS call moved the run
/// out of `awaiting_approval` (a second approve sees 0 rows → no-op).
pub async fn decide(state: &AppState, run_id: Uuid, approve: bool) -> Result<bool> {
    let status = if approve { "approved" } else { "rejected" };
    let n = sqlx::query!(
        "UPDATE agent_runs SET status = ($2::text)::agent_run_status, updated_at = now() \
         WHERE id = $1 AND status = 'awaiting_approval'",
        run_id, status,
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    Ok(n == 1)
}

/// Tell every one of the user's connected clients that a run's approval gate is
/// settled — approved and about to run, or closed (rejected, timed out,
/// auto-declined, cancelled). A pending approval card is shown on every device
/// the user has open, but only one of them takes the decision; without this, the
/// others sit asking a question that has already been answered. Best-effort and
/// process-local (the socket registry is; Postgres remains the authority), so a
/// `None` user or an absent socket simply means nobody to tell here.
pub fn broadcast_resolved(state: &AppState, user_id: Option<Uuid>, run_id: Uuid, approved: bool) {
    if let Some(uid) = user_id {
        state
            .hub
            .send_to_user(uid, crate::ws::protocol::ServerFrame::AgentApprovalResolved { run_id, approved });
    }
}

/// Final state; releases the kill-token (does NOT force-cancel — that is `kill`).
pub async fn finish(state: &AppState, run_id: Uuid, status: &str) {
    let _ = sqlx::query!(
        "UPDATE agent_runs SET status = ($2::text)::agent_run_status, finished_at = now(), updated_at = now() WHERE id = $1",
        run_id, status,
    )
    .execute(&state.pg)
    .await;
    drop_token(state, run_id).await;
}

/// Run the approved pending action, if and only if the run is currently
/// `approved` and the fleet switch is on. Idempotent (executes once, then flips to
/// `completed`, so a duplicate call — interactive winner + durable resume — is a
/// no-op). The single point that turns approval into action.
pub async fn execute_approved(state: &AppState, run_id: Uuid) -> Result<()> {
    if !state.boot.features.agents_enabled {
        return Ok(());
    }
    let status: Option<String> =
        sqlx::query_scalar!(r#"SELECT status::text AS "s!" FROM agent_runs WHERE id = $1"#, run_id)
            .fetch_optional(&state.pg)
            .await?;
    if status.as_deref() != Some("approved") {
        return Ok(()); // rejected / cancelled / already completed — not ours to run
    }
    match execute_pending(state, run_id).await? {
        ResumeOutcome::Completed => finish(state, run_id, "completed").await,
        // The folder's machine was not there to carry the call out. Return the run
        // to where it was — waiting for approval — rather than finishing it, so the
        // next approval or the machine's reconnect runs it. Keep the kill-token.
        ResumeOutcome::Deferred => {
            let _ = sqlx::query!(
                "UPDATE agent_runs SET status = 'awaiting_approval', updated_at = now() \
                 WHERE id = $1 AND status = 'approved'",
                run_id,
            )
            .execute(&state.pg)
            .await;
        }
    }
    Ok(())
}

/// Return a run to `running` after an in-loop approval decision (FEATURE B1): the
/// gated MCP call has been handled in-line, but the turn continues, so the run must
/// not stay `approved`/`awaiting_approval` (else `complete_if_running` can't finalise it).
pub async fn mark_running(state: &AppState, run_id: Uuid) {
    let _ = sqlx::query!(
        "UPDATE agent_runs SET status = 'running', updated_at = now() \
         WHERE id = $1 AND status IN ('approved', 'awaiting_approval')",
        run_id,
    )
    .execute(&state.pg)
    .await;
}

/// Close a run that finished without a gated action (read-only answer).
pub async fn complete_if_running(state: &AppState, run_id: Uuid) {
    let _ = sqlx::query!(
        "UPDATE agent_runs SET status = 'completed', finished_at = now(), updated_at = now() WHERE id = $1 AND status = 'running'",
        run_id,
    )
    .execute(&state.pg)
    .await;
    drop_token(state, run_id).await;
}

/// Audit a durable-resume refusal (fail-closed) so a blocked resume is visible rather than
/// silently dropped.
async fn refuse_resume(state: &AppState, run_id: Uuid, chat_id: Uuid, tool: &str, reason: &str) {
    let mut ev = AuditEvent::action("tool.resume_denied", "system");
    ev.resource_type = Some("agent_run".into());
    ev.resource_id = Some(run_id);
    ev.outcome = crate::audit::AuditOutcome::Failure;
    ev.payload = Some(json!({ "chat_id": chat_id, "tool": tool, "denied": "resume", "reason": reason }));
    let _ = audit::append(&state.pg, &ev).await;
}

/// Execute an approved pending call **verbatim** through the same authorisation
/// gates as the live loop — the approval that queued it is NOT a substitute for
/// re-checking (a grant, RBAC entitlement, connector, or server status can have
/// changed since). Only side-effecting MCP and custom tools ever pause for
/// approval, so only those resume here; a native pending (which no path should
/// ever persist, since native tools never pause) fails closed and is audited.
pub async fn execute_pending(state: &AppState, run_id: Uuid) -> Result<ResumeOutcome> {
    let r = sqlx::query!(
        "SELECT chat_id, turn_id, acting_user_id, agent_id, pending_tool, pending_args FROM agent_runs WHERE id = $1",
        run_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("agent run not found".into()))?;

    let (Some(chat_id), Some(turn_id)) = (r.chat_id, r.turn_id) else {
        return Ok(ResumeOutcome::Completed);
    };
    let args = r.pending_args.unwrap_or_else(|| json!({}));

    // The egress/permission-bearing tools (MCP + custom) resume through the SAME
    // authorisation gates as the live loop — the approval that queued this call is NOT a
    // substitute for re-checking. A grant, RBAC entitlement, connector, or server status
    // can have changed since approval, and the resume must fail closed when it has.
    if let Some(pending) = r.pending_tool.as_deref() {
        let is_mcp = crate::mcp::is_namespaced(pending);
        let is_custom = !is_mcp && !crate::tools::ALL.contains(&pending);
        if is_mcp || is_custom {
            // No agent ⇒ no grants to scope to; no user ⇒ no identity to authorise
            // against. Either way, refuse the resume (and audit it) rather than run it
            // unscoped. A NULL-agent run can never legitimately carry such a call.
            let (Some(agent_id), Some(user_id)) = (r.agent_id, r.acting_user_id) else {
                refuse_resume(state, run_id, chat_id, pending, "no agent or acting user").await;
                return Ok(ResumeOutcome::Completed);
            };
            let ctx = match crate::auth::load_context(&state.pg, user_id).await {
                Ok(c) => c,
                Err(_) => {
                    refuse_resume(state, run_id, chat_id, pending, "acting user unavailable").await;
                    return Ok(ResumeOutcome::Completed);
                }
            };
            // The agent's granted tools are the source of truth for both grant shapes.
            let agent_tools: Vec<String> =
                sqlx::query_scalar!("SELECT tool_name FROM agent_tools WHERE agent_id = $1", agent_id)
                    .fetch_all(&state.pg)
                    .await
                    .unwrap_or_default();

            if is_mcp {
                // Route through the one MCP dispatch path (durable = true): egress, server
                // status, RBAC, agent grant, pinned catalogue, and connection are all
                // re-checked, and the call + any refusal are audited.
                let grants = crate::mcp::parse_grants(&agent_tools);
                let res =
                    crate::mcp::dispatch(state, &ctx, &grants, chat_id, pending, &args, true).await;
                let status = match &res {
                    Ok(s) if !s.starts_with("error:") => "ok",
                    _ => "error",
                };
                metrics::counter!("tool_calls_total", "tool" => pending.to_string(), "kind" => "mcp", "status" => status)
                    .increment(1);
            } else {
                // Custom tool: enforce the agent grant, then reuse the live loader's
                // enabled + approved + agent-scoped filter so live and resume agree, then
                // dispatch (which runs `guard_egress` for http). A grant-blind lookup here
                // was the resume-time bypass.
                if !agent_tools.iter().any(|t| t == pending) {
                    refuse_resume(state, run_id, chat_id, pending, "tool not granted to agent").await;
                    return Ok(ResumeOutcome::Completed);
                }
                let (_defs, map) =
                    crate::tools::custom::load_enabled_custom(&state.pg, &agent_tools).await;
                match map.get(pending) {
                    Some(row) => {
                        crate::tools::custom::dispatch_custom_durable(state, &ctx, chat_id, row, &args)
                            .await
                    }
                    None => {
                        refuse_resume(state, run_id, chat_id, pending, "tool disabled or unapproved")
                            .await
                    }
                }
            }
            return Ok(ResumeOutcome::Completed);
        }
    }

    // A folder tool is native (it lives in `tools::ALL`), so it does not resume
    // through the MCP/custom path above and would otherwise be refused below. It
    // has its own durable resume: the machine carries the call out, addressed over
    // the hub, after the same re-checks the live loop would do. The arm is kept
    // narrow — a folder tool AND a live folder binding for this chat — so no other
    // native gains a resume path by accident.
    if let Some(pending) = r.pending_tool.as_deref() {
        if crate::tools::desktop::is_desktop_tool(pending) {
            // The machine is whichever one owns the chat's bound folder; the run
            // does not carry a device of its own, so there is one source of truth
            // and it cannot drift. No live binding ⇒ fail closed.
            let device_id: Option<Uuid> = sqlx::query_scalar!(
                "SELECT w.device_id FROM chat_workspace cw \
                 JOIN device_workspaces w ON w.id = cw.workspace_id \
                 WHERE cw.chat_id = $1 AND w.revoked_at IS NULL",
                chat_id,
            )
            .fetch_optional(&state.pg)
            .await?;

            let (Some(user_id), Some(device_id)) = (r.acting_user_id, device_id) else {
                refuse_resume(state, run_id, chat_id, pending, "no device binding for resume").await;
                return Ok(ResumeOutcome::Completed);
            };

            // Not there to carry it out: keep the pause rather than fail it, and
            // say why. A later approval, or the machine reconnecting, runs it.
            if !state.hub.is_device_online(user_id, device_id) {
                audit_run(
                    state,
                    Some(user_id),
                    "system",
                    "agent.resume_deferred",
                    run_id,
                    json!({ "tool": pending, "reason": "device offline" }),
                )
                .await;
                return Ok(ResumeOutcome::Deferred);
            }

            // Re-authorise from scratch: the approval is not a substitute for
            // re-checking that the user, the folder, and its level of trust are all
            // still what they were (tier and boundary are re-checked inside
            // `execute` → `prepare`).
            let ctx = match crate::auth::load_context(&state.pg, user_id).await {
                Ok(c) => c,
                Err(_) => {
                    refuse_resume(state, run_id, chat_id, pending, "acting user unavailable").await;
                    return Ok(ResumeOutcome::Completed);
                }
            };
            let dctx = match crate::tools::desktop::load_ctx(&state.pg, chat_id, Some(device_id)).await
            {
                Some(d) => d.with_route(crate::tools::desktop::DesktopSink::DeviceRoute {
                    user_id,
                    device_id,
                }),
                None => {
                    refuse_resume(state, run_id, chat_id, pending, "folder no longer connected").await;
                    return Ok(ResumeOutcome::Completed);
                }
            };
            // A sender kept alive only so the device route's fallback never sees a
            // closed channel; the route ignores it and addresses the hub.
            let (tx, _rx) = tokio::sync::mpsc::channel(1);
            let res = crate::tools::desktop::execute(
                state, &ctx, chat_id, &tx, &dctx, turn_id, pending, &args,
            )
            .await;
            let status = if res.is_ok() { "ok" } else { "error" };
            metrics::counter!("tool_calls_total", "tool" => pending.to_string(), "kind" => "desktop", "status" => status)
                .increment(1);
            // Ran, or errored honestly (offline mid-call, refused at the boundary):
            // either way the resume is done and the run is finished.
            return Ok(ResumeOutcome::Completed);
        }
    }

    // Nothing else resumes durably. Other native tools never pause for approval, so
    // no such name is ever persisted as a pending call. If one somehow is (a stale
    // row, or a future regression), fail closed and audit rather than run it
    // unscoped — there is no native durable-resume path.
    if let Some(pending) = r.pending_tool.as_deref() {
        refuse_resume(state, run_id, chat_id, pending, "native tool has no durable resume path").await;
    }
    Ok(ResumeOutcome::Completed)
}

/// Audit a run lifecycle event, tagged with `run_id` so the audit doubles as the
/// run's trajectory log.
pub async fn audit_run(
    state: &AppState,
    actor: Option<Uuid>,
    role: &str,
    action: &str,
    run_id: Uuid,
    mut payload: Value,
) {
    payload["run_id"] = json!(run_id.to_string());
    let mut ev = AuditEvent::action(action, role);
    ev.actor_user_id = actor;
    ev.resource_type = Some("agent_run".into());
    ev.resource_id = Some(run_id);
    ev.risk_anomaly_flag = action.contains("approval") || action.contains("started");
    ev.payload = Some(payload);
    let _ = audit::append(&state.pg, &ev).await;
}
