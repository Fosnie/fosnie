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

//! Live / streaming voice (some mode-3 aspects are deferred). A per-socket orchestrator
//! couples the existing WebSocket transport, the existing chat-turn, and cancel:
//!
//! ```text
//! client PCM ─▶ streaming STT ─▶ partials+finals ─▶ (endpoint ∧ Smart-Turn)
//!            ─▶ final transcript ─▶ chat::run_turn (LLM token stream)
//!            ─▶ SentenceAggregator (clauses) ─▶ streaming TTS ─▶ audio chunks
//!   ‖ barge-in monitor runs throughout ‖
//! ```
//!
//! Every engine is an **external, in-perimeter, swappable** service; any absent
//! engine **degrades** (batch STT per utterance / silence-threshold gate / batch
//! TTS per clause) so the loop still runs. The orchestrator lives in Rust because
//! it is transport + turn-taking + cancel, all of which Rust already owns; the LLM
//! stage reuses `chat::run_turn` verbatim (the live turn persists like any chat).

pub mod aggregate;
pub mod dictation;
pub mod session;
pub mod stt_openai_realtime;
pub mod stt_stream;
pub mod tts_stream;
pub mod turn;

pub use dictation::DictationSession;
pub use session::Session;

/// The conversation state surfaced to the SPA (`voice.state`). Distinct visuals per
/// state are mandatory for a professional voice UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceState {
    Idle,
    Listening,
    Capturing,
    Thinking,
    Speaking,
    Interrupted,
    Error,
}

impl VoiceState {
    pub fn as_str(self) -> &'static str {
        match self {
            VoiceState::Idle => "idle",
            VoiceState::Listening => "listening",
            VoiceState::Capturing => "capturing",
            VoiceState::Thinking => "thinking",
            VoiceState::Speaking => "speaking",
            VoiceState::Interrupted => "interrupted",
            VoiceState::Error => "error",
        }
    }
}

/// The runtime-tunable dials for the live-voice loop, read fresh per session from
/// the super-admin knob store (mirrors `ml::rag_overrides`). Defaults match the
/// knob registry in `http::superadmin`.
#[derive(Debug, Clone)]
pub struct VoiceKnobs {
    /// Trailing-silence (ms) before the speaker's turn is ended (the latency lever).
    pub silence_threshold_ms: u64,
    /// Minimum total *speech* (ms above the RMS gate) in an utterance before it fires a
    /// turn. Below this it's noise/a blip — discarded without transcription, so a
    /// near-silent clip can't be hallucinated into text. Low enough to keep short words.
    pub min_speech_ms: u64,
    /// Default to push-to-talk rather than an open VAD-gated mic.
    pub ptt_default: bool,
    /// Require browser echo cancellation before honouring barge-in.
    pub aec_required: bool,
    /// Consult the turn-detection sidecar (else the silence threshold alone decides).
    pub turn_detection: bool,
}

impl Default for VoiceKnobs {
    fn default() -> Self {
        Self {
            // Without a semantic turn detector the silence gate alone ends a turn, so
            // keep it generous — a natural mid-thought pause must not chop the speaker
            // (especially in hands-free). The Smart-Turn sidecar can fire sooner.
            silence_threshold_ms: 1500,
            min_speech_ms: 200,
            ptt_default: true,
            aec_required: true,
            turn_detection: false,
        }
    }
}

impl VoiceKnobs {
    /// Load the dials from the runtime config; an unset key keeps its default.
    pub async fn load(pg: &sqlx::PgPool) -> Self {
        use crate::config::runtime;
        async fn getb(pg: &sqlx::PgPool, key: &str, dflt: bool) -> bool {
            runtime::get(pg, key).await.ok().flatten().map(|e| e.value == "true").unwrap_or(dflt)
        }
        async fn getu(pg: &sqlx::PgPool, key: &str, dflt: u64) -> u64 {
            runtime::get(pg, key).await.ok().flatten().and_then(|e| e.value.parse::<u64>().ok()).unwrap_or(dflt)
        }
        let d = Self::default();
        VoiceKnobs {
            silence_threshold_ms: getu(pg, "voice.silence_threshold_ms", d.silence_threshold_ms).await,
            min_speech_ms: getu(pg, "voice.min_speech_ms", d.min_speech_ms).await,
            ptt_default: getb(pg, "voice.ptt_default", d.ptt_default).await,
            aec_required: getb(pg, "voice.aec_required", d.aec_required).await,
            turn_detection: getb(pg, "voice.turn_detection", d.turn_detection).await,
        }
    }
}

/// The live-voice **engine** config (STT/TTS endpoints + models + keys), resolved
/// fresh per session from the runtime config store with the boot `[voice_live]` as
/// fallback. Mirrors [`VoiceKnobs::load`]. API keys are
/// stored AES-256-GCM-encrypted under `voice.*_api_key_enc` (so the audit row only
/// holds ciphertext) and decrypted here with the deployment `message_key`.
#[derive(Debug, Clone)]
pub struct VoiceLiveResolved {
    pub stt_stream_kind: String, // none | websocket | openai_realtime
    pub stt_stream_url: String,
    pub stt_model: String,
    /// STT model for streaming **dictation** (composer mic). Distinct from the live-
    /// voice `stt_model`: dictation wants a live-delta transcription model
    /// (`gpt-realtime-whisper`) under server VAD. Falls back to `stt_model` if unset.
    pub dictation_model: String,
    pub stt_language: String,
    pub stt_sample_rate: u32,
    pub stt_api_key: Option<String>,
    pub tts_stream: bool,
    pub tts_stream_url: String,
    pub tts_model: String,
    pub tts_voice: String,
    pub tts_api_key: Option<String>,
    pub turn_detector_url: String,
}

impl VoiceLiveResolved {
    /// Config keys (all `voice.*`) so the admin endpoint, the generic Config editor
    /// filter, and this resolver agree on one list.
    pub const STR_KEYS: [&'static str; 10] = [
        "voice.stt_stream_kind",
        "voice.stt_stream_url",
        "voice.stt_model",
        "voice.dictation_model",
        "voice.stt_language",
        "voice.tts_stream_url",
        "voice.tts_model",
        "voice.tts_voice",
        "voice.turn_detector_url",
        "voice.stt_sample_rate", // int-as-string
    ];
    pub const ENC_KEYS: [&'static str; 2] = ["voice.stt_api_key_enc", "voice.tts_api_key_enc"];

    pub async fn load(pg: &sqlx::PgPool, message_key: Option<[u8; 32]>, boot: &crate::config::VoiceLiveConfig) -> Self {
        use crate::config::runtime;
        async fn gets(pg: &sqlx::PgPool, key: &str, dflt: &str) -> String {
            runtime::get(pg, key).await.ok().flatten().map(|e| e.value).filter(|v| !v.is_empty()).unwrap_or_else(|| dflt.to_string())
        }
        async fn getb(pg: &sqlx::PgPool, key: &str, dflt: bool) -> bool {
            runtime::get(pg, key).await.ok().flatten().map(|e| e.value == "true").unwrap_or(dflt)
        }
        // Decrypt a stored ciphertext key (None when unset or undecryptable).
        async fn getkey(pg: &sqlx::PgPool, key: &str, mk: Option<[u8; 32]>) -> Option<String> {
            let ct = runtime::get(pg, key).await.ok().flatten().map(|e| e.value).filter(|v| !v.is_empty())?;
            let _mk = mk?;
            match crate::crypto::decrypt_at_rest(&ct) {
                Ok(pt) => Some(pt),
                Err(_) => {
                    tracing::warn!(%key, "voice api key failed to decrypt; ignoring");
                    None
                }
            }
        }
        let sr = runtime::get(pg, "voice.stt_sample_rate").await.ok().flatten()
            .and_then(|e| e.value.parse::<u32>().ok()).unwrap_or(boot.stt_sample_rate);
        VoiceLiveResolved {
            stt_stream_kind: gets(pg, "voice.stt_stream_kind", &boot.stt_stream_kind).await,
            stt_stream_url: gets(pg, "voice.stt_stream_url", &boot.stt_stream_url).await,
            stt_model: gets(pg, "voice.stt_model", "").await,
            dictation_model: gets(pg, "voice.dictation_model", "gpt-realtime-whisper").await,
            stt_language: gets(pg, "voice.stt_language", "en").await,
            stt_sample_rate: sr.max(8_000),
            stt_api_key: getkey(pg, "voice.stt_api_key_enc", message_key).await,
            tts_stream: getb(pg, "voice.tts_stream", boot.tts_stream).await,
            tts_stream_url: gets(pg, "voice.tts_stream_url", &boot.tts_stream_url).await,
            tts_model: gets(pg, "voice.tts_model", "kokoro").await,
            tts_voice: gets(pg, "voice.tts_voice", "").await,
            tts_api_key: getkey(pg, "voice.tts_api_key_enc", message_key).await,
            turn_detector_url: gets(pg, "voice.turn_detector_url", &boot.turn_detector_url).await,
        }
    }

    /// Is a streaming-STT engine configured? Drives streaming dictation (else the
    /// composer mic falls back to batch transcription).
    pub fn has_streaming_stt(&self) -> bool {
        (self.stt_stream_kind == "websocket" && !self.stt_stream_url.is_empty())
            || (self.stt_stream_kind == "openai_realtime"
                && self.stt_api_key.as_deref().is_some_and(|k| !k.is_empty()))
    }
}
