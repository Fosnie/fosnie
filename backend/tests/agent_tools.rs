//! End-to-end: an Agent with tools drives a turn through the tool-call loop.
//! Asks for the current time → the model calls `current_time` → we observe
//! chat.tool frames + a tool.invoked audit row + completion. Gated on PAI_E2E=1
//! (needs Postgres + Keycloak + ML + Ollama up).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_tool_loop_calls_current_time() {
    if !enabled() {
        eprintln!("skipping agent_tool_loop (set PAI_E2E=1 with full stack up)");
        return;
    }
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
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));

    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Create an Agent with tools.
    let api = reqwest::Client::new();
    let agent: serde_json::Value = api
        .post(format!("http://127.0.0.1:{port}/api/agents"))
        .header("authorization", format!("Bearer {tok}"))
        .json(&serde_json::json!({
            "name": "Tooly",
            "system_prompt": "You are a helpful assistant. When asked the time, call the current_time tool, then answer.",
            "tools": ["current_time", "list_documents", "read_document"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let agent_id = agent["id"].as_str().unwrap().to_string();

    // Chat with that Agent, asking for the time.
    let (mut socket, _) = connect_async(format!("ws://127.0.0.1:{port}/ws?token={tok}"))
        .await
        .expect("ws connect");
    let _hello = next_json(&mut socket).await.expect("hello");
    socket
        .send(Message::Text(
            serde_json::json!({
                "version": 1, "type": "chat.send",
                "agent_id": agent_id,
                "content": "What is the current time? Use the current_time tool, then tell me."
            })
            .to_string(),
        ))
        .await
        .unwrap();

    let mut chat_id: Option<String> = None;
    let mut saw_tool = false;
    let mut completed = false;
    loop {
        let Ok(Some(frame)) = timeout(Duration::from_secs(120), next_json(&mut socket)).await else {
            break;
        };
        match frame["type"].as_str() {
            Some("chat.created") => chat_id = frame["chat_id"].as_str().map(String::from),
            Some("chat.tool") => {
                if frame["name"] == "current_time" {
                    saw_tool = true;
                }
            }
            Some("chat.completed") => {
                completed = true;
                break;
            }
            Some("chat.error") => panic!("chat.error: {}", frame["message"]),
            _ => {}
        }
    }

    assert!(completed, "turn should complete");
    assert!(saw_tool, "expected a chat.tool frame for current_time");

    let cid = uuid::Uuid::parse_str(&chat_id.expect("chat_id")).unwrap();
    // tool.invoked is written by the async audit-writer task — poll briefly
    // rather than race it (optimisation audit L6 / re-audit R9).
    let mut invoked: i64 = 0;
    for _ in 0..40 {
        invoked = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action_type = 'tool.invoked' AND resource_id = $1",
        )
        .bind(cid)
        .fetch_one(&pg)
        .await
        .unwrap();
        if invoked >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(invoked >= 1, "tool.invoked should be audited");
    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}

/// Agent version history: create → v1, update → v2, rollback to v1 → v3 (which
/// restores v1's config). REST-only (no ML/WS).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_version_history_and_rollback() {
    if !enabled() {
        return;
    }
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let tok = token("alice").await.expect("keycloak token");

    let pg = db::connect(&db_url, 5).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.keycloak.url = "http://localhost:8081".into();
    boot.keycloak.realm = "fosnie".into();
    boot.keycloak.client_id = "fosnie".into();
    boot.server.static_dir = "___no_spa___".into();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));
    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state, Some(kc), Some(ws), None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let bearer = format!("Bearer {tok}");

    // v1
    let created: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .header("authorization", &bearer)
        .json(&serde_json::json!({ "name": "Verns", "system_prompt": "v1 prompt", "tools": ["current_time"] }))
        .send().await.unwrap().json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // v2 — change prompt + tools
    let upd = api
        .patch(format!("{base}/api/agents/{id}"))
        .header("authorization", &bearer)
        .json(&serde_json::json!({ "system_prompt": "v2 prompt", "tools": ["current_time", "read_document"] }))
        .send().await.unwrap();
    assert!(upd.status().is_success());

    // history → 2 entries, newest first
    let versions: Vec<serde_json::Value> =
        api.get(format!("{base}/api/agents/{id}/versions")).header("authorization", &bearer).send().await.unwrap().json().await.unwrap();
    assert_eq!(versions.len(), 2, "two versions after one update");
    assert_eq!(versions[0]["version_number"], 2);
    assert_eq!(versions[0]["source"], "updated");
    assert_eq!(versions[1]["version_number"], 1);
    assert_eq!(versions[1]["source"], "created");

    // v1 snapshot holds the original config
    let v1: serde_json::Value =
        api.get(format!("{base}/api/agents/{id}/versions/1")).header("authorization", &bearer).send().await.unwrap().json().await.unwrap();
    assert_eq!(v1["system_prompt"], "v1 prompt");
    assert_eq!(v1["tools"], serde_json::json!(["current_time"]));

    // rollback to v1 → creates v3 with v1's config
    let rb: serde_json::Value =
        api.post(format!("{base}/api/agents/{id}/versions/1/rollback")).header("authorization", &bearer).send().await.unwrap().json().await.unwrap();
    assert_eq!(rb["version"], 3);

    // the live agent is restored to v1's prompt + tools
    let now: serde_json::Value =
        api.get(format!("{base}/api/agents/{id}")).header("authorization", &bearer).send().await.unwrap().json().await.unwrap();
    assert_eq!(now["system_prompt"], "v1 prompt");
    assert_eq!(now["tools"], serde_json::json!(["current_time"]));

    let after: Vec<serde_json::Value> =
        api.get(format!("{base}/api/agents/{id}/versions")).header("authorization", &bearer).send().await.unwrap().json().await.unwrap();
    assert_eq!(after.len(), 3);
    assert_eq!(after[0]["source"], "rollback");

    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
    let _ = sqlx::query("UPDATE agents SET archived_at = now() WHERE id = $1::uuid").bind(&id).execute(&pg).await;
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
