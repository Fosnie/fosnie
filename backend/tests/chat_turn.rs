//! End-to-end chat turn over WebSocket. Exercises stream → cancel → persistence
//! → audit in one fast pass (cancelling after the first token avoids waiting on
//! a full completion). Gated on `PAI_E2E=1` and needs Postgres + Redis +
//! Keycloak + the ML service + Ollama all up.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{auth, cache, db, http};

const KC_ISSUER: &str = "http://localhost:8081/realms/fosnie";

fn enabled() -> bool {
    std::env::var("PAI_E2E").as_deref() == Ok("1")
}

async fn mint_token() -> Option<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{KC_ISSUER}/protocol/openid-connect/token"))
        .form(&[
            ("grant_type", "password"),
            ("client_id", "fosnie"),
            ("client_secret", "fosnie-secret"),
            ("username", "carol"),
            ("password", "carol"),
            ("scope", "openid profile email"),
        ])
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    json["access_token"].as_str().map(|s| s.to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_chat_turn_streams_then_cancels_and_persists() {
    if !enabled() {
        eprintln!("skipping ws_chat_turn (set PAI_E2E=1 with full stack up)");
        return;
    }
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let ml_url = std::env::var("PAI__ML__BASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());
    let token = mint_token().await.expect("keycloak token");

    // Build state + router with auth + ws layers, serve on an ephemeral port.
    let pg = db::connect(&db_url, 5).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig {
        database_url: db_url,
        redis_url,
        ..BootConfig::default()
    };
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
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Connect.
    let url = format!("ws://127.0.0.1:{port}/ws?token={token}");
    let (mut socket, _) = connect_async(&url).await.expect("ws connect");

    // hello
    let hello = next_json(&mut socket).await.expect("hello");
    assert_eq!(hello["type"], "hello");
    assert!(hello["resume_token"].as_str().is_some());

    // Send a chat message.
    socket
        .send(Message::Text(
            serde_json::json!({ "version": 1, "type": "chat.send", "content": "Tell me a long story." })
                .to_string(),
        ))
        .await
        .unwrap();

    // Read until we have a turn_id (first token) + chat_id; then cancel.
    let mut chat_id: Option<String> = None;
    let mut turn_id: Option<String> = None;
    let mut got_token = false;
    let mut cancelled = false;
    let mut ended = false;

    for _ in 0..200 {
        let Ok(Some(frame)) = timeout(Duration::from_secs(60), next_json(&mut socket)).await else {
            break;
        };
        match frame["type"].as_str() {
            Some("chat.created") => chat_id = frame["chat_id"].as_str().map(String::from),
            Some("chat.token") => {
                got_token = true;
                if turn_id.is_none() {
                    turn_id = frame["turn_id"].as_str().map(String::from);
                }
                if !cancelled {
                    if let Some(tid) = &turn_id {
                        socket
                            .send(Message::Text(
                                serde_json::json!({ "version": 1, "type": "chat.cancel", "turn_id": tid })
                                    .to_string(),
                            ))
                            .await
                            .unwrap();
                        cancelled = true;
                    }
                }
            }
            Some("chat.interrupted") | Some("chat.completed") => {
                ended = true;
                break;
            }
            Some("chat.error") => panic!("chat.error: {}", frame["message"]),
            _ => {}
        }
    }

    assert!(got_token, "expected at least one streamed token");
    assert!(ended, "turn should end (interrupted or completed)");
    let chat_id = uuid::Uuid::parse_str(&chat_id.expect("chat_id from chat.created")).unwrap();

    // Persistence: a user message and an assistant message exist.
    let roles: Vec<String> = sqlx::query_scalar(
        "SELECT role::text FROM messages WHERE chat_id = $1 ORDER BY sequence_number",
    )
    .bind(chat_id)
    .fetch_all(&pg)
    .await
    .unwrap();
    assert!(roles.contains(&"user".to_string()));
    assert!(roles.contains(&"assistant".to_string()));

    // Audit: a chat.message.sent for this chat, and the chain verifies.
    // chat.message.sent is written by the async audit-writer task, so poll
    // briefly rather than race it (optimisation audit L6 / re-audit R9).
    let mut sent: i64 = 0;
    for _ in 0..40 {
        sent = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action_type = 'chat.message.sent' AND resource_id = $1",
        )
        .bind(chat_id)
        .fetch_one(&pg)
        .await
        .unwrap();
        if sent >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(sent, 1);
    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
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
