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

//! Configuring the telephone line.
//!
//! Break-glass rather than an ordinary admin route, because these settings decide
//! whether this instance answers a telephone at all and hold the carrier's credential.
//!
//! The lines themselves are not here. Which numbers are answered, and whose account and
//! which agent each one runs as, is ordinary administration done daily and gated by a
//! permission of its own; this is the switch and the secret beneath it, and break-glass
//! is deliberately ephemeral.
//!
//! The carrier's credential is write-only. It is stored encrypted, so the
//! configuration-changed audit row holds nothing but ciphertext, and it is reported
//! back only as whether one is present.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::breakglass::SuperAdmin;
use crate::config::runtime::{self, ConfigValueType};
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::telephony::TelephonyResolved;

const PROVIDERS: [&str; 3] = ["none", "twilio", "audiosocket"];

#[derive(Serialize)]
pub struct TelephonyOut {
    pub provider: String,
    pub public_base_url: String,
    pub max_concurrent_calls: usize,
    /// Whether a carrier credential is stored. Never the credential itself.
    pub auth_token_set: bool,
    /// Where a telephone system on the practice's own network is listened for. Empty
    /// means no port of our own is opened, which is what a deployment answering through a
    /// carrier does.
    pub audiosocket_listen: String,
    /// Whether the secret that telephone system presents is stored. Never the secret.
    pub audiosocket_key_set: bool,
    /// How many calls are up right now, so an operator can see the line working.
    pub calls_in_progress: usize,
}

/// `GET /api/admin/telephony` — the resolved line settings, credential masked.
pub async fn get_settings(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
) -> Result<Json<TelephonyOut>> {
    let cfg = TelephonyResolved::load(
        &state.pg,
        state.message_key,
        &state.boot.telephony,
        &state.boot.server.public_url,
    )
    .await;
    let auth_token_set = runtime::get(&state.pg, "telephony.auth_token_enc")
        .await
        .ok()
        .flatten()
        .map(|e| !e.value.is_empty())
        .unwrap_or(false);
    let audiosocket_key_set = runtime::get(&state.pg, "telephony.audiosocket_key_enc")
        .await
        .ok()
        .flatten()
        .map(|e| !e.value.is_empty())
        .unwrap_or(false);
    Ok(Json(TelephonyOut {
        audiosocket_listen: cfg.audiosocket_listen,
        audiosocket_key_set,
        provider: cfg.provider,
        public_base_url: cfg.public_base_url,
        max_concurrent_calls: cfg.max_concurrent_calls,
        auth_token_set,
        calls_in_progress: state.telephony.len(),
    }))
}

/// `GET /api/admin/telephony/preflight` — will a line actually answer?
///
/// Beside the settings, because the person who sets them is the person who needs to know
/// whether they are right. It makes a real request to the speech synthesiser, so it is
/// asked for rather than run on a page load.
pub async fn preflight(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
) -> Result<Json<Vec<crate::telephony::preflight::Check>>> {
    Ok(Json(crate::telephony::preflight::run(&state).await))
}

#[derive(Deserialize)]
pub struct UpsertTelephony {
    pub provider: String,
    #[serde(default)]
    pub public_base_url: String,
    #[serde(default)]
    pub max_concurrent_calls: Option<usize>,
    /// Write-only. Empty or omitted keeps the stored credential.
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Where to listen for the practice's own telephone system, as an address and port.
    /// Empty opens nothing. Takes effect when this process next starts, because a port is
    /// bound once.
    #[serde(default)]
    pub audiosocket_listen: Option<String>,
    /// Write-only, like the carrier credential.
    #[serde(default)]
    pub audiosocket_key: Option<String>,
}

/// `PUT /api/admin/telephony` — write the line settings, optionally rotating the
/// carrier credential.
pub async fn set_settings(
    State(state): State<AppState>,
    SuperAdmin(ctx): SuperAdmin,
    Json(body): Json<UpsertTelephony>,
) -> Result<Json<serde_json::Value>> {
    if !PROVIDERS.contains(&body.provider.as_str()) {
        return Err(AppError::Validation(format!("unknown telephony provider: {}", body.provider)));
    }
    let uid = ctx.user_id;
    let role = ctx.role.as_str();
    let s = |k: &'static str, v: String, t: ConfigValueType| {
        let pg = state.pg.clone();
        async move { runtime::set(&pg, k, &v, t, "global", uid, role).await }
    };
    s("telephony.provider", body.provider.clone(), ConfigValueType::String).await?;
    s("telephony.public_base_url", body.public_base_url.trim().trim_end_matches('/').to_string(), ConfigValueType::String).await?;
    if let Some(max) = body.max_concurrent_calls.filter(|n| *n > 0) {
        s("telephony.max_concurrent_calls", max.to_string(), ConfigValueType::Int).await?;
    }

    if let Some(listen) = body.audiosocket_listen.as_deref().map(str::trim) {
        s("telephony.audiosocket_listen", listen.to_string(), ConfigValueType::String).await?;
    }
    if let Some(key) = body.audiosocket_key.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        if state.message_key.is_none() {
            return Err(AppError::Validation(
                "set message_encryption_key before storing the telephone system's secret".into(),
            ));
        }
        let ct = crate::crypto::encrypt_at_rest(key)?;
        runtime::set(&state.pg, "telephony.audiosocket_key_enc", &ct, ConfigValueType::String, "global", uid, role).await?;
    }

    if let Some(token) = body.auth_token.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        if state.message_key.is_none() {
            return Err(AppError::Validation(
                "set message_encryption_key before storing the carrier credential".into(),
            ));
        }
        let ct = crate::crypto::encrypt_at_rest(token)?;
        runtime::set(&state.pg, "telephony.auth_token_enc", &ct, ConfigValueType::String, "global", uid, role).await?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}
