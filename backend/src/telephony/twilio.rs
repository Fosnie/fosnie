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

//! The carrier's side of a telephone call.
//!
//! Three endpoints. The carrier asks what to do with an incoming call and is answered
//! with an instruction to open a media socket; it opens that socket and the call's
//! audio flows both ways over it; and separately it reports when the call is over,
//! which is the safety net for a socket that has stopped responding without closing.
//!
//! The media protocol is JSON text messages in both directions. Two details of it are
//! load-bearing and silent when wrong. Every message sent to the carrier must repeat
//! the stream identifier they gave us, or they discard it without a word: no error, no
//! close, just no audio. And the audio must be nothing but samples, with no container
//! or header of any kind, because they are put straight on the line: a file header is
//! played to the caller as noise and puts everything after it out of step.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{FromRequestParts, RawQuery, State, WebSocketUpgrade};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::sign;
use super::{start_session, CallDetails, CallEntry, SourceIp, StartedCall, TelephonyResolved};
use crate::audit::{self, AuditEvent, AuditOutcome};
use crate::auth::PlatformRole;
use crate::error::AppError;
use crate::state::AppState;
use crate::voice::telephony::codec::{self, Resampler};
use crate::voice::telephony::pace::{MarkId, Outbound};
use crate::voice::telephony::{Control, TelephonySink};

use super::log;

/// The path the answer arrives on. Also the path the carrier signed, so it is written
/// once and used for both.
const ANSWER_PATH: &str = "/api/telephony/twilio/voice";
const STATUS_PATH: &str = "/api/telephony/twilio/status";
const MEDIA_PATH: &str = "/api/telephony/twilio/media";
pub const CONTINUE_PATH: &str = "/api/telephony/twilio/continue";

/// How long to let a transferred call ring before giving up on the person.
const DIAL_TIMEOUT_SECS: u32 = 25;

/// The carrier this module speaks for, as recorded against a line and a call.
const PROVIDER: &str = "twilio";

/// A media stream that has gone quiet for this long is a call that has ended without
/// anybody saying so. Well above the 20 ms a live line sends at, so it only ever fires
/// on a genuinely dead stream.
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Answering
// ---------------------------------------------------------------------------

/// What to tell a carrier when we will not take the call.
///
/// Deliberately a success with a refusal inside it, rather than an error status. A
/// refusal leaves the call unanswered, so the caller is not charged and hears their own
/// network's engaged treatment; an error status makes the carrier answer the call, play
/// a recorded apology, and bill for the privilege.
const REJECT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Response><Reject/></Response>"#;

fn reject() -> Response {
    xml(REJECT_XML.to_string())
}

fn xml(body: String) -> Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/xml")], Body::from(body)).into_response()
}

/// Tell the carrier to open a two-way media socket for this call.
///
/// What follows the instruction is what decides whether this call can be handed to a
/// person. The carrier reads the rest of this answer once our socket closes: with
/// nothing after it, it finds nothing more to do and ends the call, which is every call
/// that has ever run through here. With somewhere to come back to, that same close
/// becomes a question, and the answer to it can be "ring this person instead".
///
/// So the continuation is added only for a line that has somewhere to put callers
/// through to. A deployment that never transfers keeps exactly the answer it had, and
/// one fewer request per call.
fn connect_xml(socket_base: &str, ticket: &str, continue_at: Option<&str>) -> String {
    let tail = match continue_at {
        Some(url) => format!("<Redirect>{url}</Redirect>"),
        None => String::new(),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><Response><Connect><Stream url="{socket_base}{MEDIA_PATH}?ticket={ticket}"/></Connect>{tail}</Response>"#
    )
}

/// What to do with a call once we have stopped speaking on it.
///
/// Three answers, and the caller hears exactly one of them. Nothing here reaches out to
/// the network: it asked us, and this is the reply.
fn continue_xml(dial: Option<(&str, &str, &str)>) -> String {
    let body = match dial {
        // Ring the person this line puts callers through to. `callerId` is the line's
        // own number, which is the number this deployment owns and is entitled to
        // present; who actually rang is unverified and belongs in the written record
        // rather than in the network's caller display.
        Some((to, caller_id, action)) => format!(
            r#"<Dial action="{action}" callerId="{caller_id}" timeout="{DIAL_TIMEOUT_SECS}">{to}</Dial>"#
        ),
        None => "<Hangup/>".to_string(),
    };
    format!(r#"<?xml version="1.0" encoding="UTF-8"?><Response>{body}</Response>"#)
}

/// Nobody picked the call up, so say so and stop.
///
/// The one sentence in this whole feature that the network speaks in its own voice
/// rather than ours: by the time it is needed our side of the call has ended and there
/// is nothing of ours left to say it with. It is a constant, and it carries nothing
/// about the call, the caller, or what they wanted.
const UNANSWERED_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?><Response><Say>Sorry, nobody is available to take your call. Please try again later.</Say><Hangup/></Response>"#;

/// Note a refusal, so that a line which is not answering can be explained.
async fn audit_refusal(state: &AppState, action: &str, reason: &str, from: &str) {
    let mut ev = AuditEvent::action(action, PlatformRole::User.as_str());
    ev.outcome = AuditOutcome::Failure;
    ev.resource_type = Some("telephony".into());
    ev.payload = Some(serde_json::json!({ "reason": reason, "from": from }));
    let _ = audit::append(&state.pg, &ev).await;
    metrics::counter!("telephony_refused_total", "reason" => reason.to_string()).increment(1);
}

/// The carrier is asking what to do with an incoming call.
///
/// `body` comes last on purpose: it is the whole request body, taken as text rather
/// than through a typed extractor, because the signature covers the parameters as they
/// were sent and a typed extractor would both consume the body and quietly drop any
/// parameter it did not expect.
pub async fn answer(
    State(state): State<AppState>,
    SourceIp(ip): SourceIp,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: String,
) -> Response {
    let cfg = TelephonyResolved::load(
        &state.pg,
        state.message_key,
        &state.boot.telephony,
        &state.boot.server.public_url,
    )
    .await;
    let pairs = sign::form_pairs(&body);
    let from = sign::param(&pairs, "From").unwrap_or_default().to_string();

    if let Err(status) = verified(&state, &cfg, &headers, &ip, ANSWER_PATH, query.as_deref(), &pairs).await {
        return status.into_response();
    }

    // From here the request is known to be the carrier's, so a refusal can be a civil
    // one that the caller hears rather than a bare status code.
    //
    // The line is looked up first, and only after the signature has been checked. An
    // unsigned request must cost no database work at all, and answering one differently
    // depending on whether the number exists would tell anybody who cared to dial which
    // numbers this deployment answers.
    let to = sign::param(&pairs, "To").unwrap_or_default();
    let line = match super::line_for(&state.pg, &cfg.provider, to).await {
        Ok(line) => line,
        Err(reason) => {
            audit_refusal(&state, "telephony.refused", reason.as_str(), &from).await;
            return reject();
        }
    };
    let (owner, agent_id) = (line.owner_user_id, line.agent_id);
    let Some(call_sid) = sign::param(&pairs, "CallSid").filter(|s| !s.is_empty()) else {
        audit_refusal(&state, "telephony.refused", "no_call_id", &from).await;
        return reject();
    };
    let Some(socket_base) = sign::socket_base(&cfg.public_base_url) else {
        audit_refusal(&state, "telephony.refused", "no_socket_base", &from).await;
        return reject();
    };

    // Who the call runs as, and whether they still exist.
    let ctx = match crate::auth::load_context(&state.pg, owner).await {
        Ok(ctx) => ctx,
        Err(_) => {
            audit_refusal(&state, "telephony.refused", "owner_unavailable", &from).await;
            return reject();
        }
    };
    // A call is a live-voice session, so it needs the same capabilities one does.
    if !crate::features::enabled_for(&state, &ctx, "voice").await
        || !crate::features::enabled_for(&state, &ctx, "voice_live").await
    {
        audit_refusal(&state, "telephony.refused", "voice_not_enabled", &from).await;
        return reject();
    }

    // Two engine requirements, checked before answering rather than discovered
    // mid-call. There is no audio decoder in this process, so the reply has to arrive
    // as raw samples from a streaming synthesiser or the line is silent; and the
    // recognition rate has to be one the carrier's 8 kHz audio can be converted to.
    let vc = crate::voice::VoiceLiveResolved::load(&state.pg, state.message_key, &state.boot.voice_live).await;
    if !vc.tts_stream || vc.tts_stream_url.is_empty() {
        tracing::warn!("a telephone line needs a streaming synthesiser configured");
        audit_refusal(&state, "telephony.refused", "no_streaming_synthesiser", &from).await;
        return reject();
    }
    if !matches!(vc.stt_sample_rate.max(8_000), 8_000 | 16_000) {
        tracing::warn!(rate = vc.stt_sample_rate, "a telephone line cannot be converted to this recognition rate");
        audit_refusal(&state, "telephony.refused", "unsupported_rate", &from).await;
        return reject();
    }

    // Abuse guards. The per-caller one is the only limit that maps to who is running
    // up the bill; the per-line one catches a withheld or forged caller.
    let caller_key = if from.is_empty() { "anonymous".to_string() } else { from.clone() };
    if !crate::cache::rate_limit_ok(&state.redis, &format!("tel:from:{caller_key}"), 5, 300).await {
        audit_refusal(&state, "telephony.refused", "rate_from", &from).await;
        return reject();
    }
    if !crate::cache::rate_limit_ok(&state.redis, &format!("tel:to:{to}"), 60, 60).await {
        audit_refusal(&state, "telephony.refused", "rate_to", &from).await;
        return reject();
    }
    // The ceiling that does not depend on anything outside this process. Checked here
    // for a civil refusal, and again when the slot is actually taken.
    if state.telephony.len() >= cfg.max_concurrent_calls {
        audit_refusal(&state, "telephony.refused", "concurrency", &from).await;
        return reject();
    }

    let details = CallDetails {
        owner_user_id: owner,
        agent_id,
        opening: line.opening.clone(),
        record_calls: line.record_calls,
        call_sid: call_sid.to_string(),
        from: from.clone(),
        to: to.to_string(),
        phone_number_id: line.id,
    };
    // A ticket that could not be stored is a socket that could never be opened, so the
    // call is refused rather than answered into silence.
    let ticket = match super::issue_ticket(&state.redis, &details).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "could not mint a call ticket");
            audit_refusal(&state, "telephony.refused", "ticket_unavailable", &from).await;
            return reject();
        }
    };
    // Somewhere for the carrier to come back to, but only if this line has anywhere to
    // put a caller through to. Settled here, at the answer, because it is part of the
    // instruction the call is picked up on and cannot be changed afterwards.
    let continue_at = line
        .transfer_e164
        .as_ref()
        .map(|_| format!("{}{CONTINUE_PATH}", cfg.public_base_url.trim_end_matches('/')));
    xml(connect_xml(&socket_base, &ticket, continue_at.as_deref()))
}

/// The carrier asking what to do with a call now that we have stopped speaking on it.
///
/// Reached twice at most: once when our socket closes, and once more after a transfer
/// has been attempted, to say how it went. Both are the carrier's own requests and are
/// verified the same way as every other one.
///
/// Everything it needs is read from the call's own row rather than from anything held in
/// memory. This arrives as a fresh request that knows only which call it is about, and
/// it may well arrive after the process carrying that call has stopped: reading the row
/// is what makes the answer the same either way.
pub async fn continue_call(
    State(state): State<AppState>,
    SourceIp(ip): SourceIp,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: String,
) -> Response {
    let cfg = TelephonyResolved::load(
        &state.pg,
        state.message_key,
        &state.boot.telephony,
        &state.boot.server.public_url,
    )
    .await;
    let pairs = sign::form_pairs(&body);
    if let Err(status) = verified(&state, &cfg, &headers, &ip, CONTINUE_PATH, query.as_deref(), &pairs).await {
        return status.into_response();
    }
    let Some(call_sid) = sign::param(&pairs, "CallSid").filter(|s| !s.is_empty()) else {
        return xml(continue_xml(None));
    };

    // The second visit: the transfer has already been tried and this says how it went.
    // Anything but an answered call is somebody hearing a ringing tone and nothing else,
    // so it is worth saying why it stopped.
    if let Some(dial_status) = sign::param(&pairs, "DialCallStatus") {
        let answered = dial_status == "completed" || dial_status == "answered";
        audit_transfer(&state, call_sid, dial_status, answered).await;
        metrics::counter!("telephony_transfers_total", "outcome" => dial_status.to_string())
            .increment(1);
        return if answered { xml(continue_xml(None)) } else { xml(UNANSWERED_XML.to_string()) };
    }

    // The first visit: our side of the call has just ended. Whether it ended because
    // somebody is to be rung is written on the row.
    let row = sqlx::query!(
        "SELECT c.transfer_to, c.to_e164 FROM calls c \
         WHERE c.provider = $1 AND c.provider_call_id = $2",
        PROVIDER,
        call_sid,
    )
    .fetch_optional(&state.pg)
    .await;
    let dial = match row {
        Ok(Some(r)) => r.transfer_to.map(|to| (to, r.to_e164)),
        Ok(None) => None,
        Err(e) => {
            // A call we cannot read is a call we do not transfer. Hanging up is the
            // answer that cannot put a caller somewhere unintended.
            tracing::warn!(error = %e, %call_sid, "could not read the call being continued");
            None
        }
    };
    match dial {
        Some((to, caller_id)) => {
            let action = format!("{}{CONTINUE_PATH}", cfg.public_base_url.trim_end_matches('/'));
            tracing::info!(%call_sid, "putting the caller through");
            xml(continue_xml(Some((&to, &caller_id, &action))))
        }
        None => xml(continue_xml(None)),
    }
}

/// Note how a transfer went, against the call it was for.
async fn audit_transfer(state: &AppState, call_sid: &str, dial_status: &str, answered: bool) {
    let mut ev = AuditEvent::action("telephony.call.transferred", PlatformRole::User.as_str());
    if !answered {
        ev.outcome = AuditOutcome::Failure;
        ev.outcome_reason = Some(dial_status.to_string());
    }
    ev.resource_type = Some("telephony".into());
    ev.payload = Some(serde_json::json!({ "call_sid": call_sid, "dial_status": dial_status }));
    let _ = audit::append(&state.pg, &ev).await;
}

/// The carrier telling us how a call ended.
///
/// Always answered with a success, whatever we make of it: a report about a call this
/// process has never heard of is the normal case after a restart, and arguing about it
/// would only make the carrier retry.
pub async fn status(
    State(state): State<AppState>,
    SourceIp(ip): SourceIp,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: String,
) -> Response {
    let cfg = TelephonyResolved::load(
        &state.pg,
        state.message_key,
        &state.boot.telephony,
        &state.boot.server.public_url,
    )
    .await;
    let pairs = sign::form_pairs(&body);
    if let Err(status) = verified(&state, &cfg, &headers, &ip, STATUS_PATH, query.as_deref(), &pairs).await {
        return status.into_response();
    }
    let call_status = sign::param(&pairs, "CallStatus").unwrap_or_default();
    if matches!(call_status, "completed" | "failed" | "busy" | "no-answer" | "canceled") {
        if let Some(call_sid) = sign::param(&pairs, "CallSid") {
            // Ask the socket to tear itself down rather than doing it here, so exactly
            // one path frees the call and the two cannot race.
            if state.telephony.hangup(call_sid, log::CallEnd::CarrierEnded) {
                tracing::info!(%call_sid, %call_status, "the carrier reports the call is over");
            } else if log::close_by_provider_id(&state.pg, PROVIDER, call_sid, log::CallEnd::CarrierEnded).await {
                // Nothing here is carrying it, but the record is still open: the process
                // that took the call has stopped since. Closed now rather than waiting for
                // the sweep at the next start.
                tracing::info!(%call_sid, %call_status, "closed the record of a call this instance no longer carries");
            }
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Is this request really the carrier's?
///
/// Returns the status to answer with when it is not. Every failure is the same bare
/// 403 with no body: an unsigned request is not a call, and there is nothing to say to
/// whoever sent it.
async fn verified(
    state: &AppState,
    cfg: &TelephonyResolved,
    headers: &HeaderMap,
    ip: &str,
    path: &str,
    query: Option<&str>,
    pairs: &[(String, String)],
) -> Result<(), StatusCode> {
    let Some(token) = cfg.auth_token.as_deref().filter(|t| !t.is_empty()) else {
        // Configured to answer a telephone but with no way to tell a real call from an
        // invented one. Never guess: refuse, and say so loudly enough to be fixed.
        tracing::warn!("a telephony request arrived but no carrier credential is configured");
        audit_refusal(state, "telephony.unconfigured", "no_credential", "").await;
        return Err(StatusCode::FORBIDDEN);
    };
    if !sign::base_is_usable(&cfg.public_base_url) {
        tracing::warn!(
            base = %cfg.public_base_url,
            "the public address a carrier would have reached is not configured, so their signature cannot be checked"
        );
        audit_refusal(state, "telephony.unconfigured", "no_public_base", "").await;
        return Err(StatusCode::FORBIDDEN);
    }
    if sign::base_is_local(&cfg.public_base_url) {
        // Not refused, because it is a legitimate if unusual arrangement. But it is far
        // more often the sign of a public address that was never set, and then every
        // signature fails and the line rings without ever answering.
        tracing::warn!(
            base = %cfg.public_base_url,
            "the configured public address names only this machine; a carrier's signature will not match unless that is genuinely how they reach it"
        );
    }
    let presented = headers
        .get(sign::SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let url = sign::signed_url(&cfg.public_base_url, path, query);
    if sign::verify(token, &url, pairs, presented) {
        return Ok(());
    }
    // Counted only on failure, so the handful of addresses a real carrier calls from
    // never approach it, while anybody using this to guess at signatures does.
    if !crate::cache::rate_limit_ok(&state.redis, &format!("tel:sig:{ip}"), 30, 60).await {
        tracing::warn!(%ip, "too many rejected telephony signatures from one address");
    }
    let mut ev = AuditEvent::action("telephony.signature.rejected", PlatformRole::User.as_str());
    ev.outcome = AuditOutcome::Failure;
    ev.resource_type = Some("telephony".into());
    // The address and nothing else. The body of a rejected request is unattributed
    // input, and copying it into the audit log is how a log becomes an injection
    // surface of its own.
    ev.payload = Some(serde_json::json!({ "ip": ip, "path": path }));
    let _ = audit::append(&state.pg, &ev).await;
    Err(StatusCode::FORBIDDEN)
}

// ---------------------------------------------------------------------------
// The media socket
// ---------------------------------------------------------------------------

/// A redeemed call ticket: proof that the answer to this call was the carrier's, and
/// everything that answer decided.
pub struct CallTicket {
    details: CallDetails,
    ctx: crate::auth::AuthContext,
}

impl FromRequestParts<AppState> for CallTicket {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        // A carrier opening a socket sends no Origin. A browser always sends one. So
        // refusing anything that has one costs nothing and shuts the door on a page
        // trying to drive somebody's telephone line from a visitor's browser.
        if parts.headers.contains_key(header::ORIGIN) {
            return Err(AppError::Unauthorized("not a carrier".into()));
        }
        let query = parts.uri.query().unwrap_or_default();
        let ticket = form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k == "ticket")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        if ticket.is_empty() {
            return Err(AppError::Unauthorized("no call ticket".into()));
        }
        // Redeemed as it is read, so a ticket works exactly once. A second socket
        // opened with the same one, by the carrier retrying or by anybody who saw it,
        // finds nothing.
        let Some(details) = super::redeem_ticket(&state.redis, &ticket).await? else {
            return Err(AppError::Unauthorized("that call ticket is spent or expired".into()));
        };
        // Checked again on this second hop: an account deactivated between answering
        // the call and opening its socket must not get a session.
        let ctx = crate::auth::load_context(&state.pg, details.owner_user_id).await?;
        Ok(CallTicket { details, ctx })
    }
}

/// The carrier opening the media socket for a call we agreed to take.
pub async fn media(
    State(state): State<AppState>,
    call: CallTicket,
    ws: WebSocketUpgrade,
) -> Response {
    // A media message is a 20 ms frame in base64. These bounds are generous for that
    // and mean a peer cannot make us hold an arbitrary amount for one message.
    ws.max_message_size(32 * 1024)
        .max_frame_size(32 * 1024)
        .on_upgrade(move |socket| run_call(state, socket, call))
}

/// One call, from the moment its socket opens to the moment it is gone.
async fn run_call(state: AppState, socket: WebSocket, call: CallTicket) {
    let CallTicket { details, ctx } = call;
    let call_sid = details.call_sid.clone();

    // Opened before anything can go wrong with the call, because by the time the carrier
    // hands us this socket it has already answered and the caller is already paying. A
    // call that fails in the next instant is still a call that happened.
    let logged = match log::open(&state.pg, &details, PROVIDER).await {
        Ok(id) => Some(id),
        Err(e) => {
            // Not fatal. Refusing to take a call because the log is unavailable would
            // turn a bookkeeping fault into a service outage.
            tracing::warn!(error = %e, %call_sid, "could not open a record for this call");
            None
        }
    };

    let (end, chat_id) = carry_call(&state, socket, &ctx, &details, &call_sid, logged).await;

    // One close, reached however the call ended, including the ways that used to return
    // before the teardown could run.
    if let Some(id) = logged {
        log::close(&state.pg, id, end, chat_id).await;
    }
}

/// One call, from the moment its socket opens to the moment it is gone. Returns how it
/// ended and the conversation it produced, if it produced one.
async fn carry_call(
    state: &AppState,
    socket: WebSocket,
    ctx: &crate::auth::AuthContext,
    details: &CallDetails,
    call_sid: &str,
    logged: Option<Uuid>,
) -> (log::CallEnd, Option<Uuid>) {
    let (mut sink, mut stream) = socket.split();

    // The carrier names the stream in its opening message, and every message we send
    // has to repeat that name. Until it arrives there is nothing we may say.
    let stream_sid = match wait_for_start(&mut stream, call_sid).await {
        Some(sid) => sid,
        None => {
            tracing::warn!(%call_sid, "the media socket closed before the call started");
            return (log::CallEnd::NoMedia, None);
        }
    };

    // This carrier companded narrowband, and reports what it has played.
    let session =
        match start_session(state, ctx, details, logged, crate::voice::telephony::Wire::Mulaw, true)
            .await
        {
        Some(s) => s,
        None => {
            // Nothing to carry the call. Close the socket rather than leaving the
            // carrier holding an answered, billed, silent line.
            let _ = sink.close().await;
            return (log::CallEnd::NoMedia, None);
        }
    };
    let StartedCall {
        voice,
        sink: line,
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
    // The ceiling, taken for real this time. Checked at the answer too, but a slot is
    // only actually claimed here: a ticket nobody redeems must not hold one.
    let cfg = TelephonyResolved::load(&state.pg, state.message_key, &state.boot.telephony, &state.boot.server.public_url).await;
    if !state.telephony.try_insert(call_sid.to_string(), entry, cfg.max_concurrent_calls) {
        tracing::warn!(%call_sid, "the line filled up between answering and connecting");
        voice.shutdown().await;
        pacer_task.abort();
        let _ = sink.close().await;
        return (log::CallEnd::LineFull, None);
    }

    audit_call(state, ctx, "telephony.call.started", details, None).await;
    metrics::counter!("telephony_calls_total").increment(1);

    let writer =
        tokio::spawn(write_to_carrier(sink, out_rx, control_rx, stream_sid.clone(), recorder.clone()));
    let started = std::time::Instant::now();

    // The first thing that happens on the call, and it happens before a single word the
    // caller says is listened to. A caller has signed nothing and read nothing, so
    // everything they are told about what becomes of what they say has to be said aloud,
    // and while it is being said the session drops what it hears: the notice cannot be
    // talked over, and nothing said across it is answered underneath it.
    //
    // On its own task, alongside the reader below rather than before it, and that is not
    // an optimisation. What the carrier says back includes its reports of having played
    // the audio, and only the reader takes those in: spoken with nothing reading, the
    // notice would wait out the grace period for a report that cannot arrive and the line
    // would give up on playback reporting for the rest of the call.
    //
    // Fail closed. A line that cannot say it does not carry the call, which costs little
    // in practice because a deployment whose synthesiser is down has a line that could
    // not have answered anything anyway, and costs nothing at all in the case that
    // matters: nobody is listened to who was not told.
    // Deaf from here until the notice is out, set before the task that speaks it exists so
    // that a caller already talking as the call connected cannot get a syllable in first.
    voice.hold_for_notice();
    let notice = {
        let state = state.clone();
        let voice = voice.clone();
        let ending = ending.clone();
        let ctx = ctx.clone();
        let details = details.clone();
        let call_sid = call_sid.to_string();
        tokio::spawn(async move {
            if voice.announce(details.opening.clone()).await {
                if let Some(call_id) = logged {
                    log::record_notice(&state.pg, call_id, &details.opening).await;
                }
                return;
            }
            tracing::warn!(%call_sid, "the caller could not be told what they were speaking to; ending the call");
            audit_notice_failed(&state, &ctx, &details).await;
            metrics::counter!("telephony_notice_failed_total").increment(1);
            // Through the same latch every other decision to end a call goes through, so
            // the reader stops, the teardown runs once, and the outcome is recorded.
            ending.end_now(log::CallEnd::NoticeFailed);
        })
    };

    let end = read_from_carrier(&voice, &line, &mut stream, call_sid, &ending, recorder.as_ref()).await;
    // Whatever happened to the call, nothing is left half-said.
    notice.abort();

    // The conversation this call produced, read before the session is torn down. It is
    // created by the first thing the caller said, so a caller who said nothing leaves
    // none, and none is invented to fill the gap.
    let chat_id = voice.chat_id();

    // One teardown, reached from every way a call can end.
    if let Some(entry) = state.telephony.remove(call_sid) {
        entry.session.shutdown().await;
    } else {
        voice.shutdown().await;
    }
    writer.abort();
    pacer_task.abort();
    // The recording, once every handle has gone: the writer held one and has just been
    // stopped, and this is the last. Only then does the file know its own length.
    if let Some(task) = recording {
        drop(recorder);
        if let Ok(done) = task.await {
            if let Some(call_id) = logged {
                log::record_recording(&state.pg, call_id, &done).await;
            }
            if done.failed {
                tracing::warn!(%call_sid, "a call that was to be recorded produced no recording");
                metrics::counter!("telephony_recording_failed_total").increment(1);
            }
        }
    }
    let secs = started.elapsed().as_secs_f64();
    metrics::histogram!("telephony_call_seconds").record(secs);
    audit_call(state, ctx, "telephony.call.ended", details, Some(secs)).await;
    tracing::info!(%call_sid, secs, outcome = end.as_str(), "call ended");
    (end, chat_id)
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
    // The caller's number is recorded because it is the identity of a party to the
    // conversation, which is the thing an audit trail exists to settle.
    let mut payload = serde_json::json!({
        "call_sid": details.call_sid,
        "from": details.from,
        "to": details.to,
    });
    if let Some(secs) = secs {
        payload["seconds"] = serde_json::json!(secs.round() as u64);
    }
    ev.payload = Some(payload);
    let _ = audit::append(&state.pg, &ev).await;
}

/// A call that was answered and then ended because the caller could not be told what
/// they were speaking to.
///
/// Recorded as a failure rather than an ordinary end of call: the line answered, so the
/// caller was charged for a call that did nothing, and an operator needs to see that
/// happening rather than a run of very short conversations.
async fn audit_notice_failed(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    details: &CallDetails,
) {
    let mut ev = AuditEvent::action("telephony.notice.failed", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("telephony".into());
    ev.resource_id = Some(details.agent_id);
    ev.outcome = AuditOutcome::Failure;
    ev.payload = Some(serde_json::json!({
        "call_sid": details.call_sid,
        "from": details.from,
        "to": details.to,
        // The length only. What the line was going to say is on the line and, for a call
        // that got it out, on the call; repeating it into every audit row would put the
        // same paragraph in the trail once per call for no gain.
        "notice_chars": details.opening.chars().count(),
    }));
    let _ = audit::append(&state.pg, &ev).await;
}

/// Read the carrier's opening messages until the one that names the stream.
///
/// Also where the media format is checked: everything downstream assumes narrowband
/// single-channel audio, and a carrier offering something else must be refused rather
/// than misread.
async fn wait_for_start(
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    call_sid: &str,
) -> Option<String> {
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
        let Message::Text(text) = msg else { continue };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        match value["event"].as_str() {
            // Sent first and says nothing we need.
            Some("connected") => continue,
            Some("start") => {
                let start = &value["start"];
                let encoding = start["mediaFormat"]["encoding"].as_str().unwrap_or_default();
                let rate = start["mediaFormat"]["sampleRate"].as_u64().unwrap_or_default();
                let channels = start["mediaFormat"]["channels"].as_u64().unwrap_or_default();
                if encoding != "audio/x-mulaw" || rate != codec::TELEPHONY_RATE as u64 || channels != 1 {
                    tracing::warn!(%encoding, rate, channels, "the carrier offered audio this line cannot carry");
                    return None;
                }
                // The ticket was minted for one call. A socket opened for a different
                // one is either an instance wired to the wrong place or a ticket being
                // reused, and neither should get a session.
                let offered = start["callSid"].as_str().unwrap_or_default();
                if offered != call_sid {
                    tracing::warn!(%offered, %call_sid, "the media socket is for a different call");
                    return None;
                }
                return value["streamSid"].as_str().map(str::to_string);
            }
            _ => continue,
        }
    }
    None
}

/// Everything the carrier sends us, until the call ends.
async fn read_from_carrier(
    voice: &Arc<crate::voice::Session>,
    line: &Arc<TelephonySink>,
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    call_sid: &str,
    ending: &super::Ending,
    recorder: Option<&crate::voice::telephony::record::Recorder>,
) -> log::CallEnd {
    // One converter for the whole call, because it carries the filter's memory from one
    // frame into the next. A fresh one per frame would restart from silence fifty times
    // a second, which is a click fifty times a second.
    let mut upsample = match voice.capture_rate() {
        codec::TELEPHONY_RATE => None,
        16_000 => Some(Resampler::up_8k_to_16k()),
        other => {
            tracing::warn!(rate = other, "no conversion from a telephone line to this recognition rate");
            return log::CallEnd::NoMedia;
        }
    };
    loop {
        let next = tokio::select! {
            // Asked to end from outside: the carrier told us separately that the call
            // is over, and this socket has stopped noticing.
            end = ending.ended() => {
                tracing::info!(%call_sid, outcome = end.as_str(), "ending the call from outside the socket");
                return end;
            }
            // A live line sends every 20 ms. Silence for seconds means the far end is
            // gone without having said so, and without this the call would sit here
            // being billed for.
            msg = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => match msg {
                Ok(Some(Ok(m))) => m,
                // A clean close is the ordinary end of a call: the carrier stops the
                // stream when the caller hangs up.
                Ok(None) => return log::CallEnd::Completed,
                Ok(Some(Err(_))) => return log::CallEnd::Dropped,
                Err(_) => {
                    tracing::info!(%call_sid, "the media stream went quiet; ending the call");
                    return log::CallEnd::Dropped;
                }
            },
        };
        let Message::Text(text) = next else { continue };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        // Anything unrecognised is ignored rather than fatal: a carrier adds messages
        // over time, and a call must not drop because of one we have not heard of.
        match value["event"].as_str() {
            Some("media") => {
                let Some(payload) = value["media"]["payload"].as_str() else { continue };
                let Ok(ulaw) = B64.decode(payload.as_bytes()) else { continue };
                // The caller's own side, kept exactly as the line carried it: this is
                // already the form a recording is stored in, so nothing is converted and
                // nothing is lost.
                if let Some(rec) = recorder {
                    rec.caller_ulaw(ulaw.clone());
                }
                let samples = codec::decode(&ulaw);
                let samples = match upsample.as_mut() {
                    Some(r) => r.process(&samples),
                    None => samples,
                };
                let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                voice.on_pcm(pcm).await;
            }
            // The polite end of a call.
            Some("stop") => return log::CallEnd::Completed,
            // The carrier has played everything up to a point we asked about. The only
            // moment this process can know what the caller has actually heard, as opposed
            // to what has been handed over, so the timestamp is taken here at the parse.
            Some("mark") => {
                if let Some(name) = value["mark"]["name"].as_str() {
                    line.mark_echoed(name).await;
                }
            }
            // Keypad digits are read and dropped: nothing asks for them yet, and a caller
            // pressing a key must not end the call.
            Some("dtmf") | Some("connected") | Some("start") => continue,
            _ => continue,
        }
    }
}

/// Everything we send the carrier.
///
/// Audio arrives here already paced, one frame at a time; the one other thing that
/// leaves is the instruction to abandon whatever has been buffered, which comes by its
/// own route so that it cannot queue behind the very audio it is abandoning.
async fn write_to_carrier(
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
    mut out: mpsc::Receiver<Outbound>,
    mut control: mpsc::Receiver<Control>,
    stream_sid: String,
    recorder: Option<crate::voice::telephony::record::Recorder>,
) {
    loop {
        let text = tokio::select! {
            // Biased towards the control channel so that an interruption is not made to
            // wait behind a queue of the audio it exists to cancel.
            biased;
            cmd = control.recv() => match cmd {
                Some(Control::Clear) => {
                    serde_json::json!({ "event": "clear", "streamSid": stream_sid }).to_string()
                }
                None => return,
            },
            // Audio and marks share this arm on purpose. A mark means "you have played
            // everything before this", so it has to leave in its place among the frames;
            // put on the control channel above it would overtake the very audio it refers
            // to and mean the opposite of what it says.
            item = out.recv() => match item {
                // Nothing but the payload. Every other field a carrier sends us is
                // theirs to send, not ours, and the stream name has to be here or the
                // message is dropped in silence.
                Some(Outbound::Frame(bytes)) => {
                    // What this end said, taken where it leaves rather than where it was
                    // synthesised: a sentence the caller talked over is dropped from the
                    // queue before it reaches here, so the recording holds what they
                    // heard rather than what was prepared for them.
                    if let Some(rec) = &recorder {
                        rec.line_ulaw(bytes.clone());
                    }
                    serde_json::json!({
                        "event": "media",
                        "streamSid": stream_sid,
                        "media": { "payload": B64.encode(&bytes) },
                    })
                    .to_string()
                }
                Some(Outbound::Mark(id)) => mark_json(&stream_sid, id),
                None => return,
            },
        };
        if sink.send(Message::Text(text.into())).await.is_err() {
            return;
        }
    }
}

/// Ask the carrier to report when it has played everything sent so far.
fn mark_json(stream_sid: &str, id: MarkId) -> String {
    serde_json::json!({
        "event": "mark",
        "streamSid": stream_sid,
        "mark": { "name": id.name() },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refusal is a success carrying a refusal, and that is a commercial
    /// distinction rather than a stylistic one: this leaves the call unanswered and so
    /// unbilled, whereas an error status makes the carrier answer the call, apologise
    /// to the caller and charge for it.
    #[test]
    fn a_refusal_leaves_the_call_unanswered() {
        assert_eq!(
            REJECT_XML,
            r#"<?xml version="1.0" encoding="UTF-8"?><Response><Reject/></Response>"#
        );
        let resp = reject();
        assert_eq!(resp.status(), StatusCode::OK, "an error status would answer the call");
        assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), "text/xml");
    }

    /// The instruction that opens a two-way media socket, byte for byte.
    ///
    /// Two things are pinned here. It asks to *connect* a stream, which is the two-way
    /// form: the one-way form forks the caller's audio to us and gives us no way to
    /// reply. And on a line that cannot put callers through, nothing follows it, because
    /// the carrier resumes reading this answer when the socket closes and an empty
    /// remainder is what ends the call.
    #[test]
    fn the_answer_asks_for_a_two_way_socket_and_nothing_else() {
        let xml = connect_xml("wss://calls.example.com", "abc", None);
        assert_eq!(
            xml,
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                r#"<Response><Connect><Stream url="wss://calls.example.com"#,
                r#"/api/telephony/twilio/media?ticket=abc"/></Connect></Response>"#
            )
        );
        assert!(xml.ends_with("</Connect></Response>"), "nothing may follow the stream");
    }

    /// A line that can put callers through says where to come back to, and only then.
    ///
    /// That single trailing instruction is the whole of how a transfer is possible: it
    /// turns the closing of our socket from the end of the call into a question we get
    /// to answer. A line without one must be byte-identical to what it was before any of
    /// this existed, which the test above pins from the other side.
    #[test]
    fn a_line_that_can_transfer_says_where_to_come_back_to() {
        let xml = connect_xml(
            "wss://calls.example.com",
            "abc",
            Some("https://calls.example.com/api/telephony/twilio/continue"),
        );
        assert!(xml.contains(r#"<Stream url="wss://calls.example.com/api/telephony/twilio/media?ticket=abc"/>"#));
        assert!(xml.ends_with(
            "</Connect><Redirect>https://calls.example.com/api/telephony/twilio/continue</Redirect></Response>"
        ));
        // The order is load-bearing: the carrier reads the remainder only after the
        // socket closes, so a redirect placed before the connect would fire immediately
        // and the call would never reach us at all.
        let connect = xml.find("<Connect>").unwrap();
        assert!(xml.find("<Redirect>").unwrap() > connect);
    }

    /// What the carrier is told once we have stopped speaking.
    #[test]
    fn a_call_is_put_through_only_when_there_is_somewhere_to_put_it() {
        assert_eq!(
            continue_xml(None),
            r#"<?xml version="1.0" encoding="UTF-8"?><Response><Hangup/></Response>"#
        );
        let dialled = continue_xml(Some((
            "+441315559999",
            "+441315550000",
            "https://calls.example.com/api/telephony/twilio/continue",
        )));
        assert_eq!(
            dialled,
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8"?><Response>"#,
                r#"<Dial action="https://calls.example.com/api/telephony/twilio/continue" "#,
                r#"callerId="+441315550000" timeout="25">+441315559999</Dial></Response>"#
            )
        );
        // The number presented is the line's own, never the caller's: it is the one this
        // deployment owns, and who rang is unverified.
        assert!(dialled.contains(r#"callerId="+441315550000""#));
    }

    /// The bytes of a playback-mark request, including the stream name without which the
    /// carrier discards the message in silence.
    #[test]
    fn a_mark_names_the_stream_it_belongs_to() {
        let id = MarkId { generation: 3, kind: crate::voice::telephony::pace::MarkKind::ReplyEnd };
        assert_eq!(
            mark_json("MZtest", id),
            r#"{"event":"mark","mark":{"name":"r3-end"},"streamSid":"MZtest"}"#
        );
    }

    /// The watchdog exists to notice a dead line, so it has to be far longer than the
    /// gap between frames on a live one. Tied to the frame size so that changing the
    /// pacer cannot quietly make it fire mid-conversation.
    #[test]
    fn the_idle_watchdog_is_far_longer_than_a_frame() {
        assert!(IDLE_TIMEOUT.as_millis() > 100 * codec::FRAME_MS as u128);
    }
}
