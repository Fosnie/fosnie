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

//! Persistent storage backends for the MCP OAuth flow, plugging into the SDK's
//! `AuthorizationManager`.
//!
//! - [`PgCredentialStore`] persists one principal's encrypted tokens in a
//!   `mcp_oauth_connections` row. The credential-store contract is single-slot (its
//!   `load` takes no key), so an instance is bound to exactly one connection id. Tokens
//!   are encrypted at rest with the deployment message key; storing a token with no key
//!   configured is refused outright rather than silently written in plaintext.
//! - [`RedisStateStore`] parks the short-lived PKCE verifier + CSRF token for an
//!   in-flight authorisation, keyed by the CSRF token, with a TTL that expires abandoned
//!   flows. Its `delete` is a real delete (not a read-and-delete) because the contract
//!   separates load from delete and the SDK calls delete itself after a code exchange.

use std::time::Duration as StdDuration;

use async_trait::async_trait;
use deadpool_redis::Pool as RedisPool;
use oauth2::{basic::BasicTokenType, AccessToken, RefreshToken, Scope, TokenResponse};
use rmcp::transport::auth::{
    AuthError, CredentialStore, OAuthTokenResponse, StateStore, StoredAuthorizationState,
    StoredCredentials, VendorExtraTokenFields,
};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{cache, crypto};

/// TTL for a parked authorisation state — an abandoned flow expires rather than lingers.
const STATE_TTL_SECS: u64 = 600;

fn state_key(csrf: &str) -> String {
    format!("mcp:oauth:state:{csrf}")
}

fn internal(e: impl std::fmt::Display) -> AuthError {
    AuthError::InternalError(e.to_string())
}

/// Per-connection OAuth credential store backed by a `mcp_oauth_connections` row.
pub struct PgCredentialStore {
    pg: PgPool,
    connection_id: Uuid,
    /// Whether the deployment has an encryption key. Storing a token without one is
    /// refused (no plaintext-at-rest fallback).
    has_message_key: bool,
}

impl PgCredentialStore {
    pub fn new(pg: PgPool, connection_id: Uuid, has_message_key: bool) -> Self {
        Self { pg, connection_id, has_message_key }
    }
}

#[async_trait]
impl CredentialStore for PgCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let row = sqlx::query!(
            r#"SELECT c.access_token_enc, c.refresh_token_enc, c.expires_at, c.scopes,
                      cl.client_id
                 FROM mcp_oauth_connections c
                 JOIN mcp_oauth_clients cl ON cl.id = c.oauth_client_id
                WHERE c.id = $1 AND c.status IN ('active', 'reauth_required')"#,
            self.connection_id
        )
        .fetch_optional(&self.pg)
        .await
        .map_err(internal)?;

        let Some(row) = row else { return Ok(None) };
        // A pending row exists before any token — treat as "no credentials".
        let Some(access_enc) = row.access_token_enc else { return Ok(None) };

        let access = crypto::decrypt_at_rest(&access_enc).map_err(internal)?;
        let mut token = OAuthTokenResponse::new(
            AccessToken::new(access),
            BasicTokenType::Bearer,
            VendorExtraTokenFields::default(),
        );
        if let Some(refresh_enc) = row.refresh_token_enc {
            let refresh = crypto::decrypt_at_rest(&refresh_enc).map_err(internal)?;
            token.set_refresh_token(Some(RefreshToken::new(refresh)));
        }
        // Reconstruct the remaining lifetime from the absolute expiry we persisted, so the
        // manager's refresh-buffer arithmetic (expires_in vs received_at) comes out right.
        let mut received_at = None;
        if let Some(exp) = row.expires_at {
            let now = OffsetDateTime::now_utc();
            let remaining = (exp - now).whole_seconds().max(0) as u64;
            token.set_expires_in(Some(&StdDuration::from_secs(remaining)));
            received_at = Some(now.unix_timestamp() as u64);
        }
        if !row.scopes.is_empty() {
            token.set_scopes(Some(row.scopes.iter().cloned().map(Scope::new).collect()));
        }

        Ok(Some(StoredCredentials::new(
            row.client_id,
            Some(token),
            row.scopes,
            received_at,
        )))
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        if !self.has_message_key {
            return Err(AuthError::InternalError(
                "server encryption key not configured; refusing to store an MCP OAuth token \
                 (set message_encryption_key)"
                    .into(),
            ));
        }
        // Nothing to persist yet (a configure-only save). Leave the pending row as-is.
        let Some(token) = credentials.token_response.as_ref() else {
            return Ok(());
        };

        let access = token.access_token().secret().to_string();
        let access_enc = crypto::encrypt_at_rest(&access).map_err(internal)?;
        let refresh_enc = match token.refresh_token() {
            Some(r) => Some(crypto::encrypt_at_rest(r.secret()).map_err(internal)?),
            None => None,
        };
        let expires_at = token
            .expires_in()
            .map(|d| OffsetDateTime::now_utc() + time::Duration::seconds(d.as_secs() as i64));
        let scopes = credentials.granted_scopes;

        // COALESCE the refresh token: an authorisation server legitimately omits it on a
        // rotation, and we must keep the one we already hold rather than null it out.
        sqlx::query!(
            r#"UPDATE mcp_oauth_connections
                  SET access_token_enc  = $2,
                      refresh_token_enc = COALESCE($3, refresh_token_enc),
                      expires_at        = $4,
                      scopes            = $5,
                      status            = 'active',
                      last_used_at      = now()
                WHERE id = $1"#,
            self.connection_id,
            access_enc,
            refresh_enc,
            expires_at,
            &scopes
        )
        .execute(&self.pg)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), AuthError> {
        // Drop the ciphertext and fall out of 'active' (the row CHECK forbids an active
        // row without a token). We move to 'reauth_required' rather than 'revoked' —
        // an explicit user revoke is a separate, deliberate action on its own path.
        sqlx::query!(
            r#"UPDATE mcp_oauth_connections
                  SET access_token_enc = NULL,
                      refresh_token_enc = NULL,
                      status = 'reauth_required'
                WHERE id = $1"#,
            self.connection_id
        )
        .execute(&self.pg)
        .await
        .map_err(internal)?;
        Ok(())
    }
}

/// Redis-backed store for in-flight authorisation state (PKCE verifier + CSRF token),
/// keyed by the CSRF token. Shared across all flows (the contract is keyed, so one
/// instance serves every connection).
pub struct RedisStateStore {
    redis: RedisPool,
}

impl RedisStateStore {
    pub fn new(redis: RedisPool) -> Self {
        Self { redis }
    }
}

#[async_trait]
impl StateStore for RedisStateStore {
    async fn save(&self, csrf_token: &str, state: StoredAuthorizationState) -> Result<(), AuthError> {
        let json = serde_json::to_string(&state).map_err(internal)?;
        cache::kv_set_ex(&self.redis, &state_key(csrf_token), &json, STATE_TTL_SECS)
            .await
            .map_err(internal)?;
        Ok(())
    }

    async fn load(&self, csrf_token: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        // A peek, not a consume — the SDK calls `delete` itself after a successful exchange.
        let Some(json) = cache::kv_get(&self.redis, &state_key(csrf_token)).await.map_err(internal)?
        else {
            return Ok(None);
        };
        let state: StoredAuthorizationState = serde_json::from_str(&json).map_err(internal)?;
        Ok(Some(state))
    }

    async fn delete(&self, csrf_token: &str) -> Result<(), AuthError> {
        cache::kv_del(&self.redis, &state_key(csrf_token)).await.map_err(internal)?;
        Ok(())
    }
}
