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

//! Streaming dictation (composer mic): an **STT-only** path that reuses the live-
//! voice streaming-STT adapters but drives no LLM/TTS. The browser streams PCM16
//! frames; the engine streams live `partial`s (deltas, shown muted as the user
//! speaks) and, on stop, the settled transcript is committed into the composer.
//!
//! `gpt-realtime-whisper` (the dictation model) does NOT support server-side
//! turn detection, so we run `turn_detection:null` and commit on stop — the
//! deltas give live text-while-speaking; the commit yields the final transcript.
//! When no streaming engine is configured the SPA keeps its batch dictation.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::auth::AuthContext;
use crate::state::AppState;
use crate::ws::protocol::ServerFrame;

use super::stt_stream::{self, SttEvent};
use super::{stt_openai_realtime, VoiceLiveResolved};

/// How long to wait for the committed final transcript after the user stops.
const COMMIT_WAIT: Duration = Duration::from_secs(4);

/// One socket's streaming-dictation session: PCM ingress + a stop signal + the relay.
pub struct DictationSession {
    pcm_tx: mpsc::Sender<Vec<u8>>,
    stop_tx: mpsc::Sender<()>,
    relay: Mutex<Option<JoinHandle<()>>>,
}

impl DictationSession {
    /// Resolve the engine config, open the streaming STT, and spawn the relay.
    /// Engine/key/url are shared with live-voice STT; the **model** is the dictation
    /// model (`voice.dictation_model`, default `gpt-realtime-whisper`).
    pub async fn start(
        state: AppState,
        _ctx: AuthContext,
        tx: mpsc::Sender<ServerFrame>,
    ) -> Arc<Self> {
        let cfg = VoiceLiveResolved::load(&state.pg, state.message_key, &state.boot.voice_live).await;
        let (pcm_tx, pcm_rx) = mpsc::channel::<Vec<u8>>(256);
        let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
        let relay = tokio::spawn(relay_loop(cfg, pcm_rx, stop_rx, tx));
        Arc::new(DictationSession { pcm_tx, stop_tx, relay: Mutex::new(Some(relay)) })
    }

    /// Decode + forward one captured audio frame. Ephemeral (at-most-once): a full
    /// queue drops the frame rather than block the socket.
    pub async fn on_audio_chunk(&self, audio_base64: String) {
        if let Ok(pcm) = B64.decode(audio_base64.as_bytes()) {
            let _ = self.pcm_tx.try_send(pcm);
        }
    }

    /// Graceful stop (`voice.dictate.stop`): ask the relay to commit + flush the
    /// final transcript, then bounded-join it (so the commit isn't aborted).
    pub async fn stop(&self) {
        let _ = self.stop_tx.try_send(());
        let h = { self.relay.lock().unwrap().take() };
        if let Some(h) = h {
            let _ = tokio::time::timeout(COMMIT_WAIT + Duration::from_secs(1), h).await;
        }
    }

    /// Hard teardown (disconnect): abort the relay (drops `SttStream` → engine closes).
    pub fn shutdown(&self) {
        if let Some(h) = self.relay.lock().unwrap().take() {
            h.abort();
        }
    }
}

impl Drop for DictationSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Open the configured streaming engine and pump PCM → transcript frames. Deltas
/// stream as `voice.partial` (live interim); `completed` transcripts and the
/// commit-on-stop final go out as `voice.transcript` (appended to the composer).
async fn relay_loop(
    cfg: VoiceLiveResolved,
    mut pcm_rx: mpsc::Receiver<Vec<u8>>,
    mut stop_rx: mpsc::Receiver<()>,
    tx: mpsc::Sender<ServerFrame>,
) {
    let sr = cfg.stt_sample_rate.max(8_000);
    let model = if cfg.dictation_model.is_empty() { cfg.stt_model.clone() } else { cfg.dictation_model.clone() };
    let opened = match cfg.stt_stream_kind.as_str() {
        "openai_realtime" => {
            // `turn_detection:null` — gpt-realtime-whisper rejects server VAD; it still
            // streams deltas as audio arrives, and emits the final on our commit.
            stt_openai_realtime::open(cfg.stt_api_key.as_deref().unwrap_or_default(), &model, &cfg.stt_language, sr, false).await
        }
        "websocket" if !cfg.stt_stream_url.is_empty() => stt_stream::open(&cfg.stt_stream_url, sr).await,
        _ => {
            let _ = tx.send(ServerFrame::VoiceError { message: "no streaming dictation engine configured".into() }).await;
            return;
        }
    };
    let mut stt = match opened {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "streaming dictation engine unavailable");
            let _ = tx.send(ServerFrame::VoiceError { message: format!("dictation engine unavailable: {e}") }).await;
            return;
        }
    };

    // The engine emits INCREMENTAL deltas (one word/token at a time); accumulate them
    // so the SPA always gets the full transcript-so-far as the live partial.
    let mut partial = String::new();
    loop {
        tokio::select! {
            pcm = pcm_rx.recv() => match pcm {
                Some(pcm) => stt.send_pcm(pcm).await,
                None => break, // ingress closed
            },
            stop = stop_rx.recv() => {
                if stop.is_none() { break; }
                // Commit the buffered audio and wait briefly for the final transcript.
                if stt.manual_commit() {
                    stt.commit().await;
                    let deadline = tokio::time::Instant::now() + COMMIT_WAIT;
                    loop {
                        match tokio::time::timeout_at(deadline, stt.recv()).await {
                            Ok(Some(SttEvent::Final { text })) => {
                                partial.clear();
                                emit_transcript(&tx, &text).await;
                                break;
                            }
                            Ok(Some(SttEvent::Partial { text })) => {
                                partial.push_str(&text);
                                emit_partial(&tx, partial.trim()).await;
                            }
                            Ok(Some(SttEvent::Error { message })) => tracing::warn!(%message, "dictation STT error on commit"),
                            Ok(None) | Err(_) => break,
                        }
                    }
                }
                break;
            }
            ev = stt.recv() => match ev {
                Some(SttEvent::Partial { text }) => {
                    partial.push_str(&text);
                    emit_partial(&tx, partial.trim()).await;
                }
                Some(SttEvent::Final { text }) => {
                    partial.clear();
                    emit_transcript(&tx, &text).await;
                }
                Some(SttEvent::Error { message }) => tracing::warn!(%message, "streaming dictation STT error"),
                None => break, // engine closed
            }
        }
    }
}

async fn emit_partial(tx: &mpsc::Sender<ServerFrame>, text: &str) {
    if !text.trim().is_empty() {
        let _ = tx.send(ServerFrame::VoicePartial { text: text.to_string() }).await;
    }
}

async fn emit_transcript(tx: &mpsc::Sender<ServerFrame>, text: &str) {
    let t = text.trim();
    if !t.is_empty() {
        let _ = tx.send(ServerFrame::VoiceTranscript { text: t.to_string() }).await;
    }
}
