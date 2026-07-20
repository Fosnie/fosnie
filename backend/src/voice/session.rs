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
use super::{spec_retrieval, tts_stream, turn, VoiceKnobs, VoiceState};

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

/// Floor for the soft endpoint, so an aggressive turn-silence threshold cannot
/// collapse "probably ending" and "ended" into the same instant.
const SPEC_SOFT_FLOOR_MS: u64 = 150;
/// Below this much of its deadline left, an unfinished speculative search is not
/// worth waiting for — the wait would expire and the head start be spent for nothing.
const SPEC_MIN_AWAIT_MS: u64 = 250;

/// What the committed turn should do about speculation.
enum SpecPlan {
    /// Nothing usable; the turn retrieves as it always would.
    Cold(spec_retrieval::SpecOutcome),
    /// A finished search that answers the committed transcript.
    Ready(spec_retrieval::SpecResult),
    /// A search still running whose query answers the committed transcript.
    Await(spec_retrieval::Shot),
}

/// What the commit phase concluded about speculation.
struct SpecSettled {
    /// The retrieval to hand the turn, if one survived.
    prefetched: Option<crate::chat::prefetch::PrefetchedRag>,
    outcome: spec_retrieval::SpecOutcome,
    /// Retrieval time kept off this turn's critical path.
    saved_ms: u64,
    /// Searches this phase stopped. Returned rather than added to the session
    /// counters, which have already been read for this turn's log line — counting
    /// them there would bill them to the next turn instead.
    cancelled: u32,
    /// Barge-in landed during the wait, consuming the turn's cancel signal. The
    /// caller must abandon the turn rather than start it with a spent signal.
    interrupted: bool,
}

impl SpecSettled {
    fn none(outcome: spec_retrieval::SpecOutcome) -> Self {
        Self { prefetched: None, outcome, saved_ms: 0, cancelled: 0, interrupted: false }
    }
}

/// Hand a speculative result to the turn in the shape the turn expects.
fn into_prefetched(p: spec_retrieval::SpecResult) -> crate::chat::prefetch::PrefetchedRag {
    crate::chat::prefetch::PrefetchedRag {
        context: p.context,
        citations: p.citations,
        parts: p.parts,
        debug: p.debug,
        source_query: p.query,
    }
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
    /// Speculative retrieval fired from the partial transcript: the search still
    /// running, the last one that finished, and this utterance's resolved scope.
    /// Cleared whenever the utterance ends, so nothing crosses a turn boundary.
    spec: Mutex<spec_retrieval::SpecShared>,
    /// A speculative search is running (surfaced to the SPA alongside the state).
    spec_retrieving: AtomicBool,
    /// This session has nothing to search: no knowledge base is in scope, or the
    /// scope could not be resolved. Latched on the first refusal so a session
    /// without a Library stops attempting speculation altogether.
    spec_kb_absent: AtomicBool,
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
            spec: Mutex::new(spec_retrieval::SpecShared::default()),
            spec_retrieving: AtomicBool::new(false),
            spec_kb_absent: AtomicBool::new(false),
            tasks: Mutex::new(Vec::new()),
        });
        session
            .emit(ServerFrame::VoiceLiveState {
                state: VoiceState::Listening.as_str().into(),
                retrieving: false,
            })
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
        // Whatever the speaker was mid-way through asking has been abandoned, so
        // anything speculated for it is worthless — and a search still running would
        // otherwise leak into the next turn.
        self.spec_clear();
        self.set_retrieving(false).await;
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

        // Decide what to do with anything speculation turned up while the speaker was
        // talking. Cheap and synchronous — no waiting happens here, so the capture
        // loop is released immediately and barge-in stays responsive.
        let (plan, spec_stats) = self.spec_plan(&text);
        self.set_retrieving(false).await;

        // 1) The chat turn (LLM) — reused verbatim; streams frames into the tap.
        {
            let st = self.state.clone();
            let ctx = self.ctx.clone();
            let pid = self.project_id;
            let aid = self.agent_id;
            let socket_id = self.socket_id;
            let cancel2 = cancel.clone();
            let me = self.clone();
            let spec_timeout = std::time::Duration::from_secs(self.knobs.spec_timeout_secs);
            tokio::spawn(async move {
                // Waiting for an unfinished speculative search happens HERE, on the
                // task that would otherwise already be blocked inside the turn's own
                // retrieval — so the wait can only ever be shorter than what it
                // replaces, never additional.
                let settled = me.spec_settle(plan, spec_timeout, &cancel2).await;
                if settled.interrupted {
                    // Barge-in landed while we were waiting. The cancel notification
                    // has been consumed, so falling through to the turn would leave it
                    // with a spent signal and no way to be stopped.
                    let _ = tap_tx.send(ServerFrame::ChatInterrupted { turn_id, message_id: None }).await;
                    st.hub.remove_turn(socket_id, turn_id);
                    return;
                }
                // The dial defaults are starting points, and these counters are what
                // calibrates them: how often speculation fired, how often it was
                // thrown away, and how much of the reply budget it actually saved.
                // The two savings are kept apart deliberately — a finished search
                // saves its whole duration, a search still running saves only its
                // head start, and averaging the two together means nothing.
                let outcome = settled.outcome;
                tracing::info!(
                    %turn_id,
                    spec_outcome = outcome.as_str(),
                    spec_fires = spec_stats.fires,
                    spec_fires_eager = spec_stats.fires_eager,
                    spec_cancelled = spec_stats.cancelled + settled.cancelled,
                    spec_shot_ms = if outcome == spec_retrieval::SpecOutcome::Reused { settled.saved_ms } else { 0 },
                    spec_head_start_ms =
                        if outcome == spec_retrieval::SpecOutcome::ReusedAwaited { settled.saved_ms } else { 0 },
                    "live-voice turn retrieval"
                );
                crate::chat::run_turn(
                    &st, &ctx, turn_id, chat_id, pid, aid, text, Vec::new(), Vec::new(), false, None, None, None, settled.prefetched, &tap_tx,
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
        let retrieving = self.spec_retrieving.load(Ordering::SeqCst);
        self.emit(ServerFrame::VoiceLiveState { state: s.as_str().into(), retrieving }).await;
    }

    /// Announce that a speculative search started or stopped.
    ///
    /// This rides on the state frame rather than a frame of its own, but it cannot
    /// wait for the next state *change*: the whole window it covers is one unbroken
    /// spell of listening, during which no transition happens. So the current state
    /// is re-sent with the flag flipped, and only when the flag actually changed.
    async fn set_retrieving(&self, on: bool) {
        if self.spec_retrieving.swap(on, Ordering::SeqCst) == on {
            return;
        }
        let s = self.current_state();
        self.emit(ServerFrame::VoiceLiveState { state: s.as_str().into(), retrieving: on }).await;
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

    /// Resolve which knowledge bases a speculative search may read, and which
    /// documents within them it may not.
    ///
    /// This is the same allow-list and source-ACL deny-list the committed turn
    /// resolves, walked the same way, which is what makes speculation inherit access
    /// control rather than reimplement it. Fail-closed throughout: an error either
    /// side, or nothing in scope, means no search at all.
    ///
    /// Resolved once per utterance and cached only for its duration. Repeated
    /// searches in one utterance must not disagree with each other about scope, and
    /// nothing may survive into the next turn — the turn that uses a result always
    /// re-resolves before any of it reaches the model.
    async fn spec_acl(&self) -> Option<(Vec<String>, Vec<String>)> {
        if let Some(a) = self.spec.lock().unwrap().acl.clone() {
            return Some(a);
        }
        let chat_id = { *self.chat_id.lock().unwrap() };
        // The first turn of a new session has no chat row yet. The chat-linked arm of
        // the allow-list then contributes nothing — which is exactly what it
        // contributes on the committed turn too, since that chat is created fresh
        // with no Library attachments.
        let allow = crate::kb::retrieval_allowlist(
            &self.state.pg,
            &self.ctx,
            chat_id.unwrap_or(Uuid::nil()),
            self.project_id,
            self.agent_id,
        )
        .await
        .map_err(|e| tracing::debug!(error = %e, "speculative retrieval: allow-list unresolved"));

        let deny = match &allow {
            Ok(a) if !a.is_empty() => self
                .state
                .rbac
                .denied_kb_doc_ids(&self.state.pg, &self.ctx, a)
                .await
                .map_err(|e| tracing::debug!(error = %e, "speculative retrieval: deny-list unresolved")),
            _ => Ok(Vec::new()),
        };

        let out = spec_retrieval::acl_or_none(allow, deny);
        match &out {
            Some(a) => self.spec.lock().unwrap().acl = Some(a.clone()),
            None => self.spec_kb_absent.store(true, Ordering::SeqCst),
        }
        out
    }

    /// Start a speculative search on `query`, dropping any still running.
    ///
    /// Only one search is ever in flight: a newer partial transcript supersedes an
    /// older one, and cancelling costs nothing — aborting the task drops the
    /// retrieval stream, whose own teardown cancels the upstream request.
    ///
    /// Public because the capture loop is not the only thing that can legitimately
    /// start one, and because the scope it will search is resolved here rather than
    /// supplied: it reads exactly what the caller could already read, and refuses to
    /// run at all if that cannot be established.
    pub async fn spec_fire(self: &Arc<Self>, query: String, eager: bool) {
        let Some((kb_ids, deny_doc_ids)) = self.spec_acl().await else { return };

        // Speculation is an ordinary retrieval against the speaker's knowledge bases,
        // so it leaves the same trail: an investigator can prove what was searched.
        let mut ev = AuditEvent::action("voice.rag.prefetch", self.ctx.role.as_str());
        ev.actor_user_id = self.ctx.user_id;
        ev.payload = Some(serde_json::json!({
            "kb_ids": kb_ids,
            "denied_count": deny_doc_ids.len(),
            "eager": eager,
        }));
        let _ = audit::append(&self.state.pg, &ev).await;

        // The epoch this search belongs to. If the utterance ends before it does,
        // the epoch moves on and the result is handed up the join handle instead of
        // being parked where the next turn would find it.
        let (seq, epoch) = {
            let mut g = self.spec.lock().unwrap();
            g.next_seq += 1;
            g.stats.fires += 1;
            if eager {
                g.stats.fires_eager += 1;
            }
            (g.next_seq, g.epoch)
        };

        let me = self.clone();
        let q = query.clone();
        let started = std::time::Instant::now();
        let handle =
            tokio::spawn(async move { me.spec_run(seq, epoch, q, kb_ids, deny_doc_ids, started).await });

        // Install the new search and drop the previous one OUTSIDE the guard — the
        // lock is a plain mutex and must never be held across an await.
        let old = {
            let mut g = self.spec.lock().unwrap();
            g.inflight.replace(spec_retrieval::Shot { seq, query, started, handle })
        };
        if let Some(o) = old {
            o.handle.abort();
            self.spec.lock().unwrap().stats.cancelled += 1;
        }
        self.set_retrieving(true).await;
    }

    /// Run one speculative search to completion.
    ///
    /// The result goes to exactly one place. While the utterance it belongs to is
    /// still open it is parked in the pool, where the committing turn will find it.
    /// If that utterance has already ended, parking it would leak it into the next
    /// turn, so it is returned instead — which is what a turn waiting on this very
    /// search reads.
    async fn spec_run(
        self: Arc<Self>,
        seq: u64,
        epoch: u64,
        query: String,
        kb_ids: Vec<String>,
        deny_doc_ids: Vec<String>,
        started: std::time::Instant,
    ) -> Option<spec_retrieval::SpecResult> {
        // A light profile: no gap-filling rounds, few sub-questions. The speaker is
        // mid-sentence, so breadth is wasted — and whatever this misses, the model
        // tops up with the library-search tool once the turn is under way.
        let mut overrides = crate::ml::rag_overrides(&self.state.pg).await;
        overrides.gap_round_enabled = Some(false);
        overrides.max_subqueries = Some(2);
        overrides.max_rounds = Some(1);
        overrides.query_variants = Some(2);

        let chat_id = { *self.chat_id.lock().unwrap() };
        let llm_sel =
            crate::providers::resolve_llm(&self.state.pg, self.state.message_key, self.ctx.user_id, chat_id, None)
                .await
                .ok()
                .flatten();

        let stream = crate::ml::retrieve_stream(
            &self.state.http,
            &self.state.boot.ml.base_url,
            &query,
            &kb_ids,
            &deny_doc_ids,
            &overrides,
            crate::ml::provider_overrides_with_llm(&self.state, self.ctx.user_id, llm_sel.as_ref()).await,
            Some(std::time::Duration::from_secs(self.knobs.spec_timeout_secs)),
        )
        .await;

        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "speculative retrieval did not start");
                self.spec_finished(seq);
                return None;
            }
        };

        while let Some(ev) = stream.recv().await {
            match ev {
                // Progress belongs to a turn, and there is no turn yet — the SPA is
                // told only that a search is running.
                crate::ml::RetrieveEvent::Progress { .. } => {}
                crate::ml::RetrieveEvent::Done { context, citations, parts, debug } => {
                    let result = spec_retrieval::SpecResult {
                        query: query.clone(),
                        context,
                        citations,
                        parts,
                        debug,
                        shot_ms: started.elapsed().as_millis() as u64,
                    };
                    let late = {
                        let mut g = self.spec.lock().unwrap();
                        if g.inflight.as_ref().is_some_and(|s| s.seq == seq) {
                            g.inflight = None;
                        }
                        if g.admits(epoch) {
                            // Newest result wins; the pool holds one.
                            g.pool = Some(result);
                            None
                        } else {
                            Some(result)
                        }
                    };
                    self.set_retrieving(false).await;
                    return late;
                }
                crate::ml::RetrieveEvent::Error { message } => {
                    tracing::debug!(%message, "speculative retrieval failed");
                    break;
                }
            }
        }
        self.spec_finished(seq);
        self.set_retrieving(false).await;
        None
    }

    /// Clear the in-flight slot if it still holds this search (it may already have
    /// been superseded, in which case the newer one owns the slot).
    fn spec_finished(&self, seq: u64) {
        let mut g = self.spec.lock().unwrap();
        if g.inflight.as_ref().is_some_and(|s| s.seq == seq) {
            g.inflight = None;
        }
    }

    /// Drop everything speculative: on turn commit, on barge-in, on teardown.
    fn spec_clear(&self) {
        self.spec.lock().unwrap().clear();
    }

    /// Judge what speculation produced against what the speaker actually said, and
    /// take everything speculative off the session in one go.
    ///
    /// Synchronous by design: this runs on the capture loop's task, so it must not
    /// wait for anything. Waiting, where a search is worth waiting for, is deferred
    /// to [`Session::spec_settle`] on the turn's own task.
    fn spec_plan(&self, final_text: &str) -> (SpecPlan, spec_retrieval::SpecStats) {
        // The plan itself cancels searches — a superseded one on reuse, a rejected
        // one at the gate — so the counters are read AFTER it runs. Reading them
        // first would bill those cancellations to the following turn and skew the
        // very numbers the dials are calibrated from.
        let plan = self.spec_plan_inner(final_text);
        let stats = {
            let mut g = self.spec.lock().unwrap();
            std::mem::take(&mut g.stats)
        };
        (plan, stats)
    }

    fn spec_plan_inner(&self, final_text: &str) -> SpecPlan {
        if !self.knobs.spec_enabled {
            return SpecPlan::Cold(spec_retrieval::SpecOutcome::Disabled);
        }
        let cfg = self.knobs.reuse_cfg();
        let (pool, inflight) = {
            let mut g = self.spec.lock().unwrap();
            g.acl = None;
            let pool = g.pool.take();
            let inflight = g.inflight.take();
            // The utterance is over. From here nothing may be parked for it, so a
            // search that finishes late cannot leave a result for the next turn to
            // pick up; one this turn is waiting for is handed back directly instead.
            g.close();
            (pool, inflight)
        };

        // A finished search that answers the question is the whole point: the turn
        // skips retrieval entirely.
        if let Some(p) = pool {
            if spec_retrieval::reuse_ok(&p.query, final_text, cfg) {
                if let Some(s) = inflight {
                    s.handle.abort();
                    self.spec.lock().unwrap().stats.cancelled += 1;
                }
                return SpecPlan::Ready(p);
            }
        }
        // Otherwise a search still running may yet be the right one — it is the same
        // work the turn is about to start, only begun earlier.
        match inflight {
            Some(s) if spec_retrieval::reuse_ok(&s.query, final_text, cfg) => SpecPlan::Await(s),
            Some(s) => {
                s.handle.abort();
                self.spec.lock().unwrap().stats.cancelled += 1;
                SpecPlan::Cold(spec_retrieval::SpecOutcome::DiscardedGate)
            }
            None => SpecPlan::Cold(spec_retrieval::SpecOutcome::DiscardedNone),
        }
    }

    /// Turn a plan into what the turn will actually be handed, waiting for a search
    /// still in flight where that is worth doing.
    ///
    /// Returns the prefetched result, the outcome for the turn log, the milliseconds
    /// of retrieval kept off the turn's critical path, and whether barge-in landed
    /// during the wait — in which case the caller must abandon the turn rather than
    /// start it with a cancel signal that has already been consumed.
    async fn spec_settle(&self, plan: SpecPlan, timeout: std::time::Duration, cancel: &Notify) -> SpecSettled {
        match plan {
            SpecPlan::Cold(outcome) => SpecSettled::none(outcome),
            SpecPlan::Ready(p) => {
                let saved = p.shot_ms;
                SpecSettled {
                    prefetched: Some(into_prefetched(p)),
                    outcome: spec_retrieval::SpecOutcome::Reused,
                    saved_ms: saved,
                    cancelled: 0,
                    interrupted: false,
                }
            }
            SpecPlan::Await(mut s) => {
                let head_start = s.started.elapsed();
                let remaining = timeout.saturating_sub(head_start);
                // A search with almost no time left is not worth the gamble: the wait
                // would expire and the turn would have to search from scratch anyway,
                // having spent the difference for nothing.
                if remaining < std::time::Duration::from_millis(SPEC_MIN_AWAIT_MS) {
                    s.handle.abort();
                    return SpecSettled {
                        cancelled: 1,
                        ..SpecSettled::none(spec_retrieval::SpecOutcome::DiscardedNone)
                    };
                }
                // Poll the handle by reference. Moving it into the timeout would mean
                // that losing the race drops it — and dropping a join handle detaches
                // the task rather than stopping it, leaving the search to run on in
                // the background and its result to surface after the turn it belonged
                // to. Every exit that is not a completion aborts explicitly.
                let waited = tokio::select! {
                    r = tokio::time::timeout(remaining, &mut s.handle) => r.ok(),
                    _ = cancel.notified() => {
                        s.handle.abort();
                        return SpecSettled {
                            cancelled: 1,
                            interrupted: true,
                            ..SpecSettled::none(spec_retrieval::SpecOutcome::DiscardedNone)
                        };
                    }
                };
                match waited {
                    // The search finished while we waited, and hands its result back
                    // directly: the utterance is closed, so it could not park it.
                    Some(Ok(Some(p))) => SpecSettled {
                        prefetched: Some(into_prefetched(p)),
                        outcome: spec_retrieval::SpecOutcome::ReusedAwaited,
                        saved_ms: head_start.as_millis() as u64,
                        cancelled: 0,
                        interrupted: false,
                    },
                    // Finished with nothing to show, or the task itself failed.
                    Some(_) => SpecSettled::none(spec_retrieval::SpecOutcome::DiscardedNone),
                    // The wait ran out. Stop it rather than leave it running.
                    None => {
                        s.handle.abort();
                        SpecSettled {
                            cancelled: 1,
                            ..SpecSettled::none(spec_retrieval::SpecOutcome::DiscardedNone)
                        }
                    }
                }
            }
        }
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
    // Turn-completeness confidence from the semantic detector, if one is consulted.
    let mut turn_prob: f32 = 0.0;

    // Speculative retrieval: search the knowledge base from the partial transcript
    // while the speaker is still talking. Owned here as a plain local because only
    // this task touches it; the shared half (the search itself, its result) lives on
    // the session behind its mutex. The engine decides how partials are worded.
    let spec_cfg = session.knobs.spec_cfg();
    let partial_mode = spec_retrieval::PartialMode::for_engine(&cfg.stt_stream_kind);
    let mut spec = spec_retrieval::SpecState::default();
    // Monotonic reference for the speculator's debounce; only differences matter.
    let spec_clock = std::time::Instant::now();
    // The pause at which the turn is probably ending, which is when the transcript
    // is nearly final and most worth searching on — but before it is long enough to
    // end the turn. Floored so an aggressive silence threshold cannot collapse the
    // two into the same instant, which would leave no head start at all.
    let soft_silence_ms = (session.knobs.silence_threshold_ms * spec_cfg.soft_silence_pct / 100)
        .max(SPEC_SOFT_FLOOR_MS);

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
                        spec.observe(&text, partial_mode);
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
                            spec.reset();
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
                        turn_prob = sig.prob;
                        if sig.endpoint {
                            endpoint_pending = true;
                        }
                    }
                }

                // Speculative retrieval. Two thresholds: this soft one says the turn
                // is probably ending and the transcript is worth searching on, while
                // the hard one below actually ends it. The semantic detector, when
                // there is one, can reach the soft threshold sooner than silence does.
                if had_speech {
                    let now_ms = spec_clock.elapsed().as_millis() as u64;
                    let soft_endpoint = silence_ms >= soft_silence_ms
                        || (spec_cfg.eager_prob < 1.0 && turn_prob >= spec_cfg.eager_prob);
                    // A session with nothing to search never speculates again — the
                    // first refusal to resolve a scope settles it for the socket.
                    let kb_present = !session.spec_kb_absent.load(Ordering::SeqCst);
                    let fire = spec_retrieval::decide(&spec, &spec_cfg, now_ms, soft_endpoint, kb_present);
                    match fire {
                        spec_retrieval::SpecFire::Speculative(q) => {
                            spec.note_fired(&q, now_ms, false);
                            session.spec_fire(q, false).await;
                        }
                        spec_retrieval::SpecFire::Eager(q) => {
                            spec.note_fired(&q, now_ms, true);
                            session.spec_fire(q, true).await;
                        }
                        spec_retrieval::SpecFire::None => {}
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
                        reset(&mut utterance, &mut final_text, &mut speech_ms, &mut silence_ms, &mut had_speech, &mut consulted, &mut endpoint_pending, &mut turn_complete, &mut turn_prob, &mut spec);
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
                                reset(&mut utterance, &mut final_text, &mut speech_ms, &mut silence_ms, &mut had_speech, &mut consulted, &mut endpoint_pending, &mut turn_complete, &mut turn_prob, &mut spec);
                                continue;
                            }
                        }
                    };
                    // Strip residual ASR control tags (streaming path) and require real
                    // spoken content — never start a turn on `<asr_text>`/punctuation junk.
                    let text = sanitize_transcript(&text);
                    reset(&mut utterance, &mut final_text, &mut speech_ms, &mut silence_ms, &mut had_speech, &mut consulted, &mut endpoint_pending, &mut turn_complete, &mut turn_prob, &mut spec);
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
    turn_prob: &mut f32,
    spec: &mut spec_retrieval::SpecState,
) {
    utterance.clear();
    final_text.clear();
    *speech_ms = 0;
    *silence_ms = 0;
    *had_speech = false;
    *consulted = false;
    *endpoint_pending = false;
    *turn_complete = false;
    *turn_prob = 0.0;
    // The speculator is per-utterance: a stable prefix, a shot count and a debounce
    // clock from the last thing said must never carry into the next one.
    spec.reset();
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

    /// Park a task that flips `alive` to false as it unwinds, so aborting it is
    /// observable. Stands in for a speculative search holding a retrieval stream:
    /// what matters is that dropping it happens, since the stream's own teardown is
    /// what cancels the upstream request.
    fn parked_shot(alive: Arc<AtomicBool>) -> JoinHandle<Option<spec_retrieval::SpecResult>> {
        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        tokio::spawn(async move {
            let _g = Guard(alive);
            std::future::pending::<Option<spec_retrieval::SpecResult>>().await
        })
    }

    /// Only one speculative search runs at a time: a newer partial transcript
    /// supersedes an older one, and the older one is dropped rather than left to
    /// finish into a pool nothing will read.
    #[tokio::test]
    async fn a_newer_speculative_search_drops_the_older_one() {
        let mut shared = spec_retrieval::SpecShared::default();
        let first_alive = Arc::new(AtomicBool::new(true));
        shared.inflight = Some(spec_retrieval::Shot {
            seq: 1,
            query: "what is the holiday".into(),
            started: std::time::Instant::now(),
            handle: parked_shot(first_alive.clone()),
        });

        let second_alive = Arc::new(AtomicBool::new(true));
        let old = shared.inflight.replace(spec_retrieval::Shot {
            seq: 2,
            query: "what is the holiday allowance for contractors".into(),
            started: std::time::Instant::now(),
            handle: parked_shot(second_alive.clone()),
        });
        // Let both tasks actually start: a task aborted before its first poll never
        // ran, so nothing it holds was ever acquired and the drop proves nothing.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let old = old.expect("the first search was in flight");
        old.handle.abort();
        tokio::time::sleep(Duration::from_millis(30)).await;

        assert!(old.handle.is_finished(), "the superseded search must not still be running");
        assert!(!first_alive.load(Ordering::SeqCst), "aborting it drops what it was holding");
        assert!(second_alive.load(Ordering::SeqCst), "the newer search is untouched");
    }

    /// Ending an utterance leaves nothing behind: no search running, no result
    /// waiting to be picked up by a turn that never asked for it, no stale scope.
    #[tokio::test]
    async fn clearing_ends_the_search_and_empties_the_pool() {
        let mut shared = spec_retrieval::SpecShared::default();
        let alive = Arc::new(AtomicBool::new(true));
        shared.inflight = Some(spec_retrieval::Shot {
            seq: 1,
            query: "tell me about the holiday policy".into(),
            started: std::time::Instant::now(),
            handle: parked_shot(alive.clone()),
        });
        shared.pool = Some(spec_retrieval::SpecResult {
            query: "tell me about the holiday policy".into(),
            context: "[D1] ...".into(),
            citations: Vec::new(),
            parts: Vec::new(),
            debug: crate::ml::RetrieveDebug::default(),
            shot_ms: 900,
        });
        shared.acl = Some((vec!["kb".into()], Vec::new()));

        tokio::time::sleep(Duration::from_millis(20)).await;
        shared.clear();
        tokio::time::sleep(Duration::from_millis(30)).await;

        assert!(shared.inflight.is_none(), "no search survives the utterance");
        assert!(shared.pool.is_none(), "no result survives the utterance");
        assert!(shared.acl.is_none(), "the resolved scope is re-resolved next time, never reused");
        assert!(!alive.load(Ordering::SeqCst), "the running search was actually dropped");
        assert_eq!(shared.stats.cancelled, 1, "the drop is counted");
    }

    fn result(query: &str) -> spec_retrieval::SpecResult {
        spec_retrieval::SpecResult {
            query: query.into(),
            context: "[D1] ...".into(),
            citations: Vec::new(),
            parts: Vec::new(),
            debug: crate::ml::RetrieveDebug::default(),
            shot_ms: 900,
        }
    }

    /// A search that finishes just after its utterance ended must not leave its
    /// result lying in the pool, where the NEXT turn would pick it up and answer a
    /// question nobody asked. The epoch it started under has moved on, so it is
    /// refused — the search is handed back to whoever is waiting for it instead.
    #[test]
    fn a_result_from_a_finished_utterance_is_refused() {
        let mut shared = spec_retrieval::SpecShared::default();
        let started_under = shared.epoch;
        assert!(shared.admits(started_under), "while the utterance is open, results are parked");

        // The turn commits: the pool is taken and the utterance closed.
        shared.pool = Some(result("what is the holiday allowance"));
        shared.close();
        assert!(shared.pool.is_none(), "closing empties the pool");

        assert!(
            !shared.admits(started_under),
            "a search from the finished utterance may no longer park its result"
        );
        // ...and a search started afterwards is admitted again, so the next turn
        // still gets speculation.
        assert!(shared.admits(shared.epoch), "the next utterance speculates normally");
    }

    /// The race the epoch closes: the search parks its result at the very moment the
    /// turn is committing. Whichever order the two land in, the pool ends up empty —
    /// nothing crosses the boundary.
    #[test]
    fn a_result_parked_during_the_commit_is_cleared() {
        // Parked first, then the commit closes the utterance.
        let mut shared = spec_retrieval::SpecShared {
            pool: Some(result("what is the holiday allowance")),
            ..Default::default()
        };
        shared.close();
        assert!(shared.pool.is_none(), "a result parked before the close is cleared by it");

        // Closed first, then the search tries to park: refused by the epoch check.
        let mut shared = spec_retrieval::SpecShared::default();
        let started_under = shared.epoch;
        shared.close();
        if shared.admits(started_under) {
            shared.pool = Some(result("what is the holiday allowance"));
        }
        assert!(shared.pool.is_none(), "a result arriving after the close is refused");
    }

    /// Interrupting while the turn waits on a search must STOP that search, not
    /// merely stop waiting for it. Dropping a join handle detaches the task: it would
    /// run on to its own deadline, hold the upstream request open, and surface its
    /// result after the turn it belonged to had gone.
    #[tokio::test]
    async fn interrupting_a_wait_stops_the_search() {
        let alive = Arc::new(AtomicBool::new(true));
        let mut shot = spec_retrieval::Shot {
            seq: 1,
            query: "what is the holiday allowance for contractors".into(),
            started: std::time::Instant::now(),
            handle: parked_shot(alive.clone()),
        };
        let cancel = Arc::new(Notify::new());
        tokio::time::sleep(Duration::from_millis(20)).await; // let the search start

        // The shape of the wait in the commit path: race the search against the
        // turn's cancel, and abort it on any exit that is not a completion.
        let interrupted = {
            let c = cancel.clone();
            let waiter = async {
                tokio::select! {
                    r = tokio::time::timeout(Duration::from_secs(5), &mut shot.handle) => {
                        let _ = r;
                        false
                    }
                    _ = c.notified() => {
                        shot.handle.abort();
                        true
                    }
                }
            };
            let notifier = async {
                tokio::time::sleep(Duration::from_millis(20)).await;
                cancel.notify_waiters();
            };
            let (i, ()) = tokio::join!(waiter, notifier);
            i
        };
        tokio::time::sleep(Duration::from_millis(30)).await;

        assert!(interrupted, "the cancel arm must win this race");
        assert!(shot.handle.is_finished(), "the search must be stopped, not merely abandoned");
        assert!(!alive.load(Ordering::SeqCst), "aborting it drops the retrieval it was holding");
    }

    /// A session with nothing configured, for exercising the parts of the commit
    /// decision that are pure session state: no database, no network, no sockets.
    fn bare_session() -> Arc<Session> {
        use sqlx::postgres::PgPoolOptions;
        let pg = PgPoolOptions::new().connect_lazy("postgres://localhost/unused").expect("lazy pool");
        let redis = crate::cache::create_pool("redis://localhost:6379").expect("redis pool");
        let state = AppState::new(pg, redis, Arc::new(crate::config::BootConfig::default()));
        let (tx, _rx) = mpsc::channel(8);
        let (pcm_tx, _pcm_rx) = mpsc::channel(8);
        Arc::new(Session {
            socket_id: Uuid::now_v7(),
            ctx: AuthContext {
                user_id: Some(Uuid::now_v7()),
                email: None,
                display_name: None,
                role: crate::auth::PlatformRole::User,
                break_glass: false,
                mfa_enroll_only: false,
            },
            state,
            tx,
            pcm_tx,
            knobs: VoiceKnobs::default(),
            voice_cfg: crate::voice::VoiceLiveResolved {
                stt_stream_kind: String::new(),
                stt_stream_url: String::new(),
                stt_model: String::new(),
                dictation_model: String::new(),
                stt_language: String::new(),
                stt_sample_rate: 16_000,
                stt_api_key: None,
                tts_stream: false,
                tts_stream_url: String::new(),
                tts_model: String::new(),
                tts_voice: String::new(),
                tts_api_key: None,
                turn_detector_url: String::new(),
            },
            aec: true,
            chat_id: Mutex::new(None),
            project_id: None,
            agent_id: None,
            vstate: Mutex::new(VoiceState::Listening),
            current_turn: Mutex::new(None),
            ext_http: reqwest::Client::new(),
            spec: Mutex::new(spec_retrieval::SpecShared::default()),
            spec_retrieving: AtomicBool::new(false),
            spec_kb_absent: AtomicBool::new(false),
            tasks: Mutex::new(Vec::new()),
        })
    }

    /// A search stopped while committing a turn belongs to THAT turn's figures. The
    /// counters are what the dials are calibrated from, so a cancellation billed to
    /// the following turn would quietly bias the numbers in both directions at once.
    #[tokio::test]
    async fn a_cancellation_at_commit_is_counted_against_that_turn() {
        let session = bare_session();
        let alive = Arc::new(AtomicBool::new(true));
        {
            let mut g = session.spec.lock().unwrap();
            g.stats.fires = 1;
            g.inflight = Some(spec_retrieval::Shot {
                seq: 1,
                query: "tell me about the holiday policy".into(),
                started: std::time::Instant::now(),
                handle: parked_shot(alive.clone()),
            });
        }
        tokio::time::sleep(Duration::from_millis(20)).await; // let the search start

        // The speaker changed the subject, so the search in flight answers nothing.
        let (plan, stats) = session.spec_plan("actually what about the redundancy terms");
        assert!(matches!(plan, SpecPlan::Cold(spec_retrieval::SpecOutcome::DiscardedGate)));
        assert_eq!(stats.fires, 1, "the search this turn made");
        assert_eq!(stats.cancelled, 1, "and the one it stopped, both on this turn's line");

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!alive.load(Ordering::SeqCst), "the rejected search is stopped, not left running");

        // The next turn starts from zero rather than inheriting this turn's tally.
        let (_, next) = session.spec_plan("something else entirely");
        assert_eq!(next.cancelled, 0, "counters do not carry over");
        assert_eq!(next.fires, 0);
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
