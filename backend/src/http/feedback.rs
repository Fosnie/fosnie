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

//! Message feedback: thumbs up/down + optional comment on
//! assistant answers, tied to the Agent + model that produced them. Local only.
//! Primary use is per-Agent analytics for a power-user/admin to correct the
//! Agent by hand — never auto-correction.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::permissions;
use crate::auth::{AuthContext, PlatformRole};
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Resolve feedback access for an assistant message: it must exist, be an
/// `assistant` message, and the caller must own its chat (or be an admin).
/// Returns the chat's `agent_id` (the feedback context).
async fn require_message_access(
    state: &AppState,
    ctx: &AuthContext,
    message_id: Uuid,
) -> Result<Option<Uuid>> {
    let row = sqlx::query!(
        r#"SELECT m.role::text AS "role!", c.owner_user_id, c.agent_id, c.project_id
           FROM messages m JOIN chats c ON c.id = m.chat_id
           WHERE m.id = $1"#,
        message_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("message not found".into()))?;
    if row.role != "assistant" {
        return Err(AppError::Validation("feedback is only for assistant messages".into()));
    }
    // Owner or admin always; otherwise a project member with read access on the
    // chat's project may rate (project-shared-chat widening).
    let allowed = ctx.user_id == Some(row.owner_user_id)
        || ctx.is_admin()
        || match row.project_id {
            Some(pid) => {
                state.rbac.can(&state.pg, ctx, crate::auth::rbac::ResourceType::Project, pid, crate::auth::rbac::Permission::Read).await?
            }
            None => false,
        };
    if !allowed {
        return Err(AppError::Forbidden("not permitted to rate this message".into()));
    }
    Ok(row.agent_id)
}

/// Best-effort model name from the `chat.assistant.completed` audit row.
async fn model_for(state: &AppState, message_id: Uuid) -> Option<String> {
    sqlx::query_scalar!(
        r#"SELECT model_agent_traceability->>'model' AS model
           FROM audit_events
           WHERE action_type = 'chat.assistant.completed' AND payload->>'message_id' = $1
           ORDER BY seq DESC LIMIT 1"#,
        message_id.to_string()
    )
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten()
    .flatten()
}

#[derive(Deserialize)]
pub struct SubmitFeedback {
    pub rating: String, // up | down
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Serialize)]
pub struct FeedbackOut {
    pub id: Uuid,
    pub rating: String,
    pub comment: Option<String>,
}

pub async fn submit_feedback(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(message_id): Path<Uuid>,
    Json(body): Json<SubmitFeedback>,
) -> Result<Json<FeedbackOut>> {
    if !matches!(body.rating.as_str(), "up" | "down") {
        return Err(AppError::Validation("rating must be up|down".into()));
    }
    let agent_id = require_message_access(&state, &ctx, message_id).await?;
    let model = model_for(&state, message_id).await;
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;

    // One rating per (message, user) — re-submitting updates it.
    let id: Uuid = sqlx::query_scalar!(
        "INSERT INTO feedback (id, message_id, user_id, rating, comment, agent_id, model) \
         VALUES ($1, $2, $3, ($4::text)::feedback_rating, $5, $6, $7) \
         ON CONFLICT (message_id, user_id) DO UPDATE \
           SET rating = EXCLUDED.rating, \
               comment = COALESCE(EXCLUDED.comment, feedback.comment), \
               agent_id = EXCLUDED.agent_id, model = EXCLUDED.model, updated_at = now() \
         RETURNING id",
        db::new_id(), message_id, uid, body.rating, body.comment, agent_id, model,
    )
    .fetch_one(&state.pg)
    .await?;

    let mut ev = AuditEvent::action("feedback.submitted", ctx.role.as_str());
    ev.actor_user_id = Some(uid);
    ev.resource_type = Some("message".into());
    ev.resource_id = Some(message_id);
    ev.payload = Some(serde_json::json!({ "rating": body.rating, "agent_id": agent_id }));
    let _ = audit::append(&state.pg, &ev).await;

    Ok(Json(FeedbackOut { id, rating: body.rating, comment: body.comment }))
}

#[derive(Serialize)]
pub struct MessageFeedback {
    pub mine: Option<FeedbackOut>,
    pub up: i64,
    pub down: i64,
}

pub async fn get_feedback(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(message_id): Path<Uuid>,
) -> Result<Json<MessageFeedback>> {
    require_message_access(&state, &ctx, message_id).await?;
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let mine = sqlx::query!(
        r#"SELECT id, rating::text AS "rating!", comment FROM feedback WHERE message_id = $1 AND user_id = $2"#,
        message_id, uid
    )
    .fetch_optional(&state.pg)
    .await?
    .map(|r| FeedbackOut { id: r.id, rating: r.rating, comment: r.comment });
    let counts = sqlx::query!(
        r#"SELECT
             count(*) FILTER (WHERE rating = 'up') AS "up!",
             count(*) FILTER (WHERE rating = 'down') AS "down!"
           FROM feedback WHERE message_id = $1"#,
        message_id
    )
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(MessageFeedback { mine, up: counts.up, down: counts.down }))
}

pub async fn delete_feedback(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(message_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    // RETURNING so a real deletion is auditable; an idempotent no-op stays silent.
    let removed = sqlx::query!(
        r#"DELETE FROM feedback WHERE message_id = $1 AND user_id = $2
           RETURNING rating::text AS "rating!", agent_id"#,
        message_id, uid
    )
    .fetch_optional(&state.pg)
    .await?;

    if let Some(row) = removed {
        let mut ev = AuditEvent::action("feedback.deleted", ctx.role.as_str());
        ev.actor_user_id = Some(uid);
        ev.resource_type = Some("message".into());
        ev.resource_id = Some(message_id);
        ev.payload = Some(serde_json::json!({ "rating": row.rating, "agent_id": row.agent_id }));
        let _ = audit::append(&state.pg, &ev).await;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize)]
pub struct NegativeOut {
    pub message_id: Uuid,
    pub comment: Option<String>,
}

#[derive(Serialize)]
pub struct AgentSummary {
    pub up: i64,
    pub down: i64,
    pub total: i64,
    pub recent_negative: Vec<NegativeOut>,
}

/// Per-Agent feedback analytics (power-user/admin): the human-correction signal.
pub async fn agent_summary(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(agent_id): Path<Uuid>,
) -> Result<Json<AgentSummary>> {
    if !matches!(
        ctx.role,
        PlatformRole::PowerUser | PlatformRole::ClientAdmin | PlatformRole::SuperAdmin
    ) {
        return Err(AppError::Forbidden("feedback analytics are power-user/admin only".into()));
    }
    let counts = sqlx::query!(
        r#"SELECT
             count(*) FILTER (WHERE rating = 'up') AS "up!",
             count(*) FILTER (WHERE rating = 'down') AS "down!",
             count(*) AS "total!"
           FROM feedback WHERE agent_id = $1"#,
        agent_id
    )
    .fetch_one(&state.pg)
    .await?;
    let recent_negative = sqlx::query!(
        "SELECT message_id, comment FROM feedback \
         WHERE agent_id = $1 AND rating = 'down' ORDER BY updated_at DESC LIMIT 20",
        agent_id
    )
    .fetch_all(&state.pg)
    .await?
    .into_iter()
    .map(|r| NegativeOut { message_id: r.message_id, comment: r.comment })
    .collect();
    Ok(Json(AgentSummary { up: counts.up, down: counts.down, total: counts.total, recent_negative }))
}

#[derive(Deserialize)]
pub struct FeedbackListQuery {
    /// Optional filter: `up` | `down`.
    #[serde(default)]
    pub rating: Option<String>,
}

#[derive(Serialize)]
pub struct AdminFeedbackItem {
    pub id: Uuid,
    pub rating: String,
    pub comment: Option<String>,
    pub user_email: Option<String>,
    pub agent_name: Option<String>,
    pub model: Option<String>,
    pub message_excerpt: String,
    pub created_at: String,
}

/// Admin feedback triage: every submitted rating + comment, newest first, with the
/// rater, the Agent, and a short excerpt of the rated answer. Admin-only.
pub async fn list_feedback(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<FeedbackListQuery>,
) -> Result<Json<Vec<AdminFeedbackItem>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::FEEDBACK_VIEW).await?;
    let rating = match q.rating.as_deref() {
        Some("up") => Some("up"),
        Some("down") => Some("down"),
        _ => None,
    };
    let rows = sqlx::query!(
        r#"SELECT f.id, f.rating::text AS "rating!", f.comment, f.model, f.created_at,
                  u.email AS "user_email?", ag.name AS "agent_name?",
                  left(m.content, 200) AS "excerpt?"
           FROM feedback f
           JOIN messages m ON m.id = f.message_id
           LEFT JOIN users u ON u.id = f.user_id
           LEFT JOIN agents ag ON ag.id = f.agent_id
           WHERE ($1::text IS NULL OR f.rating::text = $1)
           ORDER BY f.created_at DESC LIMIT 200"#,
        rating
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| AdminFeedbackItem {
                id: r.id,
                rating: r.rating,
                comment: r.comment,
                user_email: r.user_email,
                agent_name: r.agent_name,
                model: r.model,
                message_excerpt: r.excerpt.unwrap_or_default(),
                created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
            })
            .collect(),
    ))
}
