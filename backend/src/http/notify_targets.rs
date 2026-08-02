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

//! Where an account wants to be told about what its lines took.
//!
//! Gated as the practice's own arrangements, like the screening list and the diary: the
//! account these lines belong to, and an administrator of the platform. Deliberately not
//! the permission that registers telephone numbers, because this decides where a caller's
//! name is announced rather than which numbers this deployment answers.
//!
//! **The address is write-only.** An incoming-webhook address is a credential: anybody
//! holding it can post into that channel as though they were the practice. It is stored
//! encrypted and never sent back out, so what a reader sees is the host it points at and
//! nothing more. That is also why there is a test send: an address pasted wrong is
//! otherwise discovered by a client who was never rung back.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::telephony::notify::{self, Event};

/// The account itself, or an administrator of the platform.
fn require(ctx: &AuthContext, owner_user_id: Uuid) -> Result<()> {
    if ctx.is_admin() || ctx.user_id == Some(owner_user_id) {
        Ok(())
    } else {
        // Refused as absent rather than forbidden: whether an account is told about its
        // calls anywhere, and where, is not a stranger's business either.
        Err(AppError::NotFound("no such notification target".into()))
    }
}

/// The kinds of service a target may be, and the events it may take. Both closed sets, so
/// a typed value that nothing will ever act on is refused when it is written rather than
/// discovered when a notification silently never arrives.
fn check_kind(kind: &str) -> Result<()> {
    if matches!(kind, "slack" | "teams" | "webhook") {
        Ok(())
    } else {
        Err(AppError::Validation(
            "a notification target is a Slack channel, a Teams channel, or any address that \
             accepts a posted message"
                .into(),
        ))
    }
}

fn check_events(events: &[String]) -> Result<()> {
    for e in events {
        if !Event::ALL.iter().any(|k| k.as_str() == e) {
            return Err(AppError::Validation(format!("nothing on a line ever produces {e:?}")));
        }
    }
    Ok(())
}

/// What a reader is shown: the host, never the address.
fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "unreadable".into())
}

#[derive(Serialize)]
pub struct TargetOut {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub label: String,
    pub kind: String,
    /// The host the address points at. The address itself never leaves this deployment.
    pub host: String,
    pub events: Vec<String>,
    pub enabled: bool,
    pub created_epoch: i64,
}

#[derive(Deserialize)]
pub struct WhoseTargets {
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
}

/// `GET /api/notify-targets` — where this account is told about its calls.
pub async fn list(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<WhoseTargets>,
) -> Result<Json<Vec<TargetOut>>> {
    let owner = q
        .owner_user_id
        .or(ctx.user_id)
        .ok_or_else(|| AppError::NotFound("no such notification target".into()))?;
    require(&ctx, owner)?;
    let rows = sqlx::query!(
        r#"SELECT id, owner_user_id, label, kind, url_enc, events, enabled,
                  extract(epoch from created_at)::bigint AS "created_epoch!"
             FROM notify_targets WHERE owner_user_id = $1 ORDER BY label"#,
        owner
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| TargetOut {
                id: r.id,
                owner_user_id: r.owner_user_id,
                label: r.label,
                kind: r.kind,
                host: crate::crypto::decrypt_at_rest(&r.url_enc)
                    .map(|u| host_of(&u))
                    .unwrap_or_else(|_| "unreadable".into()),
                events: r.events,
                enabled: r.enabled,
                created_epoch: r.created_epoch,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct NewTarget {
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
    pub label: String,
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
    /// Omitted means on: somebody who has just typed an address and chosen its events
    /// means it to work.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// `POST /api/notify-targets` — add one.
pub async fn create(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<NewTarget>,
) -> Result<Json<serde_json::Value>> {
    let owner = body
        .owner_user_id
        .or(ctx.user_id)
        .ok_or_else(|| AppError::NotFound("no such notification target".into()))?;
    require(&ctx, owner)?;
    let label = body.label.trim();
    if label.is_empty() {
        return Err(AppError::Validation("a notification target needs a name to tell it by".into()));
    }
    check_kind(&body.kind)?;
    check_events(&body.events)?;
    notify::check_url(&body.url)?;

    let id = Uuid::now_v7();
    let url_enc = crate::crypto::encrypt_at_rest(body.url.trim())
        .map_err(|_| AppError::Config("the address could not be stored safely".into()))?;
    sqlx::query!(
        "INSERT INTO notify_targets (id, owner_user_id, label, kind, url_enc, events, enabled, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        id,
        owner,
        label,
        body.kind,
        url_enc,
        &body.events,
        body.enabled,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;
    audit_target(&state, &ctx, "telephony.notify_target.created", id, &body.kind, &body.events, &body.url).await;
    Ok(Json(serde_json::json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct EditTarget {
    #[serde(default)]
    pub label: Option<String>,
    /// Absent leaves the address alone. It is never sent back, so an interface that wants
    /// to leave it unchanged simply omits it.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `PATCH /api/notify-targets/{id}` — change one.
pub async fn update(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<EditTarget>,
) -> Result<Json<serde_json::Value>> {
    let current = sqlx::query!("SELECT owner_user_id, kind, events FROM notify_targets WHERE id = $1", id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::NotFound("no such notification target".into()))?;
    require(&ctx, current.owner_user_id)?;

    if let Some(events) = body.events.as_ref() {
        check_events(events)?;
    }
    let url_enc = match body.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        Some(url) => {
            notify::check_url(url)?;
            Some(
                crate::crypto::encrypt_at_rest(url)
                    .map_err(|_| AppError::Config("the address could not be stored safely".into()))?,
            )
        }
        None => None,
    };
    sqlx::query!(
        "UPDATE notify_targets \
            SET label = COALESCE($2, label), url_enc = COALESCE($3, url_enc), \
                events = COALESCE($4, events), enabled = COALESCE($5, enabled), updated_at = now() \
          WHERE id = $1",
        id,
        body.label.as_deref().map(str::trim).filter(|l| !l.is_empty()),
        url_enc,
        body.events.as_deref(),
        body.enabled,
    )
    .execute(&state.pg)
    .await?;
    let events = body.events.unwrap_or(current.events);
    audit_target(&state, &ctx, "telephony.notify_target.updated", id, &current.kind, &events, "").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `DELETE /api/notify-targets/{id}` — stop telling it anything.
pub async fn remove(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let current = sqlx::query!("SELECT owner_user_id, kind FROM notify_targets WHERE id = $1", id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::NotFound("no such notification target".into()))?;
    require(&ctx, current.owner_user_id)?;
    sqlx::query!("DELETE FROM notify_targets WHERE id = $1", id).execute(&state.pg).await?;
    audit_target(&state, &ctx, "telephony.notify_target.deleted", id, &current.kind, &[], "").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /api/notify-targets/{id}/test` — send a specimen line to it, now.
///
/// Posted here rather than queued, because the whole point is to be told what happened: a
/// wrong address that fails quietly in a worker an hour later has taught nobody anything.
/// A specimen line rather than a real one, so testing a target never discloses a caller.
pub async fn test(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let current = sqlx::query!("SELECT owner_user_id FROM notify_targets WHERE id = $1", id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::NotFound("no such notification target".into()))?;
    require(&ctx, current.owner_user_id)?;
    notify::deliver(
        &state,
        &serde_json::json!({
            "target_id": id,
            "event": Event::MessageTaken.as_str(),
            "text": "This is a test from your telephone line. Nobody rang.",
        }),
    )
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn audit_target(
    state: &AppState,
    ctx: &AuthContext,
    action: &str,
    id: Uuid,
    kind: &str,
    events: &[String],
    url: &str,
) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("notify_target".into());
    ev.resource_id = Some(id);
    // The host and never the address: the address is a credential, and an audit trail is
    // read by more people than the one who typed it.
    let mut payload = serde_json::json!({ "kind": kind, "events": events });
    if !url.is_empty() {
        payload["host"] = serde_json::json!(host_of(url.trim()));
    }
    ev.payload = Some(payload);
    let _ = audit::append(&state.pg, &ev).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a reader is shown about a stored address. Never the path, which is the secret
    /// part of every incoming-webhook address.
    #[test]
    fn only_the_host_is_ever_shown() {
        assert_eq!(host_of("https://hooks.slack.test/services/T000/B000/xxxxSECRETxxxx"), "hooks.slack.test");
        assert_eq!(host_of("not a url"), "unreadable");
    }

    #[test]
    fn only_the_kinds_and_events_that_do_something_are_accepted() {
        assert!(check_kind("slack").is_ok());
        assert!(check_kind("teams").is_ok());
        assert!(check_kind("webhook").is_ok());
        assert!(check_kind("carrier-pigeon").is_err());
        assert!(check_events(&["message_taken".into(), "appointment_booked".into()]).is_ok());
        assert!(check_events(&["everything".into()]).is_err());
        assert!(check_events(&[]).is_ok(), "a target that takes nothing is allowed, and silent");
    }
}
