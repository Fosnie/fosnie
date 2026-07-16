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

//! HTTP surface for one-click MCP connections (OAuth 2.1).
//!
//! Admin endpoints (permission `mcp.manage`) discover a server's authorisation server,
//! approve an issuer by saving its validated metadata + client, and designate the
//! catalogue-source connection. User endpoints (RBAC `Read` on the server) let a person
//! connect, list, and disconnect under their own identity. The public callback completes
//! the authorisation-code exchange; it reconstructs the caller from the server-side flow
//! record, never from the request, and never returns a 500 to the browser.

use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::keycloak::AuthUser;
use crate::auth::permissions;
use crate::auth::rbac::{Permission, ResourceType};
use crate::auth::AuthContext;
use crate::cache;
use crate::error::{AppError, Result};
use crate::mcp::oauth_flow::{self, OAuthClientRow};
use crate::state::AppState;

// ── Shared loaders ─────────────────────────────────────────────────────────────

struct ServerRow {
    id: Uuid,
    slug: String,
    url: String,
    auth_type: String,
}

async fn load_server(state: &AppState, id: Uuid) -> Result<ServerRow> {
    let r = sqlx::query!(
        r#"SELECT id, slug, url, auth_type FROM mcp_servers WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::NotFound("MCP server not found".into()))?;
    let url = r.url.ok_or_else(|| AppError::Validation("MCP server has no URL".into()))?;
    Ok(ServerRow { id: r.id, slug: r.slug, url, auth_type: r.auth_type })
}

async fn load_client_row(state: &AppState, server_id: Uuid) -> Result<Option<OAuthClientRow>> {
    oauth_flow::load_oauth_client_row(&state.pg, server_id).await
}

async fn audit(state: &AppState, ctx: &AuthContext, action: &str, id: Uuid, payload: Value) {
    let mut ev = crate::audit::AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("mcp_server".into());
    ev.resource_id = Some(id);
    ev.payload = Some(payload);
    let _ = crate::audit::append(&state.pg, &ev).await;
}

// ── Admin: discovery + issuer approval ─────────────────────────────────────────

#[derive(Deserialize)]
pub struct DiscoverBody {
    #[serde(default)]
    pub allowed_issuer_origin: Option<String>,
}

#[derive(Serialize)]
pub struct DiscoverResponse {
    pub issuer: String,
    pub dcr_available: bool,
    pub scopes_supported: Vec<String>,
    pub s256_ok: bool,
    pub callback_url: String,
    pub warnings: Vec<String>,
}

/// Probe a server's authorisation metadata and return what was found. Persists nothing.
pub async fn discover(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<DiscoverBody>,
) -> Result<Json<DiscoverResponse>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::MCP_MANAGE).await?;
    let server = load_server(&state, id).await?;
    let disc = oauth_flow::run_discovery(
        &state,
        &ctx,
        &server.url,
        body.allowed_issuer_origin.as_deref(),
    )
    .await?;
    Ok(Json(DiscoverResponse {
        issuer: disc.issuer,
        dcr_available: disc.dcr_available,
        scopes_supported: disc.scopes_supported,
        s256_ok: disc.s256_ok,
        callback_url: oauth_flow::callback_url(&state),
        warnings: disc.warnings,
    }))
}

#[derive(Deserialize)]
pub struct PutClientBody {
    #[serde(default)]
    pub allowed_issuer_origin: Option<String>,
    /// Register automatically via dynamic client registration.
    #[serde(default)]
    pub use_dcr: bool,
    /// A manually issued client id (when the AS does not support DCR).
    #[serde(default)]
    pub client_id: Option<String>,
    /// The client secret, if the manual client is confidential. Absent = keep existing.
    #[serde(default)]
    pub client_secret: Option<String>,
    /// The scopes to request. Defaults to the server's advertised scopes.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct PutClientResponse {
    pub issuer: String,
    pub registration_source: String,
    pub has_secret: bool,
    pub scopes: Vec<String>,
}

/// Approve an issuer by saving its validated metadata and a client (manual or DCR). This
/// write is the admin's deliberate approval of the origin we will send secrets to.
pub async fn put_client(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PutClientBody>,
) -> Result<Json<PutClientResponse>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::MCP_MANAGE).await?;
    if state.message_key.is_none() {
        return Err(AppError::Validation(
            "server encryption key not configured (set message_encryption_key)".into(),
        ));
    }
    let server = load_server(&state, id).await?;
    if server.auth_type != "oauth" {
        return Err(AppError::Validation("set the server's auth type to OAuth first".into()));
    }

    // Re-run discovery and validation now: the persisted metadata must be freshly verified.
    let disc =
        oauth_flow::run_discovery(&state, &ctx, &server.url, body.allowed_issuer_origin.as_deref())
            .await?;
    if !disc.s256_ok {
        return Err(AppError::Validation(
            "authorisation server does not advertise PKCE S256; cannot approve".into(),
        ));
    }
    let scopes = body.scopes.unwrap_or_else(|| disc.scopes_supported.clone());
    let callback = oauth_flow::callback_url(&state);
    let metadata_json = serde_json::to_value(&disc.metadata)
        .map_err(|e| AppError::Other(anyhow::anyhow!("serialise metadata: {e}")))?;

    // Resolve the client: DCR mints one (capturing RFC 7592 management creds); otherwise a
    // manually supplied client id (with an optional secret).
    let (client_id, client_secret, source, reg_uri, reg_token) = if body.use_dcr {
        let reg = oauth_flow::register_dcr(&disc.metadata, &callback, &scopes).await?;
        (reg.client_id, reg.client_secret, "dcr", reg.registration_client_uri, reg.registration_access_token)
    } else {
        let cid = body
            .client_id
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::Validation("a client_id is required (or use dynamic registration)".into()))?;
        let secret = body.client_secret.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        (cid, secret, "manual", None, None)
    };

    let secret_enc = match &client_secret {
        Some(s) => Some(crate::crypto::encrypt_at_rest(s)?),
        None => None,
    };
    let reg_token_enc = match &reg_token {
        Some(t) => Some(crate::crypto::encrypt_at_rest(t)?),
        None => None,
    };
    let has_secret = secret_enc.is_some();

    sqlx::query!(
        r#"INSERT INTO mcp_oauth_clients
               (id, mcp_server_id, issuer, client_id, client_secret_enc, registration_source,
                registration_client_uri, registration_access_token_enc, scopes, metadata,
                approved_by, approved_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11, now())
           ON CONFLICT (mcp_server_id, issuer) DO UPDATE SET
               client_id = EXCLUDED.client_id,
               client_secret_enc = EXCLUDED.client_secret_enc,
               registration_source = EXCLUDED.registration_source,
               registration_client_uri = EXCLUDED.registration_client_uri,
               registration_access_token_enc = EXCLUDED.registration_access_token_enc,
               scopes = EXCLUDED.scopes,
               metadata = EXCLUDED.metadata,
               approved_by = EXCLUDED.approved_by,
               approved_at = now(),
               updated_at = now()"#,
        Uuid::now_v7(),
        server.id,
        disc.issuer,
        client_id,
        secret_enc,
        source,
        reg_uri,
        reg_token_enc,
        &scopes,
        metadata_json,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;

    // Any live connection for this server was built against the previous client; drop them.
    state.mcp.disconnect_all(&server.slug).await;
    audit(&state, &ctx, "mcp.oauth.client_registered", server.id, json!({ "issuer": disc.issuer, "source": source })).await;

    Ok(Json(PutClientResponse {
        issuer: disc.issuer,
        registration_source: source.to_string(),
        has_secret,
        scopes,
    }))
}

/// Delete the issuer approval + client. Where DCR management creds exist, also delete the
/// registration at the authorisation server (RFC 7592) so we do not orphan it.
pub async fn delete_client(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::MCP_MANAGE).await?;
    let server = load_server(&state, id).await?;
    let row = sqlx::query!(
        r#"SELECT registration_client_uri, registration_access_token_enc
             FROM mcp_oauth_clients WHERE mcp_server_id = $1"#,
        server.id
    )
    .fetch_optional(&state.pg)
    .await?;
    if let Some(r) = &row {
        if let (Some(uri), Some(tok_enc)) =
            (r.registration_client_uri.as_ref(), r.registration_access_token_enc.as_ref())
        {
            let token = crate::crypto::decrypt_at_rest(tok_enc)?;
            // Best-effort: a failure here must not block local cleanup.
            if let Ok(client) = crate::mcp::client::hardened_client() {
                let _ = client.delete(uri).bearer_auth(token).send().await;
            }
        }
    }
    sqlx::query!("DELETE FROM mcp_oauth_clients WHERE mcp_server_id = $1", server.id)
        .execute(&state.pg)
        .await?;
    state.mcp.disconnect_all(&server.slug).await;
    audit(&state, &ctx, "mcp.oauth.config_changed", server.id, json!({ "action": "client_deleted" })).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct CatalogSourceBody {
    pub connection_id: Uuid,
}

/// Designate which connection the health sweep pins the tool catalogue against.
pub async fn set_catalog_source(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<CatalogSourceBody>,
) -> Result<Json<Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::MCP_MANAGE).await?;
    let server = load_server(&state, id).await?;
    // The connection must belong to this server and hold a token.
    let ok = sqlx::query_scalar!(
        r#"SELECT 1 FROM mcp_oauth_connections
             WHERE id = $1 AND mcp_server_id = $2 AND status = 'active'"#,
        body.connection_id,
        server.id
    )
    .fetch_optional(&state.pg)
    .await?
    .is_some();
    if !ok {
        return Err(AppError::Validation("connection is not an active connection for this server".into()));
    }
    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "UPDATE mcp_oauth_connections SET is_catalog_source = false WHERE mcp_server_id = $1",
        server.id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE mcp_oauth_connections SET is_catalog_source = true WHERE id = $1",
        body.connection_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    audit(&state, &ctx, "mcp.oauth.config_changed", server.id, json!({ "action": "catalog_source_set" })).await;
    Ok(Json(json!({ "ok": true })))
}

/// Approve an OAuth server: build the connection from the designated catalogue-source
/// connection, pin its catalogue, and activate. Requires that a catalogue-source
/// connection has already been connected (register → configure client → connect → approve).
pub async fn approve_oauth(
    state: &AppState,
    ctx: &AuthContext,
    server_id: Uuid,
    slug: &str,
) -> Result<Json<Value>> {
    let server = load_server(state, server_id).await?;
    let client = load_client_row(state, server_id)
        .await?
        .ok_or_else(|| AppError::Validation("configure an OAuth client before approving".into()))?;
    let cs = sqlx::query!(
        r#"SELECT id FROM mcp_oauth_connections
             WHERE mcp_server_id = $1 AND is_catalog_source AND status = 'active'"#,
        server_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| {
        AppError::Validation(
            "connect and designate a catalogue-source connection before approving this OAuth server"
                .into(),
        )
    })?;

    let conn = oauth_flow::connect_oauth_conn(state, &server.url, &client, cs.id).await?;
    let catalog = conn.list_tools().await?;
    state.mcp.insert_conn(slug, Some(cs.id), conn).await;
    crate::mcp::record_catalog(&state.pg, server_id, &catalog).await?;
    sqlx::query!("UPDATE mcp_servers SET enabled = true WHERE id = $1", server_id)
        .execute(&state.pg)
        .await?;
    audit(state, ctx, "mcp.server.approved", server_id, json!({ "tools": catalog.len(), "auth": "oauth" })).await;
    Ok(Json(json!({ "status": "active", "tools": catalog.len() })))
}

// ── User: connect / list / disconnect ──────────────────────────────────────────

#[derive(Serialize)]
pub struct MyConnection {
    pub server_id: Uuid,
    pub slug: String,
    pub name: String,
    pub status: String, // connected | disconnected | reauth_required
    pub subject_label: Option<String>,
    pub scopes: Vec<String>,
}

/// List the OAuth-backed MCP servers visible to this user, with their connection status.
pub async fn list_my_connections(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<MyConnection>>> {
    let Some(uid) = ctx.user_id else {
        return Ok(Json(vec![]));
    };
    let rows = sqlx::query!(
        r#"SELECT s.id AS server_id, s.slug, s.name,
                  c.status AS "conn_status?", c.subject_label, c.scopes AS "scopes?"
             FROM mcp_servers s
             LEFT JOIN mcp_oauth_connections c
                    ON c.mcp_server_id = s.id AND c.user_id = $1
            WHERE s.auth_type = 'oauth' AND s.status = 'active' AND s.enabled
            ORDER BY s.name"#,
        uid
    )
    .fetch_all(&state.pg)
    .await?;

    let mut out = Vec::new();
    for r in rows {
        // Enforce per-server RBAC Read (hide servers the user cannot see).
        if state
            .rbac
            .can(&state.pg, &ctx, ResourceType::McpServer, r.server_id, Permission::Read)
            .await
            .unwrap_or(false)
        {
            let status = match r.conn_status.as_deref() {
                Some("active") => "connected",
                Some("reauth_required") => "reauth_required",
                _ => "disconnected",
            };
            out.push(MyConnection {
                server_id: r.server_id,
                slug: r.slug,
                name: r.name,
                status: status.to_string(),
                subject_label: r.subject_label,
                scopes: r.scopes.unwrap_or_default(),
            });
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize, Default)]
pub struct ConnectBody {
    /// Admin-only: create the deployment's service connection (`user_id` NULL) rather
    /// than a personal one, for unattended runs.
    #[serde(default)]
    pub service: bool,
}

#[derive(Serialize)]
pub struct ConnectResponse {
    pub authorize_url: String,
}

/// Begin an authorisation flow for the current user (or the service connection).
pub async fn connect(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(server_id): Path<Uuid>,
    Json(body): Json<ConnectBody>,
) -> Result<Json<ConnectResponse>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("authentication required".into()))?;
    let server = load_server(&state, server_id).await?;
    if server.auth_type != "oauth" {
        return Err(AppError::Validation("this server is not an OAuth server".into()));
    }
    state
        .rbac
        .require(&state.pg, &ctx, ResourceType::McpServer, server.id, Permission::Read)
        .await?;
    let client = load_client_row(&state, server.id)
        .await?
        .ok_or_else(|| AppError::Validation("this server has no approved OAuth client yet".into()))?;

    // A service connection is admin-only.
    let row_user_id: Option<Uuid> = if body.service {
        state.rbac.require_permission(&state.pg, &ctx, permissions::MCP_MANAGE).await?;
        None
    } else {
        Some(uid)
    };

    // Mint (or reset) the connection row as pending, then bind the flow to it.
    let connection_id = mint_pending_connection(&state, server.id, client.id, row_user_id).await?;

    // The initiating identity for the callback is always the caller (the admin, for a
    // service connection), never the connection row's user_id.
    let authorize_url = oauth_flow::begin_authorize(
        &state,
        &ctx,
        &server.url,
        &client,
        row_user_id,
        server.id,
        connection_id,
        None,
    )
    .await?;
    Ok(Json(ConnectResponse { authorize_url }))
}

/// Upsert a pending connection row and return its id. Two branches because NULL user_id
/// (the service connection) needs the other partial-unique index.
async fn mint_pending_connection(
    state: &AppState,
    server_id: Uuid,
    oauth_client_id: Uuid,
    user_id: Option<Uuid>,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    let out = if let Some(uid) = user_id {
        sqlx::query_scalar!(
            r#"INSERT INTO mcp_oauth_connections (id, mcp_server_id, oauth_client_id, user_id, status)
               VALUES ($1,$2,$3,$4,'pending')
               ON CONFLICT (mcp_server_id, user_id) WHERE user_id IS NOT NULL
               DO UPDATE SET oauth_client_id = EXCLUDED.oauth_client_id, status = 'pending'
               RETURNING id"#,
            id,
            server_id,
            oauth_client_id,
            uid
        )
        .fetch_one(&state.pg)
        .await?
    } else {
        sqlx::query_scalar!(
            r#"INSERT INTO mcp_oauth_connections (id, mcp_server_id, oauth_client_id, user_id, status)
               VALUES ($1,$2,$3,NULL,'pending')
               ON CONFLICT (mcp_server_id) WHERE user_id IS NULL
               DO UPDATE SET oauth_client_id = EXCLUDED.oauth_client_id, status = 'pending'
               RETURNING id"#,
            id,
            server_id,
            oauth_client_id
        )
        .fetch_one(&state.pg)
        .await?
    };
    Ok(out)
}

/// Revoke this user's connection: clear the ciphertext and, where the AS advertises a
/// revocation endpoint, tell it too (best-effort).
pub async fn disconnect(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(server_id): Path<Uuid>,
) -> Result<Json<Value>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("authentication required".into()))?;
    let server = load_server(&state, server_id).await?;
    let row = sqlx::query!(
        r#"SELECT id, access_token_enc FROM mcp_oauth_connections
             WHERE mcp_server_id = $1 AND user_id = $2"#,
        server_id,
        uid
    )
    .fetch_optional(&state.pg)
    .await?;
    let Some(row) = row else {
        return Ok(Json(json!({ "ok": true })));
    };

    // Best-effort revocation at the AS.
    if let (Some(client), Some(token_enc)) =
        (load_client_row(&state, server_id).await.ok().flatten(), row.access_token_enc.as_ref())
    {
        if let Some(rev) = client
            .metadata
            .get("revocation_endpoint")
            .and_then(Value::as_str)
        {
            if let (Ok(token), Ok(http)) =
                (crate::crypto::decrypt_at_rest(token_enc), crate::mcp::client::hardened_client())
            {
                let _ = http
                    .post(rev)
                    .form(&[("token", token.as_str()), ("token_type_hint", "access_token")])
                    .send()
                    .await;
            }
        }
    }

    sqlx::query!(
        r#"UPDATE mcp_oauth_connections
              SET status = 'revoked', access_token_enc = NULL, refresh_token_enc = NULL
            WHERE id = $1"#,
        row.id
    )
    .execute(&state.pg)
    .await?;
    state.mcp.disconnect(&server.slug, Some(row.id)).await;
    audit(&state, &ctx, "mcp.oauth.disconnected", server_id, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

// ── Public: the authorisation-code callback ─────────────────────────────────────

#[derive(Deserialize)]
pub struct CallbackParams {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub iss: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_description: Option<String>,
}

/// The single, fixed OAuth redirect endpoint. Unauthenticated: identity is reconstructed
/// from the parked flow record. Never returns a 500 — every failure becomes a redirect to
/// the SPA carrying `mcp_connect_error`.
pub async fn callback(
    State(state): State<AppState>,
    Query(params): Query<CallbackParams>,
) -> Redirect {
    let base = &state.boot.server.public_url;
    // Unauthenticated + triggers outbound token requests: rate-limit by nothing better
    // than a coarse global bucket (the state value is single-use anyway).
    let _ = cache::rate_limit_guard(&state.redis, "mcp:oauth:callback", 120, 60).await;

    match run_callback(&state, params).await {
        Ok(outcome) => Redirect::to(&redirect_url(base, "mcp_connected", &outcome.slug)),
        Err(e) => Redirect::to(&redirect_url(base, "mcp_connect_error", &e.to_string())),
    }
}

async fn run_callback(state: &AppState, params: CallbackParams) -> Result<oauth_flow::CallbackOutcome> {
    if let Some(err) = params.error {
        let desc = params.error_description.unwrap_or_default();
        return Err(AppError::Validation(format!("provider returned '{err}': {desc}")));
    }
    let code = params.code.ok_or_else(|| AppError::Validation("callback missing code".into()))?;
    let cb_state = params.state.ok_or_else(|| AppError::Validation("callback missing state".into()))?;

    oauth_flow::complete_callback(state, &code, &cb_state, params.iss.as_deref(), |server_id, _client_id| {
        let state = state.clone();
        async move {
            let server = load_server(&state, server_id).await?;
            let client = load_client_row(&state, server_id)
                .await?
                .ok_or_else(|| AppError::Validation("server OAuth client vanished mid-flow".into()))?;
            Ok((server.url, server.slug, client))
        }
    })
    .await
}

fn redirect_url(base: &str, key: &str, val: &str) -> String {
    let mut u = reqwest::Url::parse(base)
        .unwrap_or_else(|_| reqwest::Url::parse("http://localhost:8080").unwrap());
    u.set_path("/profile");
    u.query_pairs_mut().append_pair("tab", "connections").append_pair(key, val);
    u.to_string()
}
