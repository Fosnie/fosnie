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

//! Answering a telephone.
//!
//! A call is an ordinary live-voice session with a different transport at the end of
//! it, so almost nothing here is about conversation. What is here is the part that is
//! specific to a telephone: the carrier's endpoints and the proof a request came from
//! them, the settings that describe the line, who the call runs as, and the bookkeeping
//! that makes sure a call that ends stops costing money.
//!
//! Three things about this module are unlike the rest of the product and are all
//! deliberate.
//!
//! **The caller is anonymous and untrusted.** Nobody signs in to a telephone. The
//! session runs as the account the line is registered to, narrowed by the agent the
//! line is bound to: the agent's tool list and its Libraries are the whole of what a
//! caller can reach. That binding is the security boundary, so a line should be
//! registered to an ordinary account and bound to an agent that has been given only
//! what a caller ought to be able to ask about.
//!
//! **These are the first surfaces anyone on the internet can reach without an
//! account.** Every one of them is refused unless it carries a valid carrier
//! signature, and the whole surface is absent rather than merely refused when there is
//! no line configured.
//!
//! **A call costs money.** So every refusal happens before the call is answered
//! (an unanswered call is not billed), there is a hard ceiling on how many calls run at
//! once that does not depend on anything outside this process, and there are as many
//! ways to end a call as there are ways for one to go wrong.

pub mod audiosocket;
pub mod booking;
pub mod conflict;
pub mod diary;
pub mod enquiry;
pub mod frame;
pub mod log;
pub mod notice;
pub mod notify;
pub mod policy;
pub mod preflight;
pub mod retention;
pub mod sign;
pub mod twilio;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::{mpsc, Notify};
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;
use crate::voice::telephony::pace::{self, Outbound};
use crate::voice::telephony::{Control, TelephonySink, Wire};

/// How long a call ticket is good for. Long enough for a carrier to read the answer
/// and open the media socket, short enough that a leaked one is worthless.
const TICKET_TTL_SECS: u64 = 30;

/// The most calls at once when nothing says otherwise. Two, because a self-hoster who
/// has just switched a line on should discover the ceiling in testing rather than on a
/// bill.
const DEFAULT_MAX_CONCURRENT: usize = 2;

/// The settings that describe how this deployment is reached, resolved fresh per call.
///
/// Not the lines themselves: a line is a row, with its own number, agent and owning
/// account. What is left here is what belongs to the deployment rather than to any one
/// line, and the credential is the reason it is worth resolving fresh.
///
/// Follows the same shape as the live-voice engine settings: a boot section for what
/// an operator keeps beside the rest of the deployment, runtime rows overriding it,
/// and the one credential held encrypted so it is never in a config file.
#[derive(Debug, Clone)]
pub struct TelephonyResolved {
    pub provider: String,
    pub public_base_url: String,
    pub max_concurrent_calls: usize,
    /// The carrier's credential, used only to check their signature.
    pub auth_token: Option<String>,
    /// Where to listen for a telephone system on the practice's own network, as an
    /// address and port. Empty means nothing is bound: a deployment answering through a
    /// carrier opens no port of its own, which is the default and stays the default.
    pub audiosocket_listen: String,
    /// The secret a telephone system presents when it asks what to do with a call.
    ///
    /// A telephone system cannot sign a request the way a carrier does, so this stands in
    /// its place. Absent means the endpoints refuse everything: there is no unauthenticated
    /// way in, only a configured one or none.
    pub audiosocket_key: Option<String>,
}

impl TelephonyResolved {
    /// The settable keys, so the admin endpoint, the generic config editor and this
    /// resolver agree on one list.
    pub const STR_KEYS: [&'static str; 4] = [
        "telephony.provider",
        "telephony.public_base_url",
        "telephony.max_concurrent_calls", // int-as-string
        "telephony.audiosocket_listen",
    ];
    pub const ENC_KEYS: [&'static str; 2] =
        ["telephony.auth_token_enc", "telephony.audiosocket_key_enc"];

    pub async fn load(
        pg: &sqlx::PgPool,
        message_key: Option<[u8; 32]>,
        boot: &crate::config::TelephonyConfig,
        server_public_url: &str,
    ) -> Self {
        use crate::config::runtime;
        async fn gets(pg: &sqlx::PgPool, key: &str, dflt: &str) -> String {
            runtime::get(pg, key)
                .await
                .ok()
                .flatten()
                .map(|e| e.value)
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| dflt.to_string())
        }
        // Decrypt the carrier credential. An undecryptable one is treated as absent,
        // which fails the line closed rather than checking signatures against rubbish.
        async fn getkey(pg: &sqlx::PgPool, key: &str, mk: Option<[u8; 32]>) -> Option<String> {
            let ct = runtime::get(pg, key).await.ok().flatten().map(|e| e.value).filter(|v| !v.is_empty())?;
            let _mk = mk?;
            match crate::crypto::decrypt_at_rest(&ct) {
                Ok(pt) => Some(pt),
                Err(_) => {
                    tracing::warn!(%key, "the carrier credential failed to decrypt; the line will not answer");
                    None
                }
            }
        }
        let base = gets(pg, "telephony.public_base_url", &boot.public_base_url).await;
        let base = if base.is_empty() { server_public_url.to_string() } else { base };
        let max = runtime::get(pg, "telephony.max_concurrent_calls")
            .await
            .ok()
            .flatten()
            .and_then(|e| e.value.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_CONCURRENT);
        TelephonyResolved {
            provider: gets(pg, "telephony.provider", &boot.provider).await,
            public_base_url: base,
            max_concurrent_calls: max,
            auth_token: getkey(pg, "telephony.auth_token_enc", message_key).await,
            audiosocket_listen: gets(pg, "telephony.audiosocket_listen", "").await,
            audiosocket_key: getkey(pg, "telephony.audiosocket_key_enc", message_key).await,
        }
    }

    /// Is anything named that can answer a call? Below the feature flag, this is what
    /// makes the line dormant until an operator switches it on deliberately.
    pub fn has_provider(&self) -> bool {
        matches!(self.provider.as_str(), "twilio" | "audiosocket")
    }

    /// Is this deployment answering through a telephone system of its own?
    pub fn is_audiosocket(&self) -> bool {
        self.provider == "audiosocket"
    }
}

/// Put a telephone number into the one form this deployment stores and compares.
///
/// Full international form and nothing else. People write numbers with spaces, dashes,
/// brackets and a leading double nought, and every one of those is the same number, so
/// they are accepted at the point somebody types one in and reduced to a single form
/// there. `None` for anything that is not a telephone number at all.
///
/// This runs on the way **in**, never on the way through: the number a carrier says it
/// called is compared with the stored one exactly as both stand. That is what makes it
/// impossible for one incoming call to match two lines, and it only holds because
/// everything that reaches the column has been through here first.
pub fn normalise_e164(raw: &str) -> Option<String> {
    let stripped: String =
        raw.chars().filter(|c| !matches!(c, ' ' | '\t' | '-' | '(' | ')' | '.' | '\u{a0}')).collect();
    // A leading double nought is how the rest of the world writes a leading plus.
    let candidate = match stripped.strip_prefix("00") {
        Some(rest) => format!("+{rest}"),
        None => stripped,
    };
    let digits = candidate.strip_prefix('+')?;
    let usable = digits.len() >= 7
        && digits.len() <= 15
        && digits.chars().all(|c| c.is_ascii_digit())
        && !digits.starts_with('0');
    usable.then(|| candidate)
}

/// A telephone line, as the answer path needs it.
#[derive(Debug, Clone)]
pub struct Line {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub agent_id: Uuid,
    /// Where this line puts callers through to, when it can. Read at the answer because
    /// it decides what the carrier is told to do once we stop speaking, and that has to
    /// be settled before the call is picked up.
    pub transfer_e164: Option<String>,
    /// The exact words this caller will hear before anything they say is acted on,
    /// composed from the line's greeting and its notice. Settled at the answer with
    /// everything else, so a notice edited while the call is ringing cannot half-apply.
    pub opening: String,
    /// Whether this line keeps the sound of the call. Read at the answer with everything
    /// else, so a switch flipped while a call is ringing cannot produce a recording of a
    /// caller who was told there would not be one.
    pub record_calls: bool,
}

/// Why a call was not taken, in the words the audit trail records.
///
/// Every one of these produces the same thing for the caller. They are told apart only
/// here, because an operator asking "why is my line not answering?" needs the difference
/// and a stranger dialling numbers to see which ones exist must not have it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineRefusal {
    /// No line is registered on that number.
    Unknown,
    /// A line exists but has been switched off.
    Disabled,
    /// The agent that answers it has been archived.
    AgentUnavailable,
    /// The lookup itself failed, so nothing is known. Fails closed.
    LookupFailed,
}

impl LineRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            LineRefusal::Unknown => "unknown_number",
            LineRefusal::Disabled => "line_disabled",
            LineRefusal::AgentUnavailable => "agent_unavailable",
            LineRefusal::LookupFailed => "lookup_failed",
        }
    }
}

/// Find the line registered on the number a call came in on.
pub async fn line_for(pg: &sqlx::PgPool, provider: &str, to: &str) -> Result<Line, LineRefusal> {
    let found = sqlx::query!(
        r#"SELECT p.id, p.owner_user_id, p.agent_id, p.enabled, p.transfer_e164,
                  p.greeting, p.notice, p.record_calls,
                  (a.archived_at IS NOT NULL) AS "agent_archived!"
           FROM phone_numbers p
           JOIN agents a ON a.id = p.agent_id
           WHERE p.provider = $1 AND p.e164 = $2"#,
        provider,
        to
    )
    .fetch_optional(pg)
    .await;

    // One decision, in one place. Filtering the switched-off ones in the query would be
    // harder to forget, but it would also make a disabled line indistinguishable from no
    // line in our own records, and "why has my line stopped answering?" is exactly the
    // question an operator needs answered.
    match found {
        Ok(Some(row)) if !row.enabled => Err(LineRefusal::Disabled),
        Ok(Some(row)) if row.agent_archived => Err(LineRefusal::AgentUnavailable),
        Ok(Some(row)) => Ok(Line {
            id: row.id,
            owner_user_id: row.owner_user_id,
            agent_id: row.agent_id,
            transfer_e164: row.transfer_e164,
            opening: notice::opening(row.greeting.as_deref(), row.notice.as_deref(), row.record_calls),
            record_calls: row.record_calls,
        }),
        Ok(None) => Err(LineRefusal::Unknown),
        Err(e) => {
            tracing::warn!(error = %e, "could not look up the line a call came in on");
            Err(LineRefusal::LookupFailed)
        }
    }
}

/// What a redeemed ticket says about the call it belongs to.
///
/// Everything a call needs is decided at the answer, when the request is known to have
/// come from the carrier, and carried here. So the media socket resolves nothing of
/// its own, and a setting changed while a call is ringing cannot produce a call that is
/// half one configuration and half another.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CallDetails {
    /// The account the call runs as.
    pub owner_user_id: Uuid,
    /// The agent that bounds what the caller can reach.
    pub agent_id: Uuid,
    /// The carrier's identifier for this call.
    pub call_sid: String,
    /// Who is calling, in full international form, or empty if withheld.
    pub from: String,
    /// The number they rang.
    pub to: String,
    /// The line they reached, as it was when the call was answered. Carried rather than
    /// looked up again, so a line rebound or switched off while a call is still ringing
    /// cannot produce a call that is half one configuration and half another.
    pub phone_number_id: Uuid,
    /// What this caller is told before anything they say is acted on, in the words they
    /// will hear. Composed at the answer from the line's greeting and its notice.
    ///
    /// Defaulted for reading, because a ticket minted by a previous version of this
    /// process is redeemed by this one during a restart. An empty opening is treated the
    /// same way a failed one is: the call is not carried.
    #[serde(default)]
    pub opening: String,
    /// Whether the sound of this call is kept, decided at the answer along with the words
    /// the caller is about to be told, so the two can never disagree.
    #[serde(default)]
    pub record_calls: bool,
}

fn ticket_key(token: &str) -> String {
    format!("pai:tel_ticket:{token}")
}

/// Mint a single-use ticket for the media socket of one call.
///
/// The carrier does not sign the socket handshake, only the request that answers the
/// call, so this is what carries that proof forward: the answer is signed, the answer
/// contains the ticket, and the socket is opened with it. Deliberately a separate
/// namespace from the browser socket's tickets — a telephone ticket must not be
/// redeemable as a signed-in user's session, and it has to carry more than a ticket
/// for a person does.
///
/// An error here fails the call rather than being ignored: a ticket that was not
/// stored is a socket that can never be opened, and answering anyway would leave the
/// caller listening to silence.
pub async fn issue_ticket(
    redis: &deadpool_redis::Pool,
    details: &CallDetails,
) -> Result<String, AppError> {
    let token = Uuid::now_v7().to_string();
    let payload = serde_json::to_string(details)
        .map_err(|e| AppError::Config(format!("call ticket: {e}")))?;
    crate::cache::kv_set_ex(redis, &ticket_key(&token), &payload, TICKET_TTL_SECS).await?;
    Ok(token)
}

/// Mint a ticket that is also the call's own identifier.
///
/// The carrier gives a call its identifier and we mint a ticket beside it. A telephone
/// system on the practice's own network does the opposite: it is told what to open the
/// connection with, so the one value is both. Minting them as one is what makes the
/// connection authenticated, since presenting the identifier is presenting the ticket.
pub async fn issue_ticket_named(
    redis: &deadpool_redis::Pool,
    details: &CallDetails,
) -> Result<String, AppError> {
    let token = Uuid::now_v7().to_string();
    let mut details = details.clone();
    details.call_sid = token.clone();
    let payload = serde_json::to_string(&details)
        .map_err(|e| AppError::Config(format!("call ticket: {e}")))?;
    crate::cache::kv_set_ex(redis, &ticket_key(&token), &payload, TICKET_TTL_SECS).await?;
    Ok(token)
}

/// Redeem a ticket, deleting it as it is read so it cannot be used twice.
pub async fn redeem_ticket(
    redis: &deadpool_redis::Pool,
    token: &str,
) -> Result<Option<CallDetails>, AppError> {
    let Some(raw) = crate::cache::kv_get_del(redis, &ticket_key(token)).await? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&raw).ok())
}

/// How a call is brought to an end from outside the socket carrying it, and why.
///
/// The reason travels with the request rather than being assumed at the far end, because
/// there are now three of them and they are not interchangeable: the network telling us
/// the call is already over, the agent putting the caller through to somebody, and the
/// agent finishing the call itself. Each leaves a different mark on the record.
///
/// Asking and waking are separate because of when a caller can be moved. The reply the
/// agent has just given is still being played out when the decision is made, so a
/// transfer that took effect immediately would cut the line the moment the words "putting
/// you through" were generated rather than heard. [`request`](Self::request) records the
/// intent; whoever knows the reply has finished calls [`end_now`](Self::end_now).
/// The decision is a latch, not a signal, and that is the whole of why this is a type.
///
/// Waking a waiter stores nothing: a notification sent while the reader is busy with a
/// frame of audio is simply lost, and the call would then sit there until the silence
/// watchdog gave up on it minutes later, recorded as having been dropped rather than as
/// having been handed over. So what is set is a flag, and the notification only shortens
/// the wait for a flag that is already true.
#[derive(Clone, Default)]
pub struct Ending {
    reason: Arc<Mutex<Option<log::CallEnd>>>,
    now: Arc<AtomicBool>,
    woken: Arc<Notify>,
}

impl Ending {
    /// Record how this call should end, without ending it yet.
    pub fn request(&self, end: log::CallEnd) {
        *self.reason.lock().unwrap() = Some(end);
    }

    /// Has an end been asked for, and what kind?
    pub fn requested(&self) -> Option<log::CallEnd> {
        *self.reason.lock().unwrap()
    }

    /// End the call now, for this reason.
    pub fn end_now(&self, end: log::CallEnd) {
        self.request(end);
        self.now.store(true, Ordering::SeqCst);
        self.woken.notify_waiters();
    }

    /// End the call now, for whatever reason was already asked for. Does nothing when
    /// nothing was asked for, so a reply finishing on an ordinary turn is not an ending.
    pub fn end_as_asked(&self) -> bool {
        if self.requested().is_none() {
            return false;
        }
        self.now.store(true, Ordering::SeqCst);
        self.woken.notify_waiters();
        true
    }

    /// Wait to be told the call is over, and be told why.
    ///
    /// The waiter is registered before the flag is read, so a decision landing between
    /// the two cannot be missed. Falls back to the network's own notice, which is the
    /// only reason that ever arrived this way before there were others.
    pub async fn ended(&self) -> log::CallEnd {
        loop {
            let waiting = self.woken.notified();
            if self.now.load(Ordering::SeqCst) {
                return self.requested().unwrap_or(log::CallEnd::CarrierEnded);
            }
            waiting.await;
            if self.now.load(Ordering::SeqCst) {
                return self.requested().unwrap_or(log::CallEnd::CarrierEnded);
            }
        }
    }
}

/// One call in progress.
pub struct CallEntry {
    pub session: Arc<crate::voice::Session>,
    /// How this call is brought to an end from outside the socket that is carrying it,
    /// which is how the network's own notice that the call is over takes effect, and how
    /// the agent puts a caller through or finishes the call.
    pub ending: Ending,
    pub started: std::time::Instant,
    pub from: String,
}

/// The calls this process is carrying, keyed by the carrier's identifier for each.
///
/// Keyed that way because it is the only name every path has: the carrier's polite
/// end-of-stream message and its separate notice that the call is over both give the
/// call sid and nothing else.
///
/// Process-local by nature, like the browser voice sessions: the socket that has to be
/// torn down is in this process or it is nowhere.
#[derive(Clone, Default)]
pub struct TelephonyCalls(Arc<Mutex<HashMap<String, CallEntry>>>);

impl TelephonyCalls {
    /// Take a slot and record the call, or refuse because the line is full.
    ///
    /// Counting and inserting under one lock is what makes the ceiling exact. It is
    /// deliberately not a shared counter in the cache: that would be a spend limit
    /// that stops working when the cache does, and the whole point of this one is that
    /// it holds when everything else is failing.
    pub fn try_insert(&self, call_sid: String, entry: CallEntry, max: usize) -> bool {
        let mut calls = self.0.lock().unwrap();
        if calls.len() >= max && !calls.contains_key(&call_sid) {
            return false;
        }
        calls.insert(call_sid, entry);
        true
    }

    pub fn remove(&self, call_sid: &str) -> Option<CallEntry> {
        self.0.lock().unwrap().remove(call_sid)
    }

    /// Ask a call to end now, without removing it: the socket carrying it does the
    /// tearing down, so that one path frees it and not two.
    pub fn hangup(&self, call_sid: &str, end: log::CallEnd) -> bool {
        match self.0.lock().unwrap().get(call_sid) {
            Some(entry) => {
                entry.ending.end_now(end);
                true
            }
            None => false,
        }
    }

    /// Say how a call should end when the reply being spoken has finished.
    ///
    /// Nothing happens at this moment: the caller is still listening to the sentence
    /// that led to this decision, and moving the line now would take it away from them
    /// mid-word.
    pub fn ask_to_end(&self, call_sid: &str, end: log::CallEnd) -> bool {
        match self.0.lock().unwrap().get(call_sid) {
            Some(entry) => {
                entry.ending.request(end);
                true
            }
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Build the live-voice session for this call, with a telephone line as its transport.
///
/// Shared by every transport, which is what makes a second one an adapter rather than a
/// second implementation: everything above the sink is identical whether the audio comes
/// from a carrier's socket or from a telephone system on the practice's own network.
pub struct StartedCall {
    pub voice: Arc<crate::voice::Session>,
    /// The concrete transport, kept alongside the session's own reference to it because
    /// the reader has to hand it what the carrier says back, and the session holds it only
    /// as something that can be spoken through.
    pub sink: Arc<TelephonySink>,
    pub out_rx: mpsc::Receiver<Outbound>,
    pub control_rx: mpsc::Receiver<Control>,
    pub pacer_task: tokio::task::JoinHandle<()>,
    /// Shared with the sink, which is what knows when a reply has been heard, and with
    /// the registry, which is how anything outside this call reaches it.
    pub ending: Ending,
    /// Where both sides of the conversation are handed, on a line that records. `None` on
    /// every other line, which is every line until somebody switches one on.
    pub recorder: Option<crate::voice::telephony::record::Recorder>,
    /// Finishes the file and says what became of it. Awaited at teardown, after every
    /// handle has been dropped.
    pub recording: Option<tokio::task::JoinHandle<crate::voice::telephony::record::Finished>>,
}

pub async fn start_session(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    details: &CallDetails,
    logged: Option<Uuid>,
    wire: Wire,
    confirms_playback: bool,
) -> Option<StartedCall> {
    let (out_tx, out_rx) = mpsc::channel::<Outbound>(OUT_QUEUE);
    let (control_tx, control_rx) = mpsc::channel::<Control>(8);
    let (pacer, pacer_task) = pace::spawn(out_tx, pace::DEFAULT_PREBUFFER);
    let ending = Ending::default();
    // The two things a transport answers differently: how the audio is written on the
    // wire, and whether the far end will ever say it has played it.
    let sink = Arc::new(TelephonySink::new(pacer, control_tx, ending.clone(), wire, confirms_playback));
    // A line that records opens its recording here, where the call's own identifier is
    // known and before a word has been said. Only where the line records, and the line
    // that records is the line whose notice says so: the two are decided together at the
    // answer and carried here together.
    let (recorder, recording) = match (details.record_calls, logged) {
        (true, Some(call_id)) => {
            match crate::voice::telephony::record::start(&state.boot.storage.recordings_dir, call_id)
                .await
            {
                Some((rec, task)) => (Some(rec), Some(task)),
                // The caller has been told the call is recorded and it cannot be. The call
                // goes ahead and says so in the record afterwards: the notice is consent to
                // being recorded rather than a promise of a file, and hanging up on
                // somebody because a disk is full serves nobody.
                None => {
                    tracing::warn!(%call_id, "a line that records could not start a recording");
                    (None, None)
                }
            }
        }
        _ => (None, None),
    };
    let voice = crate::voice::Session::start(
        state.clone(),
        ctx.clone(),
        // A synthetic identifier. This session is not on the socket registry the
        // browser clients use, which is harmless: the one thing that consults it,
        // cancelling a turn from elsewhere, simply finds nothing, and interrupting a
        // caller does not go through it.
        Uuid::now_v7(),
        sink.clone(),
        None,
        // No project and no per-turn Libraries, which is what confines a caller to the
        // agent's own knowledge: the two other ways the readable set could widen both
        // need one of those to be present.
        None,
        Some(details.agent_id),
        // There is no button to hold on a telephone, so speech has to end turns.
        Some("vad".into()),
        // Echo cancellation is the carrier's, on the interconnect, and it is genuinely
        // there. Saying otherwise here would switch off interruption altogether, and
        // being unable to interrupt is the difference between a conversation and a
        // recorded message.
        true,
        // Tuned for a line rather than a tab: quicker to notice being talked over, and
        // it does not wait as long before deciding the caller has finished.
        crate::voice::VoiceProfile::Phone,
        // A conversation held aloud belongs in its owner's history like any other, marked
        // by where it came from rather than hidden.
        crate::chat::origin::ChatOrigin::Phone,
        // The call this session is carrying, so a turn can write down what the caller
        // wanted and say which call they wanted it on. `None` when the record could not
        // be opened, in which case nothing can be attached to it and the tools say so
        // rather than filing the caller's message against no call at all.
        logged,
    )
    .await;
    Some(StartedCall { voice, sink, out_rx, control_rx, pacer_task, ending, recorder, recording })
}

/// Where a request came from, for the rate limits on refused calls.
///
/// Reuses the platform's own resolver, which prefers the connection's real address and
/// only falls back to a forwarding header. The distinction matters here: a header-only
/// answer is whatever the caller wrote in it, so a limit keyed on one is no limit at all.
pub struct SourceIp(pub String);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for SourceIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(SourceIp(crate::auth::breakglass::source_ip(parts)))
    }
}

/// Room for a whole reply's worth of frames between the pacer and the transport.
const OUT_QUEUE: usize = 256;

/// The carrier's endpoints.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/telephony/twilio/voice", post(twilio::answer))
        .route("/api/telephony/twilio/media", get(twilio::media))
        .route("/api/telephony/twilio/status", post(twilio::status))
        .route(twilio::CONTINUE_PATH, post(twilio::continue_call))
        // And the practice's own telephone system, which asks the same two questions in
        // the same order and carries the audio itself.
        .route(audiosocket::ANSWER_PATH, get(audiosocket::answer))
        .route(audiosocket::CONTINUE_PATH, get(audiosocket::continue_call))
}

/// Make the whole surface absent unless this instance has a telephone line.
///
/// Absent rather than refused, and checked in a layer rather than in each handler, for
/// two different reasons. A refusal would tell anyone who asked that this deployment
/// has telephony configured, which is worth nothing to them and something to an
/// attacker. And a layer runs before routing, before the body is read and before any
/// signature is attempted, so an instance with no line does no work at all for
/// somebody probing it.
pub async fn enabled_guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    // The caller is anonymous, so only the deployment-wide setting can be consulted.
    if !crate::features::enabled_for_user(&state, None, "telephony").await {
        return StatusCode::NOT_FOUND.into_response();
    }
    let configured = crate::config::runtime::get(&state.pg, "telephony.provider")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| state.boot.telephony.provider.clone());
    // Either way of answering a call counts. A deployment that answers through its own
    // telephone system has no carrier configured and is not thereby without a telephone.
    if !matches!(configured.as_str(), "twilio" | "audiosocket") {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // The registry's ceiling is not tested here: an entry needs a live session, and a
    // session needs a database. It is proved end to end instead, by ringing a second
    // time while the first call is up and getting a refusal, which is the behaviour
    // that actually matters rather than the arithmetic behind it.

    #[test]
    fn the_ticket_survives_a_round_trip_through_its_stored_form() {
        let details = CallDetails {
            owner_user_id: Uuid::from_bytes([1; 16]),
            agent_id: Uuid::from_bytes([2; 16]),
            call_sid: "CAtest".into(),
            from: "+447700900000".into(),
            to: "+441315550000".into(),
            phone_number_id: Uuid::from_bytes([3; 16]),
            opening: "You are speaking to an automated assistant.".into(),
            record_calls: false,
        };
        let raw = serde_json::to_string(&details).expect("serialises");
        let back: CallDetails = serde_json::from_str(&raw).expect("parses");
        assert_eq!(back.owner_user_id, details.owner_user_id);
        assert_eq!(back.agent_id, details.agent_id);
        assert_eq!(back.call_sid, "CAtest");
        assert_eq!(back.from, "+447700900000");
        assert_eq!(back.to, "+441315550000");
        assert_eq!(back.phone_number_id, details.phone_number_id, "the line the call reached");
    }

    #[test]
    fn a_ticket_is_looked_up_under_its_own_namespace() {
        // Not the browser socket's namespace: a ticket for a telephone must never be
        // redeemable as a signed-in person's session.
        assert!(ticket_key("abc").starts_with("pai:tel_ticket:"));
        assert_ne!(ticket_key("abc"), "pai:ws_ticket:abc");
    }

    /// However somebody writes a number down, it reaches the column in one form. This is
    /// what lets the answer path compare exactly, with no transformation of its own.
    #[test]
    fn a_number_reaches_the_column_in_one_form() {
        for written in [
            "+441315550000",
            " +44 131 555 0000 ",
            "+44-131-555-0000",
            "+44 (131) 555.0000",
            "00441315550000",
            "0044 131 555 0000",
        ] {
            assert_eq!(
                normalise_e164(written).as_deref(),
                Some("+441315550000"),
                "{written:?} is the same number"
            );
        }
    }

    /// Anything that is not a telephone number in full international form is refused,
    /// rather than stored in a shape the answer path could never match.
    #[test]
    fn something_that_is_not_a_number_is_refused() {
        for junk in [
            "",
            "0131 555 0000",   // no country code, so we cannot know which country
            "+0131555000",     // a country code cannot begin with nought
            "441315550000",    // no plus, and not a double nought either
            "+44131",          // too short to be anybody
            "+4413155500001234567", // too long to be anybody
            "+44131555000a",
            "hello",
            "+",
        ] {
            assert_eq!(normalise_e164(junk), None, "{junk:?} was accepted");
        }
    }

    /// A refusal reason is for our own records. Every one of them looks the same to the
    /// caller, which is the point: the numbers a deployment answers must not be
    /// discoverable by dialling them.
    #[test]
    fn every_refusal_has_its_own_reason() {
        let reasons: Vec<&str> = [
            LineRefusal::Unknown,
            LineRefusal::Disabled,
            LineRefusal::AgentUnavailable,
            LineRefusal::LookupFailed,
        ]
        .iter()
        .map(|r| r.as_str())
        .collect();
        let mut sorted = reasons.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), reasons.len(), "two refusals share a reason");
    }

    #[test]
    fn only_something_that_can_answer_a_call_answers() {
        let mut cfg = TelephonyResolved {
            provider: "none".into(),
            public_base_url: String::new(),
            max_concurrent_calls: 2,
            auth_token: None,
            audiosocket_listen: String::new(),
            audiosocket_key: None,
        };
        assert!(!cfg.has_provider(), "a line with nothing to answer it must stay dormant");
        cfg.provider = "twilio".into();
        assert!(cfg.has_provider());
        assert!(!cfg.is_audiosocket());
        // A deployment answering through its own telephone system has no carrier, and is
        // not thereby a deployment without a telephone.
        cfg.provider = "audiosocket".into();
        assert!(cfg.has_provider());
        assert!(cfg.is_audiosocket());
        // And anything else is still nothing: an unrecognised value is not permission.
        cfg.provider = "some-other-thing".into();
        assert!(!cfg.has_provider());
    }
}
