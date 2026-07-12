//! Live voice round-trip — requires the STT + TTS llama-servers running and the
//! ML service configured with STT_*/TTS_* (deploy/voice/README.md). Gated on
//! PAI_VOICE=1; never runs in normal CI/dev. Synthesise "hello" → audio, then
//! transcribe it back → text.

use std::sync::Arc;

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{auth, cache, db, http};

const KC: &str = "http://localhost:8081/realms/fosnie";

fn enabled() -> bool {
    std::env::var("PAI_VOICE").as_deref() == Ok("1")
}

async fn token(user: &str) -> Option<String> {
    let c = reqwest::Client::new();
    let r = c
        .post(format!("{KC}/protocol/openid-connect/token"))
        .form(&[
            ("grant_type", "password"),
            ("client_id", "fosnie"),
            ("client_secret", "fosnie-secret"),
            ("username", user),
            ("password", user),
            ("scope", "openid profile email"),
        ])
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json::<serde_json::Value>().await.ok()?["access_token"].as_str().map(String::from)
}

async fn setup() -> u16 {
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let ml_url =
        std::env::var("PAI__ML__BASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());
    let pg = db::connect(&db_url, 5).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.keycloak.url = "http://localhost:8081".into();
    boot.keycloak.realm = "fosnie".into();
    boot.keycloak.client_id = "fosnie".into();
    boot.ml.base_url = ml_url;
    boot.server.static_dir = "___no_spa___".into();
    boot.features.voice = true;
    let state = AppState::new(pg, redis, Arc::new(boot));
    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn synthesize_then_transcribe_round_trip() {
    if !enabled() {
        return;
    }
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{}", setup().await);
    let alice = token("alice").await.expect("alice");

    // TTS: "hello" → audio bytes.
    let speech = api
        .post(format!("{base}/api/voice/speech"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "text": "Hello, this is a test." }))
        .send()
        .await
        .unwrap();
    assert!(speech.status().is_success(), "TTS failed: {}", speech.status());
    let mime = speech
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/wav")
        .to_string();
    let audio = speech.bytes().await.unwrap();
    assert!(!audio.is_empty(), "TTS returned no audio");

    // STT: feed the audio back → non-empty transcript.
    let tr: serde_json::Value = api
        .post(format!("{base}/api/voice/transcribe"))
        .bearer_auth(&alice)
        .header("content-type", mime)
        .body(audio.to_vec())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        tr["text"].as_str().map(|s| !s.trim().is_empty()).unwrap_or(false),
        "STT returned empty transcript: {tr:?}"
    );
}
