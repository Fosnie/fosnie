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

//! A turn detector that stops answering must not stop the conversation.
//!
//! Semantic turn detection ends a turn only when the detector agrees the speaker has
//! finished, which is the whole point: it holds a mid-thought pause that a silence timer
//! would cut. But there is no answer a detector can give that means "I have stopped
//! working", so a detector that is not there would hold every turn for ever. In a browser
//! tab that is a stalled interface. On a telephone it is a call that stays open, silent,
//! and billed, until the caller gives up.
//!
//! Neither the swallowed error nor the fallback is visible from a unit test: the decision
//! function is pure and never sees the failure, and the failure happens inside a loop that
//! owns its own state. So this drives a real session against a detector that refuses every
//! request, and requires a turn to happen anyway.
//!
//! Needs a reachable Postgres; skips when `DATABASE_URL` is unset.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::routing::post;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use common::mock_ml::{self, MlScript};
use sqlx::PgPool;
use tokio::sync::mpsc;
use uuid::Uuid;

use fosnie_backend::config::runtime::{self, ConfigValueType};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::voice::{Session, VoiceProfile, WebSocketSink};
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{auth, cache, db};

/// The dials and engine settings this test writes. Deployment-wide rows, so whatever was
/// there is put back: a developer's own voice configuration is not this test's to remove.
const TOUCHED: [(&str, ConfigValueType); 6] = [
    ("voice.turn_detection", ConfigValueType::Bool),
    ("voice.turn_detector_url", ConfigValueType::String),
    ("voice.silence_threshold_ms", ConfigValueType::Int),
    ("voice.min_speech_ms", ConfigValueType::Int),
    ("voice.stt_stream_kind", ConfigValueType::String),
    ("voice.stt_sample_rate", ConfigValueType::Int),
];

/// A sidecar that is reachable and refuses everything. The interesting failure is not a
/// closed port but a detector that answers with an error, because that is what a wedged
/// or overloaded one looks like and it is the case the code has to survive.
async fn refusing_detector() -> String {
    let app = axum::Router::new()
        .route("/detect", post(|| async { (StatusCode::SERVICE_UNAVAILABLE, "no") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://127.0.0.1:{port}")
}

async fn harness() -> Option<(PgPool, AppState, mock_ml::MockMl)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:16379".into());
    let pg = db::connect(&db_url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    });
    let redis = cache::create_pool(&redis_url).expect("redis pool");
    let ml = mock_ml::spawn(MlScript {
        transcript: "How much rain fell?".into(),
        // Nothing here plays audio, so the reply is left short.
        generate_tokens: vec!["It rained.".into()],
        ..MlScript::default()
    })
    .await;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.ml.base_url = ml.base_url.clone();
    boot.features.voice = true;
    boot.features.voice_live = true;
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot)).build();
    Some((pg, state, ml))
}

async fn mk_user(pg: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'T', $2, 'user')")
        .bind(id)
        .bind(format!("{id}@example.test"))
        .execute(pg)
        .await
        .unwrap();
    id
}

/// 20 ms of 16 kHz audio, loud or silent.
fn frame(loud: bool, phase: usize) -> String {
    use std::f32::consts::PI;
    let bytes: Vec<u8> = (0..320)
        .flat_map(|n| {
            let v = if loud {
                let t = (phase * 320 + n) as f32 / 16_000.0;
                (11_000.0 * (2.0 * PI * 440.0 * t).sin()) as i16
            } else {
                0
            };
            v.to_le_bytes()
        })
        .collect();
    B64.encode(&bytes)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_detector_that_refuses_does_not_stop_a_turn() {
    let Some((pg, state, ml)) = harness().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };

    let mut was = Vec::new();
    for (key, kind) in TOUCHED {
        was.push((key, kind, runtime::get(&pg, key).await.ok().flatten().map(|e| e.value)));
    }

    let detector = refusing_detector().await;
    let write = |k: &'static str, v: String, t: ConfigValueType| {
        let pg = pg.clone();
        async move { runtime::set(&pg, k, &v, t, "global", None, "system").await.expect("write") }
    };
    // Turn detection on and pointed at a sidecar that refuses. Recognition batched
    // through the mock, so no streaming engine is needed.
    write("voice.turn_detection", "true".into(), ConfigValueType::Bool).await;
    write("voice.turn_detector_url", detector, ConfigValueType::String).await;
    write("voice.silence_threshold_ms", "400".into(), ConfigValueType::Int).await;
    write("voice.min_speech_ms", "200".into(), ConfigValueType::Int).await;
    write("voice.stt_stream_kind", "none".into(), ConfigValueType::String).await;
    write("voice.stt_sample_rate", "16000".into(), ConfigValueType::Int).await;

    let user = mk_user(&pg).await;
    let outcome = drive(&state, &ml, user).await;

    // Put the settings back whatever happened, then judge.
    for (key, kind, value) in &was {
        match value {
            Some(v) => {
                let _ = runtime::set(&pg, key, v, *kind, "global", None, "system").await;
            }
            None => {
                let _ = runtime::unset(&pg, key, "system").await;
            }
        }
    }
    let _ = sqlx::query("DELETE FROM messages WHERE chat_id IN (SELECT id FROM chats WHERE owner_user_id = $1)")
        .bind(user)
        .execute(&pg)
        .await;
    let _ = sqlx::query("DELETE FROM chats WHERE owner_user_id = $1").bind(user).execute(&pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pg).await;

    outcome.expect("the turn never happened");
}

async fn drive(state: &AppState, ml: &mock_ml::MockMl, user: Uuid) -> Result<(), String> {
    let ctx = auth::load_context(&state.pg, user).await.map_err(|e| e.to_string())?;
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(256);
    let sink = Arc::new(WebSocketSink::new(tx));
    let session = Session::start(
        state.clone(),
        ctx,
        Uuid::now_v7(),
        sink,
        None,
        None,
        None,
        Some("vad".into()),
        true,
        VoiceProfile::Browser,
        fosnie_backend::chat::origin::ChatOrigin::Web,
        None,
    )
    .await;

    // Speak, then go quiet for well past the silence threshold. With the detector
    // refusing, only the fallback can end this turn.
    for phase in 0..30 {
        session.on_audio_chunk(frame(true, phase), phase as u64).await;
    }
    for phase in 30..150 {
        session.on_audio_chunk(frame(false, phase), phase as u64).await;
    }

    // The transcript is the proof: reaching it means the turn ended, which means the
    // detector's refusal did not hold it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(ServerFrame::VoiceFinal { text })) => {
                if text.trim().is_empty() {
                    return Err("the turn ended with an empty transcript".into());
                }
                if ml.calls.transcribes() == 0 {
                    return Err("a transcript appeared without recognition being called".into());
                }
                session.shutdown().await;
                return Ok(());
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            Err(_) => break,
        }
    }
    session.shutdown().await;
    Err(format!(
        "no turn in 20 seconds: the refusing detector held it (recognition called {} times)",
        ml.calls.transcribes()
    ))
}
