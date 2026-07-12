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

//! Admin notices: announcement banners + the login welcome message.
//! Banners are an admin-managed ordered list in the
//! `announcements` table, shown to all users in every section. The welcome
//! message is a `config_settings` singleton (`welcome.enabled|title|body`)
//! written through `config::runtime` so it inherits validation + the atomic
//! `config.changed` audit row.
//!
//! All admin mutations are audited (the `branding.updated` pattern) and broadcast
//! a `["notices"]` cache-invalidation to every connected socket so open clients
//! refresh without a reload. The read endpoint `GET /api/notices` is open to any
//! authenticated user. Content is markdown, rendered escaped client-side.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::config::runtime::{self, ConfigValueType};
use crate::db;
use crate::auth::permissions;
use crate::error::{AppError, Result};
use crate::state::AppState;

const SEVERITIES: [&str; 4] = ["info", "success", "warning", "error"];
const CONTENT_MAX: usize = 1000;
const WELCOME_TITLE_MAX: usize = 200;
const WELCOME_BODY_MAX: usize = 4000;

/// Trim, then reject empty / over-length / disallowed control chars. Markdown is
/// fine; only raw control characters (other than newline/tab) are rejected. This
/// is the free-text analogue of the `safe_colour`/`safe_font` output boundary.
fn validate_text(s: &str, max: usize, field: &str) -> Result<String> {
    let v = s.trim();
    if v.is_empty() {
        return Err(AppError::Validation(format!("{field} must not be empty")));
    }
    if v.chars().count() > max {
        return Err(AppError::Validation(format!("{field} must be ≤ {max} characters")));
    }
    if v.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(AppError::Validation(format!("{field} contains control characters")));
    }
    Ok(v.to_string())
}

fn validate_severity(s: &str) -> Result<String> {
    if SEVERITIES.contains(&s) {
        Ok(s.to_string())
    } else {
        Err(AppError::Validation("severity must be info|success|warning|error".into()))
    }
}

// --- DTOs --------------------------------------------------------------------

#[derive(Serialize)]
pub struct AnnouncementOut {
    pub id: Uuid,
    pub content: String,
    pub severity: String,
    pub dismissible: bool,
    pub active: bool,
    pub sort_order: i32,
}

#[derive(Serialize, Default)]
pub struct WelcomeOut {
    pub enabled: bool,
    pub title: String,
    pub body: String,
}

#[derive(Serialize)]
pub struct NoticesOut {
    pub banners: Vec<AnnouncementOut>,
    pub welcome: Option<WelcomeOut>,
}

#[derive(Deserialize)]
pub struct CreateAnnouncement {
    pub content: String,
    #[serde(default = "default_severity")]
    pub severity: String,
    #[serde(default = "default_true")]
    pub dismissible: bool,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default)]
    pub sort_order: i32,
}

#[derive(Deserialize)]
pub struct UpdateAnnouncement {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub dismissible: Option<bool>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub sort_order: Option<i32>,
}

#[derive(Deserialize)]
pub struct SetWelcome {
    pub enabled: bool,
    pub title: String,
    pub body: String,
}

fn default_severity() -> String {
    "info".into()
}
fn default_true() -> bool {
    true
}

// --- Read (any authenticated user) -------------------------------------------

/// Read the current welcome singleton from `config_settings`. Defaults to a
/// disabled, empty message when keys are absent.
async fn read_welcome(state: &AppState) -> WelcomeOut {
    async fn read(state: &AppState, key: &str) -> Option<String> {
        runtime::get(&state.pg, key).await.ok().flatten().map(|e| e.value)
    }
    WelcomeOut {
        enabled: read(state, "welcome.enabled").await.as_deref() == Some("true"),
        title: read(state, "welcome.title").await.unwrap_or_default(),
        body: read(state, "welcome.body").await.unwrap_or_default(),
    }
}

/// Active banners + the welcome message (only when enabled + non-empty). One
/// fetch the SPA makes on mount, refreshed by the broadcast on any admin change.
pub async fn get_notices(
    State(state): State<AppState>,
    AuthUser(_ctx): AuthUser,
) -> Result<Json<NoticesOut>> {
    let banners = sqlx::query_as!(
        AnnouncementOut,
        r#"SELECT id, content, severity, dismissible, active, sort_order
           FROM announcements WHERE active ORDER BY sort_order, created_at"#
    )
    .fetch_all(&state.pg)
    .await?;

    let w = read_welcome(&state).await;
    let welcome = (w.enabled && !w.title.trim().is_empty() && !w.body.trim().is_empty()).then_some(w);

    Ok(Json(NoticesOut { banners, welcome }))
}

// --- Admin: announcements CRUD -----------------------------------------------

pub async fn list_announcements(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<AnnouncementOut>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::ANNOUNCEMENTS_MANAGE).await?;
    let rows = sqlx::query_as!(
        AnnouncementOut,
        r#"SELECT id, content, severity, dismissible, active, sort_order
           FROM announcements ORDER BY sort_order, created_at"#
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(rows))
}

pub async fn create_announcement(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateAnnouncement>,
) -> Result<Json<AnnouncementOut>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::ANNOUNCEMENTS_MANAGE).await?;
    let content = validate_text(&body.content, CONTENT_MAX, "content")?;
    let severity = validate_severity(&body.severity)?;
    let id = db::new_id();
    let row = sqlx::query_as!(
        AnnouncementOut,
        r#"INSERT INTO announcements (id, content, severity, dismissible, active, sort_order, created_by)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           RETURNING id, content, severity, dismissible, active, sort_order"#,
        id, content, severity, body.dismissible, body.active, body.sort_order, ctx.user_id,
    )
    .fetch_one(&state.pg)
    .await?;

    audit_announcement(&state, &ctx, "announcement.created", id, &severity).await;
    state.hub.broadcast_invalidate(vec![vec!["notices".into()]]);
    Ok(Json(row))
}

pub async fn update_announcement(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateAnnouncement>,
) -> Result<Json<AnnouncementOut>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::ANNOUNCEMENTS_MANAGE).await?;
    let content = body.content.as_deref().map(|c| validate_text(c, CONTENT_MAX, "content")).transpose()?;
    let severity = body.severity.as_deref().map(validate_severity).transpose()?;
    // COALESCE keeps unspecified fields untouched; `updated_at` always bumps.
    let row = sqlx::query_as!(
        AnnouncementOut,
        r#"UPDATE announcements SET
               content     = COALESCE($2, content),
               severity    = COALESCE($3, severity),
               dismissible = COALESCE($4, dismissible),
               active      = COALESCE($5, active),
               sort_order  = COALESCE($6, sort_order),
               updated_at  = now()
           WHERE id = $1
           RETURNING id, content, severity, dismissible, active, sort_order"#,
        id, content, severity, body.dismissible, body.active, body.sort_order,
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("no announcement with that id".into()))?;

    audit_announcement(&state, &ctx, "announcement.updated", id, &row.severity).await;
    state.hub.broadcast_invalidate(vec![vec!["notices".into()]]);
    Ok(Json(row))
}

pub async fn delete_announcement(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::ANNOUNCEMENTS_MANAGE).await?;
    let done = sqlx::query!("DELETE FROM announcements WHERE id = $1", id)
        .execute(&state.pg)
        .await?
        .rows_affected();
    if done == 0 {
        return Err(AppError::Validation("no announcement with that id".into()));
    }
    audit_announcement(&state, &ctx, "announcement.deleted", id, "").await;
    state.hub.broadcast_invalidate(vec![vec!["notices".into()]]);
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Admin: welcome singleton ------------------------------------------------

/// The welcome message as stored (returned even when disabled, to seed the admin
/// form). Disabled/empty welcome text never rides the public `/api/notices`.
pub async fn get_welcome(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<WelcomeOut>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::ANNOUNCEMENTS_MANAGE).await?;
    Ok(Json(read_welcome(&state).await))
}

pub async fn set_welcome(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<SetWelcome>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::ANNOUNCEMENTS_MANAGE).await?;
    // When enabling, title + body must be present; when disabling, accept empties
    // so an admin can flip it off without re-filling the text.
    let (title, this_body) = if body.enabled {
        (
            validate_text(&body.title, WELCOME_TITLE_MAX, "title")?,
            validate_text(&body.body, WELCOME_BODY_MAX, "body")?,
        )
    } else {
        (body.title.trim().to_string(), body.body.trim().to_string())
    };

    let role = ctx.role.as_str();
    // Each write validates + audits `config.changed` atomically (config::runtime).
    runtime::set(&state.pg, "welcome.enabled", if body.enabled { "true" } else { "false" }, ConfigValueType::Bool, "global", ctx.user_id, role).await?;
    runtime::set(&state.pg, "welcome.title", &title, ConfigValueType::String, "global", ctx.user_id, role).await?;
    runtime::set(&state.pg, "welcome.body", &this_body, ConfigValueType::String, "global", ctx.user_id, role).await?;

    state.hub.broadcast_invalidate(vec![vec!["notices".into()]]);
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- audit -------------------------------------------------------------------

async fn audit_announcement(state: &AppState, ctx: &crate::auth::AuthContext, action: &str, id: Uuid, severity: &str) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("announcement".into());
    ev.resource_id = Some(id);
    ev.payload = Some(serde_json::json!({ "severity": severity }));
    let _ = audit::append(&state.pg, &ev).await;
}
