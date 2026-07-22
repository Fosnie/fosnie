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

//! Deployment-scope provider config admin (open-core). Host-admin
//! CRUD over `provider_configs` (scope='deployment'). API keys are write-only:
//! stored AES-256-GCM encrypted, returned only as a `api_key_set` boolean, and
//! never logged or audited in clear.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::device::MaybeDevice;
use crate::auth::keycloak::AuthUser;
use crate::auth::permissions;
use crate::error::{AppError, Result};
use crate::providers::ROLES;
use crate::state::AppState;

/// Whether per-user BYOK writes are allowed. A runtime override in
/// `config_settings` wins over the boot flag so a host can toggle without restart.
async fn byok_enabled(state: &AppState) -> bool {
    if let Ok(Some(e)) = crate::config::runtime::get(&state.pg, "providers.user_byok_enabled").await {
        return e.value == "true";
    }
    state.boot.providers.user_byok_enabled
}

#[derive(Serialize)]
pub struct ProviderOut {
    /// Row id — addressable for the multi-row `llm` CRUD; present for every row.
    pub id: Uuid,
    pub role: String,
    /// Display name (multi-LLM). NULL/ignored for the single-row roles.
    pub label: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub enabled: bool,
    /// Whether an encrypted API key is stored (the key itself is never returned).
    pub api_key_set: bool,
    /// Reasoning-control override (`auto|none|toggle|levels|budget|always_on`);
    /// NULL ⇒ auto-detect. Only meaningful for the `llm` role. See [`crate::reasoning`].
    pub reasoning_mode: Option<String>,
    /// The deployment-default llm row (multi-LLM). Always false for single roles.
    pub is_default: bool,
}

/// Accepted `reasoning_mode` override values (NULL/`auto` ⇒ auto-detect).
const REASONING_MODES: [&str; 6] = ["auto", "none", "toggle", "levels", "budget", "always_on"];

/// `GET /api/admin/providers` — deployment provider rows, keys masked.
pub async fn list_providers(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<ProviderOut>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::PROVIDERS_MANAGE).await?;
    let rows = sqlx::query!(
        r#"SELECT id, role, label, base_url, model, enabled, reasoning_mode, is_default,
                  (api_key_encrypted IS NOT NULL) AS "api_key_set!"
           FROM provider_configs
           WHERE scope = 'deployment'
           ORDER BY role, is_default DESC, label NULLS FIRST"#
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ProviderOut {
                id: r.id,
                role: r.role,
                label: r.label,
                base_url: r.base_url,
                model: r.model,
                enabled: r.enabled,
                api_key_set: r.api_key_set,
                reasoning_mode: r.reasoning_mode,
                is_default: r.is_default,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct UpsertProvider {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Write-only. Empty/omitted ⇒ keep the existing stored key.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Reasoning-control override; `auto`/empty/omitted ⇒ auto-detect.
    #[serde(default)]
    pub reasoning_mode: Option<String>,
}

fn default_enabled() -> bool {
    true
}

/// `PUT /api/admin/providers/{role}` — upsert a deployment provider row.
pub async fn set_provider(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(role): Path<String>,
    Json(body): Json<UpsertProvider>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::PROVIDERS_MANAGE).await?;
    if !ROLES.contains(&role.as_str()) {
        return Err(AppError::Validation(format!("unknown provider role: {role}")));
    }
    // LLM is a multi-row list now (mig 0091) — its named rows are created/edited via
    // the dedicated /api/admin/providers/llm CRUD, not this single-row upsert.
    if role == "llm" {
        return Err(AppError::Validation(
            "LLM providers are managed as a list — use /api/admin/providers/llm".into(),
        ));
    }
    // Normalise the reasoning-mode override: trim, lower-case, treat empty/`auto`
    // as "no override" (stored NULL → auto-detect), and reject unknown values.
    let reasoning_mode: Option<String> = match body
        .reasoning_mode
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty() && s != "auto")
    {
        Some(m) if REASONING_MODES.contains(&m.as_str()) => Some(m),
        Some(m) => return Err(AppError::Validation(format!("unknown reasoning_mode: {m}"))),
        None => None,
    };

    // Encrypt a non-empty key; empty/omitted leaves the stored key untouched.
    let api_key_encrypted: Option<String> = match body.api_key.as_deref().map(str::trim) {
        Some(k) if !k.is_empty() => {
            let _key = state.message_key.ok_or_else(|| {
                AppError::Validation(
                    "set message_encryption_key before storing a provider API key".into(),
                )
            })?;
            Some(crate::crypto::encrypt_at_rest(k)?)
        }
        _ => None,
    };
    let base_url = body.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let model = body.model.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        r#"INSERT INTO provider_configs
               (id, role, scope, scope_id, base_url, model, api_key_encrypted, enabled, reasoning_mode, updated_by, updated_at)
           VALUES ($1, $2, 'deployment', NULL, $3, $4, $5, $6, $7, $8, now())
           ON CONFLICT (role) WHERE scope = 'deployment' AND role <> 'llm'
           DO UPDATE SET
               base_url = EXCLUDED.base_url,
               model = EXCLUDED.model,
               api_key_encrypted = COALESCE(EXCLUDED.api_key_encrypted, provider_configs.api_key_encrypted),
               enabled = EXCLUDED.enabled,
               reasoning_mode = EXCLUDED.reasoning_mode,
               updated_by = EXCLUDED.updated_by,
               updated_at = now()"#,
        Uuid::now_v7(),
        role,
        base_url,
        model,
        api_key_encrypted,
        body.enabled,
        reasoning_mode,
        ctx.user_id,
    )
    .execute(&mut *tx)
    .await?;

    // Audit — never the key, only the fact a key was set this call.
    let mut event = crate::audit::AuditEvent::action("provider.changed", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("provider_config".into());
    event.payload = Some(json!({
        "role": role,
        "scope": "deployment",
        "base_url": base_url,
        "model": model,
        "enabled": body.enabled,
        "reasoning_mode": reasoning_mode,
        "api_key_changed": api_key_encrypted.is_some(),
    }));
    crate::audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;

    // Embed role is special: changing it changes the vector
    // space → NEVER silently recreate. Probe the desired model's dimension, compare
    // to the active index's provenance, and if different, stage it as `desired` and
    // tell the UI a re-index is required (search keeps using the active model until
    // an explicit re-index runs). When the index isn't seeded yet (no KB built),
    // there's nothing to migrate — the first ingest seeds it.
    if role == "embed" {
        let resolved = state.providers.resolve(&state.pg, "embed", None).await.ok().flatten();
        let d_base = resolved.as_ref().and_then(|p| p.base_url.clone());
        let d_model = resolved.as_ref().and_then(|p| p.model.clone());
        let d_key = resolved.as_ref().and_then(|p| p.api_key.clone());
        // Hand-built override map = the DESIRED embed config (NOT the active overlay).
        let mut probe = serde_json::Map::new();
        if let Some(u) = &d_base { probe.insert("embed_base_url".into(), u.clone().into()); }
        if let Some(m) = &d_model { probe.insert("embed_model".into(), m.clone().into()); }
        if let Some(k) = &d_key { probe.insert("embed_api_key".into(), k.clone().into()); }
        if let Ok(info) = crate::ml::embed_info(&state.http, &state.boot.ml.base_url, probe).await {
            if let Ok(Some(active)) = crate::embedding_index::active(&state.pg, state.message_key).await {
                let changed = active.model != info.model
                    || active.dim != info.dimension
                    || active.base_url.as_deref() != d_base.as_deref();
                if changed {
                    crate::embedding_index::set_desired(
                        &state.pg, state.message_key, &info.model, d_base.as_deref(), d_key.as_deref(), info.dimension, ctx.user_id,
                    )
                    .await?;
                    let n: i64 = sqlx::query_scalar!("SELECT count(*) FROM kb_documents")
                        .fetch_one(&state.pg)
                        .await?
                        .unwrap_or(0);
                    return Ok(Json(json!({ "ok": true, "reindex_required": true, "indexed_documents": n })));
                }
            }
        }
    }

    Ok(Json(json!({ "ok": true })))
}

// --- Per-user BYOK -----------------------------------------------------------

#[derive(Serialize)]
pub struct MyProviderOut {
    pub role: String,
    /// The caller's own row (drives the form), or null when they have none.
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub enabled: bool,
    pub api_key_set: bool,
    /// Which scope's value the resolver will actually use: `user` | `deployment`
    /// | `default` (the ML service's `.env`).
    pub source: String,
}

/// `GET /api/me/providers` — the caller's provider rows (keys masked) + the
/// effective source per role. Readable by any authed user; whether the UI shows
/// the editor is driven by `user_byok_enabled`.
pub async fn list_my_providers(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<serde_json::Value>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("no user".into()))?;

    let user_rows = sqlx::query!(
        r#"SELECT role, base_url, model, enabled,
                  (api_key_encrypted IS NOT NULL) AS "api_key_set!"
           FROM provider_configs WHERE scope = 'user' AND scope_id = $1"#,
        uid,
    )
    .fetch_all(&state.pg)
    .await?;
    let dep_rows = sqlx::query!(
        r#"SELECT role, enabled FROM provider_configs WHERE scope = 'deployment'"#
    )
    .fetch_all(&state.pg)
    .await?;

    let users: HashMap<&str, &_> = user_rows.iter().map(|r| (r.role.as_str(), r)).collect();
    let deps: HashMap<&str, bool> = dep_rows.iter().map(|r| (r.role.as_str(), r.enabled)).collect();

    let providers: Vec<MyProviderOut> = ROLES
        .iter()
        .map(|&role| {
            let u = users.get(role);
            // Match the resolver: a present user row wins (even disabled → it is
            // returned disabled → no inject → ML default); else deployment; else default.
            let source = match u {
                Some(r) if r.enabled => "user",
                Some(_) => "default",
                None => match deps.get(role) {
                    Some(true) => "deployment",
                    _ => "default",
                },
            };
            MyProviderOut {
                role: role.to_string(),
                base_url: u.and_then(|r| r.base_url.clone()),
                model: u.and_then(|r| r.model.clone()),
                enabled: u.map(|r| r.enabled).unwrap_or(true),
                api_key_set: u.map(|r| r.api_key_set).unwrap_or(false),
                source: source.to_string(),
            }
        })
        .collect();

    Ok(Json(json!({
        "user_byok_enabled": byok_enabled(&state).await,
        "providers": providers,
    })))
}

/// `PUT /api/me/providers/{role}` — upsert the caller's own provider row.
pub async fn set_my_provider(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    device: MaybeDevice,
    Path(role): Path<String>,
    Json(body): Json<UpsertProvider>,
) -> Result<Json<serde_json::Value>> {
    // Provider writes are barred from a paired device: overwriting a role's
    // endpoint would reroute the owner's model traffic elsewhere. Model settings
    // are changed from an interactive web session only.
    device.require_session()?;
    if !byok_enabled(&state).await {
        return Err(AppError::Forbidden("bring-your-own-key is not enabled on this deployment".into()));
    }
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("no user".into()))?;
    if !ROLES.contains(&role.as_str()) {
        return Err(AppError::Validation(format!("unknown provider role: {role}")));
    }
    // Some roles are deployment-wide only (embed shares one vector index;
    // rerank/verify are platform-quality knobs) — reject per-user writes.
    if crate::providers::DEPLOYMENT_WIDE.contains(&role.as_str()) {
        let what = match role.as_str() {
            "embed" => "the embedding model",
            "rerank" => "the reranker",
            "verify" => "the verifier",
            _ => "this provider",
        };
        return Err(AppError::Validation(format!(
            "{what} is deployment-wide and cannot be set per-user"
        )));
    }
    // LLM is a multi-row list now (mig 0091) — personal named rows go through the
    // dedicated /api/me/providers/llm CRUD, not this single-row upsert.
    if role == "llm" {
        return Err(AppError::Validation(
            "your LLM providers are managed as a list — use /api/me/providers/llm".into(),
        ));
    }

    let api_key_encrypted: Option<String> = match body.api_key.as_deref().map(str::trim) {
        Some(k) if !k.is_empty() => {
            let _key = state.message_key.ok_or_else(|| {
                AppError::Validation("set message_encryption_key before storing a provider API key".into())
            })?;
            Some(crate::crypto::encrypt_at_rest(k)?)
        }
        _ => None,
    };
    let base_url = body.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let model = body.model.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        r#"INSERT INTO provider_configs
               (id, role, scope, scope_id, base_url, model, api_key_encrypted, enabled, updated_by, updated_at)
           VALUES ($1, $2, 'user', $3, $4, $5, $6, $7, $3, now())
           ON CONFLICT (role, scope_id) WHERE scope = 'user' AND role <> 'llm'
           DO UPDATE SET
               base_url = EXCLUDED.base_url,
               model = EXCLUDED.model,
               api_key_encrypted = COALESCE(EXCLUDED.api_key_encrypted, provider_configs.api_key_encrypted),
               enabled = EXCLUDED.enabled,
               updated_by = EXCLUDED.updated_by,
               updated_at = now()"#,
        Uuid::now_v7(),
        role,
        uid,
        base_url,
        model,
        api_key_encrypted,
        body.enabled,
    )
    .execute(&mut *tx)
    .await?;

    let mut event = crate::audit::AuditEvent::action("provider.changed", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("provider_config".into());
    event.payload = Some(json!({
        "role": role,
        "scope": "user",
        "base_url": base_url,
        "model": model,
        "enabled": body.enabled,
        "api_key_changed": api_key_encrypted.is_some(),
    }));
    crate::audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;

    Ok(Json(json!({ "ok": true })))
}

/// `DELETE /api/me/providers/{role}` — drop the caller's row, reverting that role
/// to the deployment/default provider.
pub async fn delete_my_provider(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    device: MaybeDevice,
    Path(role): Path<String>,
) -> Result<Json<serde_json::Value>> {
    device.require_session()?;
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("no user".into()))?;
    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "DELETE FROM provider_configs WHERE scope = 'user' AND scope_id = $1 AND role = $2",
        uid,
        role,
    )
    .execute(&mut *tx)
    .await?;

    let mut event = crate::audit::AuditEvent::action("provider.changed", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("provider_config".into());
    event.payload = Some(json!({ "role": role, "scope": "user", "op": "delete" }));
    crate::audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;

    Ok(Json(json!({ "ok": true })))
}

// --- Multiple named LLM providers (mig 0091) ---------------------------------
//
// The `llm` role holds several named rows per scope. Deployment rows are admin
// CRUD; a user's own rows are BYOK (gated by `user_byok_enabled`). One row per
// scope is the default (the fallback when a chat has no pick). The composer picks
// per chat via GET /api/me/llm-providers + the per-turn `chat.send` field.

/// Accepted values for a create/update of a named llm provider.
#[derive(Deserialize)]
pub struct UpsertLlm {
    pub label: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Write-only. Empty/omitted ⇒ keep the existing stored key (on update).
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub reasoning_mode: Option<String>,
}

/// Normalise a reasoning-mode override: trim/lower-case, treat empty/`auto` as "no
/// override" (NULL → auto-detect), reject anything unknown.
fn norm_reasoning_mode(v: Option<&str>) -> Result<Option<String>> {
    match v.map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty() && s != "auto") {
        Some(m) if REASONING_MODES.contains(&m.as_str()) => Ok(Some(m)),
        Some(m) => Err(AppError::Validation(format!("unknown reasoning_mode: {m}"))),
        None => Ok(None),
    }
}

/// Encrypt a non-empty api_key (requires the deployment key); empty/omitted ⇒ None
/// (keep existing on update / no key on create).
fn encrypt_optional_key(state: &AppState, api_key: &Option<String>) -> Result<Option<String>> {
    match api_key.as_deref().map(str::trim) {
        Some(k) if !k.is_empty() => {
            let _ = state.message_key.ok_or_else(|| {
                AppError::Validation("set message_encryption_key before storing a provider API key".into())
            })?;
            Ok(Some(crate::crypto::encrypt_at_rest(k)?))
        }
        _ => Ok(None),
    }
}

fn nz(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(String::from)
}

/// Shared create for a named llm row at a scope. `scope_id` = NULL (deployment) or
/// the user id. When no default exists yet at this scope, the new row becomes the
/// default (so a fresh set is immediately resolvable).
async fn create_llm_row(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    scope: &str,
    scope_id: Option<Uuid>,
    body: UpsertLlm,
) -> Result<Json<serde_json::Value>> {
    let label = body.label.trim().to_string();
    if label.is_empty() {
        return Err(AppError::Validation("a display name is required".into()));
    }
    let reasoning_mode = norm_reasoning_mode(body.reasoning_mode.as_deref())?;
    let api_key_encrypted = encrypt_optional_key(state, &body.api_key)?;
    let base_url = nz(&body.base_url);
    let model = nz(&body.model);

    let mut tx = state.pg.begin().await?;
    // Duplicate-name guard within the scope (NULL-safe compare on scope_id).
    let dup = sqlx::query_scalar!(
        r#"SELECT EXISTS(
             SELECT 1 FROM provider_configs
             WHERE role = 'llm' AND scope = $1 AND scope_id IS NOT DISTINCT FROM $2 AND label = $3
           ) AS "e!""#,
        scope, scope_id, label,
    )
    .fetch_one(&mut *tx)
    .await?;
    if dup {
        return Err(AppError::Validation(format!("a provider named \"{label}\" already exists")));
    }
    let has_default = sqlx::query_scalar!(
        r#"SELECT EXISTS(
             SELECT 1 FROM provider_configs
             WHERE role = 'llm' AND scope = $1 AND scope_id IS NOT DISTINCT FROM $2 AND is_default
           ) AS "e!""#,
        scope, scope_id,
    )
    .fetch_one(&mut *tx)
    .await?;
    let is_default = !has_default;

    let id = Uuid::now_v7();
    sqlx::query!(
        r#"INSERT INTO provider_configs
               (id, role, scope, scope_id, label, base_url, model, api_key_encrypted, enabled, reasoning_mode, is_default, updated_by, updated_at)
           VALUES ($1, 'llm', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now())"#,
        id, scope, scope_id, label, base_url, model, api_key_encrypted, body.enabled, reasoning_mode, is_default, ctx.user_id,
    )
    .execute(&mut *tx)
    .await?;

    let mut event = crate::audit::AuditEvent::action("provider.changed", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("provider_config".into());
    event.payload = Some(json!({
        "role": "llm", "scope": scope, "op": "create", "label": label,
        "base_url": base_url, "model": model, "enabled": body.enabled,
        "reasoning_mode": reasoning_mode, "is_default": is_default,
        "api_key_changed": api_key_encrypted.is_some(),
    }));
    crate::audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true, "id": id, "is_default": is_default })))
}

/// Shared update for a named llm row (scoped to the caller's scope).
async fn update_llm_row(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    scope: &str,
    scope_id: Option<Uuid>,
    id: Uuid,
    body: UpsertLlm,
) -> Result<Json<serde_json::Value>> {
    let label = body.label.trim().to_string();
    if label.is_empty() {
        return Err(AppError::Validation("a display name is required".into()));
    }
    let reasoning_mode = norm_reasoning_mode(body.reasoning_mode.as_deref())?;
    let api_key_encrypted = encrypt_optional_key(state, &body.api_key)?;
    let base_url = nz(&body.base_url);
    let model = nz(&body.model);

    let mut tx = state.pg.begin().await?;
    let dup = sqlx::query_scalar!(
        r#"SELECT EXISTS(
             SELECT 1 FROM provider_configs
             WHERE role = 'llm' AND scope = $1 AND scope_id IS NOT DISTINCT FROM $2 AND label = $3 AND id <> $4
           ) AS "e!""#,
        scope, scope_id, label, id,
    )
    .fetch_one(&mut *tx)
    .await?;
    if dup {
        return Err(AppError::Validation(format!("a provider named \"{label}\" already exists")));
    }
    let n = sqlx::query!(
        r#"UPDATE provider_configs SET
               label = $4,
               base_url = $5,
               model = $6,
               api_key_encrypted = COALESCE($7, api_key_encrypted),
               enabled = $8,
               reasoning_mode = $9,
               updated_by = $10,
               updated_at = now()
           WHERE id = $1 AND role = 'llm' AND scope = $2 AND scope_id IS NOT DISTINCT FROM $3"#,
        id, scope, scope_id, label, base_url, model, api_key_encrypted, body.enabled, reasoning_mode, ctx.user_id,
    )
    .execute(&mut *tx)
    .await?;
    if n.rows_affected() == 0 {
        return Err(AppError::NotFound("provider not found".into()));
    }
    let mut event = crate::audit::AuditEvent::action("provider.changed", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("provider_config".into());
    event.payload = Some(json!({
        "role": "llm", "scope": scope, "op": "update", "id": id, "label": label,
        "base_url": base_url, "model": model, "enabled": body.enabled,
        "reasoning_mode": reasoning_mode, "api_key_changed": api_key_encrypted.is_some(),
    }));
    crate::audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

/// Shared delete for a named llm row. If it was the scope's default, promote
/// another of the caller's rows so the scope keeps a resolvable default.
async fn delete_llm_row(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    scope: &str,
    scope_id: Option<Uuid>,
    id: Uuid,
) -> Result<Json<serde_json::Value>> {
    let mut tx = state.pg.begin().await?;
    let was_default = sqlx::query_scalar!(
        r#"DELETE FROM provider_configs
           WHERE id = $1 AND role = 'llm' AND scope = $2 AND scope_id IS NOT DISTINCT FROM $3
           RETURNING is_default"#,
        id, scope, scope_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(was_default) = was_default else {
        return Err(AppError::NotFound("provider not found".into()));
    };
    if was_default {
        // Promote the oldest remaining row at this scope to default (if any).
        sqlx::query!(
            r#"UPDATE provider_configs SET is_default = true
               WHERE id = (
                 SELECT id FROM provider_configs
                 WHERE role = 'llm' AND scope = $1 AND scope_id IS NOT DISTINCT FROM $2
                 ORDER BY id LIMIT 1
               )"#,
            scope, scope_id,
        )
        .execute(&mut *tx)
        .await?;
    }
    let mut event = crate::audit::AuditEvent::action("provider.changed", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("provider_config".into());
    event.payload = Some(json!({ "role": "llm", "scope": scope, "op": "delete", "id": id }));
    crate::audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

/// Mark one llm row the scope's default (clears the previous default first to
/// satisfy the one-default-per-scope index).
async fn set_llm_default_row(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    scope: &str,
    scope_id: Option<Uuid>,
    id: Uuid,
) -> Result<Json<serde_json::Value>> {
    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        r#"UPDATE provider_configs SET is_default = false
           WHERE role = 'llm' AND scope = $1 AND scope_id IS NOT DISTINCT FROM $2 AND is_default"#,
        scope, scope_id,
    )
    .execute(&mut *tx)
    .await?;
    let n = sqlx::query!(
        r#"UPDATE provider_configs SET is_default = true
           WHERE id = $1 AND role = 'llm' AND scope = $2 AND scope_id IS NOT DISTINCT FROM $3"#,
        id, scope, scope_id,
    )
    .execute(&mut *tx)
    .await?;
    if n.rows_affected() == 0 {
        return Err(AppError::NotFound("provider not found".into()));
    }
    let mut event = crate::audit::AuditEvent::action("provider.changed", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("provider_config".into());
    event.payload = Some(json!({ "role": "llm", "scope": scope, "op": "set_default", "id": id }));
    crate::audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

// Admin (deployment) llm CRUD -------------------------------------------------

/// `POST /api/admin/providers/llm` — create a named deployment llm provider.
pub async fn create_admin_llm(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<UpsertLlm>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::PROVIDERS_MANAGE).await?;
    create_llm_row(&state, &ctx, "deployment", None, body).await
}

/// `PUT /api/admin/providers/llm/{id}` — edit a named deployment llm provider.
pub async fn update_admin_llm(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpsertLlm>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::PROVIDERS_MANAGE).await?;
    update_llm_row(&state, &ctx, "deployment", None, id, body).await
}

/// `DELETE /api/admin/providers/llm/{id}` — remove a named deployment llm provider.
pub async fn delete_admin_llm(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::PROVIDERS_MANAGE).await?;
    delete_llm_row(&state, &ctx, "deployment", None, id).await
}

/// `PUT /api/admin/providers/llm/{id}/default` — mark the deployment default llm.
pub async fn set_admin_llm_default(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::PROVIDERS_MANAGE).await?;
    set_llm_default_row(&state, &ctx, "deployment", None, id).await
}

// User (BYOK) llm CRUD --------------------------------------------------------

/// `POST /api/me/providers/llm` — create a personal named llm provider (BYOK).
pub async fn create_my_llm(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    device: MaybeDevice,
    Json(body): Json<UpsertLlm>,
) -> Result<Json<serde_json::Value>> {
    // Model-provider writes are barred from a paired device (endpoint reroute is
    // a traffic-redirection vector); web session only.
    device.require_session()?;
    if !byok_enabled(&state).await {
        return Err(AppError::Forbidden("bring-your-own-key is not enabled on this deployment".into()));
    }
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("no user".into()))?;
    create_llm_row(&state, &ctx, "user", Some(uid), body).await
}

/// `PUT /api/me/providers/llm/{id}` — edit a personal named llm provider (BYOK).
pub async fn update_my_llm(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    device: MaybeDevice,
    Path(id): Path<Uuid>,
    Json(body): Json<UpsertLlm>,
) -> Result<Json<serde_json::Value>> {
    device.require_session()?;
    if !byok_enabled(&state).await {
        return Err(AppError::Forbidden("bring-your-own-key is not enabled on this deployment".into()));
    }
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("no user".into()))?;
    update_llm_row(&state, &ctx, "user", Some(uid), id, body).await
}

/// `DELETE /api/me/providers/llm/{id}` — remove a personal named llm provider.
pub async fn delete_my_llm(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    device: MaybeDevice,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    device.require_session()?;
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("no user".into()))?;
    delete_llm_row(&state, &ctx, "user", Some(uid), id).await
}

// Selection list + per-chat active pointer ------------------------------------

#[derive(Deserialize)]
pub struct LlmListQuery {
    #[serde(default)]
    pub chat_id: Option<Uuid>,
}

/// `GET /api/me/llm-providers?chat_id=<uuid?>` — the caller's SELECTABLE llm
/// providers (enabled deployment rows + their own enabled rows), each with its
/// reasoning capability (so the composer re-derives the Tune control per pick) and
/// an `is_active` flag for the given chat (its stored provider, else the default).
pub async fn list_llm_providers(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<LlmListQuery>,
) -> Result<Json<serde_json::Value>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("no user".into()))?;
    let rows = sqlx::query!(
        r#"SELECT id, scope, label, base_url, model, reasoning_mode, is_default,
                  (api_key_encrypted IS NOT NULL) AS "api_key_set!"
           FROM provider_configs
           WHERE role = 'llm' AND enabled
             AND ( scope = 'deployment' OR (scope = 'user' AND scope_id = $1) )
           ORDER BY (scope = 'user'), is_default DESC, label"#,
        uid,
    )
    .fetch_all(&state.pg)
    .await?;

    // The chat's stored pick (when still visible), else the default (user default
    // wins over deployment default — matching resolve_llm).
    let stored: Option<Uuid> = match q.chat_id {
        Some(cid) => sqlx::query_scalar!("SELECT llm_provider_id FROM chats WHERE id = $1", cid)
            .fetch_optional(&state.pg)
            .await?
            .flatten(),
        None => None,
    };
    let active_id = stored
        .filter(|s| rows.iter().any(|r| r.id == *s))
        .or_else(|| {
            rows.iter()
                .filter(|r| r.is_default)
                .max_by_key(|r| r.scope == "user")
                .map(|r| r.id)
        });

    let list: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let cap = crate::reasoning::detect(r.base_url.as_deref(), r.model.as_deref(), r.reasoning_mode.as_deref());
            json!({
                "id": r.id,
                "label": r.label,
                "model": r.model,
                "base_url": r.base_url,
                "api_key_set": r.api_key_set,
                "source": if r.scope == "user" { "user" } else { "deployment" },
                "enabled": true,
                "is_default": r.is_default,
                "is_active": Some(r.id) == active_id,
                "reasoning": cap,
            })
        })
        .collect();
    Ok(Json(json!({ "providers": list, "active_id": active_id })))
}

#[derive(Deserialize)]
pub struct SetChatLlm {
    /// The provider to remember for this chat; `null` clears back to the default.
    #[serde(default)]
    pub provider_id: Option<Uuid>,
}

/// `PUT /api/me/chats/{chat_id}/llm-provider` — remember an llm provider for a chat
/// (composer switch on an existing chat, without sending a turn). Validates
/// visibility and chat ownership.
pub async fn set_chat_llm_provider(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
    Json(body): Json<SetChatLlm>,
) -> Result<Json<serde_json::Value>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("no user".into()))?;
    if let Some(pid) = body.provider_id {
        if !crate::providers::visible_llm(&state.pg, ctx.user_id, pid).await? {
            return Err(AppError::Validation("that provider is not available to you".into()));
        }
    }
    let n = sqlx::query!(
        "UPDATE chats SET llm_provider_id = $2 WHERE id = $1 AND owner_user_id = $3",
        chat_id, body.provider_id, uid,
    )
    .execute(&state.pg)
    .await?;
    if n.rows_affected() == 0 {
        return Err(AppError::NotFound("chat not found".into()));
    }
    Ok(Json(json!({ "ok": true })))
}

/// Build the llm probe override map: form values (possibly unsaved) win; a saved
/// row supplies the rest (its decrypted key) when `id` is given and visible.
async fn probe_llm_overrides(
    state: &AppState,
    user_id: Option<Uuid>,
    id: Option<Uuid>,
    body: &UpsertProvider,
) -> crate::ml::ProviderOverrides {
    let saved = match id {
        Some(id) => crate::providers::resolve_llm(&state.pg, state.message_key, user_id, None, Some(id))
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let base_url = nz(&body.base_url).or_else(|| saved.as_ref().and_then(|r| r.base_url.clone()));
    let model = nz(&body.model).or_else(|| saved.as_ref().and_then(|r| r.model.clone()));
    let api_key = nz(&body.api_key).or_else(|| saved.as_ref().and_then(|r| r.api_key.clone()));
    let mut map = crate::ml::ProviderOverrides::new();
    if let Some(v) = base_url { map.insert("llm_base_url".into(), v.into()); }
    if let Some(v) = model { map.insert("llm_model".into(), v.into()); }
    if let Some(v) = api_key { map.insert("llm_api_key".into(), v.into()); }
    map
}

#[derive(Deserialize)]
pub struct LlmTestBody {
    /// Optional saved row to fall back to for the key (edit-without-retyping).
    #[serde(default)]
    pub id: Option<Uuid>,
    #[serde(flatten)]
    pub form: UpsertProvider,
}

async fn run_llm_probe(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    user_id: Option<Uuid>,
    scope: &str,
    body: &LlmTestBody,
) -> Result<Json<crate::ml::ProviderTestResult>> {
    let overrides = probe_llm_overrides(state, user_id, body.id, &body.form).await;
    let result = crate::ml::test_provider(&state.http, &state.boot.ml.base_url, "llm", overrides).await?;
    let mut event = crate::audit::AuditEvent::action("provider.tested", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("provider_config".into());
    event.payload = Some(json!({ "role": "llm", "scope": scope, "ok": result.ok, "error": result.error }));
    let _ = crate::audit::append(&state.pg, &event).await;
    Ok(Json(result))
}

/// `POST /api/admin/providers/llm/test` — probe a named deployment llm (admin).
pub async fn test_admin_llm(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<LlmTestBody>,
) -> Result<Json<crate::ml::ProviderTestResult>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::PROVIDERS_MANAGE).await?;
    run_llm_probe(&state, &ctx, None, "deployment", &body).await
}

/// `POST /api/me/providers/llm/test` — probe a personal named llm (BYOK).
pub async fn test_my_llm(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<LlmTestBody>,
) -> Result<Json<crate::ml::ProviderTestResult>> {
    if !byok_enabled(&state).await {
        return Err(AppError::Forbidden("bring-your-own-key is not enabled on this deployment".into()));
    }
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("no user".into()))?;
    run_llm_probe(&state, &ctx, Some(uid), "user", &body).await
}

// --- Test connection ---------------------------------------------------------

/// Build the single-role override map for a probe: a form value (possibly
/// unsaved) wins when non-empty, else the saved + decrypted config the resolver
/// returns. An empty field stays absent ⇒ the ML service uses its own default.
async fn probe_overrides(
    state: &AppState,
    role: &str,
    user_id: Option<Uuid>,
    body: &UpsertProvider,
) -> crate::ml::ProviderOverrides {
    let resolved = state.providers.resolve(&state.pg, role, user_id).await.ok().flatten();
    let nz = |s: &Option<String>| s.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(String::from);
    let base_url = nz(&body.base_url).or_else(|| resolved.as_ref().and_then(|r| r.base_url.clone()));
    let model = nz(&body.model).or_else(|| resolved.as_ref().and_then(|r| r.model.clone()));
    let api_key = nz(&body.api_key).or_else(|| resolved.as_ref().and_then(|r| r.api_key.clone()));
    let mut map = crate::ml::ProviderOverrides::new();
    if let Some(v) = base_url {
        map.insert(format!("{role}_base_url"), v.into());
    }
    if let Some(v) = model {
        map.insert(format!("{role}_model"), v.into());
    }
    if let Some(v) = api_key {
        map.insert(format!("{role}_api_key"), v.into());
    }
    map
}

/// Resolve → probe via ML → audit. `scope` is "deployment" or "user". The api_key
/// is never put in the audit payload (only the pass/fail + reason).
async fn run_probe(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    role: &str,
    user_id: Option<Uuid>,
    scope: &str,
    body: &UpsertProvider,
) -> Result<Json<crate::ml::ProviderTestResult>> {
    if !ROLES.contains(&role) {
        return Err(AppError::Validation(format!("unknown provider role: {role}")));
    }
    let overrides = probe_overrides(state, role, user_id, body).await;
    let result = crate::ml::test_provider(&state.http, &state.boot.ml.base_url, role, overrides).await?;

    let mut event = crate::audit::AuditEvent::action("provider.tested", ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("provider_config".into());
    event.payload = Some(json!({
        "role": role,
        "scope": scope,
        "ok": result.ok,
        "error": result.error,
    }));
    let _ = crate::audit::append(&state.pg, &event).await;

    Ok(Json(result))
}

/// `POST /api/admin/providers/{role}/test` — probe the deployment provider for a
/// role (admin). Body may carry unsaved form values to test before saving.
pub async fn test_provider(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(role): Path<String>,
    Json(body): Json<UpsertProvider>,
) -> Result<Json<crate::ml::ProviderTestResult>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::PROVIDERS_MANAGE).await?;
    run_probe(&state, &ctx, &role, None, "deployment", &body).await
}

/// `POST /api/me/providers/{role}/test` — probe the caller's effective provider
/// (BYOK). Gated by `user_byok_enabled`, like `set_my_provider`.
pub async fn test_my_provider(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(role): Path<String>,
    Json(body): Json<UpsertProvider>,
) -> Result<Json<crate::ml::ProviderTestResult>> {
    if !byok_enabled(&state).await {
        return Err(AppError::Forbidden("bring-your-own-key is not enabled on this deployment".into()));
    }
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("no user".into()))?;
    run_probe(&state, &ctx, &role, Some(uid), "user", &body).await
}

// --- Embedding-index provenance + re-index -----------------------------------

/// `GET /api/admin/embedding-index` — active model/dim/collection + migration
/// status (drives the re-index progress UI). Keys never returned.
pub async fn embedding_index_status(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::PROVIDERS_MANAGE).await?;
    let p = crate::embedding_index::get(&state.pg).await?;
    Ok(Json(match p {
        Some(p) => json!({
            "seeded": true,
            "embed_model": p.embed_model,
            "embed_base_url": p.embed_base_url,
            "dim": p.dim,
            "collection_name": p.collection_name,
            "status": p.status,
            "reindex_done": p.reindex_done,
            "reindex_total": p.reindex_total,
            "error": p.error,
            "desired_model": p.desired_model,
            "desired_base_url": p.desired_base_url,
            "desired_dim": p.desired_dim,
        }),
        None => json!({ "seeded": false }),
    }))
}

/// `POST /api/admin/embedding-index/reindex` — enqueue the durable blue-green
/// re-index for the staged desired embed model (or retry a failed one).
pub async fn reindex_embeddings(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::PROVIDERS_MANAGE).await?;
    match crate::embedding_index::get(&state.pg).await? {
        Some(p) if p.status == "reindexing" => {
            return Err(AppError::Validation("a re-index is already running".into()));
        }
        Some(p) if p.desired_model.is_some() => {}
        _ => {
            return Err(AppError::Validation(
                "no embed-model change is pending — change the embed provider first".into(),
            ));
        }
    }
    let id = crate::scheduler::enqueue(&state.pg, crate::scheduler::TaskType::ReindexEmbeddings, json!({})).await?;
    Ok(Json(json!({ "ok": true, "task_id": id })))
}
