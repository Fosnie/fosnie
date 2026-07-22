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

//! Self-service profile: the logged-in user's own display name + avatar, plus
//! read-only account facts (email, created date, a link to the Keycloak account
//! console for password/MFA).
//!
//! Display name is platform-local once customised — `display_name_custom` stops
//! `auth/provisioning.rs` from overwriting it on the next request. Avatars are
//! written to `storage.avatars_dir` (zero-egress, on disk) with a pointer row;
//! `avatar_updated_at` (sent as an epoch) doubles as a cache-buster.

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::device::MaybeDevice;
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::events::{self, ActorType, NewEvent};
use crate::state::AppState;

/// Resolve the caller's real user id — break-glass principals have no profile.
fn me(ctx: &AuthContext) -> Result<Uuid> {
    ctx.user_id
        .ok_or_else(|| AppError::Forbidden("a Keycloak account is required for a profile".into()))
}

const NAME_MAX: usize = 120;
/// Image types we accept for an avatar.
const ALLOWED_MIME: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Resolve the avatars directory to an absolute path (same handling as branding).
fn avatars_dir(state: &AppState) -> Result<std::path::PathBuf> {
    Ok(crate::storage::resolve_dir(&state.boot.storage.avatars_dir))
}

// --- Profile read/update -----------------------------------------------------

/// GET /api/me/profile — the caller's own account, for the profile page.
pub async fn get_profile(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<serde_json::Value>> {
    let uid = me(&ctx)?;
    let row = sqlx::query!(
        r#"SELECT email, display_name, display_name_custom,
                  role::text AS "role!",
                  extract(epoch from created_at)::bigint AS "created_epoch!",
                  extract(epoch from avatar_updated_at)::bigint AS avatar_epoch
           FROM users WHERE id = $1"#,
        uid
    )
    .fetch_one(&state.pg)
    .await?;

    // Link to Keycloak's own account console for password/MFA — only under Keycloak
    // auth AND when KC is configured. A local-auth Core deploy that merely has a
    // KEYCLOAK_URL set must NOT surface this (users manage their own password here);
    // empty otherwise → the frontend hides the link.
    let kc = &state.boot.keycloak;
    let account_url = if state.boot.auth.mode == crate::config::AuthMode::Keycloak
        && !kc.url.trim().is_empty()
    {
        format!("{}/account", kc.issuer())
    } else {
        String::new()
    };

    Ok(Json(json!({
        "user_id": uid,
        "email": row.email,
        "display_name": row.display_name,
        "display_name_custom": row.display_name_custom,
        "role": row.role,
        "created_epoch": row.created_epoch,
        "avatar_updated_at": row.avatar_epoch,
        "account_url": account_url,
    })))
}

#[derive(Deserialize)]
pub struct NameUpdate {
    pub display_name: String,
}

/// PATCH /api/me/profile — rename yourself. Flips `display_name_custom` so the
/// login upsert stops mirroring the Keycloak name.
pub async fn update_profile(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<NameUpdate>,
) -> Result<Json<serde_json::Value>> {
    let uid = me(&ctx)?;
    let name = body.display_name.trim();
    if name.is_empty() {
        return Err(AppError::Validation("display name cannot be empty".into()));
    }
    if name.chars().count() > NAME_MAX {
        return Err(AppError::Validation(format!(
            "display name must be at most {NAME_MAX} characters"
        )));
    }

    sqlx::query!(
        "UPDATE users SET display_name = $1, display_name_custom = true WHERE id = $2",
        name,
        uid
    )
    .execute(&state.pg)
    .await?;

    let mut ev = AuditEvent::action("profile.updated", ctx.role.as_str());
    ev.actor_user_id = Some(uid);
    ev.resource_type = Some("user".into());
    ev.resource_id = Some(uid);
    let _ = audit::append(&state.pg, &ev).await;

    Ok(Json(json!({ "ok": true, "display_name": name })))
}

// --- Avatar ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AvatarUpload {
    #[serde(default)]
    pub mime: Option<String>,
}

/// POST /api/me/avatar — upload (or replace) your avatar. Raw image bytes in the
/// body, `?mime=` the content type. Body size is capped at the route layer.
pub async fn upload_avatar(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<AvatarUpload>,
    body: Bytes,
) -> Result<Json<serde_json::Value>> {
    let uid = me(&ctx)?;
    if body.is_empty() {
        return Err(AppError::Validation("empty avatar upload".into()));
    }
    let mime = q.mime.unwrap_or_else(|| "image/png".into());
    if !ALLOWED_MIME.contains(&mime.as_str()) {
        return Err(AppError::Validation(
            "avatar must be a PNG, JPEG, WebP or GIF image".into(),
        ));
    }

    let dir = avatars_dir(&state)?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("create avatars dir: {e}")))?;
    // Store the RELATIVE name (`<user_id>`) under `avatars_dir`; resolved on read.
    let disk_path = uid.to_string();
    tokio::fs::write(dir.join(&disk_path), &body)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("write avatar: {e}")))?;

    let epoch = sqlx::query_scalar!(
        r#"UPDATE users
           SET avatar_path = $1, avatar_mime = $2, avatar_updated_at = now()
           WHERE id = $3
           RETURNING extract(epoch from avatar_updated_at)::bigint AS "epoch!""#,
        disk_path,
        mime,
        uid
    )
    .fetch_one(&state.pg)
    .await?;

    let mut ev = AuditEvent::action("profile.avatar_updated", ctx.role.as_str());
    ev.actor_user_id = Some(uid);
    ev.resource_type = Some("user".into());
    ev.resource_id = Some(uid);
    let _ = audit::append(&state.pg, &ev).await;

    Ok(Json(json!({ "ok": true, "avatar_updated_at": epoch })))
}

/// DELETE /api/me/avatar — drop your avatar, fall back to initials.
pub async fn delete_avatar(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<serde_json::Value>> {
    let uid = me(&ctx)?;
    let old = sqlx::query_scalar!("SELECT avatar_path FROM users WHERE id = $1", uid)
        .fetch_optional(&state.pg)
        .await?
        .flatten();

    sqlx::query!(
        "UPDATE users SET avatar_path = NULL, avatar_mime = NULL, avatar_updated_at = NULL WHERE id = $1",
        uid
    )
    .execute(&state.pg)
    .await?;

    if let Some(p) = old {
        let abs = crate::storage::resolve_file(&state.boot.storage.avatars_dir, &p);
        let _ = tokio::fs::remove_file(&abs).await; // best-effort
    }

    let mut ev = AuditEvent::action("profile.avatar_removed", ctx.role.as_str());
    ev.actor_user_id = Some(uid);
    ev.resource_type = Some("user".into());
    ev.resource_id = Some(uid);
    let _ = audit::append(&state.pg, &ev).await;

    Ok(Json(json!({ "ok": true })))
}

// --- Self-serve account deletion (soft-archive) ------------------------------

/// DELETE /api/me/account — self-serve account deletion as a soft-archive
/// (GDPR-friendly). The row is deactivated (login already refuses deactivated
/// users) and the PII anonymised; the id is kept so audit/events/grants that FK
/// it stay intact. `self_archived_at` marks this as a self-delete (distinct from
/// an admin suspend), so it drops out of the admin user-list with no reactivation.
///
/// Core does NOT erase the underlying data — it emits `account.archived` so a
/// `fosnie-enterprise` edition can crypto-shred on top. Sessions: any live WebSocket
/// is closed here, and every other session is refused on its next request by the
/// shared deactivation check in `auth::load_context`; the SPA signs the caller out.
pub async fn delete_account(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    device: MaybeDevice,
) -> Result<Json<serde_json::Value>> {
    // Deleting the account is barred from a paired device: an irreversible act
    // on the whole account must come from an interactive web session.
    device.require_session()?;
    let uid = me(&ctx)?;
    // Deterministic tombstone — frees the unique email and removes the address.
    let tombstone = format!("deleted-{}@deleted.invalid", uid.simple());

    // Grab the avatar pointer before we clear it, so the on-disk image can be
    // unlinked after the tombstone commits (a "Deleted user" keeps no picture).
    let old_avatar = sqlx::query_scalar!("SELECT avatar_path FROM users WHERE id = $1", uid)
        .fetch_optional(&state.pg)
        .await?
        .flatten();

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        r#"UPDATE users
              SET deactivated_at = COALESCE(deactivated_at, now()),
                  self_archived_at = now(),
                  display_name = 'Deleted user',
                  display_name_custom = false,
                  email = $2
            WHERE id = $1"#,
        uid,
        tombstone,
    )
    .execute(&mut *tx)
    .await?;
    // Clear the avatar pointer (a "Deleted user" keeps no picture). Byte-identical
    // to the query in `delete_avatar` so it reuses the offline sqlx cache entry.
    sqlx::query!(
        "UPDATE users SET avatar_path = NULL, avatar_mime = NULL, avatar_updated_at = NULL WHERE id = $1",
        uid
    )
    .execute(&mut *tx)
    .await?;

    // Forward hook for the Enterprise crypto-shred (Core never shreds itself).
    let ev = NewEvent::new(events::ACCOUNT_ARCHIVED, ActorType::Human)
        .actor(Some(uid))
        .resource("user", uid);
    events::emit_with(&mut tx, &ev).await?;

    let mut a = AuditEvent::action("account.self_archived", ctx.role.as_str());
    a.actor_user_id = Some(uid);
    a.resource_type = Some("user".into());
    a.resource_id = Some(uid);
    a.risk_anomaly_flag = true; // account-state change is sensitive
    audit::append_with(&mut tx, &a).await?;

    tx.commit().await?;

    // Best-effort: drop the avatar image from disk now the pointer is cleared.
    if let Some(p) = old_avatar {
        let abs = crate::storage::resolve_file(&state.boot.storage.avatars_dir, &p);
        let _ = tokio::fs::remove_file(&abs).await;
    }

    // Kill any live WebSocket; reconnect + every other session is denied by
    // load_context (which filters deactivated_at).
    state.hub.close_user(uid);

    Ok(Json(json!({ "ok": true })))
}

/// GET /api/users/{id}/avatar — serve any user's avatar bytes (authed users).
/// 404 when there is none, so the frontend falls back to initials.
pub async fn get_avatar(
    State(state): State<AppState>,
    AuthUser(_ctx): AuthUser,
    Path(user_id): Path<Uuid>,
) -> Result<Response> {
    let row = sqlx::query!(
        "SELECT avatar_path, avatar_mime FROM users WHERE id = $1",
        user_id
    )
    .fetch_optional(&state.pg)
    .await?;

    let (Some(path), Some(mime)) = row
        .map(|r| (r.avatar_path, r.avatar_mime))
        .unwrap_or((None, None))
    else {
        return Ok((StatusCode::NOT_FOUND, "no avatar").into_response());
    };

    let abs = crate::storage::resolve_file(&state.boot.storage.avatars_dir, &path);
    match tokio::fs::read(&abs).await {
        Ok(bytes) => Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "private, max-age=300".into()),
            ],
            Body::from(bytes),
        )
            .into_response()),
        Err(_) => Ok((StatusCode::NOT_FOUND, "no avatar").into_response()),
    }
}
