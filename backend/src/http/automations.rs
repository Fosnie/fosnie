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

//! Automations REST: cron-scheduled AI runs + a
//! calendar view onto upcoming occurrences. Scheduling/execution live on the
//! background scheduler; this is CRUD + run-on-demand + history + calendar.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::automations as sched;
use crate::config::runtime;
use crate::db;
use crate::error::{AppError, Result};
use crate::scheduler::{self, TaskType};
use crate::state::AppState;

fn rfc3339(t: Option<OffsetDateTime>) -> Option<String> {
    t.and_then(|t| t.format(&Rfc3339).ok())
}

/// Owner or admin may manage an automation; returns the owner.
async fn require_owner(state: &AppState, ctx: &AuthContext, id: Uuid) -> Result<Uuid> {
    let owner: Option<Uuid> =
        sqlx::query_scalar!("SELECT owner_user_id FROM automations WHERE id = $1", id)
            .fetch_optional(&state.pg)
            .await?;
    let owner = owner.ok_or_else(|| AppError::Validation("automation not found".into()))?;
    if ctx.user_id == Some(owner) || ctx.is_admin() {
        Ok(owner)
    } else {
        Err(AppError::Forbidden("not your automation".into()))
    }
}

/// Admin-tunable guardrails (runtime config, with safe fallbacks): how many
/// automations a user may own, and the minimum gap between consecutive runs.
async fn caps(pg: &sqlx::PgPool) -> (i64, i64) {
    let read = |key: &'static str, default: i64| async move {
        runtime::get(pg, key)
            .await
            .ok()
            .flatten()
            .and_then(|e| e.value.parse::<i64>().ok())
            .unwrap_or(default)
    };
    (read("automation.max_per_user", 50).await, read("automation.min_interval_secs", 300).await)
}

/// Does the schedule fire more often than `min_secs`? Uses the gap between the
/// next two occurrences; a schedule with fewer than two future occurrences
/// (e.g. a one-off date) is never "too frequent".
fn schedule_too_frequent(schedule: &str, min_secs: i64) -> Result<bool> {
    let now = OffsetDateTime::now_utc();
    let Some(t1) = sched::next_after(schedule, now)? else { return Ok(false) };
    let Some(t2) = sched::next_after(schedule, t1)? else { return Ok(false) };
    Ok((t2 - t1).whole_seconds() < min_secs)
}

#[derive(Deserialize)]
pub struct CreateAutomation {
    pub name: String,
    pub schedule: String, // cron expression
    pub prompt: String,
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    /// Output chat lands under this Project (inherits its sector). None = personal.
    #[serde(default)]
    pub project_id: Option<Uuid>,
    /// Libraries attached at run time (intersection allow-list still applies).
    #[serde(default)]
    pub kb_ids: Vec<Uuid>,
    /// Internal group chat to post a result notice into on success (zero egress).
    #[serde(default)]
    pub deliver_group_chat_id: Option<Uuid>,
}

/// Validate optional targets before persisting — friendly errors; retrieval and
/// delivery stay fail-closed regardless. The caller must be a member of any
/// delivery chat and able to read every attached Library.
async fn validate_targets(
    state: &AppState,
    owner: Uuid,
    ctx: &AuthContext,
    kb_ids: &[Uuid],
    deliver: Option<Uuid>,
) -> Result<()> {
    if let Some(chat_id) = deliver {
        if !crate::http::messaging::is_member(state, owner, chat_id).await? {
            return Err(AppError::Validation(
                "you are not a member of the selected delivery chat".into(),
            ));
        }
    }
    for kb_id in kb_ids {
        crate::kb::require_read(&state.pg, ctx, *kb_id).await?;
    }
    Ok(())
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

pub async fn create_automation(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateAutomation>,
) -> Result<Json<CreatedId>> {
    let owner = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    if !crate::cache::rate_limit_ok(&state.redis, &format!("automation:{owner}"), 20, 60).await {
        return Err(AppError::TooManyRequests("automation rate limit; try again shortly".into()));
    }
    sched::validate(&body.schedule)?;
    if body.prompt.trim().is_empty() {
        return Err(AppError::Validation("prompt must not be empty".into()));
    }
    // Per-user guardrails.
    let (max, min_secs) = caps(&state.pg).await;
    let count = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM automations WHERE owner_user_id = $1"#,
        owner
    )
    .fetch_one(&state.pg)
    .await?;
    if count >= max {
        return Err(AppError::Validation(format!("automation limit reached ({max})")));
    }
    if schedule_too_frequent(&body.schedule, min_secs)? {
        return Err(AppError::Validation(format!(
            "schedule too frequent; minimum interval is {min_secs}s"
        )));
    }
    validate_targets(&state, owner, &ctx, &body.kb_ids, body.deliver_group_chat_id).await?;
    let next = sched::next_after(&body.schedule, OffsetDateTime::now_utc())?;
    let id = db::new_id();
    sqlx::query!(
        "INSERT INTO automations \
            (id, owner_user_id, name, schedule, prompt, agent_id, next_run_at, \
             project_id, kb_ids, deliver_group_chat_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        id, owner, body.name, body.schedule, body.prompt, body.agent_id, next,
        body.project_id, &body.kb_ids, body.deliver_group_chat_id,
    )
    .execute(&state.pg)
    .await?;
    audit_automation(&state, &ctx, "automation.created", id).await;
    Ok(Json(CreatedId { id }))
}

#[derive(Serialize)]
pub struct AutomationOut {
    pub id: Uuid,
    pub name: String,
    pub schedule: String,
    pub prompt: String,
    pub agent_id: Option<Uuid>,
    pub status: String,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub project_id: Option<Uuid>,
    pub kb_ids: Vec<Uuid>,
    pub deliver_group_chat_id: Option<Uuid>,
}

pub async fn list_automations(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<AutomationOut>>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let rows = sqlx::query!(
        r#"SELECT id, name, schedule, prompt, agent_id, status::text AS "status!", next_run_at, last_run_at,
                  project_id, kb_ids, deliver_group_chat_id
           FROM automations WHERE owner_user_id = $1 ORDER BY created_at DESC"#,
        uid
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(rows.into_iter().map(|r| AutomationOut {
        id: r.id, name: r.name, schedule: r.schedule, prompt: r.prompt, agent_id: r.agent_id,
        status: r.status, next_run_at: rfc3339(r.next_run_at), last_run_at: rfc3339(r.last_run_at),
        project_id: r.project_id, kb_ids: r.kb_ids, deliver_group_chat_id: r.deliver_group_chat_id,
    }).collect()))
}

pub async fn get_automation(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<AutomationOut>> {
    require_owner(&state, &ctx, id).await?;
    let r = sqlx::query!(
        r#"SELECT id, name, schedule, prompt, agent_id, status::text AS "status!", next_run_at, last_run_at,
                  project_id, kb_ids, deliver_group_chat_id
           FROM automations WHERE id = $1"#,
        id
    )
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(AutomationOut {
        id: r.id, name: r.name, schedule: r.schedule, prompt: r.prompt, agent_id: r.agent_id,
        status: r.status, next_run_at: rfc3339(r.next_run_at), last_run_at: rfc3339(r.last_run_at),
        project_id: r.project_id, kb_ids: r.kb_ids, deliver_group_chat_id: r.deliver_group_chat_id,
    }))
}

#[derive(Deserialize)]
pub struct UpdateAutomation {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub status: Option<String>, // active | paused
    // Targets — double-option / option semantics so a partial PATCH (e.g. the
    // status toggle) never wipes them: field absent = unchanged, present `null`
    // = clear, present value = set. `kb_ids` clears with `[]`.
    #[serde(default, with = "serde_with::rust::double_option")]
    pub project_id: Option<Option<Uuid>>,
    #[serde(default)]
    pub kb_ids: Option<Vec<Uuid>>,
    #[serde(default, with = "serde_with::rust::double_option")]
    pub deliver_group_chat_id: Option<Option<Uuid>>,
}

pub async fn update_automation(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateAutomation>,
) -> Result<Json<serde_json::Value>> {
    let owner = require_owner(&state, &ctx, id).await?;
    if let Some(s) = &body.status {
        if !matches!(s.as_str(), "active" | "paused") {
            return Err(AppError::Validation("status must be active|paused".into()));
        }
    }
    // Validate any targets being set (skip on clear / unchanged).
    let kb_to_check = body.kb_ids.as_deref().unwrap_or(&[]);
    let deliver_to_check = body.deliver_group_chat_id.flatten();
    if !kb_to_check.is_empty() || deliver_to_check.is_some() {
        validate_targets(&state, owner, &ctx, kb_to_check, deliver_to_check).await?;
    }
    // Recompute next_run_at when the schedule changes.
    let next = match &body.schedule {
        Some(s) => {
            sched::validate(s)?;
            let (_, min_secs) = caps(&state.pg).await;
            if schedule_too_frequent(s, min_secs)? {
                return Err(AppError::Validation(format!(
                    "schedule too frequent; minimum interval is {min_secs}s"
                )));
            }
            Some(sched::next_after(s, OffsetDateTime::now_utc())?)
        }
        None => None,
    };
    // Double-option → (set?, value) pairs for the SQL CASE guards.
    let set_project = body.project_id.is_some();
    let project_val = body.project_id.flatten();
    let set_deliver = body.deliver_group_chat_id.is_some();
    let deliver_val = body.deliver_group_chat_id.flatten();
    sqlx::query!(
        "UPDATE automations SET \
            name = COALESCE($2, name), \
            schedule = COALESCE($3, schedule), \
            prompt = COALESCE($4, prompt), \
            status = COALESCE(($5::text)::automation_status, status), \
            next_run_at = CASE WHEN $3 IS NULL THEN next_run_at ELSE $6 END, \
            project_id = CASE WHEN $7 THEN $8 ELSE project_id END, \
            kb_ids = COALESCE($9::uuid[], kb_ids), \
            deliver_group_chat_id = CASE WHEN $10 THEN $11 ELSE deliver_group_chat_id END \
         WHERE id = $1",
        id, body.name, body.schedule, body.prompt, body.status, next.flatten(),
        set_project, project_val, body.kb_ids.as_deref(), set_deliver, deliver_val,
    )
    .execute(&state.pg)
    .await?;
    audit_automation(&state, &ctx, "automation.updated", id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn delete_automation(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_owner(&state, &ctx, id).await?;
    sqlx::query!("DELETE FROM automations WHERE id = $1", id).execute(&state.pg).await?;
    audit_automation(&state, &ctx, "automation.deleted", id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Run on demand: enqueue the durable task immediately (the scheduler executes it).
pub async fn run_now(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_owner(&state, &ctx, id).await?;
    scheduler::enqueue(&state.pg, TaskType::AutomationRun, serde_json::json!({ "automation_id": id }))
        .await
        .map_err(AppError::from)?;
    Ok(Json(serde_json::json!({ "status": "queued" })))
}

#[derive(Serialize)]
pub struct RunOut {
    pub id: Uuid,
    pub status: String,
    pub output_chat_id: Option<Uuid>,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

pub async fn list_runs(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<RunOut>>> {
    require_owner(&state, &ctx, id).await?;
    let rows = sqlx::query!(
        r#"SELECT id, status::text AS "status!", output_chat_id, error, started_at, completed_at
           FROM automation_runs WHERE automation_id = $1 ORDER BY started_at DESC LIMIT 100"#,
        id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(rows.into_iter().map(|r| RunOut {
        id: r.id, status: r.status, output_chat_id: r.output_chat_id, error: r.error,
        started_at: rfc3339(Some(r.started_at)), completed_at: rfc3339(r.completed_at),
    }).collect()))
}

#[derive(Deserialize)]
pub struct CalendarQuery {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

#[derive(Serialize)]
pub struct CalendarEntry {
    pub automation_id: Uuid,
    pub name: String,
    pub at: String,
}

/// Upcoming occurrences across the caller's active automations within a window
/// (the calendar is a view onto automations).
pub async fn calendar(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<CalendarQuery>,
) -> Result<Json<Vec<CalendarEntry>>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let now = OffsetDateTime::now_utc();
    let from = q.from.and_then(|s| OffsetDateTime::parse(&s, &Rfc3339).ok()).unwrap_or(now);
    let to = q
        .to
        .and_then(|s| OffsetDateTime::parse(&s, &Rfc3339).ok())
        .unwrap_or(from + Duration::days(7));

    let rows = sqlx::query!(
        "SELECT id, name, schedule FROM automations WHERE owner_user_id = $1 AND status = 'active'",
        uid
    )
    .fetch_all(&state.pg)
    .await?;
    let mut out = Vec::new();
    for a in rows {
        for at in sched::occurrences_between(&a.schedule, from, to, 100)? {
            if let Ok(s) = at.format(&Rfc3339) {
                out.push(CalendarEntry { automation_id: a.id, name: a.name.clone(), at: s });
            }
        }
    }
    out.sort_by(|x, y| x.at.cmp(&y.at));
    Ok(Json(out))
}

async fn audit_automation(state: &AppState, ctx: &AuthContext, action: &str, id: Uuid) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("automation".into());
    ev.resource_id = Some(id);
    let _ = audit::append(&state.pg, &ev).await;
}
