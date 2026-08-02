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

//! Whether a line will answer, asked before somebody rings it.
//!
//! The check's whole value is that it reports what it found rather than what was
//! configured, and the one finding that can only be made by trying is the synthesiser: a
//! deployment that cannot speak answers a call and ends it. So this drives the check
//! against a real engine that answers, and against one that refuses, and requires it to
//! tell the two apart and to come back either way.
//!
//! It also holds the line nobody can see being crossed: a readiness report is read by more
//! people than can set the credentials it is about, so no finding may contain one. That is
//! tested by planting a recognisable secret in the settings and searching every field.
//!
//! Needs a reachable Postgres and Redis; skips when `DATABASE_URL` is unset.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::post;
use common::mock_ml::{self, MlScript};
use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::config::runtime::{self, ConfigValueType};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::telephony::preflight;
use fosnie_backend::{cache, db};

const MESSAGE_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

/// One check at a time in this file.
///
/// Every test writes the deployment-wide voice and telephony settings and puts them back,
/// and the check reads all of them. Overlapping, one test would be reading another's
/// half-written configuration and reporting on it.
static PREFLIGHT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The settings this file controls, and therefore restores.
const BORROWED: [&str; 8] = [
    "telephony.provider",
    "telephony.public_base_url",
    "telephony.auth_token_enc",
    "telephony.audiosocket_listen",
    "telephony.audiosocket_key_enc",
    "voice.tts_stream",
    "voice.tts_stream_url",
    "voice.stt_sample_rate",
];

struct Cast {
    state: AppState,
    pg: PgPool,
    ml: mock_ml::MockMl,
    owner: Uuid,
    agent: Uuid,
    line: Uuid,
    borrowed: Vec<(String, Option<String>)>,
}

async fn cast() -> Option<Cast> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:16379".into());
    let pg = db::connect(&db_url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    });
    let redis = cache::create_pool(&redis_url).expect("redis pool");
    let ml = mock_ml::spawn(MlScript { speech_samples: 4_800, ..MlScript::default() }).await;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.ml.base_url = ml.base_url.clone();
    boot.features.voice = true;
    boot.features.voice_live = true;
    boot.features.telephony = true;
    boot.message_encryption_key = MESSAGE_KEY.into();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot)).build();

    let mut borrowed = Vec::new();
    for key in BORROWED {
        borrowed.push((key.to_string(), runtime::get(&pg, key).await.ok().flatten().map(|e| e.value)));
    }

    let owner = mk_user(&pg).await;
    let agent = mk_agent(&pg, owner).await;
    let line = mk_line(&pg, owner, agent).await;
    Some(Cast { state, pg, ml, owner, agent, line, borrowed })
}

async fn clear_up(c: &Cast) {
    for (key, was) in &c.borrowed {
        match was {
            Some(v) => {
                let t = match key.as_str() {
                    "voice.tts_stream" => ConfigValueType::Bool,
                    "voice.stt_sample_rate" => ConfigValueType::Int,
                    _ => ConfigValueType::String,
                };
                let _ = runtime::set(&c.pg, key, v, t, "global", None, "system").await;
            }
            None => {
                let _ = runtime::unset(&c.pg, key, "system").await;
            }
        }
    }
    let _ = sqlx::query("DELETE FROM phone_numbers WHERE id = $1").bind(c.line).execute(&c.pg).await;
    let _ = sqlx::query("DELETE FROM agents WHERE id = $1").bind(c.agent).execute(&c.pg).await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(c.owner).execute(&c.pg).await;
}

async fn set(c: &Cast, key: &str, value: &str, t: ConfigValueType) {
    runtime::set(&c.pg, key, value, t, "global", None, "system").await.expect("write setting");
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

async fn mk_line(pg: &PgPool, owner: Uuid, agent: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO phone_numbers (id, e164, provider, owner_user_id, agent_id, enabled) \
         VALUES ($1, $2, 'twilio', $3, $4, true)",
    )
    .bind(id)
    .bind(format!("+44131777{:05}", Uuid::now_v7().as_u128() % 100_000))
    .bind(owner)
    .bind(agent)
    .execute(pg)
    .await
    .unwrap();
    id
}

/// A speech engine that is reachable and refuses everything, which is what an overloaded or
/// misconfigured one looks like from here.
async fn refusing_engine() -> String {
    let app = axum::Router::new()
        .route("/v1/audio/speech", post(|| async { (StatusCode::SERVICE_UNAVAILABLE, "no") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://127.0.0.1:{port}")
}

fn find<'a>(checks: &'a [preflight::Check], id: &str) -> &'a preflight::Check {
    checks.iter().find(|c| c.id == id).unwrap_or_else(|| panic!("no finding called {id:?}"))
}

/// A deployment with a working engine and a complete carrier setup: everything passes, and
/// the synthesiser finding says what it actually did.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deployment_that_is_ready_says_so() {
    let _one_at_a_time = PREFLIGHT.lock().await;
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    set(&c, "telephony.provider", "twilio", ConfigValueType::String).await;
    set(&c, "telephony.public_base_url", "https://calls.example.test", ConfigValueType::String).await;
    let ct = fosnie_backend::crypto::encrypt_at_rest("a-carrier-token").expect("encrypts");
    set(&c, "telephony.auth_token_enc", &ct, ConfigValueType::String).await;
    set(&c, "voice.tts_stream", "true", ConfigValueType::Bool).await;
    set(&c, "voice.tts_stream_url", &c.ml.base_url, ConfigValueType::String).await;
    set(&c, "voice.stt_sample_rate", "16000", ConfigValueType::Int).await;

    let checks = preflight::run(&c.state).await;
    let wrong: Vec<&str> = checks.iter().filter(|k| !k.ok).map(|k| k.id).collect();
    let synth = find(&checks, "synthesiser").clone();
    let speeches = c.ml.calls.speeches();
    clear_up(&c).await;

    assert!(wrong.is_empty(), "a complete setup still reported {wrong:?}");
    assert!(synth.ok);
    assert!(synth.detail.contains("bytes"), "the synthesiser finding says what it got: {}", synth.detail);
    assert!(speeches > 0, "the check did not actually ask the synthesiser for anything");
}

/// Nothing configured: every finding is wrong, and every one says what to do about it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deployment_with_nothing_set_says_what_to_do() {
    let _one_at_a_time = PREFLIGHT.lock().await;
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    set(&c, "telephony.provider", "none", ConfigValueType::String).await;
    set(&c, "telephony.public_base_url", "", ConfigValueType::String).await;
    set(&c, "telephony.auth_token_enc", "", ConfigValueType::String).await;
    set(&c, "voice.tts_stream", "false", ConfigValueType::Bool).await;
    set(&c, "voice.tts_stream_url", "", ConfigValueType::String).await;

    let checks = preflight::run(&c.state).await;
    let provider = find(&checks, "provider").clone();
    let synth = find(&checks, "synthesiser").clone();
    let credential = find(&checks, "carrier_credential").clone();
    let every_failure_has_a_fix = checks.iter().filter(|k| !k.ok).all(|k| k.fix.is_some());
    clear_up(&c).await;

    assert!(!provider.ok, "nothing can answer, and the check said otherwise");
    assert!(!credential.ok);
    assert!(!synth.ok, "no synthesiser is configured, and the check said otherwise");
    assert!(
        synth.detail.contains("answered and then ended") || synth.detail.contains("No streaming"),
        "the synthesiser finding does not explain the symptom: {}",
        synth.detail
    );
    assert!(every_failure_has_a_fix, "a finding said something was wrong and not what to do");
}

/// An engine that is there and refuses. The distinction that matters: configured is not the
/// same as working, and the check has to come back either way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_engine_that_refuses_is_not_reported_as_ready() {
    let _one_at_a_time = PREFLIGHT.lock().await;
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let refusing = refusing_engine().await;
    set(&c, "telephony.provider", "twilio", ConfigValueType::String).await;
    set(&c, "voice.tts_stream", "true", ConfigValueType::Bool).await;
    set(&c, "voice.tts_stream_url", &refusing, ConfigValueType::String).await;

    let started = std::time::Instant::now();
    let checks = preflight::run(&c.state).await;
    let took = started.elapsed();
    let synth = find(&checks, "synthesiser").clone();
    clear_up(&c).await;

    assert!(!synth.ok, "a refusing engine was reported as working");
    assert!(synth.fix.is_some(), "and nothing said what to do about it");
    assert!(took < std::time::Duration::from_secs(30), "the check took {took:?} to come back");
}

/// A line switched on whose agent has been archived answers and then fails, which is the
/// same invisible fault as everything else here.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_line_that_cannot_take_a_call_is_named() {
    let _one_at_a_time = PREFLIGHT.lock().await;
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let healthy = preflight::run(&c.state).await;
    let before = find(&healthy, "line_bindings").clone();

    sqlx::query("UPDATE agents SET archived_at = now() WHERE id = $1")
        .bind(c.agent)
        .execute(&c.pg)
        .await
        .unwrap();
    let after_checks = preflight::run(&c.state).await;
    let after = find(&after_checks, "line_bindings").clone();
    let lines = find(&after_checks, "lines").clone();
    clear_up(&c).await;

    assert!(before.ok, "a healthy line was reported as broken");
    assert!(!after.ok, "a line whose agent is archived was reported as fine");
    assert!(after.detail.contains("archived"), "it does not say why: {}", after.detail);
    assert!(lines.ok, "there is still a line registered and switched on");
}

/// A readiness report is read by more people than can set what it reports on. No finding
/// may carry a credential, and this plants recognisable ones to prove it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_finding_ever_carries_a_credential() {
    let _one_at_a_time = PREFLIGHT.lock().await;
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let carrier_token = "carrier-token-nobody-should-see-4c1f";
    let shared_secret = "shared-secret-nobody-should-see-8b2e";
    set(&c, "telephony.provider", "audiosocket", ConfigValueType::String).await;
    set(&c, "telephony.audiosocket_listen", "127.0.0.1:19999", ConfigValueType::String).await;
    set(
        &c,
        "telephony.auth_token_enc",
        &fosnie_backend::crypto::encrypt_at_rest(carrier_token).expect("encrypts"),
        ConfigValueType::String,
    )
    .await;
    set(
        &c,
        "telephony.audiosocket_key_enc",
        &fosnie_backend::crypto::encrypt_at_rest(shared_secret).expect("encrypts"),
        ConfigValueType::String,
    )
    .await;
    set(&c, "voice.tts_stream", "true", ConfigValueType::Bool).await;
    set(&c, "voice.tts_stream_url", &c.ml.base_url, ConfigValueType::String).await;

    let checks = preflight::run(&c.state).await;
    let whole: String = checks
        .iter()
        .map(|k| format!("{} {} {} {}", k.id, k.title, k.detail, k.fix.clone().unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(" ");
    let secret_check = find(&checks, "shared_secret").clone();
    clear_up(&c).await;

    assert!(!whole.contains(carrier_token), "a carrier credential appeared in the readiness report");
    assert!(!whole.contains(shared_secret), "a shared secret appeared in the readiness report");
    // And it still reports that one is stored, which is the useful half.
    assert!(secret_check.ok, "a stored secret was not reported as stored");
}
