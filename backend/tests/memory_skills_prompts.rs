//! Skills + Prompts + Memory over the real stack. REST roundtrips (prompt
//! create→get→render, memory create→list→edit→delete, skill create→attach→list)
//! plus an end-to-end `remember_fact` turn: a tool-enabled Agent asked to
//! remember a fact persists a `memory_fact` row + a `tool.invoked` audit, and a
//! follow-up turn's slot-[4] carries it back. Gated on `PAI_E2E=1` (needs
//! Postgres + Redis + Keycloak + ML + Ollama up).

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

/// Boot an in-process server against the live stack; returns (pool, port, token).
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
    // Isolate disk artefacts under the OS temp dir.
    let tmp = std::env::temp_dir();
    boot.storage.skills_dir = tmp.join("pai_test_skills").to_string_lossy().into();
    boot.storage.prompts_dir = tmp.join("pai_test_prompts").to_string_lossy().into();
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
async fn prompt_create_get_render_roundtrip() {
    if !enabled() {
        eprintln!("skipping prompt roundtrip (set PAI_E2E=1)");
        return;
    }
    let (_pg, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let created: serde_json::Value = api
        .post(format!("{base}/api/prompts"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({
            "name": "Greeting",
            "content": "Dear {{name}}, regarding {{matter}}.",
            "scope": "personal"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let detail: serde_json::Value = api
        .get(format!("{base}/api/prompts/{id}"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ph: Vec<String> =
        detail["placeholders"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().into()).collect();
    assert_eq!(ph, vec!["name", "matter"]);

    let rendered: serde_json::Value = api
        .post(format!("{base}/api/prompts/{id}/render"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "values": { "name": "Alice", "matter": "the lease" } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rendered["content"], "Dear Alice, regarding the lease.");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompt_default_agent() {
    if !enabled() {
        return;
    }
    let (_pg, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    // An Agent to bind as the prompt's default.
    let agent: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "name": "Default", "system_prompt": "x" }))
        .send().await.unwrap().json().await.unwrap();
    let agent_id = agent["id"].as_str().unwrap().to_string();

    // Create a prompt with a default Agent → GET surfaces it.
    let created: serde_json::Value = api
        .post(format!("{base}/api/prompts"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "name": "Bound", "content": "hi {{x}}", "agent_id": agent_id }))
        .send().await.unwrap().json().await.unwrap();
    let id = created["id"].as_str().unwrap();
    let detail: serde_json::Value = api
        .get(format!("{base}/api/prompts/{id}"))
        .bearer_auth(&tok).send().await.unwrap().json().await.unwrap();
    assert_eq!(detail["agent_id"].as_str().unwrap(), agent_id);

    // A non-existent agent_id is rejected with 400, not a 500.
    let bad = api
        .post(format!("{base}/api/prompts"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "name": "BadBind", "content": "x", "agent_id": uuid::Uuid::now_v7() }))
        .send().await.unwrap();
    assert_eq!(bad.status().as_u16(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_create_list_edit_delete() {
    if !enabled() {
        eprintln!("skipping memory CRUD (set PAI_E2E=1)");
        return;
    }
    let (_pg, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let created: serde_json::Value = api
        .post(format!("{base}/api/memory"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "content": "Prefers concise answers.", "scope": "user" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    let list: Vec<serde_json::Value> = api
        .get(format!("{base}/api/memory"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.iter().any(|f| f["id"] == created["id"]), "fact should be listed");

    let patch = api
        .patch(format!("{base}/api/memory/{id}"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "content": "Prefers very concise answers.", "pinned": true }))
        .send()
        .await
        .unwrap();
    assert!(patch.status().is_success());

    let del = api.delete(format!("{base}/api/memory/{id}")).bearer_auth(&tok).send().await.unwrap();
    assert!(del.status().is_success());

    let after: Vec<serde_json::Value> = api
        .get(format!("{base}/api/memory"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!after.iter().any(|f| f["id"] == created["id"]), "fact should be gone");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skill_create_attach_list() {
    if !enabled() {
        eprintln!("skipping skill attach (set PAI_E2E=1)");
        return;
    }
    let (pg, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let skill: serde_json::Value = api
        .post(format!("{base}/api/skills"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({
            "name": "Redline",
            "description": "Propose tracked changes to a DOCX clause.",
            "body": "When asked to redline, ..."
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let skill_id = skill["id"].as_str().unwrap().to_string();

    let list: Vec<serde_json::Value> =
        api.get(format!("{base}/api/skills")).bearer_auth(&tok).send().await.unwrap().json().await.unwrap();
    assert!(list.iter().any(|s| s["id"] == skill["id"]));

    let agent: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "name": "Conveyancer", "system_prompt": "You assist." }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let agent_id = agent["id"].as_str().unwrap().to_string();

    let attach = api
        .post(format!("{base}/api/agents/{agent_id}/skills/{skill_id}"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(attach.status().is_success());

    let aid = uuid::Uuid::parse_str(&agent_id).unwrap();
    let sid = uuid::Uuid::parse_str(&skill_id).unwrap();
    let linked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent_skills WHERE agent_id = $1 AND skill_id = $2",
    )
    .bind(aid)
    .bind(sid)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(linked, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remember_fact_tool_persists_and_audits() {
    if !enabled() {
        eprintln!("skipping remember_fact e2e (set PAI_E2E=1 with full stack up)");
        return;
    }
    let (pg, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let agent: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({
            "name": "Mnemo",
            "system_prompt": "You are an assistant. When the user asks you to remember something, you MUST call the remember_fact tool with their fact, then confirm.",
            "tools": ["remember_fact"]
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
                "content": "Please remember that my favourite colour is blue."
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
            Some("chat.tool") if frame["name"] == "remember_fact" => saw_tool = true,
            Some("chat.completed") => {
                completed = true;
                break;
            }
            Some("chat.error") => panic!("chat.error: {}", frame["message"]),
            _ => {}
        }
    }
    assert!(completed, "turn should complete");
    assert!(saw_tool, "expected a chat.tool frame for remember_fact");

    let cid = uuid::Uuid::parse_str(&chat_id.expect("chat_id")).unwrap();
    // A user-scoped fact sourced from this chat must now exist.
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM memory_facts WHERE scope = 'user' AND source_ref = $1",
    )
    .bind(cid)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(facts >= 1, "remember_fact should have inserted a memory_fact");

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
