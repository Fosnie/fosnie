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

//! The tool catalogue endpoint + native-tool overrides.
//! `GET /api/tools/catalog` is the single source of truth the frontend fetches
//! (it replaces the old hardcoded `AGENT_TOOL_CATALOG`): native tools with their
//! badges + effective enabled/description, a read-only slice of the active MCP
//! servers, and custom tools. The native override CRUD switches a
//! tool on/off per deployment and edits the description the LLM sees; both are
//! gated by the `tools.manage` permission and audited.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::{permissions, AuthContext};
use crate::error::{AppError, Result};
use crate::state::AppState;

#[derive(Serialize)]
pub struct NativeToolOut {
    pub name: String,
    pub label: String,
    pub hint: String,
    /// "read" | "proposal" | "approval" — read-only classifier badge.
    pub effect: String,
    pub egress: bool,
    pub capability: Option<String>,
    pub dormant: bool,
    pub default: bool,
    /// Effective on/off (an override row may switch a tool off per deployment).
    pub enabled: bool,
    /// Effective description advertised to the LLM (override or code default).
    pub description: String,
    /// The code default, so the UI can preview and reset to it.
    pub default_description: String,
    pub has_override: bool,
}

#[derive(Serialize)]
pub struct McpToolOut {
    pub name: String,
    pub slug: String,
    pub tool_count: i64,
    pub requires_egress: bool,
    pub status: String,
}

#[derive(Serialize)]
pub struct CustomToolOut {
    pub id: uuid::Uuid,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub kind: String,
    pub params_schema: serde_json::Value,
    pub config: serde_json::Value,
    pub requires_egress: bool,
    pub side_effecting: bool,
    pub enabled: bool,
    pub version: i32,
    pub approved_version: Option<i32>,
    pub timeout_secs: Option<i32>,
    /// A secret is stored (the value itself is never returned).
    pub has_secret: bool,
    /// version == approved_version: live and dispatchable.
    pub approved: bool,
}

#[derive(Serialize)]
pub struct Catalog {
    pub native: Vec<NativeToolOut>,
    pub mcp: Vec<McpToolOut>,
    pub custom: Vec<CustomToolOut>,
}

async fn list_custom(pg: &sqlx::PgPool) -> Vec<CustomToolOut> {
    let rows = sqlx::query!(
        r#"SELECT id, name, display_name, description, kind, params_schema, config,
                  auth_value_enc, requires_egress, side_effecting, enabled, version,
                  approved_version, timeout_secs
             FROM custom_tools ORDER BY name"#
    )
    .fetch_all(pg)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|r| CustomToolOut {
            approved: r.approved_version == Some(r.version),
            id: r.id,
            name: r.name,
            display_name: r.display_name,
            description: r.description,
            kind: r.kind,
            params_schema: r.params_schema,
            config: r.config,
            requires_egress: r.requires_egress,
            side_effecting: r.side_effecting,
            enabled: r.enabled,
            version: r.version,
            approved_version: r.approved_version,
            timeout_secs: r.timeout_secs,
            has_secret: r.auth_value_enc.is_some(),
        })
        .collect()
}

/// Build one native tool's view given the current overrides map.
fn native_view(
    e: &crate::tools::CatalogEntry,
    overrides: &std::collections::HashMap<String, crate::tools::Override>,
) -> NativeToolOut {
    let ov = overrides.get(e.name);
    let default_description = crate::tools::default_description(e.name).unwrap_or_default();
    let description = ov
        .and_then(|o| o.description_override.clone())
        .unwrap_or_else(|| default_description.clone());
    NativeToolOut {
        name: e.name.to_string(),
        label: e.label.to_string(),
        hint: e.hint.to_string(),
        effect: e.effect.to_string(),
        egress: e.egress,
        capability: e.capability.map(str::to_string),
        dormant: e.dormant,
        default: e.default,
        enabled: ov.map(|o| o.enabled).unwrap_or(true),
        description,
        default_description,
        has_override: ov.is_some(),
    }
}

/// `GET /api/tools/catalog` — any authenticated user (the agent editor needs it).
pub async fn catalog(State(state): State<AppState>, AuthUser(ctx): AuthUser) -> Result<Json<Catalog>> {
    let _ = &ctx; // authentication is the only gate; the catalogue carries no secrets.
    let overrides = crate::tools::load_overrides(&state.pg).await.unwrap_or_default();
    let native = crate::tools::catalog().iter().map(|e| native_view(e, &overrides)).collect();

    // Read-only slice of the active MCP servers (their CRUD lives in the MCP
    // Servers admin tab — the Tools UI only links to it, never duplicates it).
    let mcp_rows = sqlx::query!(
        r#"SELECT slug, name, tools_catalog, requires_egress, status
           FROM mcp_servers WHERE status = 'active' AND enabled ORDER BY name"#
    )
    .fetch_all(&state.pg)
    .await
    .unwrap_or_default();
    let mcp = mcp_rows
        .into_iter()
        .map(|r| McpToolOut {
            tool_count: r
                .tools_catalog
                .as_ref()
                .and_then(|v| v.as_array())
                .map(|a| a.len() as i64)
                .unwrap_or(0),
            name: r.name,
            slug: r.slug,
            requires_egress: r.requires_egress,
            status: r.status,
        })
        .collect();

    let custom = list_custom(&state.pg).await;
    Ok(Json(Catalog { native, mcp, custom }))
}

#[derive(Deserialize)]
pub struct OverrideIn {
    pub enabled: bool,
    #[serde(default)]
    pub description_override: Option<String>,
}

async fn audit_override(state: &AppState, ctx: &AuthContext, payload: serde_json::Value) {
    let mut ev = AuditEvent::action("tool.override.changed", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("tool".into());
    ev.payload = Some(payload);
    let _ = audit::append(&state.pg, &ev).await;
}

async fn one_native(state: &AppState, name: &str) -> Result<Json<NativeToolOut>> {
    let overrides = crate::tools::load_overrides(&state.pg).await.unwrap_or_default();
    let entry = crate::tools::catalog();
    let e = entry
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| AppError::Validation(format!("unknown native tool: {name}")))?;
    Ok(Json(native_view(e, &overrides)))
}

/// `PUT /api/admin/tools/native/{name}` — set the enabled flag and/or description
/// override for a native tool. Empty/whitespace description resets to the default.
pub async fn put_native(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(name): Path<String>,
    Json(body): Json<OverrideIn>,
) -> Result<Json<NativeToolOut>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TOOLS_MANAGE).await?;
    if !crate::tools::ALL.contains(&name.as_str()) {
        return Err(AppError::Validation(format!("unknown native tool: {name}")));
    }
    let desc: Option<String> = body
        .description_override
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let prev = sqlx::query!(
        "SELECT enabled, description_override FROM tool_overrides WHERE tool_name = $1",
        name
    )
    .fetch_optional(&state.pg)
    .await?;

    sqlx::query!(
        r#"INSERT INTO tool_overrides (tool_name, enabled, description_override, updated_by, updated_at)
           VALUES ($1, $2, $3, $4, now())
           ON CONFLICT (tool_name) DO UPDATE
             SET enabled = EXCLUDED.enabled,
                 description_override = EXCLUDED.description_override,
                 updated_by = EXCLUDED.updated_by,
                 updated_at = now()"#,
        name,
        body.enabled,
        desc,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;

    audit_override(
        &state,
        &ctx,
        json!({
            "tool": name,
            "before": prev.map(|p| json!({ "enabled": p.enabled, "description_override": p.description_override })),
            "after": { "enabled": body.enabled, "description_override": desc },
        }),
    )
    .await;

    one_native(&state, &name).await
}

/// `DELETE /api/admin/tools/native/{name}` — reset a native tool to its code
/// default (drop any override row).
pub async fn reset_native(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(name): Path<String>,
) -> Result<Json<NativeToolOut>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TOOLS_MANAGE).await?;
    if !crate::tools::ALL.contains(&name.as_str()) {
        return Err(AppError::Validation(format!("unknown native tool: {name}")));
    }
    sqlx::query!("DELETE FROM tool_overrides WHERE tool_name = $1", name)
        .execute(&state.pg)
        .await?;
    audit_override(&state, &ctx, json!({ "tool": name, "reset": true })).await;
    one_native(&state, &name).await
}

// ── Custom tools (http kind) ───────────────────────────────────────────────

fn default_kind() -> String {
    "http".into()
}
fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub struct CustomToolIn {
    pub name: String,
    pub display_name: String,
    pub description: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    pub params_schema: serde_json::Value,
    pub config: serde_json::Value,
    /// Plaintext secret (write-only). On update: `None` = leave unchanged, `Some("")`
    /// = clear, `Some(secret)` = replace.
    #[serde(default)]
    pub auth_value: Option<String>,
    #[serde(default = "default_true")]
    pub requires_egress: bool,
    #[serde(default = "default_true")]
    pub side_effecting: bool,
    #[serde(default)]
    pub timeout_secs: Option<i32>,
}

async fn audit_custom(state: &AppState, ctx: &AuthContext, action: &str, id: uuid::Uuid, payload: serde_json::Value) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("custom_tool".into());
    ev.resource_id = Some(id);
    ev.payload = Some(payload);
    let _ = audit::append(&state.pg, &ev).await;
}

/// The defining fields, snapshotted per version for history/diff.
fn snapshot_of(b: &CustomToolIn) -> serde_json::Value {
    json!({
        "name": b.name, "display_name": b.display_name, "description": b.description,
        "kind": b.kind, "params_schema": b.params_schema, "config": b.config,
        "requires_egress": b.requires_egress, "side_effecting": b.side_effecting,
        "timeout_secs": b.timeout_secs, "has_secret": b.auth_value.as_deref().is_some_and(|s| !s.is_empty()),
    })
}

/// Validate the shape common to create + update. `existing_id` excludes a row from
/// the name-collision check (the tool being updated).
async fn validate_custom(
    pg: &sqlx::PgPool,
    b: &CustomToolIn,
    existing_id: Option<uuid::Uuid>,
) -> Result<()> {
    if b.kind != "http" && b.kind != "script" {
        return Err(AppError::Validation(format!("unknown custom tool kind: {}", b.kind)));
    }
    let name = b.name.trim();
    if name.is_empty() || name.contains("__") {
        return Err(AppError::Validation("name must be non-empty and must not contain '__'".into()));
    }
    if crate::tools::ALL.contains(&name) {
        return Err(AppError::Validation(format!("'{name}' collides with a native tool")));
    }
    let clash = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM custom_tools WHERE name = $1 AND ($2::uuid IS NULL OR id <> $2)) AS "e!""#,
        name,
        existing_id
    )
    .fetch_one(pg)
    .await?;
    if clash {
        return Err(AppError::Validation(format!("a custom tool named '{name}' already exists")));
    }
    if !b.params_schema.is_object() {
        return Err(AppError::Validation("params_schema must be a JSON-Schema object".into()));
    }
    if b.kind == "script" {
        // A script tool needs a non-empty Python source; no URL/SSRF (it runs in
        // the zero-egress sandbox).
        let src = b.config.get("source").and_then(|v| v.as_str()).unwrap_or("");
        if src.trim().is_empty() {
            return Err(AppError::Validation("a script tool needs a non-empty config.source".into()));
        }
        return Ok(());
    }
    // http config sanity + a best-effort SSRF check on a fully-static URL (a
    // templated URL is validated at dispatch, on the resolved value).
    let url = b.config.get("url").and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Validation("config.url is required".into()))?;
    if !url.contains("{{") {
        crate::mcp::validate::validate_endpoint(url, b.requires_egress)?;
    }
    Ok(())
}

async fn one_custom(state: &AppState, id: uuid::Uuid) -> Result<Json<CustomToolOut>> {
    list_custom(&state.pg)
        .await
        .into_iter()
        .find(|c| c.id == id)
        .map(Json)
        .ok_or_else(|| AppError::Validation("no such custom tool".into()))
}

/// `POST /api/admin/tools/custom` — register a new (disabled, unapproved) tool.
pub async fn create_custom(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CustomToolIn>,
) -> Result<Json<CustomToolOut>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TOOLS_MANAGE).await?;
    validate_custom(&state.pg, &body, None).await?;
    let auth_value_enc = match body.auth_value.as_deref().filter(|s| !s.is_empty()) {
        Some(secret) => Some(crate::crypto::encrypt_at_rest(secret)?),
        None => None,
    };
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        r#"INSERT INTO custom_tools
             (id, name, display_name, description, kind, params_schema, config, auth_value_enc,
              requires_egress, side_effecting, enabled, approved_version, version, timeout_secs, created_by)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,false,NULL,1,$11,$12)"#,
        id,
        body.name.trim(),
        body.display_name,
        body.description,
        body.kind,
        body.params_schema,
        body.config,
        auth_value_enc,
        body.requires_egress,
        body.side_effecting,
        body.timeout_secs,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;
    sqlx::query!(
        "INSERT INTO custom_tool_versions (id, tool_id, version, snapshot, created_by) VALUES ($1,$2,1,$3,$4)",
        uuid::Uuid::now_v7(),
        id,
        snapshot_of(&body),
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;
    audit_custom(&state, &ctx, "tool.custom.created", id, json!({ "name": body.name })).await;
    one_custom(&state, id).await
}

/// `PUT /api/admin/tools/custom/{id}` — edit → a new version; approval + enable are
/// reset (anti-rug-pull: an agent never silently runs a changed tool).
pub async fn update_custom(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<CustomToolIn>,
) -> Result<Json<CustomToolOut>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TOOLS_MANAGE).await?;
    let cur = sqlx::query!("SELECT version FROM custom_tools WHERE id = $1", id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("no such custom tool".into()))?;
    validate_custom(&state.pg, &body, Some(id)).await?;
    let new_version = cur.version + 1;

    // Secret: None = keep, Some("") = clear, Some(secret) = replace.
    match body.auth_value.as_deref() {
        None => {
            sqlx::query!(
                r#"UPDATE custom_tools SET name=$2, display_name=$3, description=$4, kind=$5,
                     params_schema=$6, config=$7, requires_egress=$8, side_effecting=$9,
                     timeout_secs=$10, version=$11, approved_version=NULL, enabled=false, updated_at=now()
                   WHERE id=$1"#,
                id, body.name.trim(), body.display_name, body.description, body.kind,
                body.params_schema, body.config, body.requires_egress, body.side_effecting,
                body.timeout_secs, new_version,
            )
            .execute(&state.pg)
            .await?;
        }
        Some(secret) => {
            let enc = if secret.is_empty() { None } else { Some(crate::crypto::encrypt_at_rest(secret)?) };
            sqlx::query!(
                r#"UPDATE custom_tools SET name=$2, display_name=$3, description=$4, kind=$5,
                     params_schema=$6, config=$7, auth_value_enc=$8, requires_egress=$9,
                     side_effecting=$10, timeout_secs=$11, version=$12, approved_version=NULL,
                     enabled=false, updated_at=now()
                   WHERE id=$1"#,
                id, body.name.trim(), body.display_name, body.description, body.kind,
                body.params_schema, body.config, enc, body.requires_egress, body.side_effecting,
                body.timeout_secs, new_version,
            )
            .execute(&state.pg)
            .await?;
        }
    }
    sqlx::query!(
        "INSERT INTO custom_tool_versions (id, tool_id, version, snapshot, created_by) VALUES ($1,$2,$3,$4,$5)",
        uuid::Uuid::now_v7(),
        id,
        new_version,
        snapshot_of(&body),
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;
    audit_custom(&state, &ctx, "tool.custom.updated", id, json!({ "name": body.name, "version": new_version })).await;
    one_custom(&state, id).await
}

/// `POST /api/admin/tools/custom/{id}/enable` — approve the current version AND
/// enable it in one admin action (the UI shows the diff first).
pub async fn enable_custom(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<CustomToolOut>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TOOLS_MANAGE).await?;
    let n = sqlx::query!(
        "UPDATE custom_tools SET approved_version = version, enabled = true, updated_at = now() WHERE id = $1",
        id
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::Validation("no such custom tool".into()));
    }
    audit_custom(&state, &ctx, "tool.custom.enabled", id, json!({})).await;
    one_custom(&state, id).await
}

/// `POST /api/admin/tools/custom/{id}/disable` — switch off (approval retained).
pub async fn disable_custom(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<CustomToolOut>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TOOLS_MANAGE).await?;
    let n = sqlx::query!("UPDATE custom_tools SET enabled = false, updated_at = now() WHERE id = $1", id)
        .execute(&state.pg)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::Validation("no such custom tool".into()));
    }
    audit_custom(&state, &ctx, "tool.custom.disabled", id, json!({})).await;
    one_custom(&state, id).await
}

/// `DELETE /api/admin/tools/custom/{id}` — hard delete (versions cascade); the
/// final snapshot is preserved in the audit event.
pub async fn delete_custom(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TOOLS_MANAGE).await?;
    let row = sqlx::query!(
        "SELECT name, display_name, description, kind, params_schema, config, version FROM custom_tools WHERE id = $1",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("no such custom tool".into()))?;
    sqlx::query!("DELETE FROM custom_tools WHERE id = $1", id).execute(&state.pg).await?;
    audit_custom(
        &state,
        &ctx,
        "tool.custom.deleted",
        id,
        json!({
            "name": row.name, "display_name": row.display_name, "description": row.description,
            "kind": row.kind, "params_schema": row.params_schema, "config": row.config, "version": row.version,
        }),
    )
    .await;
    Ok(Json(json!({ "deleted": true })))
}

#[derive(Deserialize)]
pub struct TestRunIn {
    #[serde(default)]
    pub args: serde_json::Value,
}

/// `POST /api/admin/tools/custom/{id}/test-run` — execute with hand-entered args
/// under the same egress/SSRF gates (but no approval gate — admin validation).
pub async fn test_run_custom(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<TestRunIn>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TOOLS_MANAGE).await?;
    let row = crate::tools::custom::load_by_id(&state.pg, id)
        .await
        .ok_or_else(|| AppError::Validation("no such custom tool".into()))?;
    let args = if body.args.is_object() { body.args } else { json!({}) };
    let result = crate::tools::custom::test_run(&state, &ctx, &row, &args).await?;
    Ok(Json(json!({ "result": result })))
}
