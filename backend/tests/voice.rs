//! Voice gating: the `features.voice` (batch) and `features.voice_live` (live /
//! streaming) host flags control the endpoints/frames and are surfaced via whoami
//! capabilities (+ `voice_live_opts`). Deterministic, no audio engine needed (the
//! real round-trip is tests/voice_live.rs, PAI_VOICE=1). Gated on PAI_E2E=1.
//! alice=admin.

use std::sync::Arc;

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{auth, cache, db, http};

const KC: &str = "http://localhost:8081/realms/fosnie";

fn enabled() -> bool {
    std::env::var("PAI_E2E").as_deref() == Ok("1")
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

async fn setup(voice: bool, voice_live: bool) -> u16 {
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
    boot.features.voice = voice;
    boot.features.voice_live = voice_live;
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
async fn voice_disabled_is_refused_and_capability_reported() {
    if !enabled() {
        return;
    }
    let api = reqwest::Client::new();
    let alice = token("alice").await.expect("alice");

    // Feature OFF: capability false, endpoints refuse with 400.
    let off = format!("http://127.0.0.1:{}", setup(false, false).await);
    let who: serde_json::Value =
        api.get(format!("{off}/api/whoami")).bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert_eq!(who["capabilities"]["voice"], serde_json::json!(false));
    // Live voice is independently gated; off by default, and its opts are absent.
    assert_eq!(who["capabilities"]["voice_live"], serde_json::json!(false));
    assert!(who["voice_live_opts"].is_null(), "no live-voice opts when the feature is off");

    let t = api
        .post(format!("{off}/api/voice/transcribe"))
        .bearer_auth(&alice)
        .header("content-type", "audio/wav")
        .body(vec![1u8, 2, 3])
        .send()
        .await
        .unwrap();
    assert_eq!(t.status().as_u16(), 400, "transcribe refused when voice off");
    let s = api
        .post(format!("{off}/api/voice/speech"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_eq!(s.status().as_u16(), 400, "speech refused when voice off");

    // Feature ON: capability true; the gate opens (engine may be unconfigured →
    // 503, but it is NOT the 400 disabled-gate).
    let on = format!("http://127.0.0.1:{}", setup(true, true).await);
    let who2: serde_json::Value =
        api.get(format!("{on}/api/whoami")).bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert_eq!(who2["capabilities"]["voice"], serde_json::json!(true));
    // Live voice on → capability true and the client dials are surfaced.
    assert_eq!(who2["capabilities"]["voice_live"], serde_json::json!(true));
    assert!(who2["voice_live_opts"]["ptt_default"].is_boolean(), "live-voice opts present when on");
    assert!(who2["voice_live_opts"]["silence_threshold_ms"].is_number());

    let s2 = api
        .post(format!("{on}/api/voice/speech"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "text": "hello" }))
        .send()
        .await
        .unwrap();
    assert_ne!(s2.status().as_u16(), 400, "voice on → gate passes (got {})", s2.status());
}
