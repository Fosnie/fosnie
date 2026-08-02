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

//! A line answered by the practice's own telephone system.
//!
//! Everything else here goes through a carrier, which means the caller's voice leaves the
//! deployment. It need not. A telephone system on the practice's own network can hand the
//! audio straight to this process over their own network, and nothing about the call
//! reaches anybody else at all.
//!
//! The shape is the same as the carrier's, in the same order, which is what makes this an
//! adapter rather than a second implementation:
//!
//! 1. the telephone system asks what to do with a call, and is answered with an identifier;
//! 2. it opens a connection and presents that identifier, and the call runs;
//! 3. it asks, once our side has finished, whether anybody is to be rung instead.
//!
//! **Two things stand in for the carrier's signature**, because a telephone system cannot
//! sign a request. The two questions above carry a shared secret and are refused from
//! anywhere but this deployment's own network. And the identifier the connection presents
//! is the same single-use ticket the carrier's socket uses: minted only by an answer that
//! passed every check, good for thirty seconds, and redeemed exactly once. A connection
//! that presents anything else is closed without a word, which is why an open port here is
//! not an open door.
//!
//! Nothing is bound unless an operator has said where. A deployment answering through a
//! carrier opens no port of its own.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::frame::{self, Message, Step};
use super::{log, start_session, CallDetails, CallEntry, SourceIp, StartedCall, TelephonyResolved};
use crate::audit::{self, AuditEvent, AuditOutcome};
use crate::auth::PlatformRole;
use crate::state::AppState;
use crate::voice::telephony::codec::{self, Resampler};
use crate::voice::telephony::pace::Outbound;
use crate::voice::telephony::{Control, Wire};

/// What this module answers for, as recorded against a line and a call.
pub const PROVIDER: &str = "audiosocket";

/// Whether this process is listening for a telephone system right now.
///
/// Recorded where it is bound rather than inferred from the setting, because those are
/// different facts: a listening address is taken up when the process starts, so one changed
/// since is a line that will not answer and nothing else would say so. Process-wide because
/// that is what it describes: one process, one port.
static LISTENING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Is this process listening for the practice's telephone system?
pub fn is_listening() -> bool {
    LISTENING.load(std::sync::atomic::Ordering::SeqCst)
}

pub const ANSWER_PATH: &str = "/api/telephony/audiosocket/answer";
pub const CONTINUE_PATH: &str = "/api/telephony/audiosocket/continue";

/// The header the shared secret arrives in.
pub const KEY_HEADER: &str = "x-fosnie-telephony-key";

/// A connection that has said nothing for this long is gone without having said so.
///
/// A live line delivers a frame every twenty milliseconds, so seconds of nothing is a
/// telephone system that has stopped rather than a caller who is thinking.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for the identifier before giving up on a connection.
///
/// It is the first thing sent, so this is generous: a connection that has not said which
/// call it is carrying has nothing this process can do for it.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// The most bytes to hold while looking for one whole message.
///
/// Two frames' worth of slack above the protocol's own cap. Past it the far end is not
/// speaking this protocol, and holding more would be doing its buffering for it.
const READ_BUFFER_CAP: usize = frame::MAX_PAYLOAD + 2 * frame::AUDIO_FRAME_BYTES;

// ---------------------------------------------------------------------------
// Being asked what to do with a call
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct Incoming {
    /// Who is calling, in full international form. Empty or absent means withheld.
    #[serde(default)]
    pub from: String,
    /// The number they rang.
    pub to: String,
}

/// Refuse without saying why, and without saying whether there was anything to find.
fn refuse() -> Response {
    StatusCode::FORBIDDEN.into_response()
}

fn text(body: String) -> Response {
    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], body)
        .into_response()
}

/// Is this request the telephone system's, and did it come from somewhere it could be?
///
/// Both, not either. The secret alone would be enough if it could never leak, and the
/// network position alone would be enough if nothing else ran on that network; together
/// they are what a signature is on the carrier's path.
async fn permitted(state: &AppState, cfg: &TelephonyResolved, headers: &HeaderMap, ip: &str) -> bool {
    let Some(expected) = cfg.audiosocket_key.as_deref().filter(|k| !k.is_empty()) else {
        // No secret configured means no way in. Not "anybody may": there is a configured
        // way and there is none, and this is none.
        tracing::warn!("a telephone system asked what to do with a call, but no secret is set");
        return false;
    };
    let presented = headers.get(KEY_HEADER).and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !crate::http::ct_eq(expected.as_bytes(), presented.as_bytes()) {
        return false;
    }
    // And from this deployment's own network. A telephone system reachable from the
    // public internet is not the arrangement this path is for, and the secret travelling
    // over one would be a credential in the open.
    let private = ip
        .parse::<std::net::IpAddr>()
        .map(|addr| crate::mcp::validate::is_private(&addr))
        .unwrap_or(false);
    if !private {
        audit_refusal(state, "public_source", ip).await;
    }
    private
}

async fn audit_refusal(state: &AppState, reason: &str, from: &str) {
    let mut ev = AuditEvent::action("telephony.refused", PlatformRole::User.as_str());
    ev.resource_type = Some("telephony".into());
    ev.outcome = AuditOutcome::Failure;
    ev.outcome_reason = Some(reason.to_string());
    ev.payload = Some(serde_json::json!({ "provider": PROVIDER, "from": from }));
    let _ = audit::append(&state.pg, &ev).await;
}

/// `GET /api/telephony/audiosocket/answer` — what to do with a call now ringing.
///
/// Answers with the identifier to open a connection with, and nothing else. Every refusal
/// this path can make is made here, before the telephone system answers the call, so a
/// caller who is not going to be taken is never picked up.
pub async fn answer(
    State(state): State<AppState>,
    SourceIp(ip): SourceIp,
    headers: HeaderMap,
    Query(q): Query<Incoming>,
) -> Response {
    let cfg = TelephonyResolved::load(
        &state.pg,
        state.message_key,
        &state.boot.telephony,
        &state.boot.server.public_url,
    )
    .await;
    if !permitted(&state, &cfg, &headers, &ip).await {
        return refuse();
    }

    let from = q.from.trim().to_string();
    let Some(to) = super::normalise_e164(q.to.trim()) else {
        audit_refusal(&state, "unknown_number", &from).await;
        return refuse();
    };
    let line = match super::line_for(&state.pg, PROVIDER, &to).await {
        Ok(line) => line,
        Err(reason) => {
            audit_refusal(&state, reason.as_str(), &from).await;
            return refuse();
        }
    };

    // Who the call runs as, and whether they still exist.
    let ctx = match crate::auth::load_context(&state.pg, line.owner_user_id).await {
        Ok(ctx) => ctx,
        Err(_) => {
            audit_refusal(&state, "owner_unavailable", &from).await;
            return refuse();
        }
    };
    if !crate::features::enabled_for(&state, &ctx, "voice").await
        || !crate::features::enabled_for(&state, &ctx, "voice_live").await
    {
        audit_refusal(&state, "voice_not_enabled", &from).await;
        return refuse();
    }

    // The same two engine requirements the carrier's answer checks, for the same reason:
    // discovered mid-call they are a silent line, and a line that cannot speak should not
    // pick up. Nothing here decodes audio, so the reply has to arrive as raw samples.
    let vc = crate::voice::VoiceLiveResolved::load(&state.pg, state.message_key, &state.boot.voice_live).await;
    if !vc.tts_stream || vc.tts_stream_url.is_empty() {
        audit_refusal(&state, "no_streaming_synthesiser", &from).await;
        return refuse();
    }
    if !matches!(vc.stt_sample_rate.max(8_000), 8_000 | 16_000) {
        audit_refusal(&state, "unsupported_rate", &from).await;
        return refuse();
    }

    // The same abuse guards. A telephone system on the practice's own network is trusted
    // to be itself, not to be sensible: a loop in a dialplan can ring a line as fast as a
    // stranger can.
    let caller_key = if from.is_empty() { "anonymous".to_string() } else { from.clone() };
    if !crate::cache::rate_limit_ok(&state.redis, &format!("tel:from:{caller_key}"), 5, 300).await {
        audit_refusal(&state, "rate_from", &from).await;
        return refuse();
    }
    if !crate::cache::rate_limit_ok(&state.redis, &format!("tel:to:{to}"), 60, 60).await {
        audit_refusal(&state, "rate_to", &from).await;
        return refuse();
    }
    if state.telephony.len() >= cfg.max_concurrent_calls {
        audit_refusal(&state, "concurrency", &from).await;
        return refuse();
    }

    // The identifier is the ticket, which is what authenticates the connection that
    // follows. Minted last, so nothing that could refuse this call happens after it.
    let details = CallDetails {
        owner_user_id: line.owner_user_id,
        agent_id: line.agent_id,
        opening: line.opening.clone(),
        record_calls: line.record_calls,
        // Filled in once the ticket exists: the ticket is what the call is called.
        call_sid: String::new(),
        from,
        to,
        phone_number_id: line.id,
    };
    let ticket = match super::issue_ticket_named(&state.redis, &details).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "could not mint a call identifier");
            audit_refusal(&state, "ticket_unavailable", &details.from).await;
            return refuse();
        }
    };
    text(ticket)
}

#[derive(serde::Deserialize)]
pub struct Which {
    /// The identifier the call ran under.
    pub call: String,
}

/// `GET /api/telephony/audiosocket/continue` — anybody to ring now that we have finished?
///
/// The number to dial, or an empty answer meaning the call is over. Read from the call's
/// own row rather than from anything held in memory, so it answers the same whether or not
/// the process that carried the call is still running.
pub async fn continue_call(
    State(state): State<AppState>,
    SourceIp(ip): SourceIp,
    headers: HeaderMap,
    Query(q): Query<Which>,
) -> Response {
    let cfg = TelephonyResolved::load(
        &state.pg,
        state.message_key,
        &state.boot.telephony,
        &state.boot.server.public_url,
    )
    .await;
    if !permitted(&state, &cfg, &headers, &ip).await {
        return refuse();
    }
    let row = sqlx::query!(
        "SELECT transfer_to FROM calls WHERE provider = $1 AND provider_call_id = $2",
        PROVIDER,
        q.call,
    )
    .fetch_optional(&state.pg)
    .await;
    match row {
        Ok(Some(r)) => text(r.transfer_to.unwrap_or_default()),
        // A call we cannot read is a call we do not transfer. An empty answer ends it,
        // which is the answer that cannot put a caller somewhere unintended.
        Ok(None) => text(String::new()),
        Err(e) => {
            tracing::warn!(error = %e, "could not read the call being continued");
            text(String::new())
        }
    }
}

// ---------------------------------------------------------------------------
// Carrying the call
// ---------------------------------------------------------------------------

/// Listen for the practice's telephone system until the process stops.
///
/// Bound only where an operator said to bind it. Each connection is one call and is
/// carried on its own task, so a telephone system that opens two does not make them wait
/// for each other.
pub async fn listen(state: AppState, addr: String, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, %addr, "could not listen for the telephone system");
            return;
        }
    };
    tracing::info!(%addr, "listening for the practice's own telephone system");
    LISTENING.store(true, std::sync::atomic::Ordering::SeqCst);
    // Whatever ends this loop, the port goes with it, so what this reports goes too.
    let _bound = ListeningWhileAlive;
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::info!("no longer listening for the telephone system");
                    return;
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let state = state.clone();
                    tokio::spawn(async move {
                        // Frames are small and constant; waiting to batch them would add
                        // delay to every twenty milliseconds of speech.
                        let _ = stream.set_nodelay(true);
                        carry(state, stream, peer.ip().to_string()).await;
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "a telephone connection could not be accepted");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

/// Says this process is no longer listening, however the loop above ends.
struct ListeningWhileAlive;

impl Drop for ListeningWhileAlive {
    fn drop(&mut self) {
        LISTENING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// One connection, from the identifier it presents to the moment the call is gone.
async fn carry(state: AppState, stream: TcpStream, peer: String) {
    let (mut rx, tx) = stream.into_split();
    let mut buf: Vec<u8> = Vec::with_capacity(READ_BUFFER_CAP);

    // Which call is this? Nothing else may happen until it has said so.
    let ticket = match hello(&mut rx, &mut buf).await {
        Some(id) => Uuid::from_bytes(id).to_string(),
        None => {
            tracing::warn!(%peer, "a telephone connection said nothing this process could use");
            return;
        }
    };
    // Redeemed as it is read, so an identifier works exactly once. A second connection
    // presenting the same one, by a dialplan retrying or by anybody who saw it, finds
    // nothing and is closed.
    let details = match super::redeem_ticket(&state.redis, &ticket).await {
        Ok(Some(d)) => d,
        _ => {
            tracing::warn!(%peer, "a telephone connection presented an identifier nobody minted");
            return;
        }
    };
    let ctx = match crate::auth::load_context(&state.pg, details.owner_user_id).await {
        Ok(c) => c,
        Err(_) => return,
    };

    // Opened before anything can go wrong with the call: by the time a connection exists,
    // the telephone system has answered and somebody is on the line.
    let logged = match log::open(&state.pg, &details, PROVIDER).await {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::warn!(error = %e, "could not open a record for this call");
            None
        }
    };

    let (end, chat_id) = run(&state, &ctx, &details, rx, tx, buf, logged).await;
    if let Some(id) = logged {
        log::close(&state.pg, id, end, chat_id).await;
    }
}

/// Read until the far end says which call it is carrying.
async fn hello(rx: &mut tokio::net::tcp::OwnedReadHalf, buf: &mut Vec<u8>) -> Option<[u8; 16]> {
    let deadline = tokio::time::Instant::now() + HELLO_TIMEOUT;
    loop {
        // Anything already buffered, first: two messages can arrive in one read.
        loop {
            match frame::step(buf) {
                Step::Got(Message::Id(id), used) => {
                    buf.drain(..used);
                    return Some(id);
                }
                // Audio before the identifier is audio for a call we cannot name. Dropped
                // rather than carried, because there is nowhere to carry it to yet.
                Step::Got(_, used) => {
                    buf.drain(..used);
                }
                Step::More => break,
                Step::Broken(why) => {
                    tracing::warn!(why, "a telephone connection is not speaking this protocol");
                    return None;
                }
            }
        }
        let mut chunk = [0u8; 2048];
        let read = tokio::time::timeout_at(deadline, rx.read(&mut chunk)).await;
        match read {
            Ok(Ok(0)) | Err(_) => return None,
            Ok(Ok(n)) => {
                if buf.len() + n > READ_BUFFER_CAP {
                    return None;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Ok(Err(_)) => return None,
        }
    }
}

/// The call itself: the session, the notice, and audio in both directions.
async fn run(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    details: &CallDetails,
    mut rx: tokio::net::tcp::OwnedReadHalf,
    tx: tokio::net::tcp::OwnedWriteHalf,
    mut buf: Vec<u8>,
    logged: Option<Uuid>,
) -> (log::CallEnd, Option<Uuid>) {
    // Raw samples on the wire, and a telephone system that never says what it has played:
    // it does not need to, because the pacer releases a frame every twenty milliseconds
    // and what has left here is on the line.
    let Some(session) = start_session(state, ctx, details, logged, Wire::Pcm16, false).await else {
        return (log::CallEnd::NoMedia, None);
    };
    // The sink is the session's to speak through; nothing here has to hand it anything
    // back, because this wire has no messages that come the other way about a reply.
    let StartedCall {
        voice,
        sink: _,
        out_rx,
        control_rx,
        pacer_task,
        ending,
        recorder,
        recording,
    } = session;

    let entry = CallEntry {
        session: voice.clone(),
        ending: ending.clone(),
        started: std::time::Instant::now(),
        from: details.from.clone(),
    };
    let cfg = TelephonyResolved::load(
        &state.pg,
        state.message_key,
        &state.boot.telephony,
        &state.boot.server.public_url,
    )
    .await;
    if !state.telephony.try_insert(details.call_sid.clone(), entry, cfg.max_concurrent_calls) {
        tracing::warn!(call = %details.call_sid, "the line filled up between answering and connecting");
        voice.shutdown().await;
        pacer_task.abort();
        return (log::CallEnd::LineFull, None);
    }

    audit_call(state, ctx, "telephony.call.started", details, None).await;
    metrics::counter!("telephony_calls_total").increment(1);

    let writer = tokio::spawn(write_to_line(tx, out_rx, control_rx, recorder.clone()));
    let started = std::time::Instant::now();

    // The notice, before a word the caller says is listened to, on its own task alongside
    // the reader. The same fail-closed rule: a line that cannot say what it is does not
    // carry the call.
    voice.hold_for_notice();
    let notice = {
        let state = state.clone();
        let voice = voice.clone();
        let ending = ending.clone();
        let ctx = ctx.clone();
        let details = details.clone();
        tokio::spawn(async move {
            if voice.announce(details.opening.clone()).await {
                if let Some(call_id) = logged {
                    log::record_notice(&state.pg, call_id, &details.opening).await;
                }
                return;
            }
            tracing::warn!(call = %details.call_sid, "the caller could not be told what they were speaking to; ending the call");
            audit_call(&state, &ctx, "telephony.notice.failed", &details, None).await;
            metrics::counter!("telephony_notice_failed_total").increment(1);
            ending.end_now(log::CallEnd::NoticeFailed);
        })
    };

    let end = read_from_line(&voice, &mut rx, &mut buf, &ending, recorder.as_ref()).await;
    notice.abort();

    let chat_id = voice.chat_id();
    if let Some(entry) = state.telephony.remove(&details.call_sid) {
        entry.session.shutdown().await;
    } else {
        voice.shutdown().await;
    }
    writer.abort();
    pacer_task.abort();
    // The recording, once every handle has gone: the writer held one and has just been
    // stopped, and this is the last.
    if let Some(task) = recording {
        drop(recorder);
        if let Ok(done) = task.await {
            if let Some(call_id) = logged {
                log::record_recording(&state.pg, call_id, &done).await;
            }
            if done.failed {
                tracing::warn!(call = %details.call_sid, "a call that was to be recorded produced no recording");
                metrics::counter!("telephony_recording_failed_total").increment(1);
            }
        }
    }
    let secs = started.elapsed().as_secs_f64();
    metrics::histogram!("telephony_call_seconds").record(secs);
    audit_call(state, ctx, "telephony.call.ended", details, Some(secs)).await;
    tracing::info!(call = %details.call_sid, secs, outcome = end.as_str(), "call ended");
    (end, chat_id)
}

/// Everything the telephone system sends us, until the call ends.
async fn read_from_line(
    voice: &Arc<crate::voice::Session>,
    rx: &mut tokio::net::tcp::OwnedReadHalf,
    buf: &mut Vec<u8>,
    ending: &super::Ending,
    recorder: Option<&crate::voice::telephony::record::Recorder>,
) -> log::CallEnd {
    // One converter for the whole call: it carries the filter's memory from one frame to
    // the next, and a fresh one per frame is a click fifty times a second.
    let mut upsample = match voice.capture_rate() {
        codec::TELEPHONY_RATE => None,
        16_000 => Some(Resampler::up_8k_to_16k()),
        other => {
            tracing::warn!(rate = other, "no conversion from a telephone line to this recognition rate");
            return log::CallEnd::NoMedia;
        }
    };
    loop {
        // Whatever is already buffered, before waiting for more.
        loop {
            match frame::step(buf) {
                Step::Got(msg, used) => {
                    let taken: Vec<u8> = buf.drain(..used).collect();
                    let _ = taken;
                    match msg {
                        Message::Audio(payload) => {
                            let samples = frame::samples(&payload);
                            // The caller's own side. This wire carries raw samples, so
                            // they are companded on the way into the recording, which is
                            // the form a telephone recording is kept in.
                            if let Some(rec) = recorder {
                                rec.caller(&samples);
                            }
                            let samples = match upsample.as_mut() {
                                Some(r) => r.process(&samples),
                                None => samples,
                            };
                            let pcm: Vec<u8> =
                                samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                            voice.on_pcm(pcm).await;
                        }
                        // The polite end of a call.
                        Message::Hangup => return log::CallEnd::Completed,
                        // Keypad digits are read and dropped: nothing asks for them yet,
                        // and a caller pressing a key must not end the call.
                        Message::Dtmf(_) => {}
                        Message::Error(detail) => {
                            tracing::warn!(
                                detail = %String::from_utf8_lossy(&detail),
                                "the telephone system reported a fault on this call"
                            );
                            return log::CallEnd::Dropped;
                        }
                        // Somebody else's software on its own release schedule. A message
                        // this build does not know must not end a call.
                        Message::Id(_) | Message::Unknown(_) => {}
                    }
                }
                Step::More => break,
                Step::Broken(why) => {
                    tracing::warn!(why, "ending a call whose connection stopped making sense");
                    return log::CallEnd::Dropped;
                }
            }
        }

        let mut chunk = [0u8; 4096];
        let next = tokio::select! {
            // Asked to end from outside: the agent finished the call, or put the caller
            // through, or something else decided this call is over.
            end = ending.ended() => {
                tracing::info!(outcome = end.as_str(), "ending the call from outside the connection");
                return end;
            }
            read = tokio::time::timeout(IDLE_TIMEOUT, rx.read(&mut chunk)) => read,
        };
        match next {
            Ok(Ok(0)) => return log::CallEnd::Completed,
            Ok(Ok(n)) => {
                if buf.len() + n > READ_BUFFER_CAP {
                    tracing::warn!("a telephone connection sent more than one message's worth at once");
                    return log::CallEnd::Dropped;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Ok(Err(_)) => return log::CallEnd::Dropped,
            Err(_) => {
                tracing::info!("the telephone connection went quiet; ending the call");
                return log::CallEnd::Dropped;
            }
        }
    }
}

/// Everything we send the telephone system.
///
/// Audio arrives here already paced, one frame at a time. The two other things the carrier
/// path sends have no counterpart on this wire and neither is missed: a playback mark is
/// not asked for because nothing here buffers audio of its own, and an instruction to
/// abandon what is buffered is not needed for the same reason. The pacer's own queue is
/// dropped where the interruption happens, which is all there is to drop.
async fn write_to_line(
    mut tx: tokio::net::tcp::OwnedWriteHalf,
    mut out: mpsc::Receiver<Outbound>,
    mut control: mpsc::Receiver<Control>,
    recorder: Option<crate::voice::telephony::record::Recorder>,
) {
    loop {
        let item = tokio::select! {
            // Drained so the sender never blocks on a full channel. There is nothing to
            // render, and a queue nobody reads would stall the reply that fills it.
            _ = control.recv() => continue,
            item = out.recv() => item,
        };
        let Some(item) = item else { return };
        let bytes = match item {
            Outbound::Frame(f) => {
                // What this end said, taken where it leaves: a sentence the caller talked
                // over never reaches here, so the recording holds what they heard.
                if let Some(rec) = &recorder {
                    rec.line(&frame::samples(&f));
                }
                frame::encode(&Message::Audio(f))
            }
            // Nothing to send: this wire has no way to ask, and nothing to ask.
            Outbound::Mark(_) => continue,
        };
        if tx.write_all(&bytes).await.is_err() {
            return;
        }
    }
}

async fn audit_call(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    action: &str,
    details: &CallDetails,
    secs: Option<f64>,
) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("telephony".into());
    ev.resource_id = Some(details.agent_id);
    let mut payload = serde_json::json!({
        "provider": PROVIDER,
        "call": details.call_sid,
        "from": details.from,
        "to": details.to,
    });
    if let Some(secs) = secs {
        payload["seconds"] = serde_json::json!(secs.round() as u64);
    }
    ev.payload = Some(payload);
    let _ = audit::append(&state.pg, &ev).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identifier a telephone system presents is sixteen bytes, and the ticket it has
    /// to match is the text form of the same value. If these ever disagreed, every call
    /// would be refused as unminted and nothing would say why.
    #[test]
    fn an_identifier_reads_back_as_the_ticket_it_was_minted_as() {
        let minted = Uuid::now_v7();
        let on_the_wire = *minted.as_bytes();
        assert_eq!(Uuid::from_bytes(on_the_wire).to_string(), minted.to_string());
    }

    /// The read buffer holds a whole message and a little slack, and no more: past that a
    /// far end is not speaking this protocol and holding more is doing its work for it.
    #[test]
    fn the_read_buffer_holds_one_message_and_some_slack() {
        assert!(READ_BUFFER_CAP > frame::MAX_PAYLOAD);
        assert!(READ_BUFFER_CAP < frame::MAX_PAYLOAD * 2);
    }

    /// The idle bound is far longer than the twenty milliseconds a live line sends at, so
    /// it only ever fires on a connection that has genuinely stopped.
    #[test]
    fn the_idle_bound_is_far_longer_than_a_frame() {
        assert!(IDLE_TIMEOUT.as_millis() > 20 * 50);
    }
}
