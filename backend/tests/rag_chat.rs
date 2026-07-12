//! End-to-end RAG over the real stack: create project + knowledge, upload a
//! document, let the scheduler ingest it, then ask a doc-answerable question
//! over WebSocket and assert citations are emitted + persisted. Gated on
//! `PAI_E2E=1`; needs Postgres + Redis + Keycloak + ML + Ollama + Qdrant +
//! reranker all up.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch;
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
    r.json::<serde_json::Value>()
        .await
        .ok()?["access_token"]
        .as_str()
        .map(String::from)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rag_chat_emits_and_persists_citations() {
    if !enabled() {
        eprintln!("skipping rag_chat (set PAI_E2E=1 with full stack up)");
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
    boot.scheduler.poll_interval_secs = 1;
    let boot = Arc::new(boot);
    let state = AppState::new(pg.clone(), redis, boot.clone());

    // Scheduler (runs the ingest task) + HTTP server.
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let _bg = scheduler::spawn(state.clone(), boot.scheduler.clone(), shutdown_rx);

    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let bearer = format!("Bearer {tok}");

    // Create project.
    let project: serde_json::Value = api
        .post(format!("{base}/api/projects"))
        .header("authorization", &bearer)
        .json(&serde_json::json!({ "name": "Atlantis matter", "sector": "general" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let project_id = project["id"].as_str().unwrap().to_string();

    // Create knowledge base.
    let pk: reqwest::Response = api
        .post(format!("{base}/api/projects/{project_id}/knowledge"))
        .header("authorization", &bearer)
        .send()
        .await
        .unwrap();
    assert!(pk.status().is_success(), "create knowledge: {}", pk.status());

    // Upload a document.
    let doc: serde_json::Value = api
        .post(format!("{base}/api/projects/{project_id}/documents?filename=atlantis.txt"))
        .header("authorization", &bearer)
        .header("content-type", "text/plain")
        .body("The capital of Atlantis is Marisol. The Marisol Treaty was signed in 1923.")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let doc_id = uuid::Uuid::parse_str(doc["doc_id"].as_str().unwrap()).unwrap();

    // Wait for ingestion (scheduler → Python).
    let mut ready = false;
    for _ in 0..90 {
        let status: Option<String> =
            sqlx::query_scalar("SELECT ingest_status::text FROM kb_documents WHERE id = $1")
                .bind(doc_id)
                .fetch_optional(&pg)
                .await
                .unwrap();
        match status.as_deref() {
            Some("ready") => {
                ready = true;
                break;
            }
            Some("error") => panic!("ingestion errored"),
            _ => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    }
    assert!(ready, "document did not become ready");

    // Ask a doc-answerable question over WS, in the project.
    let (mut socket, _) = connect_async(format!("ws://127.0.0.1:{port}/ws?token={tok}"))
        .await
        .expect("ws connect");
    let _hello = next_json(&mut socket).await.expect("hello");
    socket
        .send(Message::Text(
            serde_json::json!({
                "version": 1, "type": "chat.send",
                "project_id": project_id,
                "content": "In one short sentence, what is the capital of Atlantis?"
            })
            .to_string(),
        ))
        .await
        .unwrap();

    let mut saw_citation = false;
    let mut completed = false;
    // Loop until completion; the per-frame timeout guards a stalled stream
    // (the model may stream many token frames before chat.completed).
    loop {
        let Ok(Some(frame)) = timeout(Duration::from_secs(120), next_json(&mut socket)).await else {
            break;
        };
        match frame["type"].as_str() {
            Some("chat.citations") => {
                saw_citation = frame["citations"].as_array().map(|a| !a.is_empty()).unwrap_or(false);
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
    assert!(saw_citation, "expected a chat.citations frame with >=1 citation");

    // Citations persisted, linked to the assistant message of this chat.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM citations c JOIN messages m ON m.id = c.message_id \
         JOIN chats ch ON ch.id = m.chat_id WHERE ch.project_id = $1",
    )
    .bind(uuid::Uuid::parse_str(&project_id).unwrap())
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(count >= 1, "citations should be persisted");
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
