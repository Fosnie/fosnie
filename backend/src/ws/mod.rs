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

//! WebSocket transport — one multiplexed socket per user.
//!
//! Auth on upgrade: a single-use `?ticket=<t>` minted over the authenticated
//! `POST /api/ws-ticket` path (the browser's route — keeps the JWT out of the
//! URL), a valid Keycloak JWT (`Authorization: Bearer` header or `?token=<jwt>`,
//! validated by the Pass-mode layer — for programmatic clients), **or** a
//! `?resume=<token>` within the resume window.
//! The same validation gates the upgrade, so revocation applies to WS too.
//! Carries the chat-token stream + cancel + presence; team-messaging replay is
//! a later slice.

pub mod hub;
pub mod protocol;
pub mod session;

use std::sync::Arc;

use base64::Engine as _;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{FromRequestParts, State, WebSocketUpgrade};
use axum::http::request::Parts;
use axum::response::Response;
use axum_keycloak_auth::KeycloakAuthStatus;
use crate::auth::keycloak::KcStatus;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Notify};
use uuid::Uuid;

use crate::auth::{self, AuthContext};
use crate::error::AppError;
use crate::state::AppState;
use protocol::{ClientFrame, ServerFrame};

/// Resolves the WS caller: validated JWT (token extension from the Pass-mode
/// layer) or a valid resume token in the query string.
pub struct WsAuth {
    pub ctx: AuthContext,
    /// True when the socket was authorised via a `?resume=` token (reconnect),
    /// so the handler replays buffered frames after `Hello`.
    pub resumed: bool,
}

impl FromRequestParts<AppState> for WsAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // Anti cross-site WebSocket hijacking: a browser always sends `Origin` on
        // the upgrade — reject it unless it is in the allow-list. A request with no
        // Origin is a non-browser client (CLI/tests), which is not a CSWSH vector,
        // so it is allowed. The token/ticket auth below still gates every socket.
        if let Some(origin) = parts
            .headers
            .get(axum::http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
        {
            if !origin_allowed(&state.boot.server, origin) {
                metrics::counter!("ws_origin_rejected_total").increment(1);
                return Err(AppError::Forbidden("origin not allowed".into()));
            }
        }

        // Pass-mode layer stores a KeycloakAuthStatus (not a bare token). Keep the
        // Keycloak-specific transport detection here (the middleware/transport
        // layer stays concrete in 2a — see auth/keycloak.rs), but route the
        // token → AuthContext identity step through the AuthProvider slot so an
        // override is honoured on WS too. Only enter this branch when a token is
        // actually present, preserving the resume/ticket fall-through and the
        // deactivated-account error propagation below byte-for-byte.
        let token_present = matches!(
            parts.extensions.get::<KcStatus>(),
            Some(KeycloakAuthStatus::Success(_))
        );
        if token_present {
            let ctx = state.auth.authenticate(parts, state).await?;
            return Ok(WsAuth { ctx, resumed: false });
        }
        if let Some(token) = parts.uri.query().and_then(|q| query_param(q, "resume")) {
            if let Some(user_id) = session::lookup_resume(&state.redis, &token).await? {
                let ctx = auth::load_context(&state.pg, user_id).await?;
                return Ok(WsAuth { ctx, resumed: true });
            }
        }
        // Single-use connect ticket (minted over the authenticated HTTP path) —
        // keeps the JWT out of the socket URL. load_context re-checks deactivation.
        if let Some(ticket) = parts.uri.query().and_then(|q| query_param(q, "ticket")) {
            if let Some(user_id) = session::redeem_ticket(&state.redis, &ticket).await? {
                let ctx = auth::load_context(&state.pg, user_id).await?;
                return Ok(WsAuth { ctx, resumed: false });
            }
        }
        Err(AppError::Unauthorized(
            "websocket requires a valid token, ticket, or resume".into(),
        ))
    }
}

/// True if `origin` (a browser `Origin` header value) may open the socket. The
/// allow-list is `server.allowed_ws_origins` when set, otherwise the single
/// origin of `server.public_url`. Exact match on scheme://host[:port].
fn origin_allowed(server: &crate::config::ServerConfig, origin: &str) -> bool {
    let origin = origin.trim();
    if !server.allowed_ws_origins.is_empty() {
        return server.allowed_ws_origins.iter().any(|o| o.trim() == origin);
    }
    origin_of(&server.public_url).is_some_and(|o| o == origin)
}

/// Extract the origin (scheme://host[:port]) from a URL, dropping path/query.
/// `None` if it is not an absolute URL.
pub(crate) fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.is_empty() {
        return None;
    }
    Some(format!("{}://{authority}", scheme.to_ascii_lowercase()))
}

pub async fn ws_handler(
    State(state): State<AppState>,
    auth: WsAuth,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(state, socket, auth.ctx, auth.resumed))
}

/// Frames worth retaining for replay — discrete events, not the high-volume
/// token stream (a reconnecting client refetches the persisted message).
fn is_replayable(frame: &ServerFrame) -> bool {
    !matches!(
        frame,
        ServerFrame::ChatToken { .. }
            // Reasoning-trace deltas — high-volume like ChatToken; the trace is
            // durable (folded into the message content) and refetched on reconnect.
            | ServerFrame::ChatReasoning { .. }
            // Streaming background-message tokens — same rationale as ChatToken:
            // high-volume, content durable via the DB row, refetched on reconnect.
            | ServerFrame::ChatMessageToken { .. }
            | ServerFrame::Pong
            | ServerFrame::Hello { .. }
            // Team messages are durable (Postgres + per-chat sequence); a
            // reconnecting client catches up via GET …/messages?since=<seq>,
            // so they are not buffered in the ephemeral per-user replay list.
            | ServerFrame::GroupMessage { .. }
            // Voice replies are responses to transient requests (and the audio
            // payload is large) — never buffer them for replay.
            | ServerFrame::VoiceTranscript { .. }
            | ServerFrame::VoiceAudio { .. }
            // Live-voice frames are ephemeral (at-most-once, like token streaming):
            // partials/finals/state/audio chunks are worthless to replay, and the
            // persisted transcript already rides the replayable chat frames.
            | ServerFrame::VoiceLiveState { .. }
            | ServerFrame::VoicePartial { .. }
            | ServerFrame::VoiceFinal { .. }
            | ServerFrame::VoiceTtsChunk { .. }
            | ServerFrame::VoiceTtsEnd
            | ServerFrame::VoiceError { .. }
    )
}

async fn handle_socket(state: AppState, socket: WebSocket, ctx: AuthContext, resumed: bool) {
    // Break-glass principals have no user identity and cannot own/drive chats.
    let Some(user_id) = ctx.user_id else {
        return;
    };
    let socket_id = Uuid::now_v7();
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(256);

    state.hub.register(socket_id, user_id, tx.clone());
    let _ = session::register_socket(&state.redis, socket_id, user_id).await;
    let resume_token = session::issue_resume(&state.redis, user_id)
        .await
        .unwrap_or_default();

    // Send Hello directly on the sink, then (if this is a resume) replay the
    // buffered frames the previous socket may not have delivered.
    let hello = ServerFrame::Hello { socket_id, user_id, resume_token }.to_json();
    let _ = sink.send(Message::Text(hello.into())).await;
    if resumed {
        if let Ok(frames) = session::replay_frames(&state.redis, user_id).await {
            for j in frames {
                let _ = sink.send(Message::Text(j.into())).await;
            }
        }
    }

    // Writer: drain outbound frames to the socket, buffering replayable ones. A
    // periodic WS Ping keeps the connection alive through long silent gaps (a slow
    // 27B prefill, a quiet retrieve) so a proxy in front (the Cloudflare tunnel)
    // doesn't idle-time out the socket mid-turn — that detaches the live stream and
    // the user has to reload to see an answer that already landed in the DB.
    let redis = state.redis.clone();
    let writer = tokio::spawn(async move {
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(25));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                frame = rx.recv() => {
                    let Some(frame) = frame else { break };
                    let json = frame.to_json();
                    if is_replayable(&frame) {
                        let _ = session::buffer_frame(&redis, user_id, &json).await;
                    }
                    if sink.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                _ = heartbeat.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Reader: handle inbound frames.
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => match serde_json::from_str::<ClientFrame>(text.as_str()) {
                Ok(ClientFrame::ChatSend { chat_id, content, project_id, agent_id, attachment_ids, thinking, reasoning, llm_provider_id }) => {
                    // Coarse per-user abuse guard on the expensive turn path.
                    if !crate::cache::rate_limit_ok(&state.redis, &format!("chat:{user_id}"), 30, 60).await {
                        let _ = tx
                            .send(ServerFrame::ChatError { turn_id: None, message: "You're sending messages too quickly — please slow down.".into() })
                            .await;
                        continue;
                    }
                    let turn_id = Uuid::now_v7();
                    let cancel = Arc::new(Notify::new());
                    state.hub.add_turn(socket_id, turn_id, cancel.clone());

                    let st = state.clone();
                    let txc = tx.clone();
                    let ctxc = ctx.clone();
                    // Prefer the structured spec; fall back to the legacy `thinking` string.
                    let reasoning = reasoning.or_else(|| crate::reasoning::ReasoningSpec::from_legacy(thinking.as_deref()));
                    tokio::spawn(async move {
                        let attachments =
                            crate::http::chat_attachments::take_attachments(&st, &ctxc, &attachment_ids).await;
                        crate::chat::run_turn(
                            &st, &ctxc, turn_id, chat_id, project_id, agent_id, content, attachments,
                            Vec::new(), false, None, reasoning, llm_provider_id, None, &txc, cancel,
                        )
                        .await;
                        st.hub.remove_turn(socket_id, turn_id);
                    });
                }
                Ok(ClientFrame::ChatRegenerate { chat_id, from_message_id, content }) => {
                    // In-place regenerate / edit / restart-from-here. Reuses the
                    // anchoring user row (never re-inserts it) and drops the stale
                    // answer + anything after — no prompt duplication.
                    if !crate::cache::rate_limit_ok(&state.redis, &format!("chat:{user_id}"), 30, 60).await {
                        let _ = tx
                            .send(ServerFrame::ChatError { turn_id: None, message: "You're sending messages too quickly — please slow down.".into() })
                            .await;
                        continue;
                    }
                    let turn_id = Uuid::now_v7();
                    let cancel = Arc::new(Notify::new());
                    state.hub.add_turn(socket_id, turn_id, cancel.clone());

                    let st = state.clone();
                    let txc = tx.clone();
                    let ctxc = ctx.clone();
                    tokio::spawn(async move {
                        match crate::chat::regenerate_prepare(&st, &ctxc, chat_id, from_message_id, content).await {
                            Ok((anchor_id, anchor_content)) => {
                                crate::chat::run_turn(
                                    &st, &ctxc, turn_id, Some(chat_id), None, None, anchor_content,
                                    Vec::new(), Vec::new(), false, Some(anchor_id), None, None, None, &txc, cancel,
                                )
                                .await;
                            }
                            Err(e) => {
                                let _ = txc
                                    .send(ServerFrame::ChatError { turn_id: Some(turn_id), message: e.to_string() })
                                    .await;
                            }
                        }
                        st.hub.remove_turn(socket_id, turn_id);
                    });
                }
                Ok(ClientFrame::ChatCancel { turn_id }) => {
                    state.hub.cancel_turn(socket_id, turn_id);
                }
                Ok(ClientFrame::Auth { token: _ }) => {
                    // In-band session refresh: extend presence + resume window
                    // and hand back a fresh resume token. (Cryptographic
                    // re-validation of the supplied token is deferred — the
                    // upgrade-time auth stands; this keeps a long-lived socket's
                    // session alive past the original token's expiry.)
                    let _ = session::refresh_presence(&state.redis, user_id).await;
                    let resume_token = session::issue_resume(&state.redis, user_id)
                        .await
                        .unwrap_or_default();
                    let _ = tx.send(ServerFrame::Hello { socket_id, user_id, resume_token }).await;
                }
                Ok(ClientFrame::GroupSend { chat_id, content, mentions }) => {
                    // Reliable team-chat send on the multiplexed socket; fan-out
                    // (incl. the echo to this socket) happens inside send_via_ws.
                    if let Err(e) =
                        crate::http::messaging::send_via_ws(&state, &ctx, chat_id, &content, mentions).await
                    {
                        let _ = tx
                            .send(ServerFrame::ChatError { turn_id: None, message: e.to_string() })
                            .await;
                    }
                }
                Ok(ClientFrame::VoiceTranscribe { audio_base64, mime, chat_id: _ }) => {
                    // Dictation: decode → transcribe → reply with the text. Runs
                    // off-thread so the socket reader keeps draining.
                    let st = state.clone();
                    let txc = tx.clone();
                    tokio::spawn(async move {
                        let frame = if !st.features.enabled_for_user(&st, Some(user_id), "voice").await {
                            ServerFrame::ChatError { turn_id: None, message: "voice is not enabled".into() }
                        } else {
                            match base64::engine::general_purpose::STANDARD.decode(audio_base64.as_bytes()) {
                                Err(e) => ServerFrame::ChatError { turn_id: None, message: format!("bad audio: {e}") },
                                Ok(bytes) => match crate::ml::transcribe(&st.http, &st.boot.ml.base_url, &bytes, &mime, crate::ml::provider_overrides(&st, Some(user_id)).await).await {
                                    Ok(text) => ServerFrame::VoiceTranscript { text },
                                    Err(e) => ServerFrame::ChatError { turn_id: None, message: e.to_string() },
                                },
                            }
                        };
                        let _ = txc.send(frame).await;
                    });
                }
                Ok(ClientFrame::VoiceSpeak { text, voice }) => {
                    let st = state.clone();
                    let txc = tx.clone();
                    tokio::spawn(async move {
                        let frame = if !st.features.enabled_for_user(&st, Some(user_id), "voice").await {
                            ServerFrame::ChatError { turn_id: None, message: "voice is not enabled".into() }
                        } else {
                            match crate::ml::synthesize(&st.http, &st.boot.ml.base_url, &text, voice.as_deref(), crate::ml::provider_overrides(&st, Some(user_id)).await).await {
                                Ok((bytes, mime)) => ServerFrame::VoiceAudio {
                                    audio_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
                                    mime,
                                },
                                Err(e) => ServerFrame::ChatError { turn_id: None, message: e.to_string() },
                            }
                        };
                        let _ = txc.send(frame).await;
                    });
                }
                // ── Live / streaming voice. A session
                // spans many frames, so it lives in `state.voice` keyed by socket.
                Ok(ClientFrame::VoiceStreamStart { chat_id, project_id, agent_id, mode, aec }) => {
                    // Built inline (not spawned) so the session exists before the
                    // next frame (an audio chunk) is read from this socket.
                    if !state.features.enabled_for_user(&state, Some(user_id), "voice_live").await {
                        let _ = tx
                            .send(ServerFrame::VoiceError { message: "live voice is not enabled".into() })
                            .await;
                    } else if !crate::cache::rate_limit_ok(&state.redis, &format!("voicelive:{user_id}"), 10, 60).await {
                        let _ = tx
                            .send(ServerFrame::VoiceError { message: "Starting voice sessions too quickly — please wait a moment.".into() })
                            .await;
                    } else {
                        if let Some(old) = state.voice.remove(socket_id) {
                            old.shutdown().await; // one live session per socket
                        }
                        let s = crate::voice::Session::start(
                            state.clone(), ctx.clone(), socket_id, tx.clone(), chat_id, project_id,
                            agent_id, mode, aec,
                        )
                        .await;
                        state.voice.insert(socket_id, s);
                    }
                }
                Ok(ClientFrame::VoiceAudioChunk { audio_base64, seq }) => {
                    if let Some(s) = state.voice.get(socket_id) {
                        s.on_audio_chunk(audio_base64, seq).await;
                    } else if let Some(d) = state.dictation.get(socket_id) {
                        d.on_audio_chunk(audio_base64).await;
                    }
                }
                Ok(ClientFrame::VoiceDictateStart) => {
                    // Built inline (not spawned) so the session exists before the next
                    // audio chunk is read from this socket.
                    if !state.features.enabled_for_user(&state, Some(user_id), "voice").await {
                        let _ = tx
                            .send(ServerFrame::VoiceError { message: "voice is not enabled".into() })
                            .await;
                    } else if !crate::cache::rate_limit_ok(&state.redis, &format!("dictate:{user_id}"), 20, 60).await {
                        let _ = tx
                            .send(ServerFrame::VoiceError { message: "Starting dictation too quickly — please wait a moment.".into() })
                            .await;
                    } else {
                        if let Some(old) = state.dictation.remove(socket_id) {
                            old.shutdown();
                        }
                        let d = crate::voice::DictationSession::start(state.clone(), ctx.clone(), tx.clone()).await;
                        state.dictation.insert(socket_id, d);
                    }
                }
                Ok(ClientFrame::VoiceDictateStop) => {
                    if let Some(d) = state.dictation.remove(socket_id) {
                        d.stop().await; // commit + flush the final transcript, then close
                    }
                }
                Ok(ClientFrame::VoiceBargeIn) => {
                    if let Some(s) = state.voice.get(socket_id) {
                        s.barge_in().await;
                    }
                }
                Ok(ClientFrame::VoiceStreamEnd) => {
                    if let Some(s) = state.voice.remove(socket_id) {
                        s.shutdown().await;
                    }
                }
                Ok(ClientFrame::Ping) => {
                    let _ = session::refresh_presence(&state.redis, user_id).await;
                    let _ = tx.send(ServerFrame::Pong).await;
                }
                Err(e) => {
                    let _ = tx
                        .send(ServerFrame::ChatError {
                            turn_id: None,
                            message: format!("bad frame: {e}"),
                        })
                        .await;
                }
            },
            Message::Ping(_) => {
                let _ = session::refresh_presence(&state.redis, user_id).await;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    // Disconnect: deregister the socket but DO NOT cancel in-flight turns — let them
    // finish and keep persisting, so a reload / return resumes the answer from the DB
    // row. (Explicit `chat.cancel` still cancels; the turn self-removes when done.)
    // A live-voice session is detached the same way: abort its audio tasks but let
    // the underlying chat turn finish persisting (run_turn tolerates the dropped tap).
    if let Some(s) = state.voice.remove(socket_id) {
        s.detach();
    }
    if let Some(d) = state.dictation.remove(socket_id) {
        d.shutdown(); // STT-only, nothing to persist — just close the engine
    }
    let _ = state.hub.deregister(socket_id);
    let _ = session::deregister_socket(&state.redis, socket_id, user_id).await;
    writer.abort();
}

/// Minimal query-string lookup (resume tokens are UUIDs — no decoding needed).
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::{origin_allowed, origin_of};
    use crate::config::ServerConfig;

    fn server(public_url: &str, allowed: &[&str]) -> ServerConfig {
        ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
            static_dir: String::new(),
            public_url: public_url.into(),
            allowed_ws_origins: allowed.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn origin_of_strips_path_and_lowercases_scheme() {
        assert_eq!(origin_of("https://app.example.com:8443/login"), Some("https://app.example.com:8443".into()));
        assert_eq!(origin_of("HTTP://localhost:8088"), Some("http://localhost:8088".into()));
        assert_eq!(origin_of("not-a-url"), None);
        assert_eq!(origin_of("https://"), None);
    }

    #[test]
    fn allow_list_falls_back_to_public_url_origin() {
        let s = server("http://localhost:8088/", &[]);
        assert!(origin_allowed(&s, "http://localhost:8088"));
        assert!(!origin_allowed(&s, "http://evil.example.com"));
        // A different port is a different origin.
        assert!(!origin_allowed(&s, "http://localhost:9999"));
    }

    #[test]
    fn explicit_allow_list_takes_precedence() {
        let s = server("http://localhost:8088", &["https://a.example.com", "https://b.example.com"]);
        assert!(origin_allowed(&s, "https://a.example.com"));
        assert!(origin_allowed(&s, "https://b.example.com"));
        // public_url is NOT auto-allowed once an explicit list is set.
        assert!(!origin_allowed(&s, "http://localhost:8088"));
    }
}
