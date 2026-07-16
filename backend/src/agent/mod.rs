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
//! A *run* wraps a chat turn (or an unattended automation run) so the agent's
//! state-changing actions can be **gated** behind human approval, **durably**
//! (the pending call is persisted and executed verbatim on approval, surviving a
//! crash), under a **per-run kill-token** that doubles as the run's identity.
//!
//! Containment order (environment layer, not the prompt): the **effect-gate**
//! here pauses state-changing/egress tools; `tools::tool_permitted` caps the run
//! to the invoking user's permissions (constrained delegation); zero-egress
//! removes exfiltration. The hash-chain audit, keyed by `run_id`, is the
//! trajectory log.

use deadpool_redis::redis;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::error::{AppError, Result};
use crate::state::AppState;

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
    execute_pending(state, run_id).await?;
    finish(state, run_id, "completed").await;
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

/// Execute the approved pending call **verbatim** — read the persisted
/// `pending_args` and generate exactly that artefact. Never re-infers with the
/// model (the human approved these specific arguments). Idempotent: a second call
/// (crash-replay) is a no-op once the artefact exists for the turn.
pub async fn execute_pending(state: &AppState, run_id: Uuid) -> Result<()> {
    let r = sqlx::query!(
        "SELECT chat_id, turn_id, acting_user_id, agent_id, pending_tool, pending_args FROM agent_runs WHERE id = $1",
        run_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("agent run not found".into()))?;

    let (Some(chat_id), Some(turn_id)) = (r.chat_id, r.turn_id) else { return Ok(()) };
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
                return Ok(());
            };
            let ctx = match crate::auth::load_context(&state.pg, user_id).await {
                Ok(c) => c,
                Err(_) => {
                    refuse_resume(state, run_id, chat_id, pending, "acting user unavailable").await;
                    return Ok(());
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
                    return Ok(());
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
            return Ok(());
        }
    }

    // Only `generate_artefact` is gated today; other gated tools reuse this hook.
    if r.pending_tool.as_deref() != Some("generate_artefact") {
        return Ok(());
    }

    // Idempotency key = (run_id, turn): if the turn already has an artefact, stop.
    let has: bool = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM generated_artefacts WHERE turn_id = $1) AS "e!""#,
        turn_id
    )
    .fetch_one(&state.pg)
    .await
    .unwrap_or(false);
    if has {
        return Ok(());
    }

    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("Document").to_string();
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
    // The format the human approved (pdf/docx/md); doubles as the file extension.
    let kind = match args.get("kind").and_then(|v| v.as_str()) {
        Some(k @ ("pdf" | "docx" | "md")) => k,
        _ => "md",
    };
    // The turn's assistant message — so the artefact renders inline under the answer.
    let message_id = args.get("message_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
    let artefact_id = Uuid::now_v7();
    // Store the RELATIVE suffix under `artefacts_dir`; resolve for the ML call only.
    let rel = format!("{chat_id}/{artefact_id}.{kind}");
    let out_path = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &rel).to_string_lossy().to_string();

    let (_path, mime) =
        crate::ml::generate_artefact(&state.http, &state.boot.ml.base_url, kind, &title, content.trim(), &out_path).await?;
    sqlx::query!(
        "INSERT INTO generated_artefacts (id, chat_id, turn_id, message_id, kind, title, disk_path, mime, created_by) \
         VALUES ($1, $2, $3, $4, ($5::text)::artefact_kind, $6, $7, $8, $9)",
        artefact_id, chat_id, turn_id, message_id, kind, title, rel, mime, r.acting_user_id,
    )
    .execute(&state.pg)
    .await?;

    let mut ev = AuditEvent::action("artefact.generated", "user");
    ev.actor_user_id = r.acting_user_id;
    ev.resource_type = Some("artefact".into());
    ev.resource_id = Some(artefact_id);
    ev.payload = Some(json!({ "chat_id": chat_id, "kind": kind, "title": title, "run_id": run_id.to_string(), "approved": true }));
    let _ = audit::append(&state.pg, &ev).await;
    Ok(())
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
