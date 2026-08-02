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

//! A call carried on the practice's own telephone system, played out against the real
//! server by something pretending to be one.
//!
//! The point of this file is that the second transport carries a whole call rather than
//! just bytes: the same session, the same notice, the same conversation, over a socket
//! nobody else can see. So it drives the real listener with the real protocol, asks the
//! real endpoint what to do with a call, and reads real audio back.
//!
//! What is proved here and nowhere else: that the identifier a connection presents is the
//! single-use ticket, so an open port is not an open door; that the reply arrives as raw
//! samples rather than the companded bytes the carrier path sends; and that the notice is
//! spoken before anything the caller says is listened to, on a transport that cannot
//! confirm having played it.
//!
//! Needs a reachable Postgres and Redis; skips when `DATABASE_URL` is unset.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::mock_ml::{self, MlScript};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use uuid::Uuid;

use fosnie_backend::config::runtime::{self, ConfigValueType};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::telephony::frame::{self, Message, Step};
use fosnie_backend::{cache, db, http};

/// The secret the telephone system presents.
const KEY: &str = "a-shared-secret-for-tests";
const KEY_HEADER: &str = "x-fosnie-telephony-key";
const ANSWER_PATH: &str = "/api/telephony/audiosocket/answer";
const CONTINUE_PATH: &str = "/api/telephony/audiosocket/continue";
const MESSAGE_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// The turn-taking this file drives, stated rather than inherited.
const TURN_SILENCE_MS: u64 = 900;
const MIN_SPEECH_MS: u64 = 250;

/// One call at a time in this file.
///
/// Every test here writes deployment-wide voice and telephony settings and puts them back.
/// Overlapping, one would restore the secret and the synthesiser while another was still
/// mid-call, which shows up as a properly presented identifier being refused.
static LINE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Harness {
    base: String,
    listen: String,
    pg: PgPool,
    state: AppState,
    ml: mock_ml::MockMl,
    owner: Uuid,
    agent: Uuid,
    line: Uuid,
    number: String,
    caller: String,
    borrowed: Vec<(String, Option<String>)>,
}

/// The settings this file has to control, and therefore has to put back.
const BORROWED_KEYS: [&str; 16] = [
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

async fn harness(transfer: Option<&str>) -> Option<Harness> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:16379".into());
    let pg = db::connect(&db_url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    });
    let redis = cache::create_pool(&redis_url).expect("redis pool");
    let ml = mock_ml::spawn(MlScript {
        transcript: "I would like an appointment".into(),
        generate_tokens: vec![
            "Of course, let me look. ".into(),
            "I have Tuesday morning free. ".into(),
        ],
        speech_samples: 24_000,
        ..MlScript::default()
    })
    .await;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.ml.base_url = ml.base_url.clone();
    boot.features.voice = true;
    boot.features.voice_live = true;
    boot.features.telephony = true;
    boot.voice_live.tts_stream = true;
    boot.voice_live.tts_stream_url = ml.base_url.clone();
    boot.voice_live.stt_sample_rate = 16_000;
    boot.message_encryption_key = MESSAGE_KEY.into();
    boot.server.static_dir = "___no_spa___".into();

    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot)).build();
    let tag = Uuid::now_v7().as_u128() % 100_000;
    let number = format!("+44131666{tag:05}");
    let caller = format!("+44770066{tag:05}");
    let owner = mk_user(&pg).await;
    let agent = mk_agent(&pg, owner).await;
    let line = mk_line(&pg, owner, agent, &number, transfer).await;

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

    let mut borrowed = Vec::new();
    for key in BORROWED_KEYS {
        borrowed.push((key.to_string(), runtime::get(&pg, key).await.ok().flatten().map(|e| e.value)));
    }
    for key in ["telephony.audiosocket_key_enc", "telephony.audiosocket_listen"] {
        borrowed.push((key.to_string(), runtime::get(&pg, key).await.ok().flatten().map(|e| e.value)));
    }

    set(&pg, "voice.phone.silence_threshold_ms", &TURN_SILENCE_MS.to_string(), ConfigValueType::Int).await;
    set(&pg, "voice.phone.min_speech_ms", &MIN_SPEECH_MS.to_string(), ConfigValueType::Int).await;
    set(&pg, "voice.phone.turn_detection", "false", ConfigValueType::Bool).await;
    set(&pg, "voice.turn_detector_url", "", ConfigValueType::String).await;
    set(&pg, "voice.silence_threshold_ms", "", ConfigValueType::String).await;
    set(&pg, "voice.stt_stream_kind", "none", ConfigValueType::String).await;
    set(&pg, "voice.stt_stream_url", "", ConfigValueType::String).await;
    set(&pg, "voice.stt_sample_rate", "16000", ConfigValueType::Int).await;
    set(&pg, "voice.tts_stream", "true", ConfigValueType::Bool).await;
    set(&pg, "voice.tts_stream_url", &ml.base_url, ConfigValueType::String).await;
    set(&pg, "voice.tts_model", "mock", ConfigValueType::String).await;
    set(&pg, "voice.tts_voice", "", ConfigValueType::String).await;
    set(&pg, "voice.tts_api_key_enc", "", ConfigValueType::String).await;
    set(&pg, "telephony.provider", "audiosocket", ConfigValueType::String).await;
    set(&pg, "telephony.public_base_url", &base, ConfigValueType::String).await;
    set(&pg, "telephony.max_concurrent_calls", "1", ConfigValueType::Int).await;
    let ct = fosnie_backend::crypto::encrypt_at_rest(KEY).expect("encrypts");
    set(&pg, "telephony.audiosocket_key_enc", &ct, ConfigValueType::String).await;

    // The listener, started the way the server starts it, on a port the operating system
    // picks so two runs never contend.
    let media = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listen = media.local_addr().unwrap().to_string();
    drop(media);
    set(&pg, "telephony.audiosocket_listen", &listen, ConfigValueType::String).await;
    let (_tx, rx) = tokio::sync::watch::channel(false);
    {
        let st = state.clone();
        let addr = listen.clone();
        tokio::spawn(async move {
            fosnie_backend::telephony::audiosocket::listen(st, addr, rx).await;
        });
    }
    // Bound before anything tries to connect.
    for _ in 0..50 {
        if TcpStream::connect(&listen).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    std::mem::forget(_tx);

    Some(Harness { base, listen, pg, state, ml, owner, agent, line, number, caller, borrowed })
}

async fn set(pg: &PgPool, key: &str, value: &str, t: ConfigValueType) {
    runtime::set(pg, key, value, t, "global", None, "system").await.expect("write setting");
}

async fn cleanup(h: &Harness) {
    for (key, was) in &h.borrowed {
        match was {
            Some(value) => {
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
    let _ = sqlx::query("DELETE FROM calls WHERE owner_user_id = $1").bind(h.owner).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM phone_numbers WHERE id = $1").bind(h.line).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM messages WHERE chat_id IN (SELECT id FROM chats WHERE owner_user_id = $1)")
        .bind(h.owner)
        .execute(&h.pg)
        .await;
    let _ = sqlx::query("DELETE FROM chats WHERE owner_user_id = $1").bind(h.owner).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM agents WHERE id = $1").bind(h.agent).execute(&h.pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(h.owner).execute(&h.pg).await;
}

async fn mk_user(pg: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'Line owner', $2, 'user')")
        .bind(id)
        .bind(format!("{id}@example.test"))
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn mk_agent(pg: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO agents (id, name, description, system_prompt, created_by, modes) \
         VALUES ($1, 'Reception', '', 'Answer the telephone.', $2, ARRAY['general'])",
    )
    .bind(id)
    .bind(owner)
    .execute(pg)
    .await
    .unwrap();
    id
}

/// A line answered by the practice's own telephone system, switched on.
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
         VALUES ($1, $2, 'audiosocket', $3, $4, true, $5)",
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

/// Ask what to do with a call, the way a dialplan does.
async fn ask(h: &Harness, to: &str, key: Option<&str>) -> (u16, String) {
    let url = format!("{}{ANSWER_PATH}?from={}&to={}", h.base, urlencode(&h.caller), urlencode(to));
    let mut req = reqwest::Client::new().get(&url);
    if let Some(k) = key {
        req = req.header(KEY_HEADER, k);
    }
    let resp = req.send().await.expect("the endpoint answers");
    (resp.status().as_u16(), resp.text().await.unwrap_or_default())
}

fn urlencode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// 20 ms of narrowband audio as the telephone system sends it: raw samples, not companded.
fn media(loud: bool, phase: usize) -> Vec<u8> {
    use std::f32::consts::PI;
    let samples: Vec<i16> = (0..160)
        .map(|n| {
            if !loud {
                return 0;
            }
            let t = (phase * 160 + n) as f32 / 8_000.0;
            (11_000.0 * (2.0 * PI * 440.0 * t).sin()) as i16
        })
        .collect();
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    frame::encode(&Message::Audio(bytes))
}

/// A whole call: the identifier, the notice, a turn, and hanging up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_on_the_practices_own_system_is_carried_end_to_end() {
    let _one_at_a_time = LINE.lock().await;
    let Some(h) = harness(None).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(90), run_call(&h)).await;
    cleanup(&h).await;
    outcome.expect("the call did not finish in time").expect("the call failed");
}

async fn run_call(h: &Harness) -> Result<(), String> {
    // ---- The secret is the whole of a telephone system's authentication. ----
    let (status, body) = ask(h, &h.number, None).await;
    if status != 403 || !body.is_empty() {
        return Err(format!("an unauthenticated request was answered: {status} {body}"));
    }
    let (status, _) = ask(h, &h.number, Some("not-the-secret")).await;
    if status != 403 {
        return Err(format!("a wrong secret was accepted: {status}"));
    }
    // A number no line is registered on, properly authenticated: still refused.
    let (status, _) = ask(h, "+441316669999", Some(KEY)).await;
    if status != 403 {
        return Err(format!("a call to an unknown number was taken: {status}"));
    }

    // ---- The real call. ----
    let (status, ticket) = ask(h, &h.number, Some(KEY)).await;
    if status != 200 || Uuid::parse_str(ticket.trim()).is_err() {
        return Err(format!("the answer was not an identifier: {status} {ticket:?}"));
    }
    let ticket = ticket.trim().to_string();

    let mut sock = TcpStream::connect(&h.listen).await.map_err(|e| format!("no connection: {e}"))?;
    sock.set_nodelay(true).ok();
    let id = *Uuid::parse_str(&ticket).unwrap().as_bytes();
    sock.write_all(&frame::encode(&Message::Id(id))).await.map_err(|e| e.to_string())?;

    // A telephone line never goes quiet: it carries silence at the same twenty
    // milliseconds a frame as it carries speech, for as long as the call is up. So the
    // line is played out by a task from the moment it connects, because a connection that
    // simply stopped sending is a dead line and the server is right to end it.
    let (mut rx, tx) = sock.into_split();
    let mode = Arc::new(std::sync::atomic::AtomicU8::new(QUIET));
    let line = tokio::spawn(carry_the_line(tx, mode.clone()));

    // ---- What the caller hears first. ----
    let notice = read_audio(&mut rx, Duration::from_secs(30), Duration::from_millis(700)).await?;
    if notice.frames < 4 {
        return Err(format!(
            "the caller heard {} frames of notice (synthesis {})",
            notice.frames,
            h.ml.calls.speeches()
        ));
    }
    if notice.frame_bytes != frame::AUDIO_FRAME_BYTES {
        return Err(format!(
            "a frame of {} bytes is not twenty milliseconds of raw samples",
            notice.frame_bytes
        ));
    }
    if notice.silent {
        return Err("the notice was silence all the way through".into());
    }
    if h.ml.calls.transcribes() != 0 {
        return Err("something was recognised before the notice had been given".into());
    }
    // The words themselves, so a line that said "hello" and stopped would not pass.
    let said = h.ml.calls.spoken_texts().join(" ");
    for must in ["automated assistant", "written down"] {
        if !said.contains(must) {
            return Err(format!("the notice never said {must:?}: {said:?}"));
        }
    }

    // ---- The caller's turn. ----
    // Spoken by the line task at the rate a telephone sends, then quiet. What makes this a
    // turn is the amount of audio, and the counts come from the dials the harness set.
    mode.store(LOUD, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(MIN_SPEECH_MS + 400)).await;
    mode.store(QUIET, std::sync::atomic::Ordering::SeqCst);

    let reply = read_audio(&mut rx, Duration::from_secs(45), Duration::from_millis(700)).await?;
    if reply.frames < 4 {
        return Err(format!(
            "only {} frames of reply (recognition {}, generation {}, synthesis {})",
            reply.frames,
            h.ml.calls.transcribes(),
            h.ml.calls.generates(),
            h.ml.calls.speeches(),
        ));
    }
    if reply.silent {
        return Err("the reply was silence all the way through".into());
    }
    // What reached recognition, and at the rate the session works at rather than the
    // line's: audio at the wrong rate scales every turn-taking threshold there is.
    let audio = h.ml.calls.transcribed_audio();
    if audio.is_empty() {
        return Err("nothing the caller said reached recognition".into());
    }
    let wav = &audio[0];
    if &wav[..4] != b"RIFF" {
        return Err("recognition was not handed an audio file".into());
    }
    let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
    if rate != 16_000 {
        return Err(format!("recognition was handed {rate} Hz audio"));
    }
    // Synthesis was asked for raw samples, as it is on the carrier path: nothing here
    // could decode anything else.
    let formats = h.ml.calls.speech_formats();
    if formats.is_empty() || formats.iter().any(|f| f != "pcm") {
        return Err(format!("synthesis was asked for {formats:?} rather than raw samples"));
    }

    // ---- Hanging up. ----
    // The telephone system says so, in its own words, on the same connection.
    mode.store(HANG_UP, std::sync::atomic::Ordering::SeqCst);
    let _ = line.await;
    drop(rx);

    // ---- What the log recorded. ----
    let logged = wait_for_closed(h, &ticket).await?;
    if logged.0 != "completed" && logged.0 != "dropped" {
        return Err(format!("the call was recorded as {}", logged.0));
    }
    if logged.1 != Some(h.line) {
        return Err("the call was not attributed to the line it came in on".into());
    }
    let Some(chat_id) = logged.2 else {
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
    // And the words the caller was told are beside the call.
    let told = sqlx::query_scalar::<_, Option<String>>(
        "SELECT notice_text FROM calls WHERE provider = 'audiosocket' AND provider_call_id = $1",
    )
    .bind(&ticket)
    .fetch_one(&h.pg)
    .await
    .map_err(|e| e.to_string())?;
    if !told.unwrap_or_default().contains("automated assistant") {
        return Err("the call does not record what the caller was told".into());
    }
    Ok(())
}

const QUIET: u8 = 0;
const LOUD: u8 = 1;
const HANG_UP: u8 = 2;

/// Keep the line alive for the rest of the call, as a telephone system does: a frame every
/// twenty milliseconds, of silence or of speech, until told to hang up.
async fn carry_the_line(mut tx: tokio::net::tcp::OwnedWriteHalf, mode: Arc<std::sync::atomic::AtomicU8>) {
    let mut phase = 0usize;
    loop {
        match mode.load(std::sync::atomic::Ordering::SeqCst) {
            HANG_UP => {
                let _ = tx.write_all(&frame::encode(&Message::Hangup)).await;
                return;
            }
            m => {
                if tx.write_all(&media(m == LOUD, phase)).await.is_err() {
                    return;
                }
                phase += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

struct Heard {
    frames: usize,
    frame_bytes: usize,
    silent: bool,
}

/// Read audio until the line stops speaking for `quiet_for`.
async fn read_audio(
    rx: &mut tokio::net::tcp::OwnedReadHalf,
    overall: Duration,
    quiet_for: Duration,
) -> Result<Heard, String> {
    let deadline = tokio::time::Instant::now() + overall;
    let mut buf: Vec<u8> = Vec::new();
    let mut heard = Heard { frames: 0, frame_bytes: 0, silent: true };
    loop {
        loop {
            match frame::step(&buf) {
                Step::Got(Message::Audio(payload), used) => {
                    heard.frames += 1;
                    heard.frame_bytes = payload.len();
                    if payload.iter().any(|b| *b != 0) {
                        heard.silent = false;
                    }
                    buf.drain(..used);
                }
                Step::Got(_, used) => {
                    buf.drain(..used);
                }
                Step::More => break,
                Step::Broken(why) => return Err(format!("the line sent something unreadable: {why}")),
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(heard);
        }
        let mut chunk = [0u8; 4096];
        // Once audio has started, a gap means the line has finished speaking.
        let wait = if heard.frames > 0 { quiet_for } else { overall };
        match tokio::time::timeout(wait, rx.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) => return Ok(heard),
            Ok(Ok(n)) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return Ok(heard),
        }
    }
}

async fn wait_for_closed(h: &Harness, ticket: &str) -> Result<(String, Option<Uuid>, Option<Uuid>), String> {
    for _ in 0..200 {
        let row = sqlx::query_as::<_, (String, Option<Uuid>, Option<Uuid>, bool)>(
            "SELECT outcome, phone_number_id, chat_id, ended_at IS NOT NULL FROM calls \
              WHERE provider = 'audiosocket' AND provider_call_id = $1",
        )
        .bind(ticket)
        .fetch_optional(&h.pg)
        .await
        .map_err(|e| e.to_string())?;
        if let Some((outcome, line, chat, ended)) = row {
            if ended {
                return Ok((outcome, line, chat));
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("the call never finished".into())
}

/// An identifier nobody minted carries no call, and neither does one that has been used.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_identifier_that_was_not_minted_carries_nothing() {
    let _one_at_a_time = LINE.lock().await;
    let Some(h) = harness(None).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = tokio::time::timeout(Duration::from_secs(60), run_unminted(&h)).await;
    cleanup(&h).await;
    outcome.expect("the connection was not dealt with in time").expect("the connection was mishandled");
}

async fn run_unminted(h: &Harness) -> Result<(), String> {
    // Invented.
    let invented = Uuid::now_v7();
    let heard = present(h, *invented.as_bytes()).await?;
    if heard.frames != 0 {
        return Err(format!("{} frames went out on an identifier nobody minted", heard.frames));
    }
    if state_has_calls(h) {
        return Err("a call was registered for an identifier nobody minted".into());
    }

    // Minted, used, and presented a second time: the ticket is redeemed once.
    let (_, ticket) = ask(h, &h.number, Some(KEY)).await;
    let ticket = ticket.trim().to_string();
    let id = *Uuid::parse_str(&ticket).map_err(|e| e.to_string())?.as_bytes();
    let first = present(h, id).await?;
    if first.frames == 0 {
        return Err("a properly presented identifier was refused".into());
    }
    // Let the first call finish tearing down before the second attempt.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let second = present(h, id).await?;
    if second.frames != 0 {
        return Err("a spent identifier opened a second connection".into());
    }
    Ok(())
}

fn state_has_calls(h: &Harness) -> bool {
    !h.state.telephony.is_empty()
}

/// Open a connection, present an identifier, and report what came back before the far end
/// gave up on us.
async fn present(h: &Harness, id: [u8; 16]) -> Result<Heard, String> {
    let mut sock = TcpStream::connect(&h.listen).await.map_err(|e| format!("no connection: {e}"))?;
    sock.set_nodelay(true).ok();
    sock.write_all(&frame::encode(&Message::Id(id))).await.map_err(|e| e.to_string())?;
    let (mut rx, tx) = sock.into_split();
    let heard = read_audio(&mut rx, Duration::from_secs(8), Duration::from_millis(700)).await?;
    drop(tx);
    drop(rx);
    Ok(heard)
}

/// What the telephone system is told to do once our side has finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_continuation_says_who_to_ring_and_nothing_more() {
    let _one_at_a_time = LINE.lock().await;
    let Some(h) = harness(Some("+441316667788")).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = run_continue(&h).await;
    cleanup(&h).await;
    outcome.expect("the continuation was wrong");
}

async fn run_continue(h: &Harness) -> Result<(), String> {
    // A call that ended without a transfer: nothing to say.
    let plain = Uuid::now_v7().to_string();
    mk_call_row(h, &plain, None).await;
    let (status, body) = continue_for(h, &plain, Some(KEY)).await;
    if status != 200 || !body.is_empty() {
        return Err(format!("an ordinary call was offered a transfer: {status} {body:?}"));
    }

    // One where the agent put the caller through.
    let handed = Uuid::now_v7().to_string();
    mk_call_row(h, &handed, Some("+441316667788")).await;
    let (status, body) = continue_for(h, &handed, Some(KEY)).await;
    if status != 200 || body.trim() != "+441316667788" {
        return Err(format!("the caller was not put through: {status} {body:?}"));
    }

    // And the same question without the secret.
    let (status, body) = continue_for(h, &handed, None).await;
    if status != 403 || !body.is_empty() {
        return Err(format!("an unauthenticated continuation was answered: {status} {body}"));
    }

    // A call nobody has heard of is simply over.
    let (status, body) = continue_for(h, "never-heard-of-it", Some(KEY)).await;
    if status != 200 || !body.is_empty() {
        return Err(format!("an unknown call was offered something: {status} {body:?}"));
    }
    Ok(())
}

async fn mk_call_row(h: &Harness, id: &str, transfer: Option<&str>) {
    sqlx::query(
        "INSERT INTO calls (id, phone_number_id, provider, provider_call_id, to_e164, from_e164, \
                            owner_user_id, agent_id, outcome, ended_at, transfer_to) \
         VALUES ($1, $2, 'audiosocket', $3, $4, $5, $6, $7, 'completed', now(), $8)",
    )
    .bind(Uuid::now_v7())
    .bind(h.line)
    .bind(id)
    .bind(&h.number)
    .bind(&h.caller)
    .bind(h.owner)
    .bind(h.agent)
    .bind(transfer)
    .execute(&h.pg)
    .await
    .unwrap();
}

async fn continue_for(h: &Harness, call: &str, key: Option<&str>) -> (u16, String) {
    let url = format!("{}{CONTINUE_PATH}?call={}", h.base, urlencode(call));
    let mut req = reqwest::Client::new().get(&url);
    if let Some(k) = key {
        req = req.header(KEY_HEADER, k);
    }
    let resp = req.send().await.expect("the endpoint answers");
    (resp.status().as_u16(), resp.text().await.unwrap_or_default())
}
