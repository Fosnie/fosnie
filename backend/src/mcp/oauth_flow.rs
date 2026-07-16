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

//! Orchestration of the MCP OAuth 2.1 flow: discovery (admin preview + issuer approval),
//! dynamic client registration, the per-user authorisation-code flow, and building the
//! authenticated runtime connection.
//!
//! This module and `client` are the two places that touch the MCP SDK's `auth` module;
//! the SSRF/protocol policy it enforces lives in `oauth_policy`, the persistent stores in
//! `oauth_store`. Discovery is admin-only and interactive: it validates every discovered
//! endpoint and persists nothing. The trust anchor is the admin saving the issuer's
//! validated metadata; the connect path thereafter feeds that saved metadata straight to
//! the SDK and never re-discovers.

use std::sync::Arc;

use rmcp::transport::auth::{AuthorizationManager, AuthorizationMetadata, OAuthClientConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::auth::AuthContext;
use crate::cache;
use crate::error::{AppError, Result};
use crate::integrations::{self, ConnectorKind};
use crate::mcp::client::{self, hardened_client, McpConn};
use crate::mcp::oauth_policy;
use crate::mcp::oauth_store::{PgCredentialStore, RedisStateStore};
use crate::state::AppState;

fn oauth_err(e: rmcp::transport::auth::AuthError) -> AppError {
    AppError::Other(anyhow::anyhow!("MCP OAuth: {e}"))
}

fn flow_key(csrf: &str) -> String {
    format!("mcp:oauth:flow:{csrf}")
}

/// The single, fixed redirect URI for every server and every user. Identity is carried
/// by the parked flow record keyed on `state`, never by the request, so one URI suffices.
pub fn callback_url(state: &AppState) -> String {
    format!(
        "{}/api/mcp/oauth/callback",
        state.boot.server.public_url.trim_end_matches('/')
    )
}

// ── Discovery ────────────────────────────────────────────────────────────────

/// The validated result of probing a server's authorisation metadata. Held only in
/// memory; persisted to `mcp_oauth_clients` only when an admin saves the client config.
pub struct ValidatedDiscovery {
    pub metadata: AuthorizationMetadata,
    pub issuer: String,
    pub scopes_supported: Vec<String>,
    pub dcr_available: bool,
    pub s256_ok: bool,
    pub warnings: Vec<String>,
}

/// Run discovery against `server_url` and validate every endpoint it names. Persists
/// nothing. Used by both the admin preview and the issuer-approval write.
pub async fn run_discovery(
    state: &AppState,
    ctx: &AuthContext,
    server_url: &str,
    allowed_issuer_origin: Option<&str>,
) -> Result<ValidatedDiscovery> {
    integrations::guard_egress(state, ctx, ConnectorKind::Mcp).await?;

    let normalised = oauth_policy::normalise_resource_url(server_url);
    let mut mgr = AuthorizationManager::new(&normalised).await.map_err(oauth_err)?;
    mgr.with_client(hardened_client()?).map_err(oauth_err)?;
    let metadata = mgr.discover_metadata().await.map_err(oauth_err)?;

    // Validate every URL the metadata carries before we would ever send it anything.
    let mut endpoints: Vec<String> = vec![
        metadata.authorization_endpoint.clone(),
        metadata.token_endpoint.clone(),
    ];
    if let Some(r) = metadata.registration_endpoint.clone() {
        endpoints.push(r);
    }
    if let Some(i) = metadata.issuer.clone() {
        endpoints.push(i);
    }
    if let Some(rev) = metadata
        .additional_fields
        .get("revocation_endpoint")
        .and_then(Value::as_str)
    {
        endpoints.push(rev.to_string());
    }
    for ep in &endpoints {
        oauth_policy::validate_discovered_endpoint(ep, server_url, allowed_issuer_origin).await?;
    }

    // PKCE S256 must be advertised (the SDK is silent when it is absent).
    let s256 = oauth_policy::enforce_pkce_s256(metadata.code_challenge_methods_supported.as_deref());
    let s256_ok = s256.is_ok();
    let mut warnings = Vec::new();

    let issuer = match metadata.issuer.clone() {
        Some(i) => i,
        None => {
            // RFC 8414 requires an issuer; fall back to the authorisation-endpoint origin
            // so the admin still has a concrete origin to approve, and flag it.
            warnings.push("authorisation server metadata did not include an issuer; using the authorization_endpoint origin".to_string());
            oauth_policy::origin_str(&metadata.authorization_endpoint).ok_or_else(|| {
                AppError::Validation("discovered metadata has no issuer and no usable authorization_endpoint".into())
            })?
        }
    };
    if !s256_ok {
        warnings.push("authorisation server does not advertise PKCE S256; one-click connect will be refused".to_string());
    }

    Ok(ValidatedDiscovery {
        scopes_supported: metadata.scopes_supported.clone().unwrap_or_default(),
        dcr_available: metadata.registration_endpoint.is_some(),
        s256_ok,
        issuer,
        warnings,
        metadata,
    })
}

// ── Dynamic client registration (RFC 7591) + management creds (RFC 7592) ───────

/// What our own DCR POST captured. We write the POST ourselves rather than using the
/// SDK's helper so we keep the RFC 7592 management URI + token, letting a DCR client be
/// updated or deleted at the authorisation server later instead of orphaned.
pub struct DcrRegistration {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub registration_client_uri: Option<String>,
    pub registration_access_token: Option<String>,
}

#[derive(Serialize)]
struct DcrRequest<'a> {
    client_name: &'a str,
    redirect_uris: Vec<&'a str>,
    grant_types: Vec<&'a str>,
    response_types: Vec<&'a str>,
    token_endpoint_auth_method: &'a str,
    application_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

/// Register a client at the authorisation server's registration endpoint.
pub async fn register_dcr(
    metadata: &AuthorizationMetadata,
    callback: &str,
    scopes: &[String],
) -> Result<DcrRegistration> {
    let endpoint = metadata.registration_endpoint.as_deref().ok_or_else(|| {
        AppError::Validation("this authorisation server does not support dynamic client registration".into())
    })?;
    let req = DcrRequest {
        client_name: "Fosnie",
        redirect_uris: vec![callback],
        grant_types: vec!["authorization_code", "refresh_token"],
        response_types: vec!["code"],
        // A DCR-registered client is public: it authenticates with PKCE, not a secret.
        token_endpoint_auth_method: "none",
        application_type: "web",
        scope: if scopes.is_empty() { None } else { Some(scopes.join(" ")) },
    };
    let resp = hardened_client()?
        .post(endpoint)
        .json(&req)
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("DCR request failed: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Validation(format!("dynamic client registration failed (HTTP {status}): {body}")));
    }
    let json: Value = resp
        .json()
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("DCR response parse failed: {e}")))?;
    let client_id = json
        .get("client_id")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Validation("DCR response missing client_id".into()))?
        .to_string();
    let client_secret = json
        .get("client_secret")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(DcrRegistration {
        client_id,
        client_secret,
        registration_client_uri: json
            .get("registration_client_uri")
            .and_then(Value::as_str)
            .map(str::to_string),
        registration_access_token: json
            .get("registration_access_token")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

// ── The authorisation-code flow ────────────────────────────────────────────────

/// A persisted OAuth client row (issuer + configured client + approved metadata), with the
/// client secret already decrypted. Loaded from `mcp_oauth_clients`.
pub struct OAuthClientRow {
    pub id: Uuid,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
    pub metadata: Value,
}

/// Load the persisted, approved OAuth client for a server, decrypting the client secret.
pub async fn load_oauth_client_row(
    pg: &sqlx::PgPool,
    server_id: Uuid,
) -> Result<Option<OAuthClientRow>> {
    let row = sqlx::query!(
        r#"SELECT id, issuer, client_id, client_secret_enc, scopes, metadata
             FROM mcp_oauth_clients WHERE mcp_server_id = $1"#,
        server_id
    )
    .fetch_optional(pg)
    .await?;
    let Some(r) = row else { return Ok(None) };
    let client_secret = match r.client_secret_enc {
        Some(enc) => Some(crate::crypto::decrypt_at_rest(&enc)?),
        None => None,
    };
    Ok(Some(OAuthClientRow {
        id: r.id,
        issuer: r.issuer,
        client_id: r.client_id,
        client_secret,
        scopes: r.scopes,
        metadata: r.metadata,
    }))
}

/// Our own record parked alongside the SDK's PKCE state, keyed by the same `state`
/// (CSRF) value. Identity travels here, server-side, never on the redirect URL.
#[derive(Serialize, Deserialize)]
struct FlowRecord {
    user_id: Option<Uuid>,
    mcp_server_id: Uuid,
    oauth_client_id: Uuid,
    connection_id: Uuid,
    redirect_after: Option<String>,
}

/// Outcome of a completed callback, for the redirect back to the SPA.
pub struct CallbackOutcome {
    pub slug: String,
    pub redirect_after: Option<String>,
}

fn parse_state_param(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.into_owned())
}

fn metadata_from_json(json: &Value) -> Result<AuthorizationMetadata> {
    serde_json::from_value(json.clone())
        .map_err(|e| AppError::Other(anyhow::anyhow!("stored OAuth metadata is unreadable: {e}")))
}

fn iss_param_supported(metadata: &Value) -> bool {
    metadata
        .get("authorization_response_iss_parameter_supported")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Build an `AuthorizationManager` from a saved client row: hardened client, approved
/// metadata, configured client. No stores bound yet (callers add the ones they need).
async fn base_manager(
    server_url: &str,
    client: &OAuthClientRow,
    callback: &str,
) -> Result<AuthorizationManager> {
    let normalised = oauth_policy::normalise_resource_url(server_url);
    let metadata = metadata_from_json(&client.metadata)?;
    let mut mgr = AuthorizationManager::new(&normalised).await.map_err(oauth_err)?;
    mgr.with_client(hardened_client()?).map_err(oauth_err)?;
    mgr.set_metadata(metadata);
    let mut cfg = OAuthClientConfig::new(client.client_id.clone(), callback).with_scopes(client.scopes.clone());
    if let Some(secret) = client.client_secret.clone() {
        cfg = cfg.with_client_secret(secret);
    }
    mgr.configure_client(cfg).map_err(oauth_err)?;
    Ok(mgr)
}

/// Begin an authorisation-code flow: mint the authorize URL, park the SDK's PKCE state
/// and our own identity record under the same `state`. The `connection_id` row must
/// already exist (minted `pending`).
pub async fn begin_authorize(
    state: &AppState,
    ctx: &AuthContext,
    server_url: &str,
    client: &OAuthClientRow,
    user_id: Option<Uuid>,
    mcp_server_id: Uuid,
    connection_id: Uuid,
    redirect_after: Option<String>,
) -> Result<String> {
    integrations::guard_egress(state, ctx, ConnectorKind::Mcp).await?;

    let mut mgr = base_manager(server_url, client, &callback_url(state)).await?;
    mgr.set_state_store(RedisStateStore::new(state.redis.clone()));
    mgr.set_credential_store(PgCredentialStore::new(
        state.pg.clone(),
        connection_id,
        state.message_key.is_some(),
    ));

    let scope_refs: Vec<&str> = client.scopes.iter().map(String::as_str).collect();
    let url = mgr.get_authorization_url(&scope_refs).await.map_err(oauth_err)?;

    let csrf = parse_state_param(&url)
        .ok_or_else(|| AppError::Other(anyhow::anyhow!("authorize URL carried no state parameter")))?;
    let record = FlowRecord {
        user_id,
        mcp_server_id,
        oauth_client_id: client.id,
        connection_id,
        redirect_after,
    };
    let json = serde_json::to_string(&record)
        .map_err(|e| AppError::Other(anyhow::anyhow!("serialise flow record: {e}")))?;
    cache::kv_set_ex(&state.redis, &flow_key(&csrf), &json, 600).await?;
    Ok(url)
}

/// Complete a callback: consume the flow record, validate `iss`, exchange the code (which
/// persists the tokens and flips the connection to active via the credential store).
/// `loader` yields the (server_url, slug, client row) for a server id.
pub async fn complete_callback<F, Fut>(
    state: &AppState,
    code: &str,
    callback_state: &str,
    iss_param: Option<&str>,
    loader: F,
) -> Result<CallbackOutcome>
where
    F: FnOnce(Uuid, Uuid) -> Fut,
    Fut: std::future::Future<Output = Result<(String, String, OAuthClientRow)>>,
{
    let raw = cache::kv_get_del(&state.redis, &flow_key(callback_state))
        .await?
        .ok_or_else(|| AppError::Validation("unknown or expired OAuth state".into()))?;
    let record: FlowRecord = serde_json::from_str(&raw)
        .map_err(|e| AppError::Other(anyhow::anyhow!("flow record unreadable: {e}")))?;

    let (server_url, slug, client) = loader(record.mcp_server_id, record.oauth_client_id).await?;

    // Reconstruct the initiating user's context (the admin, for a service connection).
    let ctx = match record.user_id {
        Some(uid) => crate::auth::load_context(&state.pg, uid).await?,
        None => {
            return Err(AppError::Validation("OAuth flow record carried no initiating user".into()))
        }
    };
    integrations::guard_egress(state, &ctx, ConnectorKind::Mcp).await?;

    // Validate the authorisation-response issuer against the approved one (exact match).
    oauth_policy::validate_callback_iss(iss_param_supported(&client.metadata), iss_param, &client.issuer)?;

    let mut mgr = base_manager(&server_url, &client, &callback_url(state)).await?;
    mgr.set_state_store(RedisStateStore::new(state.redis.clone()));
    mgr.set_credential_store(PgCredentialStore::new(
        state.pg.clone(),
        record.connection_id,
        state.message_key.is_some(),
    ));

    mgr.exchange_code_for_token(code, callback_state)
        .await
        .map_err(oauth_err)?;

    Ok(CallbackOutcome { slug, redirect_after: record.redirect_after })
}

/// Build a live, OAuth-authenticated connection for the runtime tool path. The SDK's
/// `AuthClient` injects (and refreshes) the token per request from the bound credential
/// store, so the connection survives rotation.
pub async fn connect_oauth_conn(
    state: &AppState,
    server_url: &str,
    client: &OAuthClientRow,
    connection_id: Uuid,
) -> Result<Arc<dyn McpConn>> {
    let mut mgr = base_manager(server_url, client, &callback_url(state)).await?;
    mgr.set_credential_store(PgCredentialStore::new(
        state.pg.clone(),
        connection_id,
        state.message_key.is_some(),
    ));
    client::serve_oauth(server_url, mgr, hardened_client()?).await
}
