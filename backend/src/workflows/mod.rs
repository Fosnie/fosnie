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

//! Event-driven workflow engine.
//!
//! The dispatcher [`dispatch_once`] relays undispatched `events` to enabled
//! `workflows` whose trigger matches, applies the loop guards, and enqueues
//! a durable `workflow_run` task per firing (reusing the scheduler's
//! retry/backoff/dead-letter). [`run`] executes a run's action.
//!
//! This slice ships **`system_action`** (deterministic, no GPU, no LLM); the
//! `agent_run` action and the 7b/7c guard systems land in later slices. The 7a
//! core is here: human-only default, depth cap, and idempotency (the last enforced
//! structurally by the `workflow_runs` unique index).

use serde_json::{json, Value};
use sqlx::PgConnection;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{AppError, Result};
use crate::events::{self, ActorType, EventRow};
use crate::scheduler::TaskType;
use crate::state::AppState;

/// Loop-guard depth cap when `workflows.max_depth` is unset.
const DEFAULT_MAX_DEPTH: i32 = 3;
/// How many undispatched events one relay pass claims.
const DISPATCH_BATCH: i64 = 64;
/// Task priority for workflow runs (lower runs first; ingest/automation = 100).
/// Workflow work sits behind other background work — never starves the hot path.
const WORKFLOW_TASK_PRIORITY: i32 = 150;
/// Rolling window for the per-workflow rate cap.
const RATE_WINDOW_SECS: i64 = 60;

/// Guard outcome for one (event, workflow) pair (human-only + depth).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Fire,
    Skip(&'static str),
}

/// The loop-guard core (human-only + depth cap). **Pure** → unit-tested.
/// Idempotency is enforced structurally by the `workflow_runs` unique
/// index, not here; explicit cycle-by-resource detection is a later slice.
pub fn guard_decision(
    actor_type: ActorType,
    trigger_on_system_events: bool,
    event_depth: i32,
    max_depth: i32,
) -> Decision {
    // Human-only by default: a workflow ignores agent/workflow/system-caused
    // events unless it opts in. This single rule kills the common self-trigger
    // loop (a workflow that creates a doc emits a workflow-actor event → nothing).
    if actor_type != ActorType::Human && !trigger_on_system_events {
        return Decision::Skip("system-originated event; workflow is human-only");
    }
    // Hard circuit breaker: beyond the cap, events trigger nothing.
    if event_depth >= max_depth {
        return Decision::Skip("max workflow depth reached");
    }
    Decision::Fire
}

/// Does the event fall inside the workflow's declared scope? Minimal for this
/// slice: a `project_id` (column or `trigger_scope.project_id`) must match the
/// event's project, and a `trigger_scope.kb_id` must match `payload.kb_id`. Other
/// scope keys are honoured as the catalogue grows.
fn scope_matches(wf_project: Option<Uuid>, trigger_scope: &Value, ev: &EventRow) -> bool {
    // Project scope: explicit column wins, else the scope blob.
    let scope_project = wf_project.or_else(|| {
        trigger_scope
            .get("project_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
    });
    if let Some(pid) = scope_project {
        if ev.project_id != Some(pid) {
            return false;
        }
    }
    if let Some(kb) = trigger_scope.get("kb_id").and_then(|v| v.as_str()) {
        let ev_kb = ev.payload.get("kb_id").and_then(|v| v.as_str());
        if ev_kb != Some(kb) {
            return false;
        }
    }
    true
}

/// Evaluate a SAFE declarative condition over an event context. `null` ⇒ true.
/// Shape: `{ "all": [clause…] }` / `{ "any": [clause…] }` (nestable), or a single
/// clause `{ "field": "payload.mime", "op": "eq", "value": … }`. Fixed operator set;
/// anything unknown ⇒ the clause is false (fail-closed). **No code / eval.**
pub fn eval_condition(condition: &Value, ctx: &Value) -> bool {
    match condition {
        Value::Null => true,
        Value::Object(map) => {
            if let Some(Value::Array(clauses)) = map.get("all") {
                return clauses.iter().all(|c| eval_condition(c, ctx));
            }
            if let Some(Value::Array(clauses)) = map.get("any") {
                return clauses.iter().any(|c| eval_condition(c, ctx));
            }
            eval_clause(map, ctx)
        }
        _ => false,
    }
}

fn eval_clause(map: &serde_json::Map<String, Value>, ctx: &Value) -> bool {
    let (Some(field), Some(op)) = (
        map.get("field").and_then(|v| v.as_str()),
        map.get("op").and_then(|v| v.as_str()),
    ) else {
        return false;
    };
    let want = map.get("value").unwrap_or(&Value::Null);
    let got = lookup(ctx, field).unwrap_or(&Value::Null);
    match op {
        "eq" => got == want,
        "ne" => got != want,
        "gt" => num(got).zip(num(want)).is_some_and(|(a, b)| a > b),
        "lt" => num(got).zip(num(want)).is_some_and(|(a, b)| a < b),
        "in" => want.as_array().is_some_and(|arr| arr.contains(got)),
        "contains" => match got {
            Value::String(s) => want.as_str().is_some_and(|w| s.contains(w)),
            Value::Array(arr) => arr.contains(want),
            _ => false,
        },
        _ => false, // unknown operator → fail-closed
    }
}

/// Navigate a dotted path (`payload.mime`) into a JSON context.
fn lookup<'a>(ctx: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = ctx;
    for seg in path.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

/// One relay pass: claim undispatched events (`FOR UPDATE SKIP LOCKED`), match
/// enabled workflows, apply guards + condition, and enqueue a `workflow_run` task
/// per firing — all in one transaction, so dispatch + enqueue + the dispatched
/// mark commit together (exactly-once). No-op when the feature is off.
/// Returns the number of workflow runs enqueued.
pub async fn dispatch_once(state: &AppState) -> Result<u64> {
    if !feature_enabled(state).await || paused(state).await {
        return Ok(0);
    }
    let max_depth = read_max_depth(state).await;
    let watermark = dispatch_watermark(state).await;
    let mut tx = state.pg.begin().await?;

    let evs = sqlx::query!(
        r#"SELECT id,
                  event_type,
                  actor_type AS "actor_type: ActorType",
                  actor_user_id,
                  resource_type,
                  resource_id,
                  project_id,
                  payload,
                  trigger_chain,
                  depth,
                  created_at
           FROM events
           WHERE dispatched_at IS NULL
           ORDER BY created_at
           LIMIT $1
           FOR UPDATE SKIP LOCKED"#,
        DISPATCH_BATCH
    )
    .fetch_all(&mut *tx)
    .await?;

    if evs.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }

    let mut fired = 0u64;
    let mut dispatched_ids: Vec<Uuid> = Vec::with_capacity(evs.len());

    for e in evs {
        // Fast-forward: events older than the enable-watermark are marked
        // dispatched without firing any workflow — preserved as history, never
        // replayed. Prevents an avalanche when the engine is switched back on.
        if let Some(wm) = watermark {
            if e.created_at < wm {
                dispatched_ids.push(e.id);
                continue;
            }
        }
        let ev = EventRow {
            id: e.id,
            event_type: e.event_type,
            actor_type: e.actor_type,
            actor_user_id: e.actor_user_id,
            resource_type: e.resource_type,
            resource_id: e.resource_id,
            project_id: e.project_id,
            payload: e.payload,
            trigger_chain: e.trigger_chain,
            depth: e.depth,
        };
        dispatched_ids.push(ev.id);

        let workflows = sqlx::query!(
            r#"SELECT id, owner_id, project_id, trigger_on_system_events,
                      condition, trigger_scope, coalesce_window_secs, max_runs_per_window
               FROM workflows
               WHERE enabled AND trigger_event_type = $1"#,
            ev.event_type
        )
        .fetch_all(&mut *tx)
        .await?;

        let ctx = condition_ctx(&ev);
        for w in workflows {
            if !scope_matches(w.project_id, &w.trigger_scope, &ev) {
                continue;
            }
            match guard_decision(ev.actor_type, w.trigger_on_system_events, ev.depth, max_depth) {
                Decision::Fire => {}
                Decision::Skip("max workflow depth reached") => {
                    // The safety-relevant circuit breaker is audited.
                    let mut a = crate::audit::AuditEvent::action("workflow.depth_exceeded", "system");
                    a.resource_type = Some("workflow".into());
                    a.resource_id = Some(w.id);
                    a.payload = Some(json!({ "event_id": ev.id, "depth": ev.depth, "max_depth": max_depth }));
                    let _ = crate::audit::append_with(&mut tx, &a).await;
                    continue;
                }
                Decision::Skip(_) => continue, // human-only filtering is normal; not audited
            }
            if let Some(cond) = &w.condition {
                if !eval_condition(cond, &ctx) {
                    continue;
                }
            }

            // Cycle detection: if this (workflow, resource) already appears
            // in the event's lineage, break the cycle and audit it.
            if let Some(res) = ev.resource_id {
                if cycle_detected(&mut tx, w.id, res, &ev.trigger_chain).await? {
                    let mut a = crate::audit::AuditEvent::action("workflow.cycle_detected", "system");
                    a.resource_type = Some("workflow".into());
                    a.resource_id = Some(w.id);
                    a.payload = Some(json!({ "event_id": ev.id, "resource_id": res }));
                    let _ = crate::audit::append_with(&mut tx, &a).await;
                    continue;
                }
            }

            // Coalescing: a windowed workflow buffers the event for a
            // batched run; window 0 fires immediately. Either way the rate cap +
            // idempotency apply at run creation.
            if w.coalesce_window_secs > 0 {
                buffer_coalesced(&mut tx, w.id, &ev, w.coalesce_window_secs).await?;
            } else if let RunOutcome::Created =
                try_create_run(state, &mut tx, w.id, w.owner_id, &[ev.id], ev.depth, w.max_runs_per_window).await?
            {
                fired += 1;
            }
        }
    }

    sqlx::query!(
        "UPDATE events SET dispatched_at = now() WHERE id = ANY($1)",
        &dispatched_ids
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(fired)
}

/// Fire coalescing buffers whose window has closed. Each buffered bucket
/// becomes **one** run over its accumulated batch. Called from the scheduler tick.
/// No-op when the feature is off / paused.
pub async fn scan_coalesced(state: &AppState) -> Result<u64> {
    if !feature_enabled(state).await || paused(state).await {
        return Ok(0);
    }
    let mut tx = state.pg.begin().await?;
    let due = sqlx::query!(
        r#"SELECT workflow_id, scope_key, event_ids, depth
           FROM workflow_coalesce
           WHERE fire_at <= now()
           ORDER BY fire_at
           LIMIT $1
           FOR UPDATE SKIP LOCKED"#,
        DISPATCH_BATCH
    )
    .fetch_all(&mut *tx)
    .await?;
    if due.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }
    let mut fired = 0u64;
    for b in due {
        // The workflow may have been disabled/deleted while the window was open.
        let wf = sqlx::query!(
            "SELECT owner_id, max_runs_per_window, enabled FROM workflows WHERE id = $1",
            b.workflow_id
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(wf) = wf {
            if wf.enabled {
                if let RunOutcome::Created = try_create_run(
                    state, &mut tx, b.workflow_id, wf.owner_id, &b.event_ids, b.depth, wf.max_runs_per_window,
                )
                .await?
                {
                    fired += 1;
                }
            }
        }
        sqlx::query!(
            "DELETE FROM workflow_coalesce WHERE workflow_id = $1 AND scope_key = $2",
            b.workflow_id,
            b.scope_key
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(fired)
}

/// Buffer an event into the workflow's coalescing window. Tumbling: the
/// bucket's `fire_at` is set only on the first event; later events just append.
async fn buffer_coalesced(
    conn: &mut PgConnection,
    workflow_id: Uuid,
    ev: &EventRow,
    window_secs: i32,
) -> Result<()> {
    let scope_key = ev
        .project_id
        .map(|p| p.to_string())
        .unwrap_or_else(|| "global".into());
    sqlx::query!(
        "INSERT INTO workflow_coalesce (workflow_id, scope_key, event_ids, depth, fire_at) \
         VALUES ($1, $2, ARRAY[$3]::uuid[], $4, now() + ($5::int * interval '1 second')) \
         ON CONFLICT (workflow_id, scope_key) DO UPDATE \
           SET event_ids = array_append(workflow_coalesce.event_ids, $3), \
               depth = GREATEST(workflow_coalesce.depth, $4)",
        workflow_id,
        scope_key,
        ev.id,
        ev.depth,
        window_secs,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Outcome of attempting to create one workflow run.
enum RunOutcome {
    Created,
    Throttled,
    Duplicate,
    Disabled,
}

/// How many rate-cap trips within the window auto-disable a workflow.
const AUTO_DISABLE_TRIPS: i64 = 20;

/// Create one run for `event_ids` under the per-workflow rate cap +
/// idempotency, enqueueing a low-priority durable task. Throttle/auto-disable
/// are audited in `conn`'s transaction. Returns what happened.
async fn try_create_run(
    state: &AppState,
    conn: &mut PgConnection,
    workflow_id: Uuid,
    owner: Uuid,
    event_ids: &[Uuid],
    depth: i32,
    max_per_window: i32,
) -> Result<RunOutcome> {
    // Rate cap: runs already created for this workflow in the rolling window.
    let recent: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM workflow_runs
           WHERE workflow_id = $1 AND created_at > now() - ($2::bigint * interval '1 second')"#,
        workflow_id,
        RATE_WINDOW_SECS,
    )
    .fetch_one(&mut *conn)
    .await?;

    if over_rate(recent, max_per_window) {
        // Repeated trips → auto-disable (fail safe, not fail-loud-forever).
        let trips_ok = crate::cache::rate_limit_ok(
            &state.redis,
            &format!("wf:trip:{workflow_id}"),
            AUTO_DISABLE_TRIPS,
            300,
        )
        .await;
        if !trips_ok {
            sqlx::query!("UPDATE workflows SET enabled = false WHERE id = $1", workflow_id)
                .execute(&mut *conn)
                .await?;
            let mut a = crate::audit::AuditEvent::action("workflow.auto_disabled", "system");
            a.resource_type = Some("workflow".into());
            a.resource_id = Some(workflow_id);
            a.payload = Some(json!({ "reason": "rate cap repeatedly tripped", "max_per_window": max_per_window }));
            let _ = crate::audit::append_with(&mut *conn, &a).await;
            state.hub.send_invalidate(&[owner], vec![vec!["workflows".to_string()]]);
            return Ok(RunOutcome::Disabled);
        }
        let mut a = crate::audit::AuditEvent::action("workflow.throttled", "system");
        a.resource_type = Some("workflow".into());
        a.resource_id = Some(workflow_id);
        a.payload = Some(json!({ "recent_runs": recent, "max_per_window": max_per_window }));
        let _ = crate::audit::append_with(&mut *conn, &a).await;
        return Ok(RunOutcome::Throttled);
    }

    // Idempotency: one run per (workflow, event-set).
    let run_id = Uuid::now_v7();
    let inserted = sqlx::query_scalar!(
        r#"INSERT INTO workflow_runs
             (id, workflow_id, trigger_event_ids, status, depth, run_as_user_id)
           VALUES ($1, $2, $3, 'queued', $4, $5)
           ON CONFLICT (workflow_id, trigger_event_ids) DO NOTHING
           RETURNING id"#,
        run_id,
        workflow_id,
        event_ids,
        depth,
        owner,
    )
    .fetch_optional(&mut *conn)
    .await?;
    if inserted.is_none() {
        return Ok(RunOutcome::Duplicate);
    }

    // Enqueue a low-priority durable task (behind ingest/automation) in the same tx.
    sqlx::query!(
        "INSERT INTO tasks (id, task_type, payload, priority) VALUES ($1, $2, $3, $4)",
        Uuid::now_v7(),
        TaskType::WorkflowRun as TaskType,
        json!({ "workflow_run_id": run_id }),
        WORKFLOW_TASK_PRIORITY,
    )
    .execute(&mut *conn)
    .await?;
    Ok(RunOutcome::Created)
}

/// True when the workflow has already created `>= max_per_window` runs this window
/// (`max_per_window <= 0` disables the cap). Pure → unit-tested.
pub fn over_rate(recent: i64, max_per_window: i32) -> bool {
    max_per_window > 0 && recent >= max_per_window as i64
}

/// Cycle detection: has this (workflow, resource) already fired within the
/// event's lineage chain? `chain` is the ordered workflow_run ids in the lineage.
async fn cycle_detected(
    conn: &mut PgConnection,
    workflow_id: Uuid,
    resource_id: Uuid,
    chain: &[Uuid],
) -> Result<bool> {
    if chain.is_empty() {
        return Ok(false);
    }
    let hit: bool = sqlx::query_scalar!(
        r#"SELECT EXISTS(
             SELECT 1 FROM workflow_runs wr
             JOIN events ev ON ev.id = ANY(wr.trigger_event_ids)
             WHERE wr.id = ANY($1) AND wr.workflow_id = $2 AND ev.resource_id = $3
           ) AS "e!""#,
        chain,
        workflow_id,
        resource_id,
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(hit)
}

/// Runtime-effective feature gate. Reads the admin-toggleable
/// `features.workflows` override (falling back to the boot flag) through the
/// resolver seam, so enabling the engine from the Admin console takes effect
/// without a restart. Keyed with no user (fleet-wide dispatcher), so per-group
/// restrictions don't apply to the relay itself — only the global ceiling.
async fn feature_enabled(state: &AppState) -> bool {
    crate::features::enabled_for_user(state, None, "workflows").await
}

/// The fast-forward watermark: events created before this instant are
/// skip-marked (dispatched, no run) so re-enabling the engine does not replay the
/// historical backlog that accumulated while it was off. Stamped by the Admin
/// toggle on each off→on transition. Absent (`None`) ⇒ no watermark — a fresh
/// deploy booting with the flag already true behaves as before (dispatches all).
async fn dispatch_watermark(state: &AppState) -> Option<OffsetDateTime> {
    let e = crate::config::runtime::get(&state.pg, "workflows.dispatch_watermark")
        .await
        .ok()
        .flatten()?;
    OffsetDateTime::parse(&e.value, &Rfc3339).ok()
}

/// Fleet kill-switch: `workflows.pause_all=true` halts all dispatch + runs.
async fn paused(state: &AppState) -> bool {
    matches!(
        crate::config::runtime::get(&state.pg, "workflows.pause_all").await.ok().flatten(),
        Some(e) if e.value == "true"
    )
}

/// Build the condition-evaluation context from an event (the safe surface).
fn condition_ctx(ev: &EventRow) -> Value {
    json!({
        "event_type": ev.event_type,
        "actor_type": actor_str(ev.actor_type),
        "project_id": ev.project_id.map(|u| u.to_string()),
        "resource_id": ev.resource_id.map(|u| u.to_string()),
        "payload": ev.payload,
    })
}

fn actor_str(a: ActorType) -> &'static str {
    match a {
        ActorType::Human => "human",
        ActorType::Agent => "agent",
        ActorType::Workflow => "workflow",
        ActorType::System => "system",
    }
}

/// Execute one workflow run (the `WorkflowRun` task handler). Loads the run +
/// workflow, marks it running, dispatches by `action_type`, and records the
/// outcome. A genuine execution error is returned so the task queue retries /
/// dead-letters; an unsupported action is a clean `skipped` (no retry).
pub async fn run(state: &AppState, run_id: Uuid) -> Result<()> {
    // Fleet kill-switch / mid-flight disable: don't act, don't retry.
    if !feature_enabled(state).await || paused(state).await {
        finish(state, run_id, "skipped", None, Some("workflows paused / feature disabled")).await?;
        return Ok(());
    }

    let r = sqlx::query!(
        r#"SELECT wr.trigger_event_ids,
                  wr.run_as_user_id,
                  w.action_type,
                  w.action_config,
                  w.project_id,
                  w.owner_id,
                  w.agent_id,
                  w.name
           FROM workflow_runs wr
           JOIN workflows w ON w.id = wr.workflow_id
           WHERE wr.id = $1"#,
        run_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("workflow run not found".into()))?;

    // Claim: only a queued run proceeds (idempotent against a double-deliver).
    let claimed = sqlx::query!(
        "UPDATE workflow_runs SET status = 'running', started_at = now() \
         WHERE id = $1 AND status = 'queued'",
        run_id
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    if claimed == 0 {
        return Ok(()); // already running/finished elsewhere
    }

    let events = load_events(state, &r.trigger_event_ids).await?;
    let result = match r.action_type.as_str() {
        "agent_run" => {
            run_agent_action(state, run_id, r.owner_id, r.project_id, r.agent_id, &r.action_config, &events).await
        }
        "system_action" => {
            run_system_action(state, run_id, r.owner_id, r.project_id, &r.action_config, &events).await
        }
        other => Err(AppError::Validation(format!("unknown action_type {other:?}"))),
    };

    match result {
        Ok(outcome) => {
            finish(state, run_id, "succeeded", Some(outcome.clone()), None).await?;
            audit_run(state, run_id, r.run_as_user_id, "workflow.run.succeeded", outcome).await;
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            finish(state, run_id, "failed", None, Some(&msg)).await?;
            audit_run(state, run_id, r.run_as_user_id, "workflow.run.failed", json!({ "error": msg })).await;
            Err(e) // surface to the task queue → retry / dead-letter
        }
    }
}

/// Run a deterministic `system_action` (no LLM). Returns the outcome JSON.
async fn run_system_action(
    state: &AppState,
    run_id: Uuid,
    owner: Uuid,
    project_id: Option<Uuid>,
    cfg: &Value,
    events: &[EventRow],
) -> Result<Value> {
    let kind = cfg.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let primary = events.first();
    match kind {
        "post_message" => {
            // Target: an explicit group chat, else the workflow's project chat.
            let target = match cfg
                .get("group_chat_id")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
            {
                Some(id) => id,
                None => {
                    let pid = project_id.ok_or_else(|| {
                        AppError::Validation(
                            "post_message needs group_chat_id or a project-scoped workflow".into(),
                        )
                    })?;
                    crate::http::messaging::ensure_project_chat(state, pid, owner).await?
                }
            };
            let template = cfg
                .get("template")
                .and_then(|v| v.as_str())
                .unwrap_or("A workflow was triggered.");
            let content = render_template(template, primary);
            crate::http::messaging::post_system_message(state, target, None, &content, None).await?;

            // Emit the child event so the cascade is real and depth-capped (lets
            // the loop guard be observed end-to-end). Its own tx (the message is
            // already committed; the event records that effect).
            if let Some(p) = primary {
                let mut tx = state.pg.begin().await?;
                let mut child =
                    events::child_event(events::WORKFLOW_MESSAGE_POSTED, p, run_id, Some(owner));
                child.payload = json!({ "group_chat_id": target.to_string() });
                events::emit_with(&mut tx, &child).await?;
                tx.commit().await?;
            }
            Ok(json!({ "kind": "post_message", "chat_id": target.to_string(), "content": content }))
        }
        "notify_owner" => {
            // A light cache-invalidation nudge to the owner's open views.
            state
                .hub
                .send_invalidate(&[owner], vec![vec!["workflow-runs".to_string()]]);
            Ok(json!({ "kind": "notify_owner", "owner": owner.to_string() }))
        }
        other => Err(AppError::Validation(format!(
            "unknown system_action kind {other:?}"
        ))),
    }
}

/// Run an `agent_run` action: the workflow's Agent, unattended, **as the
/// owner** — run-as-owner yields the owner∩scope intersection for free.
/// Mirrors `run_automation`: drive `chat::run_turn` headless and capture the output
/// chat id. Output always lands in a reviewable chat — never a silent destructive
/// change; a gated tool (e.g. artefact generation) still defers to the
/// owner's approval via the existing agent-run gate.
async fn run_agent_action(
    state: &AppState,
    _run_id: Uuid,
    owner: Uuid,
    project_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    cfg: &Value,
    events: &[EventRow],
) -> Result<Value> {
    use crate::ws::protocol::ServerFrame;

    let ctx = crate::auth::load_context(&state.pg, owner).await?;
    let template = cfg.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    if template.trim().is_empty() {
        return Err(AppError::Validation("agent_run needs a prompt in action_config".into()));
    }
    let prompt = render_agent_prompt(template, events);
    let kb_ids: Vec<Uuid> = cfg
        .get("kb_ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .filter_map(|s| Uuid::parse_str(s).ok())
                .collect()
        })
        .unwrap_or_default();

    // Drive run_turn headless (mirror run_automation): drain frames for the chat id.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ServerFrame>(64);
    let drain = tokio::spawn(async move {
        let mut chat_id: Option<Uuid> = None;
        let mut errored: Option<String> = None;
        while let Some(f) = rx.recv().await {
            match f {
                ServerFrame::ChatCreated { chat_id: c } => chat_id = Some(c),
                ServerFrame::ChatCompleted { chat_id: c, .. } => chat_id = Some(c),
                ServerFrame::ChatError { message, .. } => errored = Some(message),
                _ => {}
            }
        }
        (chat_id, errored)
    });

    let turn_id = Uuid::now_v7();
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    crate::chat::run_turn(
        state, &ctx, turn_id, None, project_id, agent_id, prompt, Vec::new(), kb_ids, true, None, None, None, &tx, cancel,
    )
    .await;
    drop(tx); // close the channel so the drain finishes
    let (chat_id, errored) = drain.await.unwrap_or((None, None));

    if let Some(msg) = errored {
        return Err(AppError::Other(anyhow::anyhow!("agent run failed: {msg}")));
    }
    let cid = chat_id.ok_or_else(|| AppError::Other(anyhow::anyhow!("agent run produced no chat")))?;

    // Optional in-platform delivery: post a link into a group chat (zero egress).
    if let Some(gid) = cfg
        .get("deliver_group_chat_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        if crate::http::messaging::is_member(state, owner, gid).await.unwrap_or(false) {
            let _ = crate::http::messaging::post_chat_link(
                state, gid, None, cid, "⚙ Workflow ran — read the output", true,
            )
            .await;
        }
    }
    Ok(json!({ "kind": "agent_run", "output_chat_id": cid.to_string() }))
}

/// Render the agent prompt: the user template (payload tokens) + a batch/context
/// preamble when coalesced (payload + resolved context, incl. the batch list).
fn render_agent_prompt(template: &str, events: &[EventRow]) -> String {
    let body = render_template(template, events.first());
    if events.len() > 1 {
        let items: Vec<String> = events
            .iter()
            .filter_map(|e| e.payload.get("filename").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        let list = if items.is_empty() { String::new() } else { format!(": {}", items.join(", ")) };
        format!("{body}\n\n[Triggered by a batch of {} events{}.]", events.len(), list)
    } else {
        body
    }
}

/// Substitute `{{field}}` / `{{payload.field}}` tokens from the trigger event's
/// payload. Unknown tokens render empty. Deterministic, no eval.
fn render_template(template: &str, primary: Option<&EventRow>) -> String {
    let payload = primary.map(|p| &p.payload);
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str("{{");
            rest = after;
            continue;
        };
        let key = after[..end].trim().trim_start_matches("payload.");
        let val = payload
            .and_then(|p| p.get(key))
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default();
        out.push_str(&val);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Load the trigger events for a run (ordered as created), as `EventRow`s.
async fn load_events(state: &AppState, ids: &[Uuid]) -> Result<Vec<EventRow>> {
    let rows = sqlx::query!(
        r#"SELECT id,
                  event_type,
                  actor_type AS "actor_type: ActorType",
                  actor_user_id,
                  resource_type,
                  resource_id,
                  project_id,
                  payload,
                  trigger_chain,
                  depth
           FROM events
           WHERE id = ANY($1)
           ORDER BY created_at"#,
        ids
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(rows
        .into_iter()
        .map(|e| EventRow {
            id: e.id,
            event_type: e.event_type,
            actor_type: e.actor_type,
            actor_user_id: e.actor_user_id,
            resource_type: e.resource_type,
            resource_id: e.resource_id,
            project_id: e.project_id,
            payload: e.payload,
            trigger_chain: e.trigger_chain,
            depth: e.depth,
        })
        .collect())
}

/// Terminal status write for a run.
async fn finish(
    state: &AppState,
    run_id: Uuid,
    status: &str,
    outcome: Option<Value>,
    error: Option<&str>,
) -> Result<()> {
    sqlx::query!(
        "UPDATE workflow_runs \
         SET status = ($2::text)::workflow_run_status, finished_at = now(), outcome = $3, error = $4 \
         WHERE id = $1",
        run_id,
        status,
        outcome,
        error,
    )
    .execute(&state.pg)
    .await?;
    Ok(())
}

/// Audit a workflow run into the hash-chain (trajectory).
async fn audit_run(
    state: &AppState,
    run_id: Uuid,
    actor: Option<Uuid>,
    action: &str,
    payload: Value,
) {
    let mut ev = crate::audit::AuditEvent::action(action, "workflow");
    ev.actor_user_id = actor;
    ev.resource_type = Some("workflow_run".into());
    ev.resource_id = Some(run_id);
    ev.payload = Some(payload);
    let _ = crate::audit::append(&state.pg, &ev).await;
}

/// `workflows.max_depth` (runtime-mutable), defaulting to [`DEFAULT_MAX_DEPTH`].
async fn read_max_depth(state: &AppState) -> i32 {
    crate::config::runtime::get(&state.pg, "workflows.max_depth")
        .await
        .ok()
        .flatten()
        .and_then(|e| e.value.parse::<i32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MAX_DEPTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_event_fires() {
        assert_eq!(guard_decision(ActorType::Human, false, 0, 3), Decision::Fire);
    }

    #[test]
    fn system_event_skipped_by_default() {
        // A workflow/agent/system-caused event triggers nothing unless opted in.
        assert_eq!(
            guard_decision(ActorType::Workflow, false, 0, 3),
            Decision::Skip("system-originated event; workflow is human-only")
        );
        assert_eq!(
            guard_decision(ActorType::System, false, 0, 3),
            Decision::Skip("system-originated event; workflow is human-only")
        );
    }

    #[test]
    fn system_event_fires_when_opted_in() {
        assert_eq!(guard_decision(ActorType::Workflow, true, 0, 3), Decision::Fire);
    }

    #[test]
    fn depth_cap_breaks_opted_in_cascade() {
        // An opted-in chain halts at the depth cap.
        assert_eq!(
            guard_decision(ActorType::Workflow, true, 3, 3),
            Decision::Skip("max workflow depth reached")
        );
        assert_eq!(
            guard_decision(ActorType::Human, true, 5, 3),
            Decision::Skip("max workflow depth reached")
        );
        assert_eq!(guard_decision(ActorType::Human, true, 2, 3), Decision::Fire);
    }

    #[test]
    fn condition_null_is_true() {
        assert!(eval_condition(&Value::Null, &json!({})));
    }

    #[test]
    fn condition_eq_and_in_over_payload() {
        let ctx = json!({ "payload": { "mime": "application/pdf", "pages": 60 } });
        assert!(eval_condition(
            &json!({ "field": "payload.mime", "op": "eq", "value": "application/pdf" }),
            &ctx
        ));
        assert!(!eval_condition(
            &json!({ "field": "payload.mime", "op": "eq", "value": "text/plain" }),
            &ctx
        ));
        assert!(eval_condition(
            &json!({ "field": "payload.pages", "op": "gt", "value": 50 }),
            &ctx
        ));
        assert!(eval_condition(
            &json!({ "field": "payload.mime", "op": "in",
                     "value": ["application/pdf", "text/plain"] }),
            &ctx
        ));
    }

    #[test]
    fn condition_all_any_combine() {
        let ctx = json!({ "payload": { "mime": "application/pdf", "pages": 10 } });
        let cond = json!({ "all": [
            { "field": "payload.mime", "op": "eq", "value": "application/pdf" },
            { "any": [
                { "field": "payload.pages", "op": "gt", "value": 100 },
                { "field": "payload.pages", "op": "lt", "value": 20 },
            ]}
        ]});
        assert!(eval_condition(&cond, &ctx));
    }

    #[test]
    fn condition_unknown_op_fails_closed() {
        let ctx = json!({ "payload": { "x": 1 } });
        assert!(!eval_condition(
            &json!({ "field": "payload.x", "op": "regex", "value": ".*" }),
            &ctx
        ));
    }

    #[test]
    fn render_substitutes_payload_tokens() {
        let ev = EventRow {
            id: Uuid::now_v7(),
            event_type: "document.ingested".into(),
            actor_type: ActorType::Human,
            actor_user_id: None,
            resource_type: None,
            resource_id: None,
            project_id: None,
            payload: json!({ "filename": "brief.pdf" }),
            trigger_chain: vec![],
            depth: 0,
        };
        assert_eq!(
            render_template("Ingested {{filename}} ✓", Some(&ev)),
            "Ingested brief.pdf ✓"
        );
        assert_eq!(
            render_template("Ingested {{payload.filename}}", Some(&ev)),
            "Ingested brief.pdf"
        );
        // Unknown token renders empty; no panic on a dangling brace.
        assert_eq!(render_template("a {{missing}} b", Some(&ev)), "a  b");
    }

    #[test]
    fn rate_cap_predicate() {
        // max=1: the first run (recent=0) fires; the second (recent=1) is over.
        assert!(!over_rate(0, 1));
        assert!(over_rate(1, 1));
        assert!(over_rate(60, 60));
        assert!(!over_rate(59, 60));
        // 0/negative disables the cap.
        assert!(!over_rate(1000, 0));
        assert!(!over_rate(1000, -1));
    }

    fn ev_named(file: &str) -> EventRow {
        EventRow {
            id: Uuid::now_v7(),
            event_type: "document.ingested".into(),
            actor_type: ActorType::Human,
            actor_user_id: None,
            resource_type: None,
            resource_id: None,
            project_id: None,
            payload: json!({ "filename": file }),
            trigger_chain: vec![],
            depth: 0,
        }
    }

    #[test]
    fn agent_prompt_single_vs_batch() {
        let one = [ev_named("a.pdf")];
        assert_eq!(render_agent_prompt("Summarise {{filename}}", &one), "Summarise a.pdf");

        let many = [ev_named("a.pdf"), ev_named("b.pdf"), ev_named("c.pdf")];
        let out = render_agent_prompt("Summarise {{filename}}", &many);
        assert!(out.starts_with("Summarise a.pdf"), "primary token still rendered");
        assert!(out.contains("batch of 3 events"), "batch preamble appended");
        assert!(out.contains("a.pdf, b.pdf, c.pdf"), "batch list included");
    }
}
