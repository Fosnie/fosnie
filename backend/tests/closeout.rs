//! Deferred-items closeout E2E: generated artefacts (tool → row → download),
//! and the export module (user chat MD/JSON, admin audit-evidence, admin
//! project-DB). Gated on `PAI_E2E=1`. alice is a client-admin in the dev realm,
//! so the admin exports are reachable.

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

async fn setup() -> (sqlx::PgPool, u16, String) {
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
    boot.storage.artefacts_dir =
        std::env::temp_dir().join("pai_test_artefacts").to_string_lossy().into();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));

    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (pg, port, tok)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn artefacts_and_export() {
    if !enabled() {
        eprintln!("skipping closeout (set PAI_E2E=1 with full stack up)");
        return;
    }
    let (pg, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let project: serde_json::Value = api
        .post(format!("{base}/api/projects"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "name": "Closeout", "sector": "legal" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let project_id = project["id"].as_str().unwrap().to_string();

    let agent: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({
            "name": "Drafter",
            "system_prompt": "When asked for a document, call generate_artefact with kind 'md', a title, and the content, then confirm.",
            "tools": ["generate_artefact"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let agent_id = agent["id"].as_str().unwrap().to_string();

    let (mut socket, _) =
        connect_async(format!("ws://127.0.0.1:{port}/ws?token={tok}")).await.expect("ws connect");
    let _hello = next_json(&mut socket).await.expect("hello");
    socket
        .send(Message::Text(
            serde_json::json!({
                "version": 1, "type": "chat.send",
                "agent_id": agent_id,
                "project_id": project_id,
                "content": "Generate a markdown artefact titled \"Brief\" with the content \"Hello from the brief.\""
            })
            .to_string(),
        ))
        .await
        .unwrap();

    let mut chat_id: Option<String> = None;
    let mut saw_tool = false;
    let mut completed = false;
    loop {
        let Ok(Some(f)) = timeout(Duration::from_secs(120), next_json(&mut socket)).await else { break };
        match f["type"].as_str() {
            Some("chat.created") => chat_id = f["chat_id"].as_str().map(String::from),
            Some("chat.tool") if f["name"] == "generate_artefact" => saw_tool = true,
            Some("chat.completed") => {
                completed = true;
                break;
            }
            Some("chat.error") => panic!("chat.error: {}", f["message"]),
            _ => {}
        }
    }
    assert!(completed && saw_tool, "expected a generate_artefact tool call + completion");
    let chat_id = chat_id.expect("chat_id");
    let cid = uuid::Uuid::parse_str(&chat_id).unwrap();

    // Artefact row + download.
    let artefact_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM generated_artefacts WHERE chat_id = $1 LIMIT 1")
            .bind(cid)
            .fetch_one(&pg)
            .await
            .unwrap();
    let dl = api
        .get(format!("{base}/api/artefacts/{artefact_id}/download"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(dl.status().is_success());
    let bytes = dl.bytes().await.unwrap();
    assert!(!bytes.is_empty(), "artefact download should have bytes");

    let list: Vec<serde_json::Value> = api
        .get(format!("{base}/api/chats/{chat_id}/artefacts"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.iter().any(|a| a["id"] == serde_json::json!(artefact_id.to_string())));

    // Chat export (json + md).
    let exp_json: serde_json::Value = api
        .get(format!("{base}/api/chats/{chat_id}/export?format=json"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(exp_json["messages"].as_array().map(|a| !a.is_empty()).unwrap_or(false));
    let exp_md = api
        .get(format!("{base}/api/chats/{chat_id}/export?format=md"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(exp_md.status().is_success());
    assert!(exp_md.text().await.unwrap().starts_with('#'));

    // Admin audit-evidence export (alice = client-admin) verifies the chain.
    let audit: serde_json::Value = api
        .get(format!("{base}/api/admin/audit/export"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(audit["verification"]["ok"], serde_json::json!(true));
    assert!(audit["events"].as_array().map(|a| !a.is_empty()).unwrap_or(false));

    // Admin project-DB export includes this project's chat.
    let dbexp: serde_json::Value = api
        .get(format!("{base}/api/admin/projects/{project_id}/export"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(dbexp["project"]["id"], project["id"]);
    assert!(dbexp["chats"].as_array().map(|a| !a.is_empty()).unwrap_or(false));

    // A non-admin (carol) is forbidden from the admin exports.
    if let Some(carol) = token("carol").await {
        let forbidden = api
            .get(format!("{base}/api/admin/audit/export"))
            .bearer_auth(&carol)
            .send()
            .await
            .unwrap();
        assert_eq!(forbidden.status().as_u16(), 403);
    }

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
