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

//! Pairing and management of desktop devices.
//!
//! A device joins an account through a short code: the owner mints one from a
//! signed-in web session, reads it into the desktop client, and the client
//! redeems it — with no credentials of its own — for a device token. From then
//! on the device acts with exactly the owner's rights over the native surface.
//!
//! Unlike the programmatic keys next door, pairing is a core capability and is
//! never feature-gated: a desktop client is a first-class way to reach an
//! instance, not an optional integration surface.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::{Json, response::IntoResponse};
use argon2::password_hash::rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::device::MaybeDevice;
use crate::auth::keycloak::AuthUser;
use crate::auth::{AuthContext, permissions};
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Length of a pairing code, in characters.
const CODE_LEN: usize = 8;
/// Digits and letters that cannot be confused when read off one screen and
/// typed into another: no `0`/`O`, no `1`/`I`. Exactly 32 symbols, so masking a
/// random byte to five bits is uniform with no rejection loop; eight of them
/// carry forty bits of entropy.
const CODE_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
/// How long a code is valid before it must be minted afresh.
const CODE_TTL_MINUTES: i64 = 10;
/// The platforms a device may declare.
const PLATFORMS: [&str; 3] = ["windows", "macos", "linux"];

#[derive(Debug, Serialize)]
pub struct DeviceOut {
    pub id: Uuid,
    pub name: String,
    pub platform: String,
    pub created_at: OffsetDateTime,
    pub last_seen_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
}

#[derive(Debug, Serialize)]
pub struct PairingCodeOut {
    /// The plaintext code. Shown here and nowhere else; only its hash is stored.
    pub code: String,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
pub struct PairBody {
    pub code: String,
    #[serde(default)]
    pub name: String,
    pub platform: String,
}

#[derive(Debug, Serialize)]
pub struct PairedOut {
    pub device_id: Uuid,
    /// The device token. Shown once, at pairing, and never again.
    pub token: String,
}

fn owner(ctx: &AuthContext) -> Result<Uuid> {
    ctx.user_id
        .ok_or_else(|| AppError::Forbidden("a user account is required to manage devices".into()))
}

/// A fresh pairing code and its stored hash.
fn mint_code() -> (String, Vec<u8>) {
    let mut bytes = [0u8; CODE_LEN];
    OsRng.fill_bytes(&mut bytes);
    let code: String =
        bytes.iter().map(|b| CODE_ALPHABET[(b & 31) as usize] as char).collect();
    let hash = Sha256::digest(code.as_bytes()).to_vec();
    (code, hash)
}

/// Fold a typed-in code back to its canonical form before hashing: people group
/// it, lower-case it, and paste stray spaces. Kept in step with how the code is
/// displayed (a hyphen between the two halves).
fn normalise_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn hash_code(code: &str) -> Vec<u8> {
    Sha256::digest(code.as_bytes()).to_vec()
}

/// Coarse client key for the pairing rate limiter: first hop of
/// `X-Forwarded-For` (the platform is served same-origin behind a reverse
/// proxy), else a constant. Matches the telemetry endpoint's approach.
fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

/// `POST /api/me/devices/pairing-code` — mint a code from a signed-in session.
pub async fn create_pairing_code(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    MaybeDevice(device): MaybeDevice,
) -> Result<Json<PairingCodeOut>> {
    // Enrolling a second factor is not a fully authenticated session, and pairing
    // a device from one would route around the very factor being enrolled. The
    // local provider already blocks this by path allow-list; the check is
    // repeated here so the rule holds under any auth provider.
    if ctx.mfa_enroll_only {
        return Err(AppError::Forbidden(
            "finish setting up two-factor authentication before pairing a device".into(),
        ));
    }
    // A device may not enrol further devices: pairing has to start from a live
    // web session, or a single compromised machine could quietly multiply itself.
    if device.is_some() {
        return Err(AppError::Forbidden(
            "pairing a new device must be done from the web, not from a device".into(),
        ));
    }
    let uid = owner(&ctx)?;
    crate::cache::rate_limit_guard(&state.redis, &format!("pair:{uid}"), 5, 600).await?;

    let (code, hash) = mint_code();
    let expires_at = OffsetDateTime::now_utc() + time::Duration::minutes(CODE_TTL_MINUTES);

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO device_pairing_codes (code_hash, user_id, expires_at) VALUES ($1, $2, $3)",
        &hash,
        uid,
        expires_at,
    )
    .execute(&mut *tx)
    .await?;

    let mut event = AuditEvent::action("device.pairing_code.created", ctx.role.as_str());
    event.actor_user_id = Some(uid);
    event.resource_type = Some("device_pairing_code".into());
    // Neither the code nor its hash is logged: the audit trail records that a
    // code was issued and when it lapses, never the secret itself.
    event.payload = Some(json!({ "expires_at": expires_at }));
    audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;

    Ok(Json(PairingCodeOut { code, expires_at }))
}

/// `POST /api/device/pair` — redeem a code for a device token. Public: the
/// client has no credential yet, and the single-use code minted from an
/// authenticated session is the whole authority.
pub async fn pair_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PairBody>,
) -> Result<impl IntoResponse> {
    let ip = client_ip(&headers);
    crate::cache::rate_limit_guard(&state.redis, &format!("pairdev:{ip}"), 10, 600).await?;

    if !PLATFORMS.contains(&body.platform.as_str()) {
        return Err(AppError::Validation(
            "platform must be one of windows, macos, linux".into(),
        ));
    }
    let name = {
        let n = body.name.trim();
        if n.is_empty() { "Untitled device".to_string() } else { n.chars().take(120).collect() }
    };
    let hash = hash_code(&normalise_code(&body.code));

    let mut tx = state.pg.begin().await?;
    // Consume the code in the same statement that checks it: a conditional
    // UPDATE ... RETURNING is atomic, so two clients racing the same code cannot
    // both win. A miss (unknown, spent or lapsed) is a flat 404 with no detail —
    // the caller learns nothing about which.
    let claimed = sqlx::query!(
        "UPDATE device_pairing_codes SET consumed_at = now() \
         WHERE code_hash = $1 AND consumed_at IS NULL AND expires_at > now() \
         RETURNING user_id",
        &hash,
    )
    .fetch_optional(&mut *tx)
    .await?;

    let Some(claimed) = claimed else {
        tx.rollback().await?;
        return Err(AppError::NotFound("not found".into()));
    };
    let uid = claimed.user_id;

    // Load the owner's context: this rejects a deactivated account before a token
    // is ever minted, and gives the audit event a real actor role.
    let owner_ctx = crate::auth::load_context(&state.pg, uid).await?;

    let device_id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO devices (id, user_id, name, platform) VALUES ($1, $2, $3, $4)",
        device_id,
        uid,
        name,
        body.platform,
    )
    .execute(&mut *tx)
    .await?;

    let (token, token_hash, display_prefix) = crate::auth::api_key::mint();
    let key_id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO api_keys (id, user_id, kind, device_id, name, token_hash, display_prefix) \
         VALUES ($1, $2, 'device', $3, $4, $5, $6)",
        key_id,
        uid,
        device_id,
        name,
        &token_hash,
        display_prefix,
    )
    .execute(&mut *tx)
    .await?;

    let mut event = AuditEvent::action("device.paired", owner_ctx.role.as_str());
    event.actor_user_id = Some(uid);
    event.resource_type = Some("device".into());
    event.resource_id = Some(device_id);
    event.payload =
        Some(json!({ "name": name, "platform": body.platform, "display_prefix": display_prefix }));
    audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(PairedOut { device_id, token })))
}

pub async fn list_devices(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<DeviceOut>>> {
    let uid = owner(&ctx)?;
    Ok(Json(load_devices(&state, uid).await?))
}

pub async fn revoke_device(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    let uid = owner(&ctx)?;
    revoke(&state, &ctx, id, Some(uid)).await
}

/// An administrator's view of a user's devices. Read plus revoke, in the shape
/// of the per-user key view.
pub async fn admin_list_devices(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(user_id): Path<Uuid>,
) -> Result<Json<Vec<DeviceOut>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::USERS_VIEW).await?;
    Ok(Json(load_devices(&state, user_id).await?))
}

pub async fn admin_revoke_device(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    device: MaybeDevice,
    Path((_user_id, id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    // Signing out another user's device is barred from a paired device, even for
    // an administrator: a device may sign itself out, but reaching across to
    // other users' machines is a web-session action.
    device.require_session()?;
    state.rbac.require_permission(&state.pg, &ctx, permissions::USERS_MANAGE).await?;
    revoke(&state, &ctx, id, None).await
}

async fn load_devices(state: &AppState, user_id: Uuid) -> Result<Vec<DeviceOut>> {
    let rows = sqlx::query!(
        "SELECT id, name, platform, created_at, last_seen_at, revoked_at \
         FROM devices WHERE user_id = $1 ORDER BY created_at DESC",
        user_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DeviceOut {
            id: r.id,
            name: r.name,
            platform: r.platform,
            created_at: r.created_at,
            last_seen_at: r.last_seen_at,
            revoked_at: r.revoked_at,
        })
        .collect())
}

/// Withdraw a device and its token together. `restrict_to` scopes the update to
/// one owner (the self-service path) so a caller cannot revoke a device that is
/// not theirs by guessing an id. Revoking a device that is missing, foreign or
/// already withdrawn is an indistinguishable no-op, exactly as for a key.
async fn revoke(
    state: &AppState,
    ctx: &AuthContext,
    id: Uuid,
    restrict_to: Option<Uuid>,
) -> Result<StatusCode> {
    let mut tx = state.pg.begin().await?;
    let row = sqlx::query!(
        "UPDATE devices SET revoked_at = now() \
         WHERE id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND revoked_at IS NULL \
         RETURNING user_id, name",
        id,
        restrict_to,
    )
    .fetch_optional(&mut *tx)
    .await?;

    let Some(row) = row else {
        tx.rollback().await?;
        return Ok(StatusCode::NO_CONTENT);
    };

    // The device's token is revoked in the same transaction, so the two never
    // disagree: withdrawing the machine and disabling its credential is one act.
    sqlx::query!(
        "UPDATE api_keys SET revoked_at = now() WHERE device_id = $1 AND revoked_at IS NULL",
        id,
    )
    .execute(&mut *tx)
    .await?;

    let mut event = AuditEvent::action("device.revoked", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("device".into());
    event.resource_id = Some(id);
    event.payload = Some(json!({ "name": row.name, "owner_user_id": row.user_id }));
    audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_alphabet_excludes_confusable_characters() {
        for _ in 0..1000 {
            let (code, _) = mint_code();
            assert_eq!(code.len(), CODE_LEN);
            assert!(
                code.chars().all(|c| !matches!(c, '0' | 'O' | '1' | 'I')),
                "confusable character in {code}"
            );
            assert!(code.chars().all(|c| CODE_ALPHABET.contains(&(c as u8))));
        }
    }

    #[test]
    fn normalise_folds_grouping_and_case() {
        assert_eq!(normalise_code(" k7m2-qx4d "), "K7M2QX4D");
        assert_eq!(normalise_code("K7M2QX4D"), "K7M2QX4D");
    }
}
