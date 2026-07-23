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

//! The socket, held outside the web view.
//!
//! A chat answer arrives as a long stream of small frames over a connection that
//! has to survive minutes of silence, a laptop lid, and a change of network. Web
//! views are not reliable at that: the Windows one drops long-lived connections
//! without saying so, and the macOS one times an idle socket out after about a
//! minute. Either failure looks to the user like an answer that stopped
//! half-way.
//!
//! So the connection lives here, in the client's own process, with the reconnect
//! and resume logic beside it. The web view neither opens nor holds a socket: it
//! receives frames as events and sends them back through one command. What
//! crosses that boundary is the instance's own JSON, untouched, so the
//! application parses exactly what it would have parsed in a browser.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::Result;
use fosnie_protocol::{ClientFrame, ServerFrame};
use futures_util::{SinkExt, StreamExt};
use tauri::{AppHandle, Emitter, Manager};
use tauri::async_runtime::JoinHandle;
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::instance;
use crate::store::Pairing;

/// The channel every server frame arrives on, carrying the instance's JSON as it
/// came off the socket. One channel rather than one per frame type: the
/// application already has a parser, and a single ordered stream is what a token
/// sequence needs.
pub const EVENT_FRAME: &str = "ws:frame";
/// Connection state for the application's own indicator.
pub const EVENT_STATUS: &str = "ws:status";
/// The instance has stopped accepting this device's token. The pairing is over.
pub const EVENT_UNPAIRED: &str = "shell:unpaired";

/// The origin this client presents on the socket. Instances admit it explicitly
/// (a desktop client is not a browsing context, so it is not a cross-site
/// hijacking risk), and it is one of the addresses a web view serves the
/// application from.
const ORIGIN: &str = "tauri://localhost";

/// Reconnect backoff: doubles to a ceiling, with jitter so that a fleet of
/// clients coming back from the same network outage does not arrive together.
const BACKOFF_BASE_MS: u64 = 500;
const BACKOFF_CEILING_MS: u64 = 30_000;

/// How often the client checks that it is still a trusted device.
const TRUST_POLL: Duration = Duration::from_secs(60);

/// How many outgoing frames may wait for the socket.
///
/// Bounded on purpose. The window can produce frames far faster than a stalled
/// connection drains them — captured audio arrives dozens of times a second —
/// and an unbounded queue would answer a wedged socket by growing until the
/// machine complained. Full means the send waits, which the window sees as a
/// call that has not come back yet: the truth, rather than a cheerful acceptance
/// of frames nobody is sending anywhere.
const OUTBOUND_CAPACITY: usize = 256;

/// The single connection and the handle to whatever is running it.
#[derive(Default)]
pub struct Bridge {
    inner: Mutex<Option<Running>>,
}

struct Running {
    outbound: mpsc::Sender<String>,
    task: JoinHandle<()>,
}

impl Bridge {
    /// Connect (or reconnect from scratch) for this pairing. Any connection
    /// already running is dropped first, so a re-pair never leaves two sockets
    /// racing to deliver the same user's frames.
    pub async fn start(
        &self,
        app: AppHandle,
        http: reqwest::Client,
        pairing: Pairing,
    ) -> Result<()> {
        self.stop().await;
        let (tx, rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let task = tauri::async_runtime::spawn(run(app, http, pairing, rx));
        *self.inner.lock().await = Some(Running { outbound: tx, task });
        Ok(())
    }

    pub async fn stop(&self) {
        if let Some(running) = self.inner.lock().await.take() {
            running.task.abort();
        }
    }

    /// Hand a frame the application composed to the socket. False when there is
    /// no connection to hand it to. Waits when the queue is full, which is a
    /// socket that has stopped draining rather than a reason to discard a frame
    /// the user meant to send.
    pub async fn send(&self, frame: String) -> bool {
        let outbound = match self.inner.lock().await.as_ref() {
            Some(running) => running.outbound.clone(),
            None => return false,
        };
        outbound.send(frame).await.is_ok()
    }
}

/// A task that ends with whatever spawned it. Aborting the connection task drops
/// its locals, and this makes that drop stop the work it started.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn emit_status(app: &AppHandle, status: &str) {
    let _ = app.emit(EVENT_STATUS, status);
}

/// Cheap jittered backoff. No random number generator is pulled in for this: the
/// low bits of the clock spread reconnects far enough apart to stop a thundering
/// herd, which is all the jitter is for.
fn backoff(attempt: u32) -> Duration {
    let base = BACKOFF_BASE_MS.saturating_mul(1 << attempt.min(6)).min(BACKOFF_CEILING_MS);
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_millis() as u64 % (base / 2 + 1))
        .unwrap_or(0);
    Duration::from_millis(base / 2 + jitter)
}

/// The connection's whole life: connect, stream, lose it, come back.
async fn run(
    app: AppHandle,
    http: reqwest::Client,
    pairing: Pairing,
    mut outbound: mpsc::Receiver<String>,
) {
    let attempts = Arc::new(AtomicU32::new(0));
    // Held across reconnects: within its window the server restores the socket's
    // state instead of starting a fresh one, so a turn survives a blip.
    let mut resume: Option<String> = None;

    // Tied to this connection's life. A watcher that outlived its pairing would
    // keep asking about a token that has been replaced, and answer for the
    // pairing that replaced it.
    let _trust = AbortOnDrop(tauri::async_runtime::spawn(trust_watch(
        app.clone(),
        http.clone(),
        pairing.clone(),
    )));

    loop {
        emit_status(&app, "connecting");
        match connect(&http, &pairing, resume.take()).await {
            Ok(Some(stream)) => {
                attempts.store(0, Ordering::Relaxed);
                emit_status(&app, "open");
                resume = pump(&app, &http, &pairing, stream, &mut outbound).await;
                emit_status(&app, "closed");
            }
            Ok(None) => {
                // The token was refused. Nothing to retry: this device is no
                // longer trusted and the application has to say so.
                unpair(&app).await;
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not open the socket");
                emit_status(&app, "closed");
            }
        }
        let attempt = attempts.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(backoff(attempt)).await;
    }
}

type Socket = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Open the socket. `Ok(None)` means the instance refused this device's token.
async fn connect(
    http: &reqwest::Client,
    pairing: &Pairing,
    resume: Option<String>,
) -> Result<Option<Socket>> {
    let query = match &resume {
        Some(token) => format!("resume={}", urlencode(token)),
        None => match instance::ws_ticket(http, &pairing.base_url, &pairing.token).await? {
            Some(ticket) => format!("ticket={}", urlencode(&ticket)),
            None => return Ok(None),
        },
    };

    let mut request = instance::ws_url(&pairing.base_url, &query).into_client_request()?;
    // Instances allow-list the origins a socket may be opened from. This client
    // is not a browser and sets its own; it is admitted by name.
    request.headers_mut().insert("Origin", HeaderValue::from_static(ORIGIN));
    let (stream, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(Some(stream))
}

/// Percent-encode the few characters a ticket or resume token could carry. They
/// are opaque server-issued strings, not user input, so this stays small and
/// obvious rather than pulling in a query-string builder.
fn urlencode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Run one connection until it closes, returning the resume token to come back
/// with (when the server issued one).
async fn pump(
    app: &AppHandle,
    http: &reqwest::Client,
    pairing: &Pairing,
    stream: Socket,
    outbound: &mut mpsc::Receiver<String>,
) -> Option<String> {
    let (mut sink, mut source) = stream.split();
    let mut resume: Option<String> = None;

    // Say who is calling. Advisory — the server records it — and it is what makes
    // a chat started here show as having come from a desktop.
    let hello = ClientFrame::ClientHello {
        client_kind: Some("desktop".into()),
        client_version: Some(env!("CARGO_PKG_VERSION").into()),
        capabilities: vec![],
    };
    if sink.send(Message::Text(hello.to_json())).await.is_err() {
        return None;
    }

    loop {
        tokio::select! {
            outgoing = outbound.recv() => {
                match outgoing {
                    // The application already serialised this; it goes out as it
                    // arrived, so the client cannot reshape what the user sent.
                    Some(frame) => {
                        if sink.send(Message::Text(frame)).await.is_err() {
                            return resume;
                        }
                    }
                    None => return resume,
                }
            }
            incoming = source.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(token) = resume_token(&text) {
                            resume = Some(token);
                        }
                        // Deciding whether a frame is worth a notification can
                        // involve asking the instance for a chat's name, and
                        // nothing about a stream of tokens should ever wait on
                        // an HTTP call. It happens beside the socket, not in it.
                        let considering = (app.clone(), http.clone(), pairing.clone(), text.clone());
                        tauri::async_runtime::spawn(async move {
                            let (app, http, pairing, frame) = considering;
                            crate::notify::consider(&app, &http, &pairing, &frame).await;
                        });
                        let _ = app.emit(EVENT_FRAME, text);
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if sink.send(Message::Pong(payload)).await.is_err() {
                            return resume;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::debug!(error = %e, "socket ended");
                        return resume;
                    }
                    None => return resume,
                }
            }
        }
    }
}

/// The resume token from a `hello` frame, if this is one. Parsed through the
/// shared frame types, so a change to the handshake on the server is a change
/// here too.
fn resume_token(text: &str) -> Option<String> {
    match serde_json::from_str::<ServerFrame>(text).ok()? {
        ServerFrame::Hello { resume_token, .. } => Some(resume_token),
        _ => None,
    }
}

/// Watch, quietly and on a timer, whether this device is still trusted.
///
/// Signing a machine out is done from the web, and the machine in question may
/// be sitting on a socket that was authorised before that happened. Without this
/// it would keep looking connected until something forced it to reconnect.
async fn trust_watch(app: AppHandle, http: reqwest::Client, pairing: Pairing) {
    loop {
        tokio::time::sleep(TRUST_POLL).await;
        let trust = instance::check_trust(&http, &pairing.base_url, &pairing.token).await;
        tracing::debug!(?trust, "checked whether this device is still trusted");
        if trust == instance::Trust::Revoked {
            tracing::info!("this device has been signed out of the instance");
            unpair(&app).await;
            return;
        }
    }
}

/// Forget the pairing and tell the application, which returns to its pairing
/// screen. Called when the instance has refused the token: keeping a credential
/// that no longer works helps nobody.
async fn unpair(app: &AppHandle) {
    tracing::info!("clearing the pairing and returning the window to its pairing screen");
    if let Err(e) = crate::store::forget() {
        tracing::warn!(error = %e, "could not clear the pairing");
    }
    // Also from what is held in memory: the credential store is what survives a
    // restart, but this process would otherwise hand the withdrawn token to the
    // window at its next request for it.
    if let Some(shell) = app.try_state::<crate::state::Shell>() {
        shell.clear_pairing().await;
    }
    let _ = app.emit(EVENT_UNPAIRED, ());
    emit_status(app, "closed");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_stops_growing() {
        assert!(backoff(0) <= Duration::from_millis(BACKOFF_BASE_MS));
        assert!(backoff(3) > backoff(0));
        assert!(backoff(20) <= Duration::from_millis(BACKOFF_CEILING_MS));
    }

    #[test]
    fn the_resume_token_is_read_from_a_hello_and_nothing_else() {
        let hello = ServerFrame::Hello {
            socket_id: uuid::Uuid::nil(),
            user_id: uuid::Uuid::nil(),
            resume_token: "abc".into(),
            server_version: "0.3.0".into(),
            features: vec![],
        };
        assert_eq!(resume_token(&hello.to_json()).as_deref(), Some("abc"));
        let token = ServerFrame::ChatToken { turn_id: uuid::Uuid::nil(), delta: "hi".into() };
        assert_eq!(resume_token(&token.to_json()), None);
        assert_eq!(resume_token("not json"), None);
    }

    #[test]
    fn ticket_values_are_escaped_into_the_query() {
        assert_eq!(urlencode("plain-token_1.0~"), "plain-token_1.0~");
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
    }
}
