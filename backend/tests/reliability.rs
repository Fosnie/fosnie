//! Closeout of the buildable-now bucket: legal holds, audit retention, Ed25519
//! signing, runtime config + branding, memory relevance recall, WS replay +
//! in-band refresh, context compaction, and the read_skill tool. Gated on
//! `PAI_E2E=1`. alice is a client-admin in the dev realm.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{auth, cache, db, http, scheduler};

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

async fn setup_ctx(max_context_tokens: i64) -> (sqlx::PgPool, AppState, u16, String) {
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let ml_url =
        std::env::var("PAI__ML__BASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());
    let tok = token("alice").await.expect("keycloak token");

    let pg = db::connect(&db_url, 5).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.keycloak.url = "http://localhost:8081".into();
    boot.keycloak.realm = "fosnie".into();
    boot.keycloak.client_id = "fosnie".into();
    boot.ml.base_url = ml_url;
    boot.server.static_dir = "___no_spa___".into();
    boot.max_context_tokens = max_context_tokens;
    boot.storage.branding_dir =
        std::env::temp_dir().join("pai_test_branding").to_string_lossy().into();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));

    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app2 = app.clone();
    tokio::spawn(async move { axum::serve(listener, app2).await.unwrap() });
    (pg, state, port, tok)
}

async fn setup() -> (sqlx::PgPool, AppState, u16, String) {
    setup_ctx(0).await
}

// --- Legal holds -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn holds_lifecycle() {
    if !enabled() {
        return;
    }
    let (_pg, _state, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let created: serde_json::Value = api
        .post(format!("{base}/api/admin/holds"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "resource_type": "project", "resource_id": uuid::Uuid::now_v7(), "reason": "litigation" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let list: Vec<serde_json::Value> =
        api.get(format!("{base}/api/admin/holds")).bearer_auth(&tok).send().await.unwrap().json().await.unwrap();
    assert!(list.iter().any(|h| h["id"] == created["id"]));

    let cleared = api.delete(format!("{base}/api/admin/holds/{id}")).bearer_auth(&tok).send().await.unwrap();
    assert!(cleared.status().is_success());

    // carol (user) is forbidden.
    if let Some(carol) = token("carol").await {
        let r = api.get(format!("{base}/api/admin/holds")).bearer_auth(&carol).send().await.unwrap();
        assert_eq!(r.status().as_u16(), 403);
    }
}

// --- Audit retention ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audit_retention_drops_old_and_respects_hold() {
    if !enabled() {
        return;
    }
    let (pg, state, _port, _tok) = setup().await;

    // No active holds → an old empty partition is dropped.
    sqlx::query("DELETE FROM legal_holds WHERE active").execute(&pg).await.unwrap();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS audit_events_2020_01 PARTITION OF audit_events \
         FOR VALUES FROM ('2020-01-01') TO ('2020-02-01')",
    )
    .execute(&pg)
    .await
    .unwrap();
    let dropped = scheduler::run_audit_retention(&state).await.unwrap();
    assert!(dropped >= 1, "old partition should be dropped");
    let gone: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.audit_events_2020_01')::text")
            .fetch_one(&pg)
            .await
            .unwrap();
    assert!(gone.is_none(), "audit_events_2020_01 should be gone");

    // An active hold blocks the sweep.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS audit_events_2019_01 PARTITION OF audit_events \
         FOR VALUES FROM ('2019-01-01') TO ('2019-02-01')",
    )
    .execute(&pg)
    .await
    .unwrap();
    let hid = db::new_id();
    sqlx::query("INSERT INTO legal_holds (id, resource_type, resource_id) VALUES ($1, 'project', $2)")
        .bind(hid)
        .bind(uuid::Uuid::now_v7())
        .execute(&pg)
        .await
        .unwrap();
    let dropped2 = scheduler::run_audit_retention(&state).await.unwrap();
    assert_eq!(dropped2, 0, "hold blocks retention");
    let still: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('public.audit_events_2019_01')::text")
            .fetch_one(&pg)
            .await
            .unwrap();
    assert!(still.is_some(), "partition kept while a hold is active");

    // Cleanup so the shared DB stays tidy.
    sqlx::query("DROP TABLE IF EXISTS audit_events_2019_01").execute(&pg).await.unwrap();
    sqlx::query("UPDATE legal_holds SET active = false WHERE id = $1").bind(hid).execute(&pg).await.unwrap();
}

// --- Ed25519 signing ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ed25519_signs_and_verifies() {
    if !enabled() {
        return;
    }
    let (pg, _state, _port, _tok) = setup().await;
    fosnie_backend::audit::init_signing(
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    );
    let mut ev = fosnie_backend::audit::AuditEvent::action("test.signed", "system");
    ev.resource_type = Some("test".into());
    let r = fosnie_backend::audit::append(&pg, &ev).await.unwrap();

    let sig: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT signature FROM audit_events WHERE seq = $1").bind(r.seq).fetch_one(&pg).await.unwrap();
    assert!(sig.is_some(), "row should be signed when a key is configured");
    assert!(fosnie_backend::audit::public_key_hex().is_some());
    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}

// --- Runtime config + branding ----------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_and_branding() {
    if !enabled() {
        return;
    }
    let (pg, _state, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let put = api
        .put(format!("{base}/api/admin/config/rag.top_k"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "value": "8", "value_type": "int" }))
        .send()
        .await
        .unwrap();
    assert!(put.status().is_success());
    let list: Vec<serde_json::Value> =
        api.get(format!("{base}/api/admin/config")).bearer_auth(&tok).send().await.unwrap().json().await.unwrap();
    assert!(list.iter().any(|c| c["key"] == "rag.top_k" && c["value"] == "8"));
    let changed: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type = 'config.changed'")
        .fetch_one(&pg)
        .await
        .unwrap();
    assert!(changed >= 1);

    let up = api
        .post(format!("{base}/api/admin/branding/logo?mime=image/png"))
        .bearer_auth(&tok)
        .body(b"\x89PNG\r\n\x1a\nfake".to_vec())
        .send()
        .await
        .unwrap();
    assert!(up.status().is_success());
    let logo = api.get(format!("{base}/api/branding/logo")).bearer_auth(&tok).send().await.unwrap();
    assert!(logo.status().is_success());
    assert!(logo.bytes().await.unwrap().starts_with(b"\x89PNG"));
}

// --- Memory relevance recall -------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_recall_ranks_relevant() {
    if !enabled() {
        return;
    }
    let (_pg, _state, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    // >20 facts forces the Qdrant-ranked path; one is clearly about the ocean.
    for i in 0..22 {
        let content = if i == 0 {
            "My favourite place is the ocean, vast and blue.".to_string()
        } else {
            format!("Unrelated administrative note number {i} about scheduling.")
        };
        let r = api
            .post(format!("{base}/api/memory"))
            .bearer_auth(&tok)
            .json(&serde_json::json!({ "content": content, "scope": "user" }))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success());
    }

    let recalled: Vec<String> = api
        .get(format!("{base}/api/memory/recall?q=tell%20me%20about%20the%20ocean"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        recalled.iter().any(|c| c.contains("ocean")),
        "ranked recall should surface the ocean fact; got {recalled:?}"
    );
}

// --- WS replay + in-band refresh --------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_replay_and_refresh() {
    if !enabled() {
        return;
    }
    let (_pg, _state, port, tok) = setup().await;

    let (mut socket, _) =
        connect_async(format!("ws://127.0.0.1:{port}/ws?token={tok}")).await.expect("ws connect");
    let hello = next_json(&mut socket).await.expect("hello");
    let resume = hello["resume_token"].as_str().unwrap().to_string();

    // In-band refresh → a fresh Hello.
    socket
        .send(Message::Text(serde_json::json!({ "type": "auth", "token": tok }).to_string()))
        .await
        .unwrap();
    // Drive a turn so replayable frames are buffered.
    socket
        .send(Message::Text(
            serde_json::json!({ "version": 1, "type": "chat.send", "content": "Say hello briefly." }).to_string(),
        ))
        .await
        .unwrap();

    let mut refreshed = false;
    let mut completed = false;
    loop {
        let Ok(Some(f)) = timeout(Duration::from_secs(120), next_json(&mut socket)).await else { break };
        match f["type"].as_str() {
            Some("hello") => refreshed = true, // the Auth-triggered Hello
            Some("chat.completed") => {
                completed = true;
                break;
            }
            Some("chat.error") => panic!("chat.error: {}", f["message"]),
            _ => {}
        }
    }
    assert!(refreshed, "in-band auth should yield a fresh hello");
    assert!(completed, "turn completes");

    // Drop and reconnect with the resume token → buffered frames replay.
    drop(socket);
    let (mut sock2, _) = connect_async(format!("ws://127.0.0.1:{port}/ws?resume={resume}"))
        .await
        .expect("resume connect");
    let mut saw_replay = false;
    for _ in 0..10 {
        let Ok(Some(f)) = timeout(Duration::from_secs(10), next_json(&mut sock2)).await else { break };
        if f["type"].as_str() != Some("hello") {
            saw_replay = true;
            break;
        }
    }
    assert!(saw_replay, "resume should replay a buffered (non-hello) frame");
}

// --- Context compaction ------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_compaction_fires() {
    if !enabled() {
        return;
    }
    // Small budget so a preloaded history triggers compaction. Kept above the
    // (capped) injected-memory size so the history — not the system prompt — is
    // what overflows.
    let (pg, _state, port, tok) = setup_ctx(800).await;

    let uid: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'alice@example.com'")
            .fetch_one(&pg)
            .await
            .unwrap();
    let chat_id = db::new_id();
    sqlx::query("INSERT INTO chats (id, owner_user_id, title) VALUES ($1, $2, 'compaction')")
        .bind(chat_id)
        .bind(uid)
        .execute(&pg)
        .await
        .unwrap();
    for i in 1..=8 {
        let role = if i % 2 == 1 { "user" } else { "assistant" };
        let content = format!("Message {i}: {}", "lorem ipsum dolor sit amet ".repeat(20));
        sqlx::query("INSERT INTO messages (id, chat_id, role, sequence_number, content) VALUES ($1,$2,$3::text::message_role,$4,$5)")
            .bind(db::new_id())
            .bind(chat_id)
            .bind(role)
            .bind(i)
            .bind(content)
            .execute(&pg)
            .await
            .unwrap();
    }

    let (mut socket, _) =
        connect_async(format!("ws://127.0.0.1:{port}/ws?token={tok}")).await.expect("ws");
    let _ = next_json(&mut socket).await;
    socket
        .send(Message::Text(
            serde_json::json!({ "version": 1, "type": "chat.send", "chat_id": chat_id, "content": "Summarise so far." }).to_string(),
        ))
        .await
        .unwrap();

    let mut compacted = false;
    loop {
        let Ok(Some(f)) = timeout(Duration::from_secs(120), next_json(&mut socket)).await else { break };
        match f["type"].as_str() {
            Some("chat.compacted") => compacted = true,
            Some("chat.completed") => break,
            Some("chat.error") => panic!("chat.error: {}", f["message"]),
            _ => {}
        }
    }
    assert!(compacted, "expected a chat.compacted frame under a tiny context budget");
}

// --- read_skill tool ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_skill_loads_body() {
    if !enabled() {
        return;
    }
    let (_pg, _state, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let skill: serde_json::Value = api
        .post(format!("{base}/api/skills"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({
            "name": "Cipher", "description": "Reveals the passphrase.",
            "body": "The secret passphrase is ZEBRA-42."
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let skill_id = skill["id"].as_str().unwrap().to_string();

    let agent: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({
            "name": "Keeper",
            "system_prompt": "When asked about a skill, call read_skill with its id (shown in your Skills list), then answer.",
            "tools": ["read_skill"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let agent_id = agent["id"].as_str().unwrap().to_string();
    let r = api
        .post(format!("{base}/api/agents/{agent_id}/skills/{skill_id}"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());

    let (mut socket, _) =
        connect_async(format!("ws://127.0.0.1:{port}/ws?token={tok}")).await.expect("ws");
    let _ = next_json(&mut socket).await;
    socket
        .send(Message::Text(
            serde_json::json!({ "version": 1, "type": "chat.send", "agent_id": agent_id, "content": "Open your Cipher skill and tell me the passphrase." }).to_string(),
        ))
        .await
        .unwrap();

    let mut saw_tool = false;
    let mut completed = false;
    loop {
        let Ok(Some(f)) = timeout(Duration::from_secs(120), next_json(&mut socket)).await else { break };
        match f["type"].as_str() {
            Some("chat.tool") if f["name"] == "read_skill" => saw_tool = true,
            Some("chat.completed") => {
                completed = true;
                break;
            }
            Some("chat.error") => panic!("chat.error: {}", f["message"]),
            _ => {}
        }
    }
    assert!(completed && saw_tool, "expected a read_skill tool call + completion");
}

async fn next_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<serde_json::Value> {
    while let Some(msg) = socket.next().await {
        match msg.ok()? {
            Message::Text(t) => return serde_json::from_str(&t).ok(),
            Message::Close(_) => return None,
            _ => continue,
        }
    }
    None
}
