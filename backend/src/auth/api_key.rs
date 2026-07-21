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

//! Bearer API keys: the credential an external application presents to act as a
//! user against this instance.
//!
//! This is a **separate authentication surface** from the browser session. The
//! `AuthProvider` slot on `AppState` is the seam for interactive login (cookie
//! or OIDC) and an edition may replace it wholesale; a machine credential must
//! keep working regardless of which provider is installed, and must never be
//! reachable from a browser's ambient cookie. So the key extractor reads the
//! `Authorization` header itself and never consults `state.auth`.
//!
//! A key carries the **full rights of its owner**. There are no per-key scopes:
//! every check downstream (RBAC, ACLs, retrieval filters) runs against the
//! owner's context exactly as it would for that person in the UI.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::AuthContext;
use crate::http::v1::error::ApiError;
use crate::state::AppState;

/// The recognisable prefix every platform key carries. `sk-` maximises client
/// compatibility (SDKs and scanners assume it); the middle segment makes a
/// leaked key attributable to this platform at a glance.
pub const TOKEN_PREFIX: &str = "sk-fosnie-";

/// How much of the secret is kept in clear so a key is identifiable in a list.
/// Twelve characters covers the prefix plus a couple of random ones: enough to
/// tell three keys apart, far too little to help an attacker.
const DISPLAY_PREFIX_LEN: usize = 12;

/// A fresh key: `(plaintext shown once, SHA-256 for storage, display prefix)`.
///
/// 256 bits of CSPRNG, matching the local-session token — the secret is the
/// sole credential, so it is full random rather than a UUID.
pub fn mint() -> (String, Vec<u8>, String) {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    let token = format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(b));
    let hash = hash_token(&token);
    let display_prefix: String = token.chars().take(DISPLAY_PREFIX_LEN).collect();
    (token, hash, display_prefix)
}

/// The stored form of a token. Raw digest bytes, not hex: the column is `bytea`
/// and the comparison is on the index, so there is nothing to gain from a text
/// encoding.
pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Resolve a presented token to its key id and the owner's auth context.
///
/// Expiry and revocation are enforced in the query; deactivated owners are
/// rejected by `load_context`. Every failure returns the same message: which of
/// the four reasons applied is not the caller's business.
pub async fn resolve(state: &AppState, token: &str) -> Result<(Uuid, AuthContext), ApiError> {
    if !token.starts_with(TOKEN_PREFIX) {
        return Err(invalid_key());
    }
    let hash = hash_token(token);
    let row = sqlx::query!(
        "SELECT id, user_id FROM api_keys \
         WHERE token_hash = $1 AND revoked_at IS NULL \
           AND (expires_at IS NULL OR expires_at > now())",
        &hash
    )
    .fetch_optional(&state.pg)
    .await
    .map_err(ApiError::from)?
    .ok_or_else(invalid_key)?;

    let ctx = crate::auth::load_context(&state.pg, row.user_id)
        .await
        .map_err(|_| invalid_key())?;
    Ok((row.id, ctx))
}

fn invalid_key() -> ApiError {
    ApiError::unauthorised(
        "invalid API key — provide a valid key as 'Authorization: Bearer <key>'",
    )
}

/// Record that a key was used, at most once a minute.
///
/// `last_used_at` is bookkeeping for the owner ("is this key still in use?"),
/// not an audit record, so a coarse write is right: throttling through the
/// existing fixed-window limiter avoids an `UPDATE` per request without adding
/// a second Redis primitive. Fire-and-forget; a lost tick is immaterial.
pub async fn touch_last_used(state: &AppState, key_id: Uuid) {
    let fresh =
        crate::cache::rate_limit_ok(&state.redis, &format!("apikey-touch:{key_id}"), 1, 60).await;
    if !fresh {
        return;
    }
    let pg = state.pg.clone();
    tokio::spawn(async move {
        let _ = sqlx::query!("UPDATE api_keys SET last_used_at = now() WHERE id = $1", key_id)
            .execute(&pg)
            .await;
    });
}

/// Extractor for the OpenAI-compatible surface: `Authorization: Bearer sk-fosnie-…`
/// to `(key id, owner context)`.
///
/// Deliberately not an `AuthProvider` implementation — see the module docs.
pub struct ApiKeyAuth(pub AuthContext, pub Uuid);

impl FromRequestParts<AppState> for ApiKeyAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let raw = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                ApiError::unauthorised(
                    "missing API key — provide one as 'Authorization: Bearer <key>'",
                )
            })?;
        let token = raw
            .strip_prefix("Bearer ")
            .or_else(|| raw.strip_prefix("bearer "))
            .unwrap_or(raw)
            .trim();

        let (key_id, ctx) = resolve(state, token).await?;
        touch_last_used(state, key_id).await;
        Ok(ApiKeyAuth(ctx, key_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_key_is_prefixed_and_hashes_stably() {
        let (token, hash, prefix) = mint();
        assert!(token.starts_with(TOKEN_PREFIX));
        assert_eq!(prefix.len(), DISPLAY_PREFIX_LEN);
        assert!(token.starts_with(&prefix));
        assert_eq!(hash, hash_token(&token));
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn two_keys_differ() {
        let (a, _, _) = mint();
        let (b, _, _) = mint();
        assert_ne!(a, b);
    }
}
