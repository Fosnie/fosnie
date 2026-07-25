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

//! Keycloak token validation — the only place `axum-keycloak-auth` is touched.
//!
//! [`auth_layer`] is the tower layer that validates the Bearer JWT against
//! Keycloak's JWKS and injects the decoded token. [`AuthUser`] is the extractor
//! the rest of the codebase uses: it reads that token, normalises roles,
//! auto-provisions the user, and enforces revocation (a deactivated user is
//! rejected — the same path gates WebSocket upgrades, so revocation applies to
//! WS too).

use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_keycloak_auth::decode::{Email, KeycloakToken, Profile};
use axum_keycloak_auth::extract::{
    AuthHeaderTokenExtractor, QueryParamTokenExtractor, TokenExtractor,
};
use axum_keycloak_auth::instance::{KeycloakAuthInstance, KeycloakConfig};
use axum_keycloak_auth::layer::KeycloakAuthLayer;
use axum_keycloak_auth::role::KeycloakRole;
use axum_keycloak_auth::{KeycloakAuthStatus, NonEmpty, PassthroughMode, Url};
use uuid::Uuid;

/// Token `extra` claims we care about: the standard profile + email (as
/// [`ProfileAndEmail`](axum_keycloak_auth::decode::ProfileAndEmail) carried), plus
/// the brokered-IdP `groups` claim (surfaced by the realm's `groups` mapper). An
/// absent `groups` claim deserialises to an empty vec, so non-brokered logins are
/// unaffected.
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ProfileEmailGroups {
    #[serde(flatten)]
    pub profile: Profile,
    #[serde(flatten)]
    pub email: Email,
    #[serde(default)]
    pub groups: Vec<String>,
}

/// The decoded Keycloak token, parameterised with our groups-aware extra claims.
pub type KcToken = KeycloakToken<String, ProfileEmailGroups>;
/// The Bearer-JWT validation tower layer, parameterised likewise.
pub type KcLayer = KeycloakAuthLayer<String, ProfileEmailGroups>;
/// The pass-mode status wrapper (WebSocket transport).
pub type KcStatus = KeycloakAuthStatus<String, ProfileEmailGroups>;

use crate::auth::provisioning::{self, ProvisionClaims};
use crate::auth::{normalise_roles, AuthContext};
use crate::config::KeycloakConfig as PaiKeycloakConfig;
use crate::error::AppError;
use crate::ext;
use crate::state::AppState;

/// Build the Keycloak instance (lazy discovery — does not fail if Keycloak is
/// down at construction, so break-glass stays independent).
pub fn build_instance(cfg: &PaiKeycloakConfig) -> Result<KeycloakAuthInstance, AppError> {
    let server = Url::parse(&cfg.url)
        .map_err(|e| AppError::Config(format!("invalid keycloak url {:?}: {e}", cfg.url)))?;
    Ok(KeycloakAuthInstance::new(
        KeycloakConfig::builder()
            .server(server)
            .realm(cfg.realm.clone())
            .build(),
    ))
}

/// The Bearer-JWT validation layer. `expected_audience` must appear in the
/// token's `aud` (the realm's audience mapper adds the client id).
pub fn auth_layer(
    instance: Arc<KeycloakAuthInstance>,
    expected_audience: String,
) -> KcLayer {
    KeycloakAuthLayer::<String, ProfileEmailGroups>::builder()
        .instance(instance)
        .passthrough_mode(PassthroughMode::Block)
        .persist_raw_claims(false)
        .expected_audiences(vec![expected_audience])
        .build()
}

/// Pass-mode variant for the WebSocket upgrade: injects the token if a valid
/// JWT is present but does not block, so a `?resume=` reconnect (no JWT) can be
/// handled by the handler instead.
pub fn auth_layer_passthrough(
    instance: Arc<KeycloakAuthInstance>,
    expected_audience: String,
) -> KcLayer {
    // Browsers cannot set an Authorization header on a WebSocket, so accept the
    // JWT from `?token=` as well as the header.
    let extractors: NonEmpty<Arc<dyn TokenExtractor>> = NonEmpty {
        head: Arc::new(AuthHeaderTokenExtractor {}),
        tail: vec![Arc::new(QueryParamTokenExtractor::default())],
    };
    KeycloakAuthLayer::<String, ProfileEmailGroups>::builder()
        .instance(instance)
        .passthrough_mode(PassthroughMode::Pass)
        .persist_raw_claims(false)
        .expected_audiences(vec![expected_audience])
        .token_extractors(extractors)
        .build()
}

/// Turn a validated Keycloak token into an [`AuthContext`]: normalise roles,
/// auto-provision the user, and enforce revocation (deactivated → 401). Shared
/// by the [`AuthUser`] extractor and the WebSocket upgrade.
pub async fn context_from_token(
    token: &KcToken,
    pg: &sqlx::PgPool,
) -> Result<AuthContext, AppError> {
    let sub = Uuid::parse_str(&token.subject)
        .map_err(|_| AppError::Unauthorized("token subject is not a UUID".into()))?;
    let roles = role_strings(token);
    let role = normalise_roles(&roles);
    let email = token.extra.email.email.clone();
    let display_name = token
        .extra
        .profile
        .full_name
        .clone()
        .unwrap_or_else(|| token.extra.profile.preferred_username.clone());

    // The canonical user id may differ from `sub` when this login links to a
    // directory-created (SCIM) user by verified email — provisioning returns it.
    // The `groups` claim (brokered-IdP groups) drives JIT group sync when enabled.
    let user_id = provisioning::upsert_from_claims(
        pg,
        &ProvisionClaims {
            sub,
            email: email.clone(),
            display_name: display_name.clone(),
            role,
            groups: token.extra.groups.clone(),
        },
    )
    .await?;

    if is_deactivated(pg, user_id).await? {
        return Err(AppError::Unauthorized("account deactivated".into()));
    }

    Ok(AuthContext {
        user_id: Some(user_id),
        email: Some(email),
        display_name: Some(display_name),
        role,
        break_glass: false,
        mfa_enroll_only: false,
    })
}

/// Realm + client role strings, flattened (the normaliser only cares about the
/// names).
fn role_strings(token: &KcToken) -> Vec<String> {
    token
        .roles
        .iter()
        .map(|r| match r {
            KeycloakRole::Realm { role } => role.clone(),
            KeycloakRole::Client { role, .. } => role.clone(),
        })
        .collect()
}

/// The Core [`ext::AuthProvider`]: maps a Keycloak-authenticated request to an
/// [`AuthContext`]. The trait lives in `ext` (the extension surface); this
/// Keycloak-specific default stays in the auth module. A future `fosnie-enterprise`
/// crate (or `LocalAuth`) injects its own provider via
/// [`crate::state::AppStateBuilder::with_auth`].
pub struct KeycloakAuthProvider;

#[async_trait::async_trait]
impl ext::AuthProvider for KeycloakAuthProvider {
    async fn authenticate(
        &self,
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<AuthContext, AppError> {
        // The HTTP `auth_layer` (Block mode) injects a bare `KeycloakToken`; the
        // WebSocket `auth_layer_passthrough` (Pass mode) wraps it in a
        // `KeycloakAuthStatus::Success`. Accept either so this one provider serves
        // both transports identically.
        let token = parts
            .extensions
            .get::<KcToken>()
            .or_else(|| {
                match parts.extensions.get::<KcStatus>() {
                    Some(KeycloakAuthStatus::Success(t)) => Some(t),
                    _ => None,
                }
            })
            .ok_or_else(|| AppError::Unauthorized("missing or invalid token".into()))?;
        context_from_token(token, &state.pg).await
    }
}

/// Authenticated user. Behind [`auth_layer`] the token is guaranteed present and
/// valid; this extractor routes the request through the configured
/// [`ext::AuthProvider`] slot (`state.auth`) to produce the [`AuthContext`]. The
/// external shape `AuthUser(pub AuthContext)` is unchanged, so handlers are
/// untouched.
pub struct AuthUser(pub AuthContext);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match state.auth.authenticate(parts, state).await {
            Ok(ctx) => Ok(AuthUser(ctx)),
            Err(e) => {
                // A paired desktop client has no browser session, so the
                // configured provider cannot authenticate it. It presents a
                // device token instead, which carries the owner's rights over
                // this same surface. The session is always tried first and
                // wins; a request with no platform token of ours takes the
                // provider's own error untouched, so behaviour without such a
                // header is unchanged.
                match crate::auth::device::try_device_auth(parts, state).await {
                    Some((ctx, device_id)) => {
                        parts.extensions.insert(crate::auth::device::DeviceAuth(device_id));
                        Ok(AuthUser(ctx))
                    }
                    None => Err(e),
                }
            }
        }
    }
}

async fn is_deactivated(pool: &sqlx::PgPool, id: Uuid) -> Result<bool, AppError> {
    let row = sqlx::query_scalar!(
        r#"SELECT (deactivated_at IS NOT NULL) AS "deactivated!" FROM users WHERE id = $1"#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.unwrap_or(false))
}
