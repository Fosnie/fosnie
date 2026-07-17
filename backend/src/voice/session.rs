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

//! The per-socket live-voice orchestrator. One
//! [`Session`] per socket runs a capture loop that turns continuous PCM into user
//! turns and drives the reply:
//!
//! 1. **Capture** — incoming PCM frames feed energy-VAD silence timing (and, when
//!    present, the streaming-STT engine for live partials). The end-of-turn fires on
//!    `should_fire_turn` (silence threshold, optionally gated by the Smart-Turn
//!    sidecar).
//! 2. **Reply** — the final transcript drives `chat::run_turn` **verbatim** (the live
//!    turn persists like any chat). run_turn streams `ChatToken` frames into a **tap**
//!    channel; a drain task forwards every frame to the socket (so the SPA gets the
//!    editable transcript, citations, persistence for free) and feeds the deltas to a
//!    [`SentenceAggregator`], queueing each clause for streaming TTS.
//! 3. **Barge-in** — speech during playback fires the turn's `cancel` (stops the LLM,
//!    persists the partial) **and** a `tts_abort` (drops the TTS stream — no clip).
//!    Both must fire: the Hub's cancel is TTS-unaware (`signal_barge_in`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::AuthContext;
use crate::state::AppState;
use crate::ws::protocol::ServerFrame;

use super::aggregate::SentenceAggregator;
use super::stt_stream::{self, SttEvent};
use super::{tts_stream, turn, VoiceKnobs, VoiceState};

/// RMS (normalised 0..1) above which a PCM frame counts as speech.
const SPEECH_RMS: f64 = 0.012;
/// Barge-in needs a LOUDER, SUSTAINED signal than plain capture: the assistant's
/// own audio echoing into an open mic (imperfect AEC) is quieter than direct
/// speech, and a single spike must never cut the reply. Require `BARGE_RMS` for at
/// least `BARGE_MIN_MS` of continuous speech before interrupting.
const BARGE_RMS: f64 = 0.035;
const BARGE_MIN_MS: u64 = 320;
/// Consult the turn-detection sidecar once trailing silence reaches this (debounce).
const DETECT_AFTER_MS: u64 = 200;
/// Hard ceiling on a live turn that emits no frames at all. A stuck `run_turn`
/// (e.g. a hung generate with no token timeout) must never strand the UI on
/// "Thinking…": the drain loop resets this on every frame, so only a truly silent
/// turn trips it. Generous — well above the agent wall-clock.
const TURN_WATCHDOG_SECS: u64 = 240;

/// A live turn's cancel handles, shared with the capture loop / barge-in.
#[derive(Clone)]
struct TurnHandle {
    turn_id: Uuid,
    /// Stops `chat::run_turn` (LLM) — it persists the partial + emits `ChatInterrupted`.
    cancel: Arc<Notify>,
    /// Wakes every in-flight `speak_clause` so it drops its TTS stream (no audio clip).
    tts_abort: Arc<Notify>,
    /// Gate checked before each clause + chunk; cleared on barge-in.
    speaking: Arc<AtomicBool>,
}

/// Fire BOTH cancels for an in-flight live turn. The classic barge-in bug is
/// cancelling only the LLM and letting TTS run on — so this stops the LLM **and**
/// the TTS together, and clears the speaking gate.
fn signal_barge_in(h: &TurnHandle) {
    h.speaking.store(false, Ordering::SeqCst);
    h.cancel.notify_waiters();
    h.tts_abort.notify_waiters();
}

/// A clause queued for the TTS consumer, or a terminal marker.
enum TtsCmd {
    Clause(String),
    End,
    Stop,
}

/// One socket's live-voice session.
pub struct Session {
    socket_id: Uuid,
    ctx: AuthContext,
    state: AppState,
    /// The real socket sender (relayed chat frames + voice frames go here).
    tx: mpsc::Sender<ServerFrame>,
    /// To the capture loop (decoded PCM frames).
    pcm_tx: mpsc::Sender<Vec<u8>>,
    knobs: VoiceKnobs,
    /// Live-voice engine config resolved at session start (runtime override + boot
    /// fallback) — STT/TTS kind, URLs, models, voices, decrypted keys.
    voice_cfg: super::VoiceLiveResolved,
    /// Browser echo-cancellation is on (gates energy-driven barge-in).
    aec: bool,
    /// The chat this session drives (adopted from `voice.stream.start` or created by
    /// the first turn — captured back from the relayed `chat.created`).
    chat_id: Mutex<Option<Uuid>>,
    project_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    vstate: Mutex<VoiceState>,
    current_turn: Mutex<Option<TurnHandle>>,
    /// A plain client (no ML shared-secret header) for the external voice engines.
    ext_http: reqwest::Client,
    /// Background tasks (capture loop + per-turn drain/TTS), aborted on teardown.
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Session {
    /// Build the session, announce `listening`, and spawn its capture loop.
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        state: AppState,
        ctx: AuthContext,
        socket_id: Uuid,
        tx: mpsc::Sender<ServerFrame>,
        chat_id: Option<Uuid>,
        project_id: Option<Uuid>,
        agent_id: Option<Uuid>,
        mode: Option<String>,
        aec: bool,
    ) -> Arc<Self> {
        let knobs = VoiceKnobs::load(&state.pg).await;
        let voice_cfg = super::VoiceLiveResolved::load(&state.pg, state.message_key, &state.boot.voice_live).await;
        let mode = mode.unwrap_or_else(|| if knobs.ptt_default { "ptt".into() } else { "vad".into() });
        tracing::debug!(%socket_id, %mode, aec, stt = %voice_cfg.stt_stream_kind, "live-voice session start");

        let (pcm_tx, pcm_rx) = mpsc::channel::<Vec<u8>>(256);
        let session = Arc::new(Session {
            socket_id,
            ctx,
            state,
            tx,
            pcm_tx,
            knobs,
            voice_cfg,
            aec,
            chat_id: Mutex::new(chat_id),
            project_id,
            agent_id,
            vstate: Mutex::new(VoiceState::Listening),
            current_turn: Mutex::new(None),
            ext_http: reqwest::Client::new(),
            tasks: Mutex::new(Vec::new()),
        });
        session
            .emit(ServerFrame::VoiceLiveState { state: VoiceState::Listening.as_str().into() })
            .await;
        let cap = tokio::spawn(capture_loop(session.clone(), pcm_rx));
        session.tasks.lock().unwrap().push(cap);
        session
    }

    /// Decode + forward one captured audio frame to the capture loop. Ephemeral
    /// (at-most-once): a full queue drops the frame rather than block the socket.
    pub async fn on_audio_chunk(&self, audio_base64: String, _seq: u64) {
        if let Ok(pcm) = B64.decode(audio_base64.as_bytes()) {
            let _ = self.pcm_tx.try_send(pcm);
        }
    }

    /// User spoke over the assistant: cancel the in-flight reply (LLM + TTS) and
    /// return to listening. The partial answer is already persisted/shown.
    pub async fn barge_in(&self) {
        let handle = { self.current_turn.lock().unwrap().take() };
        if let Some(h) = handle {
            signal_barge_in(&h);
            metrics::counter!("voice_barge_in_total").increment(1);
            self.state.hub.cancel_turn(self.socket_id, h.turn_id); // parity (TTS-unaware)
            let mut ev = AuditEvent::action("voice.barge_in", self.ctx.role.as_str());
            ev.actor_user_id = self.ctx.user_id;
            ev.payload = Some(serde_json::json!({ "turn_id": h.turn_id }));
            let _ = audit::append(&self.state.pg, &ev).await;
        }
        self.set_state(VoiceState::Listening).await;
    }

    /// Explicit teardown (`voice.stream.end`): cancel any in-flight turn and abort
    /// all tasks (which drops their `Arc<Session>` clones, so the session can drop).
    pub async fn shutdown(&self) {
        self.barge_in().await;
        self.abort_tasks();
        *self.vstate.lock().unwrap() = VoiceState::Idle;
    }

    /// Disconnect teardown: abort the voice tasks but DO NOT cancel an in-flight
    /// `run_turn` — let it finish persisting so a reload/return resumes the answer
    /// from the DB row (the socket policy at `ws::handle_socket`). Aborting the drain
    /// task drops the tap, which `run_turn` tolerates (its `detached` path).
    pub fn detach(&self) {
        self.abort_tasks();
    }

    fn abort_tasks(&self) {
        let handles: Vec<JoinHandle<()>> = {
            let mut g = self.tasks.lock().unwrap();
            std::mem::take(&mut *g)
        };
        for h in handles {
            h.abort();
        }
    }

    /// Fire one user turn: spawn `chat::run_turn` against a tap channel, plus the
    /// drain + TTS tasks. Reuses the chat-turn verbatim (persists like any chat).
    pub async fn start_turn(self: &Arc<Self>, text: String) {
        let t0 = std::time::Instant::now(); // for the final→first-audio latency metric
        let turn_id = Uuid::now_v7();
        let cancel = Arc::new(Notify::new());
        let tts_abort = Arc::new(Notify::new());
        let speaking = Arc::new(AtomicBool::new(true));
        self.state.hub.add_turn(self.socket_id, turn_id, cancel.clone());
        *self.current_turn.lock().unwrap() = Some(TurnHandle {
            turn_id,
            cancel: cancel.clone(),
            tts_abort: tts_abort.clone(),
            speaking: speaking.clone(),
        });
        self.set_state(VoiceState::Thinking).await;

        let chat_id = { *self.chat_id.lock().unwrap() };
        let (tap_tx, tap_rx) = mpsc::channel::<ServerFrame>(256);
        let (tts_tx, tts_rx) = mpsc::channel::<TtsCmd>(8);

        // 1) The chat turn (LLM) — reused verbatim; streams frames into the tap.
        {
            let st = self.state.clone();
            let ctx = self.ctx.clone();
            let pid = self.project_id;
            let aid = self.agent_id;
            let socket_id = self.socket_id;
            let cancel2 = cancel.clone();
            tokio::spawn(async move {
                crate::chat::run_turn(
                    &st, &ctx, turn_id, chat_id, pid, aid, text, Vec::new(), Vec::new(), false, None, None, None, &tap_tx,
                    cancel2,
                )
                .await;
                st.hub.remove_turn(socket_id, turn_id);
                // tap_tx drops here → the drain loop ends.
            });
        }
        // 2) TTS consumer — speaks queued clauses in order, abortable by barge-in.
        let tts_task = {
            let me = self.clone();
            tokio::spawn(async move { me.tts_loop(tts_rx, tts_abort, speaking, t0).await })
        };
        // 3) Tap drain — forwards every frame to the socket; feeds the aggregator.
        let drain_task = {
            let me = self.clone();
            tokio::spawn(async move { me.drain_loop(tap_rx, tts_tx).await })
        };
        let mut t = self.tasks.lock().unwrap();
        t.push(tts_task);
        t.push(drain_task);
    }

    /// Drain the chat-turn tap: relay every frame to the SPA and chunk the token
    /// stream into clauses for TTS.
    async fn drain_loop(self: Arc<Self>, mut tap_rx: mpsc::Receiver<ServerFrame>, tts_tx: mpsc::Sender<TtsCmd>) {
        let mut agg = SentenceAggregator::new();
        loop {
            let frame = match tokio::time::timeout(
                std::time::Duration::from_secs(TURN_WATCHDOG_SECS),
                tap_rx.recv(),
            )
            .await
            {
                Ok(Some(frame)) => frame,
                Ok(None) => break, // tap closed: run_turn finished → normal end
                Err(_) => {
                    // Watchdog: no frame for the whole window — the turn is stuck (e.g. a
                    // hung generate). Cancel it, surface the timeout, stop TTS, and let
                    // `tts_loop` return the UI to listening so it never hangs on "Thinking…".
                    tracing::warn!("live-voice turn watchdog fired; forcing teardown");
                    let handle = { self.current_turn.lock().unwrap().clone() };
                    if let Some(h) = handle {
                        h.cancel.notify_waiters();
                        h.tts_abort.notify_waiters();
                    }
                    self.emit(ServerFrame::VoiceError {
                        message: "the assistant timed out".into(),
                    })
                    .await;
                    let _ = tts_tx.send(TtsCmd::Stop).await;
                    break;
                }
            };
            match &frame {
                ServerFrame::ChatCreated { chat_id } => {
                    *self.chat_id.lock().unwrap() = Some(*chat_id);
                }
                ServerFrame::ChatToken { delta, .. } => {
                    for clause in agg.push(delta) {
                        if tts_tx.send(TtsCmd::Clause(clause)).await.is_err() {
                            break;
                        }
                    }
                }
                ServerFrame::ChatCompleted { .. } => {
                    if let Some(tail) = agg.flush() {
                        let _ = tts_tx.send(TtsCmd::Clause(tail)).await;
                    }
                    let _ = tts_tx.send(TtsCmd::End).await;
                }
                ServerFrame::ChatInterrupted { .. } => {
                    let _ = tts_tx.send(TtsCmd::Stop).await;
                }
                ServerFrame::ChatError { message, .. } => {
                    self.emit(ServerFrame::VoiceError { message: message.clone() }).await;
                    let _ = tts_tx.send(TtsCmd::Stop).await;
                }
                _ => {}
            }
            // Relay EVERY chat frame to the SPA (transcript text, citations, …).
            let _ = self.tx.send(frame).await;
        }
    }

    /// Speak queued clauses in order; finalise the turn on End/Stop or tap close.
    async fn tts_loop(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<TtsCmd>,
        tts_abort: Arc<Notify>,
        speaking: Arc<AtomicBool>,
        t0: std::time::Instant,
    ) {
        let seq = AtomicU64::new(0);
        let latency_done = AtomicBool::new(false);
        let mut spoke = false;
        let mut total_bytes = 0usize;
        while let Some(cmd) = rx.recv().await {
            match cmd {
                TtsCmd::Clause(text) => {
                    if !speaking.load(Ordering::SeqCst) {
                        continue;
                    }
                    if !spoke {
                        self.set_state(VoiceState::Speaking).await;
                        spoke = true;
                    }
                    total_bytes += self.speak_clause(text, &tts_abort, &speaking, &seq, t0, &latency_done).await;
                }
                TtsCmd::End => {
                    self.emit(ServerFrame::VoiceTtsEnd).await;
                    break;
                }
                TtsCmd::Stop => break,
            }
        }
        if total_bytes > 0 {
            self.audit_synthesized(total_bytes).await;
        }
        self.finish_turn(VoiceState::Listening).await;
    }

    /// Synthesise one clause, streaming its audio chunks out as `voice.tts.chunk`.
    /// Returns the bytes synthesised. A barge-in (`tts_abort` / cleared `speaking`)
    /// stops it immediately and drops the stream (cuts audio cleanly).
    #[allow(clippy::too_many_arguments)]
    async fn speak_clause(
        &self,
        text: String,
        tts_abort: &Notify,
        speaking: &AtomicBool,
        seq: &AtomicU64,
        t0: std::time::Instant,
        latency_done: &AtomicBool,
    ) -> usize {
        if !speaking.load(Ordering::SeqCst) {
            return 0;
        }
        let vc = &self.voice_cfg;
        let voice_opt = (!vc.tts_voice.is_empty()).then_some(vc.tts_voice.as_str());
        let opened = if vc.tts_stream && !vc.tts_stream_url.is_empty() {
            tts_stream::stream_clause(&self.ext_http, &vc.tts_stream_url, &vc.tts_model, &text, voice_opt, vc.tts_api_key.as_deref()).await
        } else {
            tts_stream::batch_clause(&self.state.http, &self.state.boot.ml.base_url, &text, voice_opt, crate::ml::provider_overrides(&self.state, self.ctx.user_id).await).await
        };
        let mut ts = match opened {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "tts clause failed");
                return 0;
            }
        };
        let mime = ts.mime.clone();
        // Accumulate the WHOLE clause, then emit it as one frame. Each engine call
        // returns a complete, self-contained clip (e.g. an OpenAI mp3 with its own
        // header); the browser decodes a complete clip reliably, whereas separate
        // per-network-chunk frames concatenated client-side break the mp3 decoder.
        let mut buf: Vec<u8> = Vec::new();
        let aborted = loop {
            tokio::select! {
                chunk = ts.recv() => match chunk {
                    Some(b) => {
                        if !speaking.load(Ordering::SeqCst) {
                            break true; // barge-in mid-clause → drop the partial
                        }
                        buf.extend_from_slice(&b);
                    }
                    None => break false, // clause fully synthesised
                },
                _ = tts_abort.notified() => break true, // barge-in: drop `ts` → engine stops
            }
        };
        if aborted || buf.is_empty() || !speaking.load(Ordering::SeqCst) {
            return 0; // a clean cut emits nothing (no clipped/garbled tail)
        }
        // Voice-to-voice latency: final transcript → first audio out.
        if !latency_done.swap(true, Ordering::SeqCst) {
            metrics::histogram!("voice_turn_latency_ms").record(t0.elapsed().as_millis() as f64);
        }
        let s = seq.fetch_add(1, Ordering::SeqCst);
        self.emit(ServerFrame::VoiceTtsChunk {
            audio_base64: B64.encode(&buf),
            mime,
            seq: s,
        })
        .await;
        buf.len()
    }

    async fn emit(&self, f: ServerFrame) {
        let _ = self.tx.send(f).await;
    }

    async fn emit_final(&self, text: &str) {
        self.emit(ServerFrame::VoiceFinal { text: text.to_string() }).await;
    }

    async fn emit_error(&self, message: String) {
        self.emit(ServerFrame::VoiceError { message }).await;
        self.set_state(VoiceState::Listening).await;
    }

    async fn set_state(&self, s: VoiceState) {
        *self.vstate.lock().unwrap() = s;
        self.emit(ServerFrame::VoiceLiveState { state: s.as_str().into() }).await;
    }

    fn current_state(&self) -> VoiceState {
        *self.vstate.lock().unwrap()
    }

    async fn finish_turn(&self, to: VoiceState) {
        *self.current_turn.lock().unwrap() = None;
        self.set_state(to).await;
    }

    async fn audit_transcribed(&self, ms: u64, chars: usize) {
        let mut ev = AuditEvent::action("voice.transcribed", self.ctx.role.as_str());
        ev.actor_user_id = self.ctx.user_id;
        ev.payload = Some(serde_json::json!({ "ms": ms, "chars": chars }));
        let _ = audit::append(&self.state.pg, &ev).await;
    }

    async fn audit_synthesized(&self, bytes: usize) {
        let mut ev = AuditEvent::action("voice.synthesized", self.ctx.role.as_str());
        ev.actor_user_id = self.ctx.user_id;
        ev.payload = Some(serde_json::json!({ "bytes": bytes }));
        let _ = audit::append(&self.state.pg, &ev).await;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for h in self.tasks.lock().unwrap().iter() {
            h.abort();
        }
    }
}

// --- Capture loop ------------------------------------------------------------

fn rms_i16(pcm: &[u8]) -> f64 {
    let n = pcm.len() / 2;
    if n == 0 {
        return 0.0;
    }
    let mut sum = 0f64;
    for i in 0..n {
        let s = i16::from_le_bytes([pcm[2 * i], pcm[2 * i + 1]]) as f64;
        sum += s * s;
    }
    (sum / n as f64).sqrt() / 32768.0
}

fn frame_ms(bytes: usize, sr: u32) -> u64 {
    let samples = (bytes / 2) as u64;
    samples.saturating_mul(1000) / sr.max(1) as u64
}

/// The trailing ~1s of PCM, for the (debounced) turn-detector window.
fn recent_window(pcm: &[u8], sr: u32) -> &[u8] {
    let want = (sr as usize).saturating_mul(2);
    if pcm.len() > want {
        &pcm[pcm.len() - want..]
    } else {
        pcm
    }
}

enum Wake {
    Pcm(Option<Vec<u8>>),
    Stt(Option<SttEvent>),
}

/// The session's capture loop: PCM (and optional streaming-STT events) in, user
/// turns out. Ends when the session is torn down (the PCM channel closes / the task
/// is aborted).
async fn capture_loop(session: Arc<Session>, mut pcm_rx: mpsc::Receiver<Vec<u8>>) {
    let cfg = session.voice_cfg.clone();
    let sr = cfg.stt_sample_rate.max(8000);
    let detector_present = session.knobs.turn_detection && !cfg.turn_detector_url.is_empty();

    // Open the streaming-STT engine per the configured kind; on any failure the batch
    // fallback is used (zero-config and graceful-degradation paths both land here).
    let mut stt: Option<stt_stream::SttStream> = match cfg.stt_stream_kind.as_str() {
        "websocket" if !cfg.stt_stream_url.is_empty() => {
            stt_stream::open(&cfg.stt_stream_url, sr).await.map_err(|e| {
                tracing::warn!(error = %e, "streaming STT unavailable; using batch fallback");
            }).ok()
        }
        "openai_realtime" if cfg.stt_api_key.as_deref().is_some_and(|k| !k.is_empty()) => {
            super::stt_openai_realtime::open(
                cfg.stt_api_key.as_deref().unwrap_or_default(),
                &cfg.stt_model,
                &cfg.stt_language,
                sr,
                false, // live voice: our Smart-Turn drives commit (turn_detection:null)
            )
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "OpenAI realtime STT unavailable; using batch fallback");
            })
            .ok()
        }
        _ => None,
    };

    let mut utterance: Vec<u8> = Vec::new();
    let mut final_text = String::new();
    let mut speech_ms: u64 = 0;
    let mut silence_ms: u64 = 0;
    let mut barge_ms: u64 = 0; // sustained talk-over while the assistant speaks
    let mut had_speech = false;
    let mut consulted = false;
    let mut endpoint_pending = false;
    let mut turn_complete = false;

    loop {
        let wake = if let Some(s) = stt.as_mut() {
            tokio::select! {
                p = pcm_rx.recv() => Wake::Pcm(p),
                e = s.recv() => Wake::Stt(e),
            }
        } else {
            Wake::Pcm(pcm_rx.recv().await)
        };

        match wake {
            Wake::Pcm(None) => break, // session torn down
            Wake::Stt(None) => stt = None, // engine closed → fall back silently
            Wake::Stt(Some(ev)) => match ev {
                SttEvent::Partial { text } => {
                    if !text.trim().is_empty() {
                        session.emit(ServerFrame::VoicePartial { text }).await;
                    }
                    had_speech = true;
                    silence_ms = 0;
                }
                SttEvent::Final { text } => {
                    let t = text.trim();
                    if !t.is_empty() {
                        if !final_text.is_empty() {
                            final_text.push(' ');
                        }
                        final_text.push_str(t);
                    }
                    endpoint_pending = true;
                }
                SttEvent::Error { message } => tracing::warn!(%message, "streaming STT error"),
            },
            Wake::Pcm(Some(pcm)) => {
                let rms = rms_i16(&pcm);
                let speaking_now = rms >= SPEECH_RMS;

                // While the assistant is replying, the only thing audio can do is
                // trigger barge-in — but only on SUSTAINED, louder-than-echo speech,
                // so the assistant's own audio / a noise spike can't cut the reply.
                let turn_active = session.current_turn.lock().unwrap().is_some();
                if turn_active {
                    let aec_ok = session.aec || !session.knobs.aec_required;
                    if aec_ok && session.current_state() == VoiceState::Speaking {
                        if update_barge(&mut barge_ms, rms >= BARGE_RMS, frame_ms(pcm.len(), sr)) {
                            session.barge_in().await; // clears current_turn; capture this frame next
                        } else {
                            continue;
                        }
                    } else {
                        continue;
                    }
                } else {
                    barge_ms = 0;
                }

                // Capture the user's utterance + advance the silence timer. Accumulate
                // real speech (frames above the RMS gate) so a blip can't fire a turn.
                if speaking_now {
                    silence_ms = 0;
                    had_speech = true;
                    speech_ms = speech_ms.saturating_add(frame_ms(pcm.len(), sr));
                } else if had_speech {
                    silence_ms = silence_ms.saturating_add(frame_ms(pcm.len(), sr));
                }
                utterance.extend_from_slice(&pcm);
                if let Some(s) = stt.as_ref() {
                    s.send_pcm(pcm).await;
                }

                // Semantic turn detector (debounced to when silence begins).
                if detector_present && had_speech && silence_ms >= DETECT_AFTER_MS && !consulted {
                    consulted = true;
                    if let Ok(sig) =
                        turn::detect(&session.ext_http, &cfg.turn_detector_url, recent_window(&utterance, sr), sr).await
                    {
                        turn_complete = sig.turn_complete;
                        if sig.endpoint {
                            endpoint_pending = true;
                        }
                    }
                }

                if had_speech
                    && turn::should_fire_turn(
                        endpoint_pending,
                        silence_ms,
                        session.knobs.silence_threshold_ms,
                        detector_present,
                        turn_complete,
                    )
                {
                    // A blip below the speech floor is noise — discard WITHOUT calling
                    // STT, so a near-silent clip can't be hallucinated into a transcript.
                    if speech_ms < session.knobs.min_speech_ms {
                        reset(&mut utterance, &mut final_text, &mut speech_ms, &mut silence_ms, &mut had_speech, &mut consulted, &mut endpoint_pending, &mut turn_complete);
                        continue;
                    }
                    // Manual-commit engines (OpenAI realtime, `turn_detection:null`)
                    // transcribe only after we signal end-of-utterance. Commit, then
                    // wait briefly for the completed transcript (emitting any partials);
                    // on timeout/close fall through to the batch path on `utterance`.
                    if stt.as_ref().is_some_and(|s| s.manual_commit()) {
                        let mut closed = false;
                        if let Some(s) = stt.as_mut() {
                            s.commit().await;
                            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
                            loop {
                                match tokio::time::timeout_at(deadline, s.recv()).await {
                                    Ok(Some(SttEvent::Final { text })) => {
                                        let t = text.trim();
                                        if !t.is_empty() {
                                            if !final_text.is_empty() {
                                                final_text.push(' ');
                                            }
                                            final_text.push_str(t);
                                        }
                                        break;
                                    }
                                    Ok(Some(SttEvent::Partial { text })) => {
                                        if !text.trim().is_empty() {
                                            session.emit(ServerFrame::VoicePartial { text }).await;
                                        }
                                    }
                                    Ok(Some(SttEvent::Error { message })) => {
                                        tracing::warn!(%message, "realtime STT error awaiting commit");
                                    }
                                    Ok(None) => {
                                        closed = true;
                                        break;
                                    }
                                    Err(_) => break, // timeout → batch fallback
                                }
                            }
                        }
                        if closed {
                            stt = None;
                        }
                    }
                    let dur_ms = frame_ms(utterance.len(), sr);
                    let text = if !final_text.trim().is_empty() {
                        final_text.trim().to_string()
                    } else {
                        match crate::ml::transcribe(
                            &session.state.http,
                            &session.state.boot.ml.base_url,
                            &stt_stream::pcm_to_wav(&utterance, sr),
                            "audio/wav",
                            crate::ml::provider_overrides(&session.state, session.ctx.user_id).await,
                        )
                        .await
                        {
                            Ok(t) => t.trim().to_string(),
                            Err(e) => {
                                session.emit_error(format!("transcription failed: {e}")).await;
                                reset(&mut utterance, &mut final_text, &mut speech_ms, &mut silence_ms, &mut had_speech, &mut consulted, &mut endpoint_pending, &mut turn_complete);
                                continue;
                            }
                        }
                    };
                    // Strip residual ASR control tags (streaming path) and require real
                    // spoken content — never start a turn on `<asr_text>`/punctuation junk.
                    let text = sanitize_transcript(&text);
                    reset(&mut utterance, &mut final_text, &mut speech_ms, &mut silence_ms, &mut had_speech, &mut consulted, &mut endpoint_pending, &mut turn_complete);
                    if !is_speech_like(&text) {
                        continue;
                    }
                    session.emit_final(&text).await;
                    session.audit_transcribed(dur_ms, text.chars().count()).await;
                    session.start_turn(text).await;
                }
            }
        }
    }
}

/// Accumulate sustained talk-over and decide whether to interrupt the assistant.
/// `loud` is a frame above the (higher) barge RMS gate; a quiet frame resets the
/// run so an echo/noise blip never reaches the threshold. Fires (and resets) once
/// `BARGE_MIN_MS` of continuous loud speech has accrued.
fn update_barge(barge_ms: &mut u64, loud: bool, frame_ms: u64) -> bool {
    if loud {
        *barge_ms = barge_ms.saturating_add(frame_ms);
    } else {
        *barge_ms = 0;
    }
    if *barge_ms >= BARGE_MIN_MS {
        *barge_ms = 0;
        true
    } else {
        false
    }
}

#[allow(clippy::too_many_arguments)]
fn reset(
    utterance: &mut Vec<u8>,
    final_text: &mut String,
    speech_ms: &mut u64,
    silence_ms: &mut u64,
    had_speech: &mut bool,
    consulted: &mut bool,
    endpoint_pending: &mut bool,
    turn_complete: &mut bool,
) {
    utterance.clear();
    final_text.clear();
    *speech_ms = 0;
    *silence_ms = 0;
    *had_speech = false;
    *consulted = false;
    *endpoint_pending = false;
    *turn_complete = false;
}

/// Defensive strip of ASR control tokens a streaming engine might emit (the batch path
/// is already cleaned in the ML service): drop anything inside `<…>` (`<asr_text>`,
/// `</asr_text>`, `<|…|>`). A plain transcript passes through unchanged.
fn sanitize_transcript(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Does the transcript carry real spoken content? True only if it has at least one
/// alphanumeric character — rejects empty / punctuation-only / tag-only junk (a leaked
/// `<asr_text>` sanitises to "", a stray "。" has no alphanumerics), so it never starts
/// a turn. (Near-silent hallucinations like "北山乡。" are alphanumeric and are instead
/// stopped upstream by the minimum-speech gate.)
fn is_speech_like(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The barge-in invariant: one barge-in cancels BOTH the LLM generation and the TTS
    /// stream. Modelled with fakes (no DB) — two parked tasks stand in for run_turn's
    /// cancel-wait and a speak_clause abort-wait; both must observe the signal.
    #[tokio::test]
    async fn barge_in_cancels_both_llm_and_tts() {
        let h = TurnHandle {
            turn_id: Uuid::now_v7(),
            cancel: Arc::new(Notify::new()),
            tts_abort: Arc::new(Notify::new()),
            speaking: Arc::new(AtomicBool::new(true)),
        };
        let llm = Arc::new(AtomicBool::new(true));
        let tts = Arc::new(AtomicBool::new(true));
        let lt = {
            let c = h.cancel.clone();
            let f = llm.clone();
            tokio::spawn(async move {
                c.notified().await;
                f.store(false, Ordering::SeqCst);
            })
        };
        let tt = {
            let c = h.tts_abort.clone();
            let f = tts.clone();
            tokio::spawn(async move {
                c.notified().await;
                f.store(false, Ordering::SeqCst);
            })
        };
        // Let both waiters park (notify_waiters wakes only current waiters).
        tokio::time::sleep(Duration::from_millis(30)).await;
        signal_barge_in(&h);
        tokio::time::timeout(Duration::from_secs(1), lt).await.expect("llm task joined").unwrap();
        tokio::time::timeout(Duration::from_secs(1), tt).await.expect("tts task joined").unwrap();
        assert!(!llm.load(Ordering::SeqCst), "barge-in must cancel the LLM generation");
        assert!(!tts.load(Ordering::SeqCst), "barge-in must cancel the TTS stream");
        assert!(!h.speaking.load(Ordering::SeqCst), "speaking gate cleared");
    }

    #[test]
    fn sanitize_strips_asr_control_tags() {
        assert_eq!(sanitize_transcript("<asr_text>"), "");
        assert_eq!(sanitize_transcript("<asr_text></asr_text>"), "");
        assert_eq!(sanitize_transcript("<|im_end|>"), "");
        assert_eq!(sanitize_transcript("hello world"), "hello world");
        assert_eq!(sanitize_transcript("  trimmed  "), "trimmed");
    }

    #[test]
    fn barge_needs_sustained_loud_speech() {
        let frame = 20u64; // 20 ms frames
        // A single loud frame must NOT interrupt (echo/noise spike).
        let mut b = 0u64;
        assert!(!update_barge(&mut b, true, frame));
        // A quiet frame mid-run resets, so brief blips never accumulate to threshold.
        for _ in 0..5 {
            assert!(!update_barge(&mut b, true, frame));
        }
        assert!(!update_barge(&mut b, false, frame));
        assert_eq!(b, 0);
        // Sustained loud speech past BARGE_MIN_MS fires exactly once, then resets.
        let mut fired = false;
        for _ in 0..((BARGE_MIN_MS / frame) + 1) {
            if update_barge(&mut b, true, frame) {
                fired = true;
                break;
            }
        }
        assert!(fired, "sustained talk-over must interrupt");
        assert_eq!(b, 0, "fires and resets");
    }

    #[test]
    fn speech_like_rejects_junk_keeps_real_text() {
        // Junk that must never start a turn.
        assert!(!is_speech_like(""));
        assert!(!is_speech_like("   "));
        assert!(!is_speech_like("。"));
        assert!(!is_speech_like("…!?"));
        assert!(!is_speech_like(&sanitize_transcript("<asr_text>"))); // leaked tag → ""
        // Real speech (any script) passes.
        assert!(is_speech_like("hello"));
        assert!(is_speech_like("привет"));
        assert!(is_speech_like("北山乡。")); // alphanumeric → passes here; gated by min-speech
    }
}
