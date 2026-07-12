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

//! Admin registry for MCP servers (FEATURE B1). Client-admin registers allow-listed,
//! client-internal servers; **approval** connects, discovers + pins the tool catalog,
//! and activates. Egress still requires the global `integration.mcp.enabled` flag
//! (super-admin, via the integrations endpoint), so a registered server flows no
//! traffic until MCP is enabled platform-wide. Per-principal access is then granted
//! through the existing AccessGrants matrix (`resource_type = mcp_server`).

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::permissions;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::integrations::{self, ConnectorKind};
use crate::mcp::client::{HttpAuth, Transport};
use crate::state::AppState;

async fn audit_server(state: &AppState, ctx: &AuthContext, action: &str, id: Uuid, payload: Value) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("mcp_server".into());
    ev.resource_id = Some(id);
    ev.payload = Some(payload);
    let _ = audit::append(&state.pg, &ev).await;
}

#[derive(Serialize)]
pub struct ServerOut {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub transport: String,
    pub url: Option<String>,
    pub status: String,
    pub enabled: bool,
    pub connected: bool,
    pub tool_count: i64,
    pub last_health_at: Option<String>,
    pub created_at: String,
    /// `none | bearer | api_key | header` — the auth scheme injected into requests.
    pub auth_type: String,
    pub auth_header_name: Option<String>,
    /// Whether an encrypted secret is stored. The secret itself is NEVER returned.
    pub has_secret: bool,
    /// True ⇒ the server reaches a remote/public endpoint (egress-gated, public URL).
    pub requires_egress: bool,
}

pub async fn list(State(state): State<AppState>, AuthUser(ctx): AuthUser) -> Result<Json<Vec<ServerOut>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::MCP_MANAGE).await?;
    let rows = sqlx::query!(
        r#"SELECT id, slug, name, transport, url, status, enabled, tools_catalog, last_health_at,
                  created_at, auth_type, auth_header_name, auth_value_enc, requires_egress
           FROM mcp_servers ORDER BY created_at DESC"#
    )
    .fetch_all(&state.pg)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let tool_count = r
            .tools_catalog
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|a| a.len() as i64)
            .unwrap_or(0);
        out.push(ServerOut {
            connected: state.mcp.is_connected(&r.slug).await,
            id: r.id,
            slug: r.slug,
            name: r.name,
            transport: r.transport,
            url: r.url,
            status: r.status,
            enabled: r.enabled,
            tool_count,
            last_health_at: r.last_health_at.map(|t| t.to_string()),
            created_at: r.created_at.to_string(),
            auth_type: r.auth_type,
            auth_header_name: r.auth_header_name,
            has_secret: r.auth_value_enc.is_some(),
            requires_egress: r.requires_egress,
        });
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct RegisterBody {
    pub slug: String,
    pub name: String,
    pub transport: String, // stdio | http
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub url: Option<String>,
    /// Auth scheme for a remote http server: `none | bearer | api_key | header`.
    #[serde(default)]
    pub auth_type: Option<String>,
    /// Custom header name (required for `api_key`/`header`; ignored for `bearer`).
    #[serde(default)]
    pub auth_header_name: Option<String>,
    /// The secret (bearer token / api-key / header value). Stored encrypted.
    #[serde(default)]
    pub auth_value: Option<String>,
    /// Opt this server into egress (public/remote endpoint). Lifts the private-only
    /// URL guard; `requires_egress` is surfaced in the UI and the egress audit.
    #[serde(default)]
    pub requires_egress: bool,
}

/// Normalise register-time auth into `(auth_type, header_name, encrypted_secret)`.
/// `bearer` fixes the header to `Authorization`; `api_key`/`header` need an explicit
/// header name; `none` clears everything. The secret is encrypted with the deployment
/// message key (as for dm_bodies / provider api-keys).
fn normalise_auth(
    state: &AppState,
    auth_type: Option<&str>,
    header_name: Option<&str>,
    value: Option<&str>,
) -> Result<(String, Option<String>, Option<String>)> {
    let at = auth_type.unwrap_or("none");
    match at {
        "none" => Ok(("none".into(), None, None)),
        "bearer" | "api_key" | "header" => {
            let secret = value.map(str::trim).filter(|v| !v.is_empty()).ok_or_else(|| {
                AppError::Validation(format!("auth_type '{at}' requires a non-empty auth_value"))
            })?;
            let hname = if at == "bearer" {
                "Authorization".to_string()
            } else {
                header_name
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .ok_or_else(|| {
                        AppError::Validation(format!("auth_type '{at}' requires auth_header_name"))
                    })?
                    .to_string()
            };
            if state.message_key.is_none() {
                return Err(AppError::Validation("server encryption key not configured (set message_encryption_key)".into()));
            }
            let enc = crate::crypto::encrypt_at_rest(secret)?;
            Ok((at.to_string(), Some(hname), Some(enc)))
        }
        other => Err(AppError::Validation(format!(
            "auth_type must be none|bearer|api_key|header, got '{other}'"
        ))),
    }
}

/// Decrypt the stored secret and build the wire header injected on every request.
/// `bearer` ⇒ `Authorization: Bearer <token>`; `api_key`/`header` ⇒ `<name>: <value>`;
/// `none` ⇒ no auth. Returns `Ok(None)` when the server carries no auth.
fn build_http_auth(
    state: &AppState,
    auth_type: &str,
    header_name: Option<&str>,
    value_enc: Option<&str>,
) -> Result<Option<HttpAuth>> {
    if auth_type == "none" {
        return Ok(None);
    }
    let enc = value_enc.ok_or_else(|| AppError::Validation("auth secret missing".into()))?;
    if state.message_key.is_none() {
        return Err(AppError::Validation("server encryption key not configured".into()));
    }
    let secret = crate::crypto::decrypt_at_rest(enc)?;
    let auth = match auth_type {
        "bearer" => HttpAuth::Bearer(secret),
        "api_key" | "header" => HttpAuth::Header {
            name: header_name
                .ok_or_else(|| AppError::Validation("auth header name missing".into()))?
                .to_string(),
            value: secret,
        },
        other => return Err(AppError::Validation(format!("invalid auth_type '{other}'"))),
    };
    Ok(Some(auth))
}

pub async fn register(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<RegisterBody>,
) -> Result<Json<Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::MCP_MANAGE).await?;
    if body.slug.trim().is_empty() || body.slug.contains("__") {
        return Err(AppError::Validation("slug must be non-empty and must not contain '__'".into()));
    }
    let mut auth_type = "none".to_string();
    let mut auth_header_name: Option<String> = None;
    let mut auth_value_enc: Option<String> = None;
    let command_json = match body.transport.as_str() {
        "stdio" => {
            let cmd = body.command.as_ref().filter(|c| !c.is_empty()).ok_or_else(|| {
                AppError::Validation("a stdio server requires a non-empty command".into())
            })?;
            Some(json!(cmd))
        }
        "http" => {
            let url = body.url.as_deref().ok_or_else(|| AppError::Validation("an http server requires a url".into()))?;
            // Remote servers (requires_egress) may reach a public https host; otherwise
            // private-only (zero-egress). SSRF guard holds in both (acceptance #2/#6).
            crate::mcp::validate::validate_endpoint(url, body.requires_egress)?;
            let (at, hname, enc) = normalise_auth(
                &state,
                body.auth_type.as_deref(),
                body.auth_header_name.as_deref(),
                body.auth_value.as_deref(),
            )?;
            auth_type = at;
            auth_header_name = hname;
            auth_value_enc = enc;
            None
        }
        other => return Err(AppError::Validation(format!("transport must be stdio|http, got '{other}'"))),
    };
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO mcp_servers (id, slug, name, transport, command, url, created_by, \
         auth_type, auth_header_name, auth_value_enc, requires_egress) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        id,
        body.slug,
        body.name,
        body.transport,
        command_json,
        body.url,
        ctx.user_id,
        auth_type,
        auth_header_name,
        auth_value_enc,
        body.requires_egress,
    )
    .execute(&state.pg)
    .await
    .map_err(|e| {
        if e.to_string().contains("mcp_servers_slug_key") {
            AppError::Validation(format!("an MCP server with slug '{}' already exists", body.slug))
        } else {
            AppError::from(e)
        }
    })?;
    audit_server(&state, &ctx, "mcp.server.registered", id, json!({
        "slug": body.slug, "transport": body.transport,
        "auth_type": auth_type, "requires_egress": body.requires_egress,
    })).await;
    Ok(Json(json!({ "id": id, "status": "pending" })))
}

/// Approve a server: connect, discover + pin its catalog, activate. Re-approving
/// from `quarantined` re-pins the reviewed catalog (the admin "re-pin" action).
pub async fn approve(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::MCP_MANAGE).await?;
    // Connecting reaches the server → require MCP enabled platform-wide first.
    if !integrations::is_enabled(&state.pg, ConnectorKind::Mcp).await? {
        return Err(AppError::Forbidden(
            "enable MCP (integration.mcp.enabled, super-admin) before approving servers".into(),
        ));
    }
    let s = sqlx::query!(
        "SELECT slug, transport, command, url, auth_type, auth_header_name, auth_value_enc, requires_egress \
         FROM mcp_servers WHERE id = $1",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("MCP server not found".into()))?;

    let transport = match s.transport.as_str() {
        "stdio" => {
            let cmd: Vec<String> = s.command.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
            Transport::Stdio { command: cmd }
        }
        "http" => {
            let url = s.url.ok_or_else(|| AppError::Validation("http server missing url".into()))?;
            // Re-validate at connect (DNS-rebinding TOCTOU), honouring the egress gate.
            crate::mcp::validate::validate_endpoint(&url, s.requires_egress)?;
            let auth = build_http_auth(&state, &s.auth_type, s.auth_header_name.as_deref(), s.auth_value_enc.as_deref())?;
            Transport::Http { url, auth }
        }
        _ => return Err(AppError::Validation("invalid transport".into())),
    };

    let catalog = state.mcp.connect(&s.slug, transport).await?;
    crate::mcp::record_catalog(&state.pg, id, &catalog).await?;
    sqlx::query!("UPDATE mcp_servers SET enabled = true WHERE id = $1", id)
        .execute(&state.pg)
        .await?;
    audit_server(&state, &ctx, "mcp.server.approved", id, json!({ "tools": catalog.len() })).await;
    Ok(Json(json!({ "status": "active", "tools": catalog.len() })))
}

pub async fn delete(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::MCP_MANAGE).await?;
    if let Some(slug) = sqlx::query_scalar!("SELECT slug FROM mcp_servers WHERE id = $1", id)
        .fetch_optional(&state.pg)
        .await?
    {
        state.mcp.disconnect(&slug).await;
    }
    sqlx::query!("DELETE FROM mcp_servers WHERE id = $1", id).execute(&state.pg).await?;
    audit_server(&state, &ctx, "mcp.server.deleted", id, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}
