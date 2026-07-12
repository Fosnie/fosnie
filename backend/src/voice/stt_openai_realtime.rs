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

//! OpenAI Realtime streaming-STT adapter. Speaks the OpenAI
//! Realtime WebSocket (GA `intent=transcription`) but presents the same
//! [`SttStream`](super::stt_stream::SttStream) contract as the local sherpa engine, so
//! the orchestrator is unchanged except for calling `commit()` at end-of-utterance.
//!
//! Differences from the local engine, handled here so they stay invisible upstream:
//! - **24 kHz mono** in: the platform captures PCM16 at `stt_sample_rate` (16 kHz),
//!   so each chunk is linearly upsampled ×1.5 before `input_audio_buffer.append`.
//! - **Manual commit**: `turn_detection:null` means the server won't segment; our
//!   Smart-Turn drives `input_audio_buffer.commit`, after which the transcript lands.
//! - **Reconnect**: on a dropped socket we reconnect with exponential backoff+jitter
//!   and re-send `session.update` before any audio; auth/quota errors are fatal
//!   (surface + stop, the orchestrator then degrades to the batch fallback).

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::time::interval;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{header, HeaderValue};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use crate::error::AppError;

use super::stt_stream::{SttEvent, SttStream};

const URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";
const TARGET_RATE: u32 = 24_000;
const PING_SECS: u64 = 18;
const BACKOFF_START_MS: u64 = 500;
const BACKOFF_CAP_MS: u64 = 30_000;
/// Cap the WS handshake so a stalled connect can never freeze the capture loop
/// (which awaits `open()` before draining PCM) — on elapse we degrade to batch.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Connection parameters, kept so the driver can re-`session.update` on reconnect.
#[derive(Clone)]
struct Cfg {
    api_key: String,
    model: String,
    language: String,
    src_rate: u32,
    /// `true` ⇒ let OpenAI's server VAD auto-segment and stream deltas continuously
    /// (dictation: live text-while-speaking). `false` ⇒ `turn_detection:null`, our
    /// orchestrator drives `commit` (live voice, Smart-Turn).
    vad: bool,
}

/// Open an OpenAI Realtime transcription session. Errors only on the *initial*
/// connect/handshake (so the orchestrator falls back to batch, exactly like the
/// local engine); once open, transient failures self-reconnect and fatal ones
/// arrive as [`SttEvent::Error`] then end the stream.
pub async fn open(
    api_key: &str,
    model: &str,
    language: &str,
    src_rate: u32,
    vad: bool,
) -> Result<SttStream, AppError> {
    let cfg = Cfg {
        api_key: api_key.to_string(),
        model: if model.is_empty() { "gpt-realtime-whisper".into() } else { model.to_string() },
        language: if language.is_empty() { "en".into() } else { language.to_string() },
        src_rate: src_rate.max(8_000),
        vad,
    };
    // Initial connect surfaces a bad endpoint/key up front → batch fallback.
    let ws = connect(&cfg).await?;

    let (pcm_tx, pcm_rx) = mpsc::channel::<Vec<u8>>(64);
    let (commit_tx, commit_rx) = mpsc::channel::<()>(8);
    let (ev_tx, ev_rx) = mpsc::channel::<SttEvent>(64);

    // Server-VAD auto-segments (no manual commit); `turn_detection:null` needs `commit`.
    let manual_commit = !cfg.vad;
    let driver = tokio::spawn(drive(cfg, ws, pcm_rx, commit_rx, ev_tx));
    // The `SttStream` contract holds two abortable handles; the realtime path runs a
    // single driver, so the second is a trivial completed task.
    let noop = tokio::spawn(async {});
    Ok(SttStream::new(pcm_tx, commit_tx, ev_rx, driver, noop, manual_commit))
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open the WS with the bearer header. A 401/403 handshake is mapped to a clearly
/// fatal `Unauthorized` so the driver doesn't retry it.
async fn connect(cfg: &Cfg) -> Result<Ws, AppError> {
    let mut req = URL
        .into_client_request()
        .map_err(|e| AppError::Unavailable(format!("openai realtime request: {e}")))?;
    let bearer = HeaderValue::from_str(&format!("Bearer {}", cfg.api_key))
        .map_err(|_| AppError::Validation("invalid STT api key".into()))?;
    req.headers_mut().insert(header::AUTHORIZATION, bearer);
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(req)).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(WsError::Http(resp))) if resp.status() == 401 || resp.status() == 403 => {
            Err(AppError::Unauthorized("openai realtime auth rejected".into()))
        }
        Ok(Err(e)) => Err(AppError::Unavailable(format!("openai realtime connect: {e}"))),
        Err(_) => Err(AppError::Unavailable("openai realtime connect timed out".into())),
    }
}

/// `session.update` body (GA nested transcription shape). With `vad`, OpenAI's
/// server VAD segments the stream and emits deltas + completed transcripts on its
/// own (dictation); without it, `turn_detection:null` defers to our `commit`.
fn session_update(cfg: &Cfg) -> String {
    let turn_detection = if cfg.vad {
        json!({"type": "server_vad"})
    } else {
        Value::Null
    };
    json!({
        "type": "session.update",
        "session": {
            "type": "transcription",
            "audio": {"input": {
                "format": {"type": "audio/pcm", "rate": TARGET_RATE},
                "transcription": {"model": cfg.model, "language": cfg.language},
                "turn_detection": turn_detection,
                "noise_reduction": {"type": "near_field"}
            }}
        }
    })
    .to_string()
}

/// Reconnect loop: serve a connection until it drops, then back off (with jitter)
/// and reconnect — re-sending `session.update` each time — until the consumer goes
/// away or a fatal (auth/quota) error is hit.
async fn drive(
    cfg: Cfg,
    first: Ws,
    mut pcm_rx: mpsc::Receiver<Vec<u8>>,
    mut commit_rx: mpsc::Receiver<()>,
    ev_tx: mpsc::Sender<SttEvent>,
) {
    // `None` ⇒ (re)connect at the top with backoff; `Some` ⇒ serve it. This keeps the
    // connection always-valid at the loop head regardless of which arm we came from.
    let mut ws: Option<Ws> = Some(first);
    let mut backoff = BACKOFF_START_MS;
    loop {
        let conn = match ws.take() {
            Some(w) => w,
            None => {
                // Backoff with jitter (±25%) before each reconnect attempt.
                let jitter = (backoff / 4).max(1);
                let delay = backoff.saturating_sub(jitter) + (nanos_jitter() % (2 * jitter + 1));
                tokio::time::sleep(Duration::from_millis(delay)).await;
                backoff = (backoff * 2).min(BACKOFF_CAP_MS);
                match connect(&cfg).await {
                    Ok(w) => {
                        backoff = BACKOFF_START_MS; // recovered
                        w
                    }
                    Err(AppError::Unauthorized(m)) => {
                        let _ = ev_tx.send(SttEvent::Error { message: m }).await;
                        return;
                    }
                    Err(_) => continue, // transient: ws stays None → keep backing off
                }
            }
        };
        match serve(&cfg, conn, &mut pcm_rx, &mut commit_rx, &ev_tx).await {
            Outcome::ConsumerGone => return, // session torn down → drop everything
            Outcome::Fatal(msg) => {
                let _ = ev_tx.send(SttEvent::Error { message: msg }).await;
                return; // ev_tx drop → orchestrator sees stream end → batch fallback
            }
            Outcome::Reconnect => {} // ws already None → reconnect next iteration
        }
    }
}

enum Outcome {
    /// The socket dropped — reconnect.
    Reconnect,
    /// Unrecoverable (auth/quota) — surface + stop.
    Fatal(String),
    /// The consumer (orchestrator) dropped the receiver — tear down.
    ConsumerGone,
}

/// Serve one live connection: send `session.update`, then pump PCM/commit out and
/// transcript events in, with a keep-alive ping, until the socket drops or the
/// consumer goes away.
async fn serve(
    cfg: &Cfg,
    ws: Ws,
    pcm_rx: &mut mpsc::Receiver<Vec<u8>>,
    commit_rx: &mut mpsc::Receiver<()>,
    ev_tx: &mpsc::Sender<SttEvent>,
) -> Outcome {
    let (mut sink, mut stream) = ws.split();
    if sink.send(Message::Text(session_update(cfg).into())).await.is_err() {
        return Outcome::Reconnect;
    }
    let mut ping = interval(Duration::from_secs(PING_SECS));
    ping.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            pcm = pcm_rx.recv() => match pcm {
                Some(pcm) => {
                    let wav24 = resample_16k_to_24k(&pcm, cfg.src_rate);
                    let frame = json!({
                        "type": "input_audio_buffer.append",
                        "audio": B64.encode(&wav24)
                    }).to_string();
                    if sink.send(Message::Text(frame.into())).await.is_err() {
                        return Outcome::Reconnect;
                    }
                }
                None => return Outcome::ConsumerGone,
            },
            commit = commit_rx.recv() => match commit {
                Some(()) => {
                    let frame = json!({"type": "input_audio_buffer.commit"}).to_string();
                    if sink.send(Message::Text(frame.into())).await.is_err() {
                        return Outcome::Reconnect;
                    }
                }
                None => return Outcome::ConsumerGone,
            },
            _ = ping.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return Outcome::Reconnect;
                }
            }
            msg = stream.next() => {
                let Some(msg) = msg else { return Outcome::Reconnect };
                let Ok(msg) = msg else { return Outcome::Reconnect };
                let txt = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => return Outcome::Reconnect,
                    _ => continue, // pong/binary/ping — ignore
                };
                match parse_event(&txt) {
                    Some(Parsed::Event(ev)) => {
                        if ev_tx.send(ev).await.is_err() {
                            return Outcome::ConsumerGone;
                        }
                    }
                    Some(Parsed::Fatal(msg)) => return Outcome::Fatal(msg),
                    None => {}
                }
            }
        }
    }
}

enum Parsed {
    Event(SttEvent),
    Fatal(String),
}

/// Map one server event to an [`SttEvent`] (or a fatal error). Transcript text is
/// keyed by `item_id` upstream; for our single-utterance-at-a-time loop we forward
/// deltas as partials and the `completed` transcript as the final.
fn parse_event(txt: &str) -> Option<Parsed> {
    let v: Value = serde_json::from_str(txt).ok()?;
    match v.get("type").and_then(Value::as_str)? {
        "conversation.item.input_audio_transcription.delta" => {
            let text = v.get("delta").and_then(Value::as_str).unwrap_or_default().to_string();
            Some(Parsed::Event(SttEvent::Partial { text }))
        }
        "conversation.item.input_audio_transcription.completed" => {
            let text = v.get("transcript").and_then(Value::as_str).unwrap_or_default().to_string();
            Some(Parsed::Event(SttEvent::Final { text }))
        }
        "error" => {
            let err = v.get("error").cloned().unwrap_or(Value::Null);
            let code = err.get("code").and_then(Value::as_str).unwrap_or_default();
            let param = err.get("param").and_then(Value::as_str).unwrap_or_default();
            let message = err.get("message").and_then(Value::as_str).unwrap_or("realtime error").to_string();
            // Validation errors carry a `param` — log it to find the offending field.
            if !param.is_empty() {
                tracing::warn!(%param, %code, "openai realtime validation error");
            }
            // Auth/quota are unrecoverable; so is a rejected request shape — most often
            // a wrong transcription `model` (param points at it). Keeping the socket open
            // on those produces zero transcripts forever, so the orchestrator must degrade
            // to the batch fallback at once rather than stall every turn on the commit-wait.
            // Everything else (rate, transient) stays non-fatal: keep the socket.
            let bad_request = code == "invalid_request_error"
                || code == "invalid_value"
                || code == "model_not_found"
                || param.contains("model");
            let fatal = code == "insufficient_quota"
                || code.contains("invalid_api_key")
                || code == "unauthorized"
                || bad_request;
            if fatal {
                Some(Parsed::Fatal(message))
            } else {
                Some(Parsed::Event(SttEvent::Error { message }))
            }
        }
        _ => None, // session.updated / committed / speech_started / etc. — no surface
    }
}

/// Linearly resample a PCM16-LE mono chunk from `src_rate` to 24 kHz. Per-chunk
/// (chunks are 20–100 ms, so the boundary error is negligible for STT). `×1.5` for
/// the 16 kHz default; general for any `src_rate`.
pub fn resample_16k_to_24k(pcm: &[u8], src_rate: u32) -> Vec<u8> {
    let n = pcm.len() / 2;
    if n == 0 {
        return Vec::new();
    }
    let samples: Vec<i16> =
        (0..n).map(|i| i16::from_le_bytes([pcm[2 * i], pcm[2 * i + 1]])).collect();
    if src_rate == TARGET_RATE {
        return pcm.to_vec();
    }
    let out_len = ((n as u64) * TARGET_RATE as u64 / src_rate as u64).max(1) as usize;
    let mut out = Vec::with_capacity(out_len * 2);
    // Map output index j → input position over the same span [0, n-1].
    let denom = (out_len as f64 - 1.0).max(1.0);
    let span = (n as f64 - 1.0).max(0.0);
    for j in 0..out_len {
        let t = j as f64 * span / denom;
        let i0 = t.floor() as usize;
        let i1 = (i0 + 1).min(n - 1);
        let frac = t - i0 as f64;
        let s = samples[i0] as f64 * (1.0 - frac) + samples[i1] as f64 * frac;
        out.extend_from_slice(&(s.round() as i16).to_le_bytes());
    }
    out
}

/// Cheap jitter source (no `rand` dependency): low bits of the wall clock.
fn nanos_jitter() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_16k_to_24k_is_1_5x() {
        // 320 samples (640 bytes) @16k → 480 samples (960 bytes) @24k.
        let pcm = vec![0u8; 640];
        let out = resample_16k_to_24k(&pcm, 16_000);
        assert_eq!(out.len(), 960);
    }

    #[test]
    fn resample_passthrough_at_target_rate() {
        let pcm = vec![1u8, 2, 3, 4];
        assert_eq!(resample_16k_to_24k(&pcm, 24_000), pcm);
    }

    #[test]
    fn resample_empty_is_empty() {
        assert!(resample_16k_to_24k(&[], 16_000).is_empty());
    }

    #[test]
    fn parse_delta_completed_error() {
        let p = parse_event(r#"{"type":"conversation.item.input_audio_transcription.delta","delta":"he"}"#);
        assert!(matches!(p, Some(Parsed::Event(SttEvent::Partial { text })) if text == "he"));
        let f = parse_event(r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"hello"}"#);
        assert!(matches!(f, Some(Parsed::Event(SttEvent::Final { text })) if text == "hello"));
        let e = parse_event(r#"{"type":"error","error":{"code":"server_error","message":"x"}}"#);
        assert!(matches!(e, Some(Parsed::Event(SttEvent::Error { .. }))));
        let fatal = parse_event(r#"{"type":"error","error":{"code":"insufficient_quota","message":"no funds"}}"#);
        assert!(matches!(fatal, Some(Parsed::Fatal(_))));
        // A wrong transcription model is fatal-for-this-engine → batch fallback, not a
        // socket kept open transcribing nothing.
        let bad_model = parse_event(r#"{"type":"error","error":{"code":"invalid_request_error","param":"session.audio.input.transcription.model","message":"bad model"}}"#);
        assert!(matches!(bad_model, Some(Parsed::Fatal(_))));
        // A plain transient error stays non-fatal (keep the socket).
        let transient = parse_event(r#"{"type":"error","error":{"code":"server_error","message":"hiccup"}}"#);
        assert!(matches!(transient, Some(Parsed::Event(SttEvent::Error { .. }))));
        assert!(parse_event(r#"{"type":"session.updated"}"#).is_none());
    }
}
