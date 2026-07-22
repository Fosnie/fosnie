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

//! Management of platform API keys: the owner's own keys under `/api/me`, and an
//! administrator's read-and-revoke view over another user's.
//!
//! These routes live on the ordinary session-authenticated surface — a key is
//! minted from the browser and then used elsewhere. The secret is returned
//! exactly once, by the create call; nothing else can ever reveal it.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::device::MaybeDevice;
use crate::auth::keycloak::AuthUser;
use crate::auth::{AuthContext, permissions};
use crate::error::{AppError, Result};
use crate::state::AppState;

/// The longest life a key may be given, in days. A key that never expires is
/// still allowed (omit the field); this only bounds an explicit choice so a
/// typo cannot mint something outliving the deployment.
const MAX_EXPIRY_DAYS: i64 = 3650;

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    #[serde(default)]
    pub name: String,
    /// Days until the key expires. Omitted or null = never expires.
    #[serde(default)]
    pub expires_in_days: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct KeyOut {
    pub id: Uuid,
    pub name: String,
    pub display_prefix: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize)]
pub struct CreatedOut {
    /// The plaintext secret. Shown here and nowhere else, ever.
    pub token: String,
    #[serde(flatten)]
    pub key: KeyOut,
}

/// Key management follows the availability of the programmatic surface itself:
/// with it switched off there is nothing a key could be used for, so the
/// management routes disappear too (404, not 403 — the feature is absent, not
/// forbidden).
async fn gate(state: &AppState, ctx: &AuthContext) -> Result<()> {
    if state.features.enabled_for(state, ctx, "public_api").await {
        Ok(())
    } else {
        Err(AppError::NotFound("not found".into()))
    }
}

fn owner(ctx: &AuthContext) -> Result<Uuid> {
    ctx.user_id
        .ok_or_else(|| AppError::Forbidden("a user account is required to manage API keys".into()))
}

pub async fn create_key(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    device: MaybeDevice,
    Json(body): Json<CreateBody>,
) -> Result<impl IntoResponse> {
    // Minting a key is barred from a paired device: the key would outlive the
    // device's own revocation, turning a revocable device token into permanent
    // access. Keys are minted from an interactive web session only.
    device.require_session()?;
    gate(&state, &ctx).await?;
    // A session that exists only to finish enrolling a second factor is not a
    // fully authenticated session, and minting a long-lived credential from one
    // would route around the very factor being enrolled. The local provider
    // already blocks this by path allow-list; the check is repeated here so the
    // rule holds under any auth provider.
    if ctx.mfa_enroll_only {
        return Err(AppError::Forbidden(
            "finish setting up two-factor authentication before creating API keys".into(),
        ));
    }
    let uid = owner(&ctx)?;

    let expires_at = match body.expires_in_days {
        None => None,
        Some(d) if d >= 1 && d <= MAX_EXPIRY_DAYS => {
            Some(OffsetDateTime::now_utc() + time::Duration::days(d))
        }
        Some(_) => {
            return Err(AppError::Validation(format!(
                "expires_in_days must be between 1 and {MAX_EXPIRY_DAYS}"
            )));
        }
    };

    let name = {
        let n = body.name.trim();
        if n.is_empty() { "Untitled key".to_string() } else { n.chars().take(120).collect() }
    };

    let (token, hash, display_prefix) = crate::auth::api_key::mint();
    let id = Uuid::now_v7();

    let mut tx = state.pg.begin().await?;
    let row = sqlx::query!(
        "INSERT INTO api_keys (id, user_id, kind, name, token_hash, display_prefix, expires_at) \
         VALUES ($1, $2, 'api', $3, $4, $5, $6) \
         RETURNING created_at, last_used_at, expires_at, revoked_at",
        id,
        uid,
        name,
        &hash,
        display_prefix,
        expires_at,
    )
    .fetch_one(&mut *tx)
    .await?;

    let mut event = AuditEvent::action("api.key.created", ctx.role.as_str());
    event.actor_user_id = Some(uid);
    event.resource_type = Some("api_key".into());
    event.resource_id = Some(id);
    // The name and prefix identify the key; the secret never touches the log.
    event.payload = Some(json!({ "name": name, "display_prefix": display_prefix, "expires_at": expires_at }));
    audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedOut {
            token,
            key: KeyOut {
                id,
                name,
                display_prefix,
                created_at: row.created_at,
                last_used_at: row.last_used_at,
                expires_at: row.expires_at,
                revoked_at: row.revoked_at,
            },
        }),
    ))
}

pub async fn list_keys(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<KeyOut>>> {
    gate(&state, &ctx).await?;
    let uid = owner(&ctx)?;
    Ok(Json(load_keys(&state, uid).await?))
}

pub async fn revoke_key(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    gate(&state, &ctx).await?;
    let uid = owner(&ctx)?;
    revoke(&state, &ctx, id, Some(uid)).await
}

/// An administrator's view of a user's keys. Read-only detail: the secret is
/// unrecoverable for an admin exactly as it is for the owner.
pub async fn admin_list_keys(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(user_id): Path<Uuid>,
) -> Result<Json<Vec<KeyOut>>> {
    gate(&state, &ctx).await?;
    state.rbac.require_permission(&state.pg, &ctx, permissions::USERS_VIEW).await?;
    Ok(Json(load_keys(&state, user_id).await?))
}

pub async fn admin_revoke_key(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    device: MaybeDevice,
    Path((_user_id, id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    // Reaching across to revoke another user's key is a web-session action, even
    // for an administrator: a device does not manage credentials on the account,
    // and this is the same cross-user admin write as signing out their devices.
    device.require_session()?;
    gate(&state, &ctx).await?;
    state.rbac.require_permission(&state.pg, &ctx, permissions::USERS_MANAGE).await?;
    revoke(&state, &ctx, id, None).await
}

async fn load_keys(state: &AppState, user_id: Uuid) -> Result<Vec<KeyOut>> {
    let rows = sqlx::query!(
        "SELECT id, name, display_prefix, created_at, last_used_at, expires_at, revoked_at \
         FROM api_keys WHERE user_id = $1 AND kind = 'api' ORDER BY created_at DESC",
        user_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| KeyOut {
            id: r.id,
            name: r.name,
            display_prefix: r.display_prefix,
            created_at: r.created_at,
            last_used_at: r.last_used_at,
            expires_at: r.expires_at,
            revoked_at: r.revoked_at,
        })
        .collect())
}

/// Soft-revoke. `restrict_to` scopes the update to one owner (the self-service
/// path), so a caller cannot revoke a key that is not theirs by guessing an id.
/// Revoking twice is a no-op, not an error.
async fn revoke(
    state: &AppState,
    ctx: &AuthContext,
    id: Uuid,
    restrict_to: Option<Uuid>,
) -> Result<StatusCode> {
    let mut tx = state.pg.begin().await?;
    let row = sqlx::query!(
        "UPDATE api_keys SET revoked_at = now() \
         WHERE id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND revoked_at IS NULL \
         RETURNING user_id, name",
        id,
        restrict_to,
    )
    .fetch_optional(&mut *tx)
    .await?;

    let Some(row) = row else {
        // Either it does not exist, is not the caller's, or was already revoked.
        // All three are indistinguishable to the caller by design.
        tx.rollback().await?;
        return Ok(StatusCode::NO_CONTENT);
    };

    let mut event = AuditEvent::action("api.key.revoked", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("api_key".into());
    event.resource_id = Some(id);
    event.payload = Some(json!({ "name": row.name, "owner_user_id": row.user_id }));
    audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
