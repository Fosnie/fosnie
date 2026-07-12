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

//! Live-voice engine config admin. Host-admin GET/PUT over
//! the `voice.*` runtime settings that select the STT/TTS engines (Off / local
//! WebSocket / OpenAI). API keys are write-only: stored AES-256-GCM encrypted under
//! `voice.*_api_key_enc` (so the `config.changed` audit row only ever holds
//! ciphertext), returned only as a `*_api_key_set` boolean, never logged in clear.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::keycloak::AuthUser;
use crate::auth::permissions;
use crate::config::runtime::{self, ConfigValueType};
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::voice::VoiceLiveResolved;

const STT_KINDS: [&str; 3] = ["none", "websocket", "openai_realtime"];

#[derive(Serialize)]
pub struct VoiceLiveOut {
    pub stt_stream_kind: String,
    pub stt_stream_url: String,
    pub stt_model: String,
    pub dictation_model: String,
    pub stt_language: String,
    pub stt_sample_rate: u32,
    pub tts_stream: bool,
    pub tts_stream_url: String,
    pub tts_model: String,
    pub tts_voice: String,
    pub turn_detector_url: String,
    /// Whether an encrypted key is stored (the key itself is never returned).
    pub stt_api_key_set: bool,
    pub tts_api_key_set: bool,
}

/// `GET /api/admin/voice-live` — the resolved engine config, keys masked.
pub async fn get(State(state): State<AppState>, AuthUser(ctx): AuthUser) -> Result<Json<VoiceLiveOut>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::VOICE_MANAGE).await?;
    let c = VoiceLiveResolved::load(&state.pg, state.message_key, &state.boot.voice_live).await;
    async fn key_set(pg: &sqlx::PgPool, k: &str) -> bool {
        runtime::get(pg, k).await.ok().flatten().map(|e| !e.value.is_empty()).unwrap_or(false)
    }
    let stt_api_key_set = key_set(&state.pg, "voice.stt_api_key_enc").await;
    let tts_api_key_set = key_set(&state.pg, "voice.tts_api_key_enc").await;
    Ok(Json(VoiceLiveOut {
        stt_stream_kind: c.stt_stream_kind,
        stt_stream_url: c.stt_stream_url,
        stt_model: c.stt_model,
        dictation_model: c.dictation_model,
        stt_language: c.stt_language,
        stt_sample_rate: c.stt_sample_rate,
        tts_stream: c.tts_stream,
        tts_stream_url: c.tts_stream_url,
        tts_model: c.tts_model,
        tts_voice: c.tts_voice,
        turn_detector_url: c.turn_detector_url,
        stt_api_key_set,
        tts_api_key_set,
    }))
}

#[derive(Deserialize)]
pub struct UpsertVoiceLive {
    pub stt_stream_kind: String,
    #[serde(default)]
    pub stt_stream_url: String,
    #[serde(default)]
    pub stt_model: String,
    #[serde(default)]
    pub dictation_model: String,
    #[serde(default)]
    pub stt_language: String,
    #[serde(default = "default_rate")]
    pub stt_sample_rate: u32,
    #[serde(default)]
    pub tts_stream: bool,
    #[serde(default)]
    pub tts_stream_url: String,
    #[serde(default)]
    pub tts_model: String,
    #[serde(default)]
    pub tts_voice: String,
    #[serde(default)]
    pub turn_detector_url: String,
    /// Write-only. Empty/omitted ⇒ keep the existing stored key.
    #[serde(default)]
    pub stt_api_key: Option<String>,
    #[serde(default)]
    pub tts_api_key: Option<String>,
}

fn default_rate() -> u32 {
    16_000
}

/// `PUT /api/admin/voice-live` — write the engine config + (optionally) rotate keys.
pub async fn set(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<UpsertVoiceLive>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::VOICE_MANAGE).await?;
    if !STT_KINDS.contains(&body.stt_stream_kind.as_str()) {
        return Err(AppError::Validation(format!("unknown stt_stream_kind: {}", body.stt_stream_kind)));
    }
    let uid = ctx.user_id;
    let role = ctx.role.as_str();

    // Non-secret fields → plain runtime settings.
    let s = |k: &'static str, v: String, t: ConfigValueType| {
        let pg = state.pg.clone();
        async move { runtime::set(&pg, k, &v, t, "global", uid, role).await }
    };
    s("voice.stt_stream_kind", body.stt_stream_kind.clone(), ConfigValueType::String).await?;
    s("voice.stt_stream_url", body.stt_stream_url.trim().to_string(), ConfigValueType::String).await?;
    s("voice.stt_model", body.stt_model.trim().to_string(), ConfigValueType::String).await?;
    s("voice.dictation_model", body.dictation_model.trim().to_string(), ConfigValueType::String).await?;
    s("voice.stt_language", body.stt_language.trim().to_string(), ConfigValueType::String).await?;
    s("voice.stt_sample_rate", body.stt_sample_rate.max(8_000).to_string(), ConfigValueType::Int).await?;
    s("voice.tts_stream", body.tts_stream.to_string(), ConfigValueType::Bool).await?;
    s("voice.tts_stream_url", body.tts_stream_url.trim().to_string(), ConfigValueType::String).await?;
    s("voice.tts_model", body.tts_model.trim().to_string(), ConfigValueType::String).await?;
    s("voice.tts_voice", body.tts_voice.trim().to_string(), ConfigValueType::String).await?;
    s("voice.turn_detector_url", body.turn_detector_url.trim().to_string(), ConfigValueType::String).await?;

    // Keys → encrypted; empty/omitted leaves the stored key untouched.
    for (key, plain) in [("voice.stt_api_key_enc", &body.stt_api_key), ("voice.tts_api_key_enc", &body.tts_api_key)] {
        if let Some(k) = plain.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            if state.message_key.is_none() {
                return Err(AppError::Validation("set message_encryption_key before storing a voice API key".into()));
            }
            let ct = crate::crypto::encrypt_at_rest(k)?;
            runtime::set(&state.pg, key, &ct, ConfigValueType::String, "global", uid, role).await?;
        }
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}
