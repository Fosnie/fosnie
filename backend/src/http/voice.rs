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

//! Voice REST primitives: dictation (audio → text) and read-aloud
//! (text → audio). The WebSocket carries the same primitives for the live path;
//! these REST endpoints back non-WS clients and deterministic tests. Both require
//! an authenticated user and the `features.voice` host flag; the engines live on
//! the ML service (OpenAI-audio contract).

use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Voice is allowed when the host enables it AND none of the caller's groups
/// disables it (per-group feature flags, Tier-2 #8).
async fn require_voice(state: &AppState, ctx: &AuthContext) -> Result<()> {
    if state.features.enabled_for(state, ctx, "voice").await {
        Ok(())
    } else {
        Err(AppError::Validation("voice is not enabled for you".into()))
    }
}

async fn audit_voice(state: &AppState, ctx: &AuthContext, action: &str, bytes: usize) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("voice".into());
    ev.payload = Some(serde_json::json!({ "bytes": bytes })); // size only, never content
    let _ = audit::append(&state.pg, &ev).await;
}

#[derive(Deserialize)]
pub struct TranscribeQuery {
    #[serde(default)]
    pub mime: Option<String>,
}

/// Dictation: raw audio body → `{ text }`.
pub async fn transcribe(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<TranscribeQuery>,
    body: Bytes,
) -> Result<Json<serde_json::Value>> {
    require_voice(&state, &ctx).await?;
    crate::cache::rate_limit_guard(&state.redis, &format!("voice:{}", ctx.user_id.unwrap_or_default()), 40, 60).await?;
    if body.is_empty() {
        return Err(AppError::Validation("empty audio".into()));
    }
    let mime = q.mime.as_deref().unwrap_or("application/octet-stream");
    let text = crate::ml::transcribe(&state.http, &state.boot.ml.base_url, &body, mime, crate::ml::provider_overrides(&state, ctx.user_id).await).await?;
    audit_voice(&state, &ctx, "voice.transcribed", body.len()).await;
    Ok(Json(serde_json::json!({ "text": text })))
}

#[derive(Deserialize)]
pub struct SpeakRequest {
    pub text: String,
    #[serde(default)]
    pub voice: Option<String>,
}

/// Read-aloud: `{ text, voice? }` → audio bytes (Content-Type from the engine).
pub async fn speak(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<SpeakRequest>,
) -> Result<Response> {
    require_voice(&state, &ctx).await?;
    crate::cache::rate_limit_guard(&state.redis, &format!("voice:{}", ctx.user_id.unwrap_or_default()), 40, 60).await?;
    if body.text.trim().is_empty() {
        return Err(AppError::Validation("empty text".into()));
    }
    let (audio, mime) =
        crate::ml::synthesize(&state.http, &state.boot.ml.base_url, &body.text, body.voice.as_deref(), crate::ml::provider_overrides(&state, ctx.user_id).await)
            .await?;
    audit_voice(&state, &ctx, "voice.synthesized", audio.len()).await;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, mime)],
        Body::from(audio),
    )
        .into_response())
}
