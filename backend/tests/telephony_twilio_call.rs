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

//! A telephone call, played out against the real server by something pretending to be
//! a carrier.
//!
//! Almost everything about a telephone call is unverifiable from a unit test and
//! painful to verify from an actual telephone: the signature, the answer a carrier will
//! accept, the socket handshake, the audio conversion in both directions, the pacing,
//! and the tidying up afterwards. So this speaks the carrier's protocol at the real
//! router: it signs a webhook, reads the answer, opens the media socket with the ticket
//! it was given, sends narrowband audio and asserts that narrowband audio comes back.
//!
//! Two things make it dependable rather than flaky.
//!
//! Speech and silence are measured from how much audio has arrived, not from the clock,
//! so the caller's turn can be sent as fast as the socket will take it and still be
//! heard as nearly a second of speech followed by more than a second of quiet. Nothing
//! here sleeps waiting for that.
//!
//! And no streaming recognition engine is needed. With none configured the session falls
//! back to transcribing each finished utterance in one go through the platform's own
//! service, which the mock stands in for. So the test asserts what reached recognition
//! rather than needing something that could recognise it.
//!
//! Needs a reachable Postgres and Redis; skips when `DATABASE_URL` is unset.

mod common;

use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use common::mock_ml::{self, MlScript};
use futures_util::{SinkExt, StreamExt};
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha1::Sha1;
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use uuid::Uuid;

use fosnie_backend::config::BootConfig;
use fosnie_backend::config::runtime::{self, ConfigValueType};
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::voice::telephony::codec;
use fosnie_backend::{cache, db, http};

const AUTH_TOKEN: &str = "carrier-token-for-tests";

/// One call at a time in this file.
///
/// Both calls here configure the line by writing deployment-wide settings and put them
/// back afterwards, and the tests in one binary run at the same time. Overlapping, one
/// would restore the carrier credential and the synthesiser while the other was still
/// mid-call, which shows up as a properly signed request being refused and a reply that
/// is never spoken. Serialising them is the honest fix: the settings really are shared,
/// and pretending otherwise would only move the race.
static CARRIER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The turn-taking this test drives, set explicitly so the audio it sends and the
/// behaviour it expects come from the same numbers.
const TURN_SILENCE_MS: u64 = 900;
const MIN_SPEECH_MS: u64 = 250;
const STREAM_SID: &str = "MZtestteststream";
const ANSWER_PATH: &str = "/api/telephony/twilio/voice";
const STATUS_PATH: &str = "/api/telephony/twilio/status";

/// A base64 32-byte key, so the carrier credential can be stored encrypted.
const MESSAGE_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// The signature a carrier would put on a request, computed here rather than through the
/// code under test.
///
/// This being a second, independent implementation is the point. Calling the production
/// one would prove only that it agrees with itself; the value it has to agree with is
/// the one the carrier's own documented example produces, which the signing module pins
/// separately.
fn sign(url: &str, params: &[(&str, &str)]) -> String {
    let mut sorted: Vec<&(&str, &str)> = params.iter().collect();
    sorted.sort_by_key(|(k, _)| *k);
    let mut signed = url.to_string();
    for (k, v) in sorted {
        signed.push_str(k);
        signed.push_str(v);
    }
    let mut mac = Hmac::<Sha1>::new_from_slice(AUTH_TOKEN.as_bytes()).unwrap();
    mac.update(signed.as_bytes());
    B64.encode(mac.finalize().into_bytes())
}

fn form_body(params: &[(&str, &str)]) -> String {
    let mut s = form_urlencoded::Serializer::new(String::new());
    for (k, v) in params {
        s.append_pair(k, v);
    }
    s.finish()
}

struct Harness {
    base: String,
    pg: PgPool,
    state: AppState,
    ml: mock_ml::MockMl,
    owner: Uuid,
    agent: Uuid,
    /// The line this run registered.
    line: Uuid,
    /// The line's own number, and the number calling it.
    ///
    /// Fresh every run, because the abuse guards are real and are keyed on exactly these:
    /// reusing a number would mean the second run of the day is refused for looking like
    /// somebody ringing repeatedly, which is precisely what those guards are for.
    number: String,
    caller: String,
    /// What the voice and telephony settings were before this test touched them.
    borrowed: Vec<(String, Option<String>)>,
}

/// The settings this test has to control, and therefore has to put back.
///
/// The voice ones are not incidental. They are deployment-wide, and a developer's
/// database quite reasonably has them pointing at whatever engines that developer uses,
/// which may well be a cloud service. A test that read them instead of setting them
/// would send its audio there.
const BORROWED_KEYS: [&str; 16] = [
    // The turn-taking dials this test's frame counts are derived from. Pinned rather than
    // inherited: left to the defaults, a change to either the shared or the telephone
    // value would leave the test passing while no longer testing what it says it does.
    "voice.phone.silence_threshold_ms",
    "voice.phone.min_speech_ms",
    "voice.phone.turn_detection",
    "voice.turn_detector_url",
    "voice.silence_threshold_ms",
    "voice.stt_stream_kind",
    "voice.stt_stream_url",
    "voice.stt_sample_rate",
    "voice.tts_stream",
    "voice.tts_stream_url",
    "voice.tts_model",
    "voice.tts_voice",
    "voice.tts_api_key_enc",
    "telephony.provider",
    "telephony.public_base_url",
    "telephony.max_concurrent_calls",
];

/// The ordinary line: a receptionist that answers, and nowhere to put anybody through.
async fn harness() -> Option<Harness> {
    harness_for(
        MlScript {
            // A reply of several sentences, each a second of audio. Length is the point:
            // it is released at the rate a line plays it, so a short reply is over before
            // there is anything to interrupt.
            generate_tokens: vec![
                "Good afternoon, thank you for calling. ".into(),
                "Let me check the diary for you. ".into(),
                "I have a slot on Tuesday morning. ".into(),
                "Would that suit you? ".into(),
            ],
            speech_samples: 24_000,
            ..MlScript::default()
        },
        None,
        &[],
    )
    .await
}

/// Build a line, its agent and a server in front of both.
///
/// `transfer` is where this line puts callers through to, and `tools` is what its agent
/// may do, because the two things a transfer test needs to vary are exactly those.
async fn harness_for(
    script: MlScript,
    transfer: Option<&str>,
    tools: &[&str],
) -> Option<Harness> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    // Skipping is for "no database configured". A database that IS configured but
    // cannot be reached is an environment fault, and quietly reporting a pass for it is
    // how an untested change looks tested.
    let pg = db::connect(&db_url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    });
    let redis = cache::create_pool(&redis_url).expect("redis pool");

    // A reply of several sentences, each a second of audio. Length is the point: it is
    // released at the rate a line plays it, so a short reply is over before there is
    // anything to interrupt.
    let ml = mock_ml::spawn(script).await;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.ml.base_url = ml.base_url.clone();
    boot.features.voice = true;
    boot.features.voice_live = true;
    boot.features.telephony = true;
    // Synthesis has to be the streaming kind, reached directly: the reply must arrive as
    // raw samples, because nothing in the server can decode a container.
    boot.voice_live.tts_stream = true;
    boot.voice_live.tts_stream_url = ml.base_url.clone();
    // Left at the default so the caller's 8 kHz audio really does go through the
    // conversion to 16 kHz on its way to recognition.
    boot.voice_live.stt_sample_rate = 16_000;
    boot.message_encryption_key = MESSAGE_KEY.into();
    boot.server.static_dir = "___no_spa___".into();

    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot)).build();
    let tag = Uuid::now_v7().as_u128() % 100_000;
    let number = format!("+44131555{tag:05}");
    let caller = format!("+44770090{tag:05}");
    let owner = mk_user(&pg).await;
    let agent = mk_agent(&pg, owner).await;
    // The line itself, as an operator would register it: its own number, its own agent,
    // its own owning account. Per run like the numbers, so two runs of the suite no longer
    // contend over one deployment-wide setting.
    let line = mk_line(&pg, owner, agent, &number, transfer).await;
    for tool in tools {
        sqlx::query("INSERT INTO agent_tools (agent_id, tool_name) VALUES ($1, $2)")
            .bind(agent)
            .bind(tool)
            .execute(&pg)
            .await
            .unwrap();
    }

    let app = http::router(state.clone(), None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    let base = format!("http://127.0.0.1:{port}");

    // Note what is there now, so it can be put back exactly.
    let mut borrowed = Vec::new();
    for key in BORROWED_KEYS {
        let existing = runtime::get(&pg, key).await.ok().flatten().map(|e| e.value);
        borrowed.push((key.to_string(), existing));
    }
    // Also add the credential key, which is written but has no entry above.
    borrowed.push((
        "telephony.auth_token_enc".to_string(),
        runtime::get(&pg, "telephony.auth_token_enc").await.ok().flatten().map(|e| e.value),
    ));

    // The turn-taking this test depends on, stated rather than inherited.
    set(&pg, "voice.phone.silence_threshold_ms", &TURN_SILENCE_MS.to_string(), ConfigValueType::Int).await;
    set(&pg, "voice.phone.min_speech_ms", &MIN_SPEECH_MS.to_string(), ConfigValueType::Int).await;
    // Semantic turn detection OFF, and this is not a preference. A telephone line turns it
    // on by default, and there is no sidecar here: the call would then consult something
    // that is not there on every utterance. It recovers, but it recovers by falling back,
    // and a test of the carrier path should not be exercising that fallback silently.
    set(&pg, "voice.phone.turn_detection", "false", ConfigValueType::Bool).await;
    set(&pg, "voice.turn_detector_url", "", ConfigValueType::String).await;
    // Cleared so the shared value cannot reach the line and override the one above.
    set(&pg, "voice.silence_threshold_ms", "", ConfigValueType::String).await;

    // Recognition batches each finished utterance through the platform's own service,
    // which the mock stands in for, so no streaming engine is needed.
    set(&pg, "voice.stt_stream_kind", "none", ConfigValueType::String).await;
    set(&pg, "voice.stt_stream_url", "", ConfigValueType::String).await;
    set(&pg, "voice.stt_sample_rate", "16000", ConfigValueType::Int).await;
    // Synthesis has to be the streaming kind and has to be the mock. Left to whatever a
    // developer had configured, this test would send its audio to their engine.
    set(&pg, "voice.tts_stream", "true", ConfigValueType::Bool).await;
    set(&pg, "voice.tts_stream_url", &ml.base_url, ConfigValueType::String).await;
    set(&pg, "voice.tts_model", "mock", ConfigValueType::String).await;
    set(&pg, "voice.tts_voice", "", ConfigValueType::String).await;
    set(&pg, "voice.tts_api_key_enc", "", ConfigValueType::String).await;

    // The line, configured as an operator would. The public address is this test's own
    // server, which is also what makes the signature assertions meaningful: the string
    // being signed is stated in configuration, so the test knows it exactly.
    set(&pg, "telephony.provider", "twilio", ConfigValueType::String).await;
    set(&pg, "telephony.public_base_url", &base, ConfigValueType::String).await;
    // One at a time, so the ceiling can be used as an instrument: a second call being
    // refused says the first is up, and being accepted says it is gone.
    set(&pg, "telephony.max_concurrent_calls", "1", ConfigValueType::Int).await;
    let ct = fosnie_backend::crypto::encrypt_at_rest(AUTH_TOKEN).expect("encrypts");
    set(&pg, "telephony.auth_token_enc", &ct, ConfigValueType::String).await;

    Some(Harness { base, pg, state, ml, owner, agent, line, number, caller, borrowed })
}

async fn set(pg: &PgPool, key: &str, value: &str, t: ConfigValueType) {
    runtime::set(pg, key, value, t, "global", None, "system").await.expect("write setting");
}

/// Put everything back.
///
/// These settings are deployment-wide, not per-user like most test fixtures. Left as
/// this test set them, they are a live telephone configuration and a rerouted voice
/// engine in whoever's database runs the suite next; blindly removed instead, they take
/// that developer's own engine configuration with them. So each one is restored to
/// exactly what it was, and only the ones that were genuinely absent are removed.
async fn cleanup(h: &Harness) {
    for (key, was) in &h.borrowed {
        match was {
            Some(value) => {
                // The type is not recorded, and the two that are not strings would be
                // rejected as one, so they are restored as what they are.
                let t = match key.as_str() {
                    "voice.tts_stream" => ConfigValueType::Bool,
                    "voice.stt_sample_rate" | "telephony.max_concurrent_calls" => ConfigValueType::Int,
                    _ => ConfigValueType::String,
                };
                let _ = runtime::set(&h.pg, key, value, t, "global", None, "system").await;
            }
            None => {
                let _ = runtime::unset(&h.pg, key, "system").await;
            }
        }
    }
    // Before the conversations and the account: a call points at both.
    let _ = sqlx::query("DELETE FROM calls WHERE owner_user_id = $1").bind(h.owner).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM phone_numbers WHERE id = $1").bind(h.line).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM messages WHERE chat_id IN (SELECT id FROM chats WHERE owner_user_id = $1)")
        .bind(h.owner)
        .execute(&h.pg)
        .await;
    let _ = sqlx::query("DELETE FROM chats WHERE owner_user_id = $1").bind(h.owner).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM agent_tools WHERE agent_id = $1").bind(h.agent).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM agents WHERE id = $1").bind(h.agent).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(h.owner).execute(&h.pg).await;
}

async fn mk_user(pg: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    // An ordinary account on purpose. An administrator's line would read every Library
    // the agent is bound to whether it had been granted them or not, because being an
    // administrator short-circuits that check everywhere.
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'Line owner', $2, 'user')")
        .bind(id)
        .bind(format!("{id}@example.test"))
        .execute(pg)
        .await
        .unwrap();
    id
}

/// A line answering `number`, switched on.
///
/// Stated explicitly, because a line is created switched off: that is what the negative
/// tests rely on, and it is what stops a number answering in the seconds between being
/// registered and being checked.
async fn mk_line(
    pg: &PgPool,
    owner: Uuid,
    agent: Uuid,
    number: &str,
    transfer: Option<&str>,
) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO phone_numbers \
           (id, e164, provider, owner_user_id, agent_id, enabled, transfer_e164) \
         VALUES ($1, $2, 'twilio', $3, $4, true, $5)",
    )
    .bind(id)
    .bind(number)
    .bind(owner)
    .bind(agent)
    .bind(transfer)
    .execute(pg)
    .await
    .unwrap();
    id
}

/// A receptionist with no tools at all, which is what a caller should reach by default.
async fn mk_agent(pg: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO agents (id, name, description, system_prompt, created_by, modes)
         VALUES ($1, 'Reception', '', 'Answer the telephone.', $2, ARRAY['general'])",
    )
    .bind(id)
    .bind(owner)
    .execute(pg)
    .await
    .unwrap();
    id
}

/// Ring the line. Returns the response status and body.
async fn ring(h: &Harness, call_sid: &str, to: &str, signature: Option<&str>) -> (u16, String) {
    let params: [(&str, &str); 6] = [
        ("AccountSid", "ACtest"),
        ("CallSid", call_sid),
        ("CallStatus", "ringing"),
        ("Direction", "inbound"),
        ("From", &h.caller),
        ("To", to),
    ];
    let url = format!("{}{ANSWER_PATH}", h.base);
    let sig = signature.map(str::to_string).unwrap_or_else(|| sign(&url, &params));
    let mut req = reqwest::Client::new()
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body(&params));
    if signature != Some("") {
        req = req.header("X-Twilio-Signature", sig);
    }
    let resp = req.send().await.expect("the webhook answers");
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap_or_default())
}

/// 20 ms of narrowband audio, as a carrier would send it.
fn media_frame(loud: bool, phase: usize) -> String {
    use std::f32::consts::PI;
    let samples: Vec<i16> = (0..codec::FRAME_SAMPLES)
        .map(|n| {
            if !loud {
                return 0;
            }
            let t = (phase * codec::FRAME_SAMPLES + n) as f32 / codec::TELEPHONY_RATE as f32;
            (11_000.0 * (2.0 * PI * 440.0 * t).sin()) as i16
        })
        .collect();
    B64.encode(codec::encode(&samples))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_is_answered_carried_and_cleared_up() {
    let _one_at_a_time = CARRIER.lock().await;
    let Some(h) = harness().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    // Everything below runs inside one guard so that a failing assertion still removes
    // the deployment-wide settings.
    let outcome = tokio::time::timeout(Duration::from_secs(60), run_call(&h)).await;
    cleanup(&h).await;
    outcome.expect("the call did not finish in time").expect("the call failed");
}

async fn run_call(h: &Harness) -> Result<(), String> {
    let url = format!("{}{ANSWER_PATH}", h.base);

    // ---- The signature is the whole of a carrier's authentication. ----
    let (status, body) = ring(h, "CA-unsigned", &h.number, Some("")).await;
    if status != 403 || !body.is_empty() {
        return Err(format!("an unsigned webhook was not refused: {status} {body}"));
    }

    // One character out. A check that compared lengths, or stopped early, would pass
    // this.
    let params: [(&str, &str); 6] = [
        ("AccountSid", "ACtest"),
        ("CallSid", "CA-bad-signature"),
        ("CallStatus", "ringing"),
        ("Direction", "inbound"),
        ("From", &h.caller),
        ("To", &h.number),
    ];
    let mut wrong: Vec<char> = sign(&url, &params).chars().collect();
    wrong[0] = if wrong[0] == 'A' { 'B' } else { 'A' };
    let wrong: String = wrong.into_iter().collect();
    let (status, _) = ring(h, "CA-bad-signature", &h.number, Some(&wrong)).await;
    if status != 403 {
        return Err(format!("a forged signature was accepted: {status}"));
    }

    // A properly signed call to a number no line is registered on. Refused, but as a
    // refusal the caller hears rather than an error: an error status would make the
    // carrier answer the call and bill for it.
    let (status, body) = ring(h, "CA-wrong-number", "+441315559999", None).await;
    if status != 200 || !body.contains("<Reject/>") {
        return Err(format!("a call to the wrong number was not politely refused: {status} {body}"));
    }

    // ---- The real call. ----
    let call_sid = "CAtest0000000001";
    let (status, twiml) = ring(h, call_sid, &h.number, None).await;
    if status != 200 {
        return Err(format!("a properly signed call was refused: {status} {twiml}"));
    }
    let socket_url = stream_url(&twiml)?;
    // The scheme is derived from the configured address rather than fixed, which is both
    // why a real deployment gets a secure socket and why this test can connect at all.
    if !socket_url.starts_with("ws://127.0.0.1:") {
        return Err(format!("the answer named an address a carrier could not use: {socket_url}"));
    }

    let (socket, resp) = tokio_tungstenite::connect_async(&socket_url)
        .await
        .map_err(|e| format!("the media socket was refused: {e}"))?;
    if resp.status().as_u16() != 101 {
        return Err(format!("the media socket did not open: {}", resp.status()));
    }
    let (mut ws_tx, mut ws_rx) = socket.split();

    ws_tx
        .send(WsMessage::Text(json!({ "event": "connected", "protocol": "Call", "version": "1.0.0" }).to_string().into()))
        .await
        .map_err(|e| e.to_string())?;
    ws_tx
        .send(WsMessage::Text(
            json!({
                "event": "start",
                "sequenceNumber": "1",
                "streamSid": STREAM_SID,
                "start": {
                    "accountSid": "ACtest",
                    "streamSid": STREAM_SID,
                    "callSid": call_sid,
                    "tracks": ["inbound"],
                    "mediaFormat": { "encoding": "audio/x-mulaw", "sampleRate": 8000, "channels": 1 },
                    "customParameters": {},
                },
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|e| e.to_string())?;

    // A telephone line never goes quiet: it carries silence at the same twenty
    // milliseconds a frame as it carries speech, for as long as the call is up. So the
    // rest of the call is played out by a task that keeps sending, because a stream
    // that simply stopped is a dead line and the server is right to end it.
    //
    // Started before anything is asked of the line, because the first thing that happens
    // on a call is the line talking: the notice is not finished until the carrier has said
    // it played it, and only this task can say so.
    //
    // And it starts by talking over the notice, which is the point. A caller who speaks
    // across it must not cut it short and must not be answered underneath it.
    let mode = Arc::new(std::sync::atomic::AtomicU8::new(MODE_SPEAKING));
    // Marks the server asks about go back to it the way a carrier sends them: from the same
    // side of the socket that is sending the audio, in among it. The reading half cannot
    // reply, because the writing half belongs to the task below.
    let (echo_tx, echo_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let line = tokio::spawn(carry_the_line(ws_tx, mode.clone(), call_sid.to_string(), echo_rx));

    // ---- What the caller is told, before a word of theirs is acted on. ----
    let notice = hear_the_notice(&mut ws_rx, &echo_tx, &mode).await?;
    if notice.frames < 4 {
        return Err(format!(
            "the caller heard {} frames of notice, which is not a spoken notice (synthesis {})",
            notice.frames,
            h.ml.calls.speeches(),
        ));
    }
    if notice.cleared {
        return Err("talking over the notice interrupted it".into());
    }
    if h.ml.calls.transcribes() != 0 {
        return Err("something said over the notice was sent to recognition".into());
    }
    if h.ml.calls.generates() != 0 {
        return Err("something said over the notice was answered underneath it".into());
    }
    // The words themselves. Composed from the line, so a test that only counted audio
    // would pass on a line that said "hello" and nothing else.
    let said = h.ml.calls.spoken_texts().join(" ");
    for must in ["automated assistant", "written down", "speak to a person"] {
        if !said.contains(must) {
            return Err(format!("the notice never said {must:?}: {said:?}"));
        }
    }
    // And what was said is written beside the call, because that is the question a
    // complaint asks. Polled rather than read once: the last thing out here to know the
    // notice is over is the carrier's own report of playing it, and the row is written
    // after that report has been dealt with.
    let mut recorded = None;
    for _ in 0..100 {
        recorded = sqlx::query_as::<_, (Option<String>, bool)>(
            "SELECT notice_text, notice_at IS NOT NULL FROM calls \
             WHERE provider = 'twilio' AND provider_call_id = $1",
        )
        .bind(call_sid)
        .fetch_optional(&h.pg)
        .await
        .map_err(|e| e.to_string())?;
        if matches!(&recorded, Some((Some(_), true))) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    match recorded {
        Some((Some(text), true)) if text.contains("automated assistant") => {}
        other => return Err(format!("the call does not record what the caller was told: {other:?}")),
    }

    // ---- The caller's own turn. ----
    // Spoken by the line task at the rate a telephone sends, then quiet: what makes this a
    // turn is the amount of audio, and the counts come from the dials the harness set, so
    // the test states its assumptions rather than relying on a default that may move.
    let speech_ms = MIN_SPEECH_MS + 20 * codec::FRAME_MS as u64;
    mode.store(MODE_SPEAKING, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(speech_ms)).await;
    mode.store(MODE_SILENT, std::sync::atomic::Ordering::SeqCst);

    // ---- What came back. ----
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut first_at: Option<tokio::time::Instant> = None;
    let mut last_at = tokio::time::Instant::now();
    let mut cleared = false;
    // What the server asked to be told about, and how much of the reply had already gone
    // by when it asked.
    let mut marks_asked: Vec<String> = Vec::new();
    let mut marks_at: Vec<usize> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    while frames.len() < 4 && tokio::time::Instant::now() < deadline {
        let Ok(Ok(Some(Ok(msg)))) =
            tokio::time::timeout(Duration::from_secs(20), ws_rx.next()).await.map(Ok::<_, ()>)
        else {
            break;
        };
        let WsMessage::Text(text) = msg else { continue };
        let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        // Every message must carry the stream name the carrier gave us. Without it the
        // carrier discards the message in silence: no error, no close, just no audio.
        if value["streamSid"].as_str() != Some(STREAM_SID) {
            return Err(format!("a message went out without the right stream name: {text}"));
        }
        match value["event"].as_str() {
            Some("media") => {
                let media = &value["media"];
                let keys: Vec<&String> = media.as_object().map(|o| o.keys().collect()).unwrap_or_default();
                if keys != vec!["payload"] {
                    return Err(format!("outbound audio carried fields that are the carrier's to send: {keys:?}"));
                }
                let bytes = B64
                    .decode(media["payload"].as_str().unwrap_or_default())
                    .map_err(|e| format!("the payload was not base64: {e}"))?;
                // Exactly one frame, and nothing but samples. A file header would show
                // up here as both a wrong length and a wrong first frame.
                if bytes.len() != codec::MULAW_FRAME_BYTES {
                    return Err(format!("a frame of {} bytes is not 20 ms of audio", bytes.len()));
                }
                if first_at.is_none() {
                    first_at = Some(tokio::time::Instant::now());
                }
                last_at = tokio::time::Instant::now();
                frames.push(bytes);
            }
            Some("clear") => cleared = true,
            Some("mark") => {
                let name = value["mark"]["name"].as_str().unwrap_or_default().to_string();
                if name.is_empty() {
                    return Err("a playback request arrived with no name".into());
                }
                marks_asked.push(name.clone());
                // Every frame of the reply so far has already been read off this socket,
                // which is the property no unit test can show: the request to report
                // playback arrives after the audio it is about, not before it.
                marks_at.push(frames.len());
                let _ = echo_tx.send(name);
            }
            _ => {}
        }
    }
    if frames.len() < 4 {
        return Err(format!(
            "only {} frames of reply audio arrived (recognition {}, generation {}, synthesis {}, formats {:?})",
            frames.len(),
            h.ml.calls.transcribes(),
            h.ml.calls.generates(),
            h.ml.calls.speeches(),
            h.ml.calls.speech_formats(),
        ));
    }
    if frames.iter().all(|f| f.iter().all(|b| *b == codec::MULAW_SILENCE)) {
        return Err("the reply was silence all the way through".into());
    }

    // Pacing. Only a lower bound, because the exact 20 ms spacing is already pinned on
    // a clock the test controls, in the pacer's own tests. What cannot be got from those
    // is that the pacing survives the whole chain: unpaced output would arrive here in
    // very nearly no time at all.
    let spread = last_at.duration_since(first_at.expect("a first frame"));
    let expected = Duration::from_millis((frames.len() as u64 - 1) * codec::FRAME_MS as u64);
    if spread * 2 < expected {
        return Err(format!(
            "{} frames arrived over {spread:?}, which is faster than a telephone line can play them",
            frames.len()
        ));
    }

    // ---- What reached the engines. ----
    let audio = h.ml.calls.transcribed_audio();
    if audio.len() != 1 {
        return Err(format!("recognition was called {} times, not once", audio.len()));
    }
    let wav = &audio[0];
    if &wav[..4] != b"RIFF" {
        return Err("recognition was not handed an audio file".into());
    }
    // The rate is stored little-endian at offset 24 of a WAV header. It has to be the
    // rate the session works at, not the line's: turn detection measures speech from
    // byte counts, so audio at the wrong rate scales every threshold it uses.
    let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
    if rate != 16_000 {
        return Err(format!("recognition was handed {rate} Hz audio, not the rate the session expects"));
    }
    let channels = u16::from_le_bytes([wav[22], wav[23]]);
    if channels != 1 {
        return Err(format!("recognition was handed {channels} channels"));
    }

    // ---- Playback reporting. ----
    // The opening request must come after audio has started, not before: it is what makes
    // "the caller began hearing it" a real moment rather than a guess.
    if marks_asked.is_empty() {
        return Err("the server never asked to be told what the caller had heard".into());
    }
    if marks_at[0] == 0 {
        return Err("playback was asked about before any audio had been sent".into());
    }
    // One reply, one set of requests, all naming the same reply.
    let generations: std::collections::BTreeSet<&str> = marks_asked
        .iter()
        .map(|n| n.split('-').next().unwrap_or_default())
        .collect();
    if generations.len() != 1 {
        return Err(format!("requests spanned more than one reply: {marks_asked:?}"));
    }
    if !marks_asked.iter().any(|n| n.ends_with("-begin")) {
        return Err(format!("no request about the start of the reply: {marks_asked:?}"));
    }

    let formats = h.ml.calls.speech_formats();
    if formats.is_empty() || formats.iter().any(|f| f != "pcm") {
        return Err(format!("synthesis was asked for {formats:?} rather than raw samples"));
    }

    // ---- Interrupting. ----
    // Talking over the reply must reach the carrier as well as stopping the queue: the
    // carrier is holding audio the caller has not heard yet, and only it can drop that.
    if !cleared {
        mode.store(MODE_SPEAKING, std::sync::atomic::Ordering::SeqCst);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while !cleared && tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(10), ws_rx.next()).await {
                Ok(Some(Ok(WsMessage::Text(text)))) => {
                    let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                    if value["event"].as_str() == Some("clear") {
                        if value["streamSid"].as_str() != Some(STREAM_SID) {
                            return Err("the interruption went out without the right stream name".into());
                        }
                        cleared = true;
                    }
                }
                Ok(Some(Ok(_))) => continue,
                _ => break,
            }
        }
        mode.store(MODE_SILENT, std::sync::atomic::Ordering::SeqCst);
    }
    if !cleared {
        return Err("talking over the reply never reached the carrier".into());
    }

    // ---- The ceiling, used as an instrument. ----
    let (status, body) = ring(h, "CAtest0000000002", &h.number, None).await;
    if status != 200 || !body.contains("<Reject/>") {
        return Err(format!("a second call was taken while the first was up: {status} {body}"));
    }


    // ---- Hanging up. ----
    mode.store(MODE_HANG_UP, std::sync::atomic::Ordering::SeqCst);
    let _ = line.await;

    // The call really has gone, observed only through the public surface: the line is
    // free again, which it would not be if anything had been left behind.
    let freed = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut taken = false;
    while tokio::time::Instant::now() < freed {
        let (status, body) = ring(h, "CAtest0000000003", &h.number, None).await;
        if status == 200 && body.contains("<Connect>") {
            taken = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !taken {
        return Err("the line never became free again after the call ended".into());
    }
    if !h.state.telephony.is_empty() {
        return Err(format!("{} calls are still recorded as in progress", h.state.telephony.len()));
    }

    // ---- A ticket works exactly once. ----
    if tokio_tungstenite::connect_async(&socket_url).await.is_ok() {
        return Err("a spent call ticket opened a second media socket".into());
    }

    // ---- What the log recorded. ----
    // One query for the whole of this slice: the call was logged once, closed, attributed
    // to the line it came in on, and tied to a conversation stamped as having come from a
    // telephone.
    let logged = sqlx::query_as::<_, (String, bool, Option<Uuid>, Option<Uuid>, i64)>(
        "SELECT c.outcome, c.ended_at IS NOT NULL, c.chat_id, c.phone_number_id, \
                (SELECT count(*) FROM calls d WHERE d.provider_call_id = c.provider_call_id) \
         FROM calls c WHERE c.provider = 'twilio' AND c.provider_call_id = $1",
    )
    .bind(call_sid)
    .fetch_optional(&h.pg)
    .await
    .map_err(|e| e.to_string())?;
    let Some((outcome, ended, chat_id, number_id, rows)) = logged else {
        return Err("the call was never recorded".into());
    };
    if rows != 1 {
        return Err(format!("the call was recorded {rows} times"));
    }
    if outcome != "completed" || !ended {
        return Err(format!("a call that ended cleanly was recorded as {outcome:?}, ended={ended}"));
    }
    if number_id != Some(h.line) {
        return Err("the call was not attributed to the line it came in on".into());
    }
    let Some(chat_id) = chat_id else {
        return Err("the call recorded no conversation, though the caller spoke".into());
    };
    let origin = sqlx::query_scalar::<_, String>("SELECT origin FROM chats WHERE id = $1")
        .bind(chat_id)
        .fetch_one(&h.pg)
        .await
        .map_err(|e| e.to_string())?;
    if origin != "phone" {
        return Err(format!("the conversation is marked as coming from {origin:?}"));
    }

    // ---- The carrier's own notice that a call is over. ----
    let status_url = format!("{}{STATUS_PATH}", h.base);
    let params = [("CallSid", call_sid), ("CallStatus", "completed")];
    let resp = reqwest::Client::new()
        .post(&status_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("X-Twilio-Signature", sign(&status_url, &params))
        .body(form_body(&params))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().as_u16() != 204 {
        return Err(format!("the end-of-call notice was not accepted: {}", resp.status()));
    }

    Ok(())
}

/// What the line said before it listened to anything.
struct Notice {
    /// Frames of audio the caller heard.
    frames: usize,
    /// Whether the line ever asked the carrier to abandon what it was playing, which on a
    /// notice would mean it had been interrupted.
    cleared: bool,
}

/// Listen to the notice out, echoing playback the way a carrier does.
///
/// Ends when the line asks to be told it has played the last of the notice, which is the
/// only moment from out here at which the caller has actually heard all of it: everything
/// before that is audio handed over, not audio played. The caller stops talking over it at
/// the same instant, so the turn that follows starts from silence.
async fn hear_the_notice(
    ws_rx: &mut (impl StreamExt<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin),
    echo_tx: &tokio::sync::mpsc::UnboundedSender<String>,
    mode: &Arc<std::sync::atomic::AtomicU8>,
) -> Result<Notice, String> {
    let mut heard = Notice { frames: 0, cleared: false };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_secs(20), ws_rx.next()).await
        else {
            return Err(format!("the line went quiet part-way through the notice ({} frames)", heard.frames));
        };
        let WsMessage::Text(text) = msg else { continue };
        let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        match value["event"].as_str() {
            Some("media") => heard.frames += 1,
            Some("clear") => heard.cleared = true,
            Some("mark") => {
                let name = value["mark"]["name"].as_str().unwrap_or_default().to_string();
                let last = name.ends_with("-end");
                if last {
                    // Quiet from here, so any leftover talk-over is well under the floor a
                    // turn needs and the caller's real turn is the next thing to happen.
                    mode.store(MODE_SILENT, std::sync::atomic::Ordering::SeqCst);
                }
                let _ = echo_tx.send(name);
                if last {
                    return Ok(heard);
                }
            }
            _ => {}
        }
    }
    Err(format!("the notice never finished ({} frames)", heard.frames))
}

/// The writing half of the carrier's socket.
type LineOut = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    WsMessage,
>;

const MODE_SILENT: u8 = 0;
const MODE_SPEAKING: u8 = 1;
const MODE_HANG_UP: u8 = 2;

/// Keep the line alive for the rest of the call, as a carrier does: a frame every
/// twenty milliseconds, of silence or of speech, until told to hang up.
async fn carry_the_line(
    mut out: LineOut,
    mode: Arc<std::sync::atomic::AtomicU8>,
    call_sid: String,
    mut echo: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    let mut phase = 200usize;
    loop {
        match mode.load(std::sync::atomic::Ordering::SeqCst) {
            MODE_HANG_UP => {
                let _ = out
                    .send(WsMessage::Text(
                        json!({ "event": "stop", "streamSid": STREAM_SID, "stop": { "accountSid": "ACtest", "callSid": call_sid } })
                            .to_string()
                            .into(),
                    ))
                    .await;
                let _ = out.close().await;
                return;
            }
            m => {
                // Report any playback the server asked about, as a carrier would once it
                // has played that far. Sent before the next frame so the report is
                // plainly about audio already gone by.
                while let Ok(name) = echo.try_recv() {
                    let reply = json!({
                        "event": "mark",
                        "streamSid": STREAM_SID,
                        "mark": { "name": name },
                    });
                    if out.send(WsMessage::Text(reply.to_string().into())).await.is_err() {
                        return;
                    }
                }
                let loud = m == MODE_SPEAKING;
                if send_media(&mut out, &media_frame(loud, phase), phase).await.is_err() {
                    return;
                }
                phase += 1;
                tokio::time::sleep(Duration::from_millis(codec::FRAME_MS as u64)).await;
            }
        }
    }
}

async fn send_media(out: &mut LineOut, payload: &str, phase: usize) -> Result<(), String> {
    out
        .send(WsMessage::Text(
            json!({
                "event": "media",
                // Strings, as a carrier sends them. Typed as numbers they would fail to
                // parse and the whole call would be silent.
                "sequenceNumber": (phase + 2).to_string(),
                "streamSid": STREAM_SID,
                "media": {
                    "track": "inbound",
                    "chunk": (phase + 1).to_string(),
                    "timestamp": (phase * 20).to_string(),
                    "payload": payload,
                },
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|e| e.to_string())
}

/// Pull the socket address out of the answer, and check the answer's shape while there.
///
/// What may follow the stream is exactly one thing: somewhere to come back to, on a line
/// that can put callers through. Anything else would be read the moment our socket closes
/// and would happen to every call, wanted or not.
fn stream_url(twiml: &str) -> Result<String, String> {
    if !twiml.contains("<Connect><Stream url=\"") {
        return Err(format!("the answer does not connect a two-way stream: {twiml}"));
    }
    let tail = twiml.split("</Connect>").nth(1).unwrap_or_default();
    let tail_ok = tail == "</Response>"
        || (tail.starts_with("<Redirect>") && tail.ends_with("</Redirect></Response>"));
    if !tail_ok {
        return Err(format!("something unexpected follows the stream: {twiml}"));
    }
    let start = twiml.find("url=\"").ok_or("no stream address")? + 5;
    let rest = &twiml[start..];
    let end = rest.find('"').ok_or("unterminated stream address")?;
    Ok(rest[..end].to_string())
}

/// The surface is absent, not merely refused, when this instance has no line.
///
/// A refusal would tell anyone who asked that telephony is configured here, which is
/// worth nothing to them and something to somebody looking for a line to abuse.
#[tokio::test]
async fn an_instance_with_no_line_has_no_telephone_surface() {
    let Some(db_url) = std::env::var("DATABASE_URL").ok() else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    });
    let redis = cache::create_pool(&redis_url).expect("redis pool");
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into();
    // The same key the other test uses. At-rest encryption is resolved once per
    // process, by whichever state is built first, so two tests in this file disagreeing
    // about it would be a race: whichever lost would find encryption switched off.
    boot.message_encryption_key = MESSAGE_KEY.into();
    // The flag is off, which is the default: an instance nobody has given a telephone.
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot)).build();
    let app = http::router(state, None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await;
    });

    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}{ANSWER_PATH}"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body("CallSid=CAx&From=%2B447700900123&To=%2B441315550000")
        .send()
        .await
        .expect("the server answers");
    assert_eq!(resp.status().as_u16(), 404, "a deployment with no line must have no line to find");
}

/// The transfer number this line hands callers to.
const TRANSFER_TO: &str = "+441315557788";
const CONTINUE_PATH: &str = "/api/telephony/twilio/continue";

/// A caller asks for a person, and the call leaves us for one.
///
/// The whole of a transfer is one instruction after the media socket in the answer, and
/// what the carrier is told when it comes back to read it. So this drives a real signed
/// call, has the model ask to put the caller through, and then checks the two things that
/// could each silently ruin it: that the caller heard the sentence explaining the transfer
/// **before** the line moved, and that the answer given to the carrier afterwards names
/// the right person and presents the right number.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_is_put_through_after_hearing_why() {
    let _one_at_a_time = CARRIER.lock().await;
    let Some(h) = harness_for(
        MlScript {
            // The first tool-loop step asks to put the caller through; the reply that
            // follows is what they have to hear before anything moves.
            generate_tool_call: Some((
                "transfer_call".into(),
                json!({
                    "subject": "Wants to speak to the practice manager",
                    "summary": "Calling about a survey that went to the old address.",
                    "caller_name": "Alex Fraser",
                    "urgency": "urgent",
                }),
            )),
            generate_tokens: vec![
                "Of course, I am connecting you now. ".into(),
                "One moment please. ".into(),
            ],
            speech_samples: 24_000,
            ..MlScript::default()
        },
        Some(TRANSFER_TO),
        &["transfer_call"],
    )
    .await
    else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(60), run_transfer(&h)).await;
    cleanup(&h).await;
    outcome.expect("the transfer did not finish in time").expect("the transfer failed");
}

async fn run_transfer(h: &Harness) -> Result<(), String> {
    let call_sid = "CAtransfer000001";
    let (status, twiml) = ring(h, call_sid, &h.number, None).await;
    if status != 200 {
        return Err(format!("a properly signed call was refused: {status} {twiml}"));
    }
    // The one instruction that makes any of this possible. Without it the carrier reads
    // the rest of this answer when our socket closes, finds nothing, and ends the call.
    if !twiml.contains(&format!("<Redirect>{}{CONTINUE_PATH}</Redirect>", h.base)) {
        return Err(format!("a transferring line did not say where to come back to: {twiml}"));
    }
    let socket_url = stream_url(&twiml)?;

    let (socket, _) = tokio_tungstenite::connect_async(&socket_url)
        .await
        .map_err(|e| format!("the media socket was refused: {e}"))?;
    let (mut ws_tx, mut ws_rx) = socket.split();
    ws_tx
        .send(WsMessage::Text(
            json!({ "event": "connected", "protocol": "Call", "version": "1.0.0" }).to_string().into(),
        ))
        .await
        .map_err(|e| e.to_string())?;
    ws_tx
        .send(WsMessage::Text(
            json!({
                "event": "start",
                "sequenceNumber": "1",
                "streamSid": STREAM_SID,
                "start": {
                    "accountSid": "ACtest",
                    "streamSid": STREAM_SID,
                    "callSid": call_sid,
                    "tracks": ["inbound"],
                    "mediaFormat": { "encoding": "audio/x-mulaw", "sampleRate": 8000, "channels": 1 },
                    "customParameters": {},
                },
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|e| e.to_string())?;

    // The line talks first here too, and until the notice is out nothing the caller says
    // is listened to, so the task that keeps the line alive has to be running for it.
    let mode = Arc::new(std::sync::atomic::AtomicU8::new(MODE_SILENT));
    let (echo_tx, echo_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let line = tokio::spawn(carry_the_line(ws_tx, mode.clone(), call_sid.to_string(), echo_rx));
    hear_the_notice(&mut ws_rx, &echo_tx, &mode).await?;

    // Then the caller asks for a person.
    let speech_ms = MIN_SPEECH_MS + 20 * codec::FRAME_MS as u64;
    mode.store(MODE_SPEAKING, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(speech_ms)).await;
    mode.store(MODE_SILENT, std::sync::atomic::Ordering::SeqCst);

    // Read until the server closes the socket, which is how a transfer begins: it stops
    // speaking, lets go, and the carrier comes back to ask what next.
    let mut frames = 0usize;
    let mut closed_after_audio = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(30), ws_rx.next()).await {
            // The socket ended. Whether any reply audio had gone by first is the whole
            // question: closing before it would put the caller through mid-sentence.
            Ok(None) | Ok(Some(Err(_))) | Err(_) => {
                closed_after_audio = frames > 0;
                break;
            }
            Ok(Some(Ok(msg))) => {
                let WsMessage::Text(text) = msg else { continue };
                let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                match value["event"].as_str() {
                    Some("media") => frames += 1,
                    Some("mark") => {
                        let name = value["mark"]["name"].as_str().unwrap_or_default().to_string();
                        let _ = echo_tx.send(name);
                    }
                    _ => {}
                }
            }
        }
    }
    line.abort();
    if frames == 0 {
        return Err(format!(
            "the caller heard nothing at all before the line moved (generation {}, synthesis {})",
            h.ml.calls.generates(),
            h.ml.calls.speeches(),
        ));
    }
    if !closed_after_audio {
        return Err("the socket never closed, so the call was never handed over".into());
    }

    // ---- The record of it. ----
    // The ability has to have been put in front of the model at all: it is offered only
    // on a call, and only on a line with somewhere to put people through to, so a
    // silently unoffered tool would look exactly like a model that chose not to use it.
    if !h.ml.calls.was_offered("transfer_call") {
        return Err(format!(
            "putting a caller through was never offered: {:?}",
            h.ml.calls.offered()
        ));
    }
    let (outcome, transfer_to) = wait_for_transferred(h, call_sid).await?;
    if outcome != "transferred" {
        return Err(format!(
            "the call was recorded as {outcome}, not as put through (to {transfer_to:?})"
        ));
    }
    if transfer_to.as_deref() != Some(TRANSFER_TO) {
        return Err(format!("the call was to be put through to {transfer_to:?}"));
    }

    // Whoever picks the telephone up gets what the caller had already explained, so they
    // do not ask it all again.
    let handover = sqlx::query_as::<_, (String, String, String)>(
        "SELECT e.subject, e.body, e.urgency FROM enquiries e \
           JOIN calls c ON c.id = e.call_id \
          WHERE c.provider_call_id = $1 AND e.kind = 'handover'",
    )
    .bind(call_sid)
    .fetch_optional(&h.pg)
    .await
    .map_err(|e| format!("could not read the handover: {e}"))?
    .ok_or("no handover was written for the person picking up")?;
    if !handover.0.contains("practice manager") || !handover.1.contains("survey") {
        return Err(format!("the handover lost what the caller wanted: {handover:?}"));
    }
    if handover.2 != "urgent" {
        return Err(format!("the handover was recorded as {}", handover.2));
    }

    // ---- What the carrier is told when it comes back. ----
    let (status, body) = ask_what_next(h, call_sid, None, Some("")).await;
    if status != 403 || !body.is_empty() {
        return Err(format!("an unsigned continuation was not refused: {status} {body}"));
    }

    let (status, body) = ask_what_next(h, call_sid, None, None).await;
    if status != 200 || !body.contains(&format!(">{TRANSFER_TO}</Dial>")) {
        return Err(format!("the caller was not put through: {status} {body}"));
    }
    // The number presented is the line's own: the one this deployment owns and is
    // entitled to present. Who rang is unverified and belongs in the written record.
    if !body.contains(&format!("callerId=\"{}\"", h.number)) {
        return Err(format!("the transfer presented the wrong number: {body}"));
    }

    // Nobody picked up. The caller is told so rather than left listening to nothing.
    let (status, body) = ask_what_next(h, call_sid, Some("no-answer"), None).await;
    if status != 200 || !body.contains("<Say>") || !body.contains("<Hangup/>") {
        return Err(format!("an unanswered transfer said nothing: {status} {body}"));
    }
    // And when somebody does pick up, there is nothing more to say.
    let (status, body) = ask_what_next(h, call_sid, Some("completed"), None).await;
    if status != 200 || body.contains("<Say>") {
        return Err(format!("an answered transfer said something it should not: {status} {body}"));
    }

    // A call nobody asked to transfer is simply over, which is every call on a line that
    // has a transfer number and did not use it.
    let (status, body) = ask_what_next(h, "CAnever-heard-of-it", None, None).await;
    if status != 200 || !body.contains("<Hangup/>") || body.contains("<Dial") {
        return Err(format!("an unknown call was offered a transfer: {status} {body}"));
    }
    Ok(())
}

/// The call row, once the socket's teardown has written it.
async fn wait_for_transferred(
    h: &Harness,
    call_sid: &str,
) -> Result<(String, Option<String>), String> {
    for _ in 0..100 {
        let row = sqlx::query_as::<_, (String, Option<String>, bool)>(
            "SELECT outcome, transfer_to, (ended_at IS NOT NULL) FROM calls \
              WHERE provider_call_id = $1",
        )
        .bind(call_sid)
        .fetch_optional(&h.pg)
        .await
        .map_err(|e| format!("could not read the call: {e}"))?;
        if let Some((outcome, to, ended)) = row {
            if ended {
                return Ok((outcome, to));
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("the call never finished".into())
}

/// A synthesiser that is reachable and refuses everything.
///
/// The interesting failure is not a closed port but an engine that answers with an error,
/// because that is what an overloaded or misconfigured one looks like, and it is the case
/// the line has to survive without carrying a caller it cannot talk to.
async fn refusing_synthesiser() -> String {
    let app = axum::Router::new().fallback(|| async {
        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "no synthesis today")
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://127.0.0.1:{port}")
}

/// A line that cannot tell the caller what they are speaking to does not take the call.
///
/// Fail closed, and the whole of why it is worth a test of its own: every other failure in
/// this file leaves the caller talking to something, whereas this one is the deliberate
/// refusal to listen to somebody who has not been told. The alternative, answering and
/// conversing in silence about it, is the one outcome that would be a compliance failure
/// rather than a fault.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_that_cannot_say_what_it_is_does_not_take_the_call() {
    let _one_at_a_time = CARRIER.lock().await;
    let Some(h) = harness().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(60), run_notice_failure(&h)).await;
    cleanup(&h).await;
    outcome.expect("the call did not finish in time").expect("the call was mishandled");
}

async fn run_notice_failure(h: &Harness) -> Result<(), String> {
    // Configured, reachable, and refusing: the line is answered on the strength of having a
    // streaming synthesiser, and only discovers at the notice that it has nothing to say.
    let refusing = refusing_synthesiser().await;
    set(&h.pg, "voice.tts_stream_url", &refusing, ConfigValueType::String).await;

    let call_sid = "CAnonotice000001";
    let (status, twiml) = ring(h, call_sid, &h.number, None).await;
    if status != 200 || !twiml.contains("<Connect>") {
        return Err(format!("the call was not answered: {status} {twiml}"));
    }
    let socket_url = stream_url(&twiml)?;
    let (socket, _) = tokio_tungstenite::connect_async(&socket_url)
        .await
        .map_err(|e| format!("the media socket was refused: {e}"))?;
    let (mut ws_tx, mut ws_rx) = socket.split();
    ws_tx
        .send(WsMessage::Text(
            json!({ "event": "connected", "protocol": "Call", "version": "1.0.0" }).to_string().into(),
        ))
        .await
        .map_err(|e| e.to_string())?;
    ws_tx
        .send(WsMessage::Text(
            json!({
                "event": "start",
                "sequenceNumber": "1",
                "streamSid": STREAM_SID,
                "start": {
                    "accountSid": "ACtest",
                    "streamSid": STREAM_SID,
                    "callSid": call_sid,
                    "tracks": ["inbound"],
                    "mediaFormat": { "encoding": "audio/x-mulaw", "sampleRate": 8000, "channels": 1 },
                    "customParameters": {},
                },
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|e| e.to_string())?;

    // The caller talks throughout, which is what makes the assertion below worth making:
    // there is plenty for the line to have listened to, and it must not have.
    let mode = Arc::new(std::sync::atomic::AtomicU8::new(MODE_SPEAKING));
    let (_echo_tx, echo_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let line = tokio::spawn(carry_the_line(ws_tx, mode, call_sid.to_string(), echo_rx));

    let mut frames = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut closed = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(20), ws_rx.next()).await {
            Ok(None) | Ok(Some(Err(_))) | Err(_) => {
                closed = true;
                break;
            }
            Ok(Some(Ok(WsMessage::Text(text)))) => {
                let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                if value["event"].as_str() == Some("media") {
                    frames += 1;
                }
            }
            Ok(Some(Ok(_))) => continue,
        }
    }
    line.abort();
    if frames != 0 {
        return Err(format!("{frames} frames of audio went out on a line that could not speak"));
    }
    if !closed {
        return Err("the call was left open with nothing being said on it".into());
    }

    // The record: answered, ended for this reason, and with nothing kept about a caller who
    // was never told anything.
    for _ in 0..100 {
        let row = sqlx::query_as::<_, (String, bool, Option<Uuid>, bool)>(
            "SELECT outcome, ended_at IS NOT NULL, chat_id, notice_at IS NOT NULL \
               FROM calls WHERE provider = 'twilio' AND provider_call_id = $1",
        )
        .bind(call_sid)
        .fetch_optional(&h.pg)
        .await
        .map_err(|e| e.to_string())?;
        if let Some((outcome, ended, chat_id, notice_given)) = row {
            if !ended {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            if outcome != "notice_failed" {
                return Err(format!("the call was recorded as {outcome:?}"));
            }
            if notice_given {
                return Err("the call claims the caller was told something".into());
            }
            if chat_id.is_some() {
                return Err("a conversation was kept for a caller who was never told anything".into());
            }
            if h.ml.calls.transcribes() != 0 || h.ml.calls.generates() != 0 {
                return Err("the caller was listened to without having been told".into());
            }
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("the call was never recorded as finished".into())
}

/// Ask the continuation endpoint what to do, the way the carrier does.
async fn ask_what_next(
    h: &Harness,
    call_sid: &str,
    dial_status: Option<&str>,
    signature: Option<&str>,
) -> (u16, String) {
    let mut params: Vec<(&str, &str)> = vec![
        ("AccountSid", "ACtest"),
        ("CallSid", call_sid),
        ("From", &h.caller),
        ("To", &h.number),
    ];
    if let Some(s) = dial_status {
        params.push(("DialCallStatus", s));
    }
    let url = format!("{}{CONTINUE_PATH}", h.base);
    let sig = signature.map(str::to_string).unwrap_or_else(|| sign(&url, &params));
    let resp = reqwest::Client::new()
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("X-Twilio-Signature", sig)
        .body(form_body(&params))
        .send()
        .await
        .expect("the continuation endpoint answers");
    (resp.status().as_u16(), resp.text().await.unwrap_or_default())
}
