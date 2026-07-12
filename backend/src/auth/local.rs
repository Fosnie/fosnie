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

//! Local email/password authentication — the Core default `AuthProvider`.
//! Independent of Keycloak: registration writes an
//! Argon2id hash into the `users` cache, login mints an opaque server-side
//! session in Redis (the break-glass pattern), and the session is carried in an
//! `httpOnly` cookie. Revocation is free (logout deletes the key); the
//! deactivation check is shared with the Keycloak path via [`super::load_context`].

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::http::request::Parts;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use deadpool_redis::redis;
use sqlx::PgPool;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::{AuthContext, PlatformRole};
use crate::error::AppError;
use crate::ext;
use crate::state::AppState;

/// Name of the session cookie the browser carries.
pub const SESSION_COOKIE: &str = "pai_session";

fn session_key(token: &str) -> String {
    format!("pai:session:{token}")
}

/// Reverse index: a Redis SET of a user's live session tokens, so a SCIM/admin
/// deactivate can force-logout every session (the forward `pai:session:{token}`
/// keys are not enumerable per-user). Individual tokens still auto-expire; stale
/// members are harmless (a `DEL` of an already-expired key is a no-op).
fn user_sessions_key(user_id: Uuid) -> String {
    format!("pai:session:by_user:{user_id}")
}

/// A fresh 256-bit CSPRNG session token (base64url, ~43 chars) — the sole
/// credential for a local session, so a full random secret (not a UUID).
fn new_token() -> String {
    use argon2::password_hash::rand_core::RngCore;
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

/// Encode the Redis session *value*. A normal session stays a
/// plain UUID string — byte-identical to the pre-MFA format, so every session that
/// pre-dates this deploy keeps round-tripping. An *enrolment-only* session (MFA
/// mandatory but not yet enrolled) is a small JSON object instead.
fn encode_session(user_id: Uuid, enroll_only: bool) -> String {
    if enroll_only {
        format!(r#"{{"u":"{user_id}","e":true}}"#)
    } else {
        user_id.to_string()
    }
}

/// Decode a session value written by [`encode_session`]. Dual-read: a bare UUID is
/// a legacy/normal full session; a JSON object carries the enrolment-only flag.
fn decode_session(s: &str) -> Option<(Uuid, bool)> {
    if let Ok(u) = Uuid::parse_str(s) {
        return Some((u, false));
    }
    #[derive(serde::Deserialize)]
    struct Sv {
        u: Uuid,
        #[serde(default)]
        e: bool,
    }
    serde_json::from_str::<Sv>(s).ok().map(|sv| (sv.u, sv.e))
}

/// Mint a session bound to `user_id`, TTL = `session_ttl_secs`. Returns the token
/// to set as the cookie value. `enroll_only` marks a restricted MFA-enrolment
/// session (D6); pass `false` for a normal session.
pub async fn issue_session(
    state: &AppState,
    user_id: Uuid,
    ttl_secs: u64,
    enroll_only: bool,
) -> Result<String, AppError> {
    let token = new_token();
    let mut conn = state.redis.get().await?;
    redis::cmd("SET")
        .arg(session_key(&token))
        .arg(encode_session(user_id, enroll_only))
        .arg("EX")
        .arg(ttl_secs)
        .query_async::<()>(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis SET failed: {e}")))?;
    // Track the token under the per-user reverse index (best-effort; a failure here
    // must not fail login — it only weakens force-logout, which also has the DB
    // `deactivated_at` lazy gate behind it).
    let _ = redis::cmd("SADD")
        .arg(user_sessions_key(user_id))
        .arg(&token)
        .query_async::<i64>(&mut conn)
        .await;
    let _ = redis::cmd("EXPIRE")
        .arg(user_sessions_key(user_id))
        .arg(ttl_secs)
        .query_async::<i64>(&mut conn)
        .await;
    Ok(token)
}

/// Force-revoke EVERY session of a user (SCIM/admin deactivate, local mode). Drops
/// each tracked token key and clears the reverse index. Idempotent; best-effort per
/// key. The DB `deactivated_at` gate remains the durable backstop.
pub async fn revoke_all_for_user(state: &AppState, user_id: Uuid) -> Result<(), AppError> {
    let mut conn = state.redis.get().await?;
    let tokens: Vec<String> = redis::cmd("SMEMBERS")
        .arg(user_sessions_key(user_id))
        .query_async(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis SMEMBERS failed: {e}")))?;
    for token in &tokens {
        let _ = redis::cmd("DEL")
            .arg(session_key(token))
            .query_async::<i64>(&mut conn)
            .await;
    }
    let _ = redis::cmd("DEL")
        .arg(user_sessions_key(user_id))
        .query_async::<i64>(&mut conn)
        .await;
    Ok(())
}

/// Resolve a session token to `(user_id, enroll_only)`, if still live.
pub async fn lookup_session(state: &AppState, token: &str) -> Result<Option<(Uuid, bool)>, AppError> {
    let mut conn = state.redis.get().await?;
    let val: Option<String> = redis::cmd("GET")
        .arg(session_key(token))
        .query_async(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis GET failed: {e}")))?;
    Ok(val.as_deref().and_then(decode_session))
}

/// Revoke a session (logout). Idempotent. Also prunes the per-user reverse index.
pub async fn revoke_session(state: &AppState, token: &str) -> Result<(), AppError> {
    let mut conn = state.redis.get().await?;
    // Read the owner first so we can drop the reverse-index member too (best-effort).
    let owner: Option<String> = redis::cmd("GET")
        .arg(session_key(token))
        .query_async(&mut conn)
        .await
        .ok()
        .flatten();
    redis::cmd("DEL")
        .arg(session_key(token))
        .query_async::<i64>(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis DEL failed: {e}")))?;
    if let Some((uid, _)) = owner.as_deref().and_then(decode_session) {
        let _ = redis::cmd("SREM")
            .arg(user_sessions_key(uid))
            .arg(token)
            .query_async::<i64>(&mut conn)
            .await;
    }
    Ok(())
}

/// Argon2id hash of a plaintext password (PHC string, embeds the random salt).
pub fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Other(anyhow::anyhow!("password hashing failed: {e}")))
}

/// Verify a plaintext password against a stored PHC hash. False on any parse or
/// mismatch (never panics).
pub fn verify_password(stored_hash: &str, password: &str) -> bool {
    match PasswordHash::new(stored_hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// True when there is no active admin yet, so the first registrant should be made
/// `client_admin` (Open WebUI / LibreChat pattern). A deactivated admin does not
/// count.
pub async fn first_user_is_admin(pg: &PgPool) -> Result<bool, AppError> {
    let exists: bool = sqlx::query_scalar!(
        r#"SELECT EXISTS(
            SELECT 1 FROM users
            WHERE role IN ('super_admin', 'client_admin') AND deactivated_at IS NULL
        ) AS "e!""#
    )
    .fetch_one(pg)
    .await?;
    Ok(!exists)
}

/// True if a (non-deactivated or deactivated) user already holds this email.
pub async fn email_taken(pg: &PgPool, email: &str) -> Result<bool, AppError> {
    let exists: bool = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM users WHERE email = $1) AS "e!""#,
        email
    )
    .fetch_one(pg)
    .await?;
    Ok(exists)
}

/// Create a local user (with an Argon2id hash) and audit `user.registered`. The
/// password itself is never logged or audited. Returns the new user id.
pub async fn register_user(
    pg: &PgPool,
    email: &str,
    display_name: &str,
    password_hash: &str,
    role: PlatformRole,
) -> Result<Uuid, AppError> {
    let id = Uuid::now_v7();
    let mut tx = pg.begin().await?;

    sqlx::query!(
        r#"INSERT INTO users (id, display_name, email, role, password_hash, last_seen_at)
           VALUES ($1, $2, $3, $4, $5, now())"#,
        id,
        display_name,
        email,
        role as PlatformRole,
        password_hash,
    )
    .execute(&mut *tx)
    .await?;

    let mut event = AuditEvent::action("user.registered", role.as_str());
    event.actor_user_id = Some(id);
    event.resource_type = Some("user".into());
    event.resource_id = Some(id);
    event.payload = Some(serde_json::json!({ "email": email, "role": role.as_str() }));
    audit::append_with(&mut tx, &event).await?;

    tx.commit().await?;
    Ok(id)
}

/// A local user's credential row, fetched by email for login.
pub struct Credential {
    pub id: Uuid,
    pub password_hash: Option<String>,
    pub deactivated: bool,
}

/// Look up the login credential for an email (None = no such user).
pub async fn credential_by_email(pg: &PgPool, email: &str) -> Result<Option<Credential>, AppError> {
    let row = sqlx::query!(
        r#"SELECT id, password_hash, (deactivated_at IS NOT NULL) AS "deactivated!"
           FROM users WHERE email = $1"#,
        email
    )
    .fetch_optional(pg)
    .await?;
    Ok(row.map(|r| Credential {
        id: r.id,
        password_hash: r.password_hash,
        deactivated: r.deactivated,
    }))
}

/// Set a user's password hash (password change).
pub async fn set_password_hash(pg: &PgPool, user_id: Uuid, hash: &str) -> Result<(), AppError> {
    sqlx::query!(
        r#"UPDATE users SET password_hash = $2 WHERE id = $1"#,
        user_id,
        hash
    )
    .execute(pg)
    .await?;
    Ok(())
}

/// Read the `pai_session` value from the request's `Cookie` header, if present.
pub fn session_cookie(parts: &Parts) -> Option<String> {
    let header = parts
        .headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())?;
    cookie_value(header, SESSION_COOKIE)
}

/// Extract a single cookie value from a `Cookie` header string.
fn cookie_value(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k.trim() == name {
            Some(v.trim().to_string())
        } else {
            None
        }
    })
}

/// Paths an enrolment-only session may still reach: the MFA
/// setup/confirm wizard, logout, and the identity/config reads the SPA needs to
/// render that wizard. Everything else is refused until the user enrols a factor.
fn enroll_only_allowed(path: &str) -> bool {
    matches!(
        path,
        "/api/auth/mfa/setup"
            | "/api/auth/mfa/confirm"
            | "/api/auth/mfa/status"
            | "/api/auth/logout"
            | "/api/whoami"
            | "/api/auth/config"
    )
}

/// The Core local-login [`AuthProvider`]: cookie → Redis session → user id →
/// [`super::load_context`] (which enforces the deactivation check, shared with the
/// Keycloak path). No middleware layer needed — this is the sole local-mode
/// per-request chokepoint, so the enrolment-only gate (D6) lives here.
pub struct LocalAuthProvider;

#[async_trait::async_trait]
impl ext::AuthProvider for LocalAuthProvider {
    async fn authenticate(
        &self,
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<AuthContext, AppError> {
        let token = session_cookie(parts)
            .ok_or_else(|| AppError::Unauthorized("no session".into()))?;
        let (user_id, enroll_only) = lookup_session(state, &token)
            .await?
            .ok_or_else(|| AppError::Unauthorized("session expired".into()))?;
        // Fence an enrolment-only session to the setup surface before it can touch
        // any real endpoint (defence-in-depth: the SPA also redirects into the
        // wizard, but the gate is enforced here, not just in the UI).
        if enroll_only && !enroll_only_allowed(parts.uri.path()) {
            return Err(AppError::Forbidden("mfa enrolment required".into()));
        }
        let mut ctx = crate::auth::load_context(&state.pg, user_id).await?;
        ctx.mfa_enroll_only = enroll_only;
        Ok(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_roundtrips() {
        let h = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password(&h, "correct horse battery staple"));
        assert!(!verify_password(&h, "wrong password"));
        assert!(!verify_password("not-a-phc-string", "x"));
    }

    #[test]
    fn cookie_value_parses_pai_session() {
        assert_eq!(cookie_value("a=1; pai_session=tok; b=2", "pai_session").as_deref(), Some("tok"));
        assert_eq!(cookie_value("pai_session=tok", "pai_session").as_deref(), Some("tok"));
        assert_eq!(cookie_value("other=1", "pai_session"), None);
    }
}
