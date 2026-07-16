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

//! Runtime config + branding admin. Typed, validated,
//! audited config edits over the existing `config_settings` (via
//! `config::runtime`); branding logo/favicon upload to disk + a pointer row in
//! `branding_assets`. Admin-gated.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::keycloak::AuthUser;
use crate::auth::permissions;
use crate::auth::AuthContext;
use crate::config::runtime::{self, ConfigValueType};
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Shared "any client-admin" gate. Retained as the canonical helper that the
/// Enterprise crate imports for its own admin surfaces; Core's own gates now
/// resolve through `state.rbac.require_permission` with a catalogue permission.
pub fn require_admin(ctx: &AuthContext) -> Result<()> {
    if ctx.is_admin() || ctx.break_glass {
        Ok(())
    } else {
        Err(AppError::Forbidden("admin only".into()))
    }
}

/// Edition gate: white-label branding is Enterprise-only. Off in Core
/// ⇒ branding writes 403 (defense-in-depth — never rely on the SPA hiding the UI).
/// Resolved through the `FeatureResolver` seam (`whoami.capabilities.white_label`).
async fn require_white_label(state: &AppState, ctx: &AuthContext) -> Result<()> {
    crate::http::require_capability(state, ctx, "white_label", "white-label").await
}

// --- Config ------------------------------------------------------------------

#[derive(Serialize)]
pub struct ConfigOut {
    pub key: String,
    pub value: String,
    pub value_type: String,
    pub scope: String,
}

pub async fn list_config(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<ConfigOut>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::CONFIG_MANAGE).await?;
    // Exclude keys that don't belong in the raw config console:
    //  - `test.*`        — integration-test fixtures (config_audit.rs) polluting a shared dev DB.
    //  - `integration.*` — connector on/off flags, managed in the Integrations tab.
    //  - `branding.*`    — colours/fonts, edited in the Branding tab's theme form.
    //  - `moderation.*`  — weights/lawful-basis/notice, edited in the Moderation tab.
    //  - `welcome.*`     — login welcome message, edited in the Announcements tab.
    // What's left are genuine tuning knobs (automation limits, audit retention, …).
    let rows = sqlx::query!(
        r#"SELECT key, value, value_type::text AS "value_type!", scope FROM config_settings
           WHERE key NOT LIKE 'test.%' AND key NOT LIKE 'integration.%'
             AND key NOT LIKE 'branding.%' AND key NOT LIKE 'moderation.%'
             AND key NOT LIKE 'welcome.%'
           ORDER BY key"#
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ConfigOut { key: r.key, value: r.value, value_type: r.value_type, scope: r.scope })
            .collect(),
    ))
}

// --- Public theme (branding colours/fonts) -----------------------------------

#[derive(Serialize, Default)]
pub struct ThemeOut {
    pub primary: Option<String>,
    pub accent: Option<String>,
    pub bg: Option<String>,
    pub fg: Option<String>,
    pub font_sans: Option<String>,
    pub font_serif: Option<String>,
}

/// A conservative CSS colour: #hex, rgb()/hsl()/oklch()-style functions, or a
/// plain named colour. Anything carrying characters that could break out of a
/// CSS custom-property value is rejected — the SPA injects these into `:root`,
/// so sanitising here is the injection boundary.
fn safe_colour(s: &str) -> Option<String> {
    let v = s.trim();
    if v.is_empty() || v.len() > 64 {
        return None;
    }
    let lower = v.to_ascii_lowercase();
    if lower.contains(['<', '>', ';', '{', '}', '\\'])
        || lower.contains("url(")
        || lower.contains("expression")
        || lower.contains("/*")
    {
        return None;
    }
    let ok = v.chars().all(|c| c.is_ascii_alphanumeric() || " #.,%()/-".contains(c));
    ok.then(|| v.to_string())
}

/// A conservative CSS font-family string (names, quotes, commas, spaces).
fn safe_font(s: &str) -> Option<String> {
    let v = s.trim();
    if v.is_empty() || v.len() > 120 {
        return None;
    }
    if v.contains(['<', '>', ';', '{', '}', '\\']) || v.to_ascii_lowercase().contains("url(") {
        return None;
    }
    let ok = v.chars().all(|c| c.is_ascii_alphanumeric() || " ,'\"-".contains(c));
    ok.then(|| v.to_string())
}

/// Public branding theme: colours/fonts the SPA applies as `:root` CSS variables
/// at boot. No auth — it loads before sign-in. Values are
/// sanitised; an absent/invalid value is omitted and the SPA keeps its default.
pub async fn get_theme(State(state): State<AppState>) -> Json<ThemeOut> {
    async fn read(state: &AppState, key: &str) -> Option<String> {
        runtime::get(&state.pg, key).await.ok().flatten().map(|e| e.value)
    }
    Json(ThemeOut {
        primary: read(&state, "branding.primary").await.and_then(|v| safe_colour(&v)),
        accent: read(&state, "branding.accent").await.and_then(|v| safe_colour(&v)),
        bg: read(&state, "branding.bg").await.and_then(|v| safe_colour(&v)),
        fg: read(&state, "branding.fg").await.and_then(|v| safe_colour(&v)),
        font_sans: read(&state, "branding.font_sans").await.and_then(|v| safe_font(&v)),
        font_serif: read(&state, "branding.font_serif").await.and_then(|v| safe_font(&v)),
    })
}

#[derive(Deserialize)]
pub struct SetConfig {
    pub value: String,
    #[serde(default = "default_type")]
    pub value_type: String,
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_type() -> String {
    "string".into()
}
fn default_scope() -> String {
    "global".into()
}

fn parse_type(s: &str) -> Result<ConfigValueType> {
    Ok(match s {
        "string" => ConfigValueType::String,
        "int" => ConfigValueType::Int,
        "float" => ConfigValueType::Float,
        "bool" => ConfigValueType::Bool,
        "json" => ConfigValueType::Json,
        other => return Err(AppError::Validation(format!("unknown value_type: {other}"))),
    })
}

pub async fn set_config(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(key): Path<String>,
    Json(body): Json<SetConfig>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::CONFIG_MANAGE).await?;
    // Theme colours/fonts are white-label (Enterprise); welcome.*/moderation.*/tuning
    // keys stay Core. Gate only the branding.* writes (defense-in-depth).
    if key.starts_with("branding.") {
        require_white_label(&state, &ctx).await?;
    }
    // Enabling the workflows engine (off → on): stamp a dispatch watermark so the
    // historical event backlog accumulated while the feature was off is
    // fast-forwarded rather than replayed in one avalanche when the relay resumes.
    // Stamp only on the transition — re-saving
    // "true" while already on must not move the watermark forward (that would skip
    // legitimately-queued events).
    if key == "features.workflows" && body.value == "true" {
        let was_on = crate::features::enabled_for_user(&state, None, "workflows").await;
        if !was_on {
            let now = time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|e| AppError::Other(anyhow::anyhow!("format watermark: {e}")))?;
            runtime::set(
                &state.pg,
                "workflows.dispatch_watermark",
                &now,
                ConfigValueType::String,
                "global",
                ctx.user_id,
                ctx.role.as_str(),
            )
            .await?;
        }
    }
    let vt = parse_type(&body.value_type)?;
    // runtime::set validates the value, writes, and audits config.changed atomically.
    runtime::set(&state.pg, &key, &body.value, vt, &body.scope, ctx.user_id, ctx.role.as_str()).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Branding ----------------------------------------------------------------

#[derive(Serialize)]
pub struct BrandingOut {
    pub kind: String,
    pub mime: String,
}

pub async fn list_branding(
    State(state): State<AppState>,
    AuthUser(_ctx): AuthUser,
) -> Result<Json<Vec<BrandingOut>>> {
    let rows = sqlx::query!(r#"SELECT kind::text AS "kind!", mime FROM branding_assets ORDER BY kind"#)
        .fetch_all(&state.pg)
        .await?;
    Ok(Json(rows.into_iter().map(|r| BrandingOut { kind: r.kind, mime: r.mime }).collect()))
}

pub async fn get_branding(
    State(state): State<AppState>,
    AuthUser(_ctx): AuthUser,
    Path(kind): Path<String>,
) -> Result<Response> {
    let row = sqlx::query!(
        "SELECT disk_path, mime FROM branding_assets WHERE kind = ($1::text)::branding_kind",
        kind
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("no such branding asset".into()))?;
    // Branding assets are written by the Enterprise edition; resolve the stored
    // (relative, once backfilled) path under `branding_dir`. Legacy-guard keeps an
    // absolute value working until the boot backfill normalises it.
    let abs = crate::storage::resolve_file(&state.boot.storage.branding_dir, &row.disk_path);
    let bytes = tokio::fs::read(&abs)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read branding: {e}")))?;
    Ok((StatusCode::OK, [(header::CONTENT_TYPE, row.mime)], Body::from(bytes)).into_response())
}
