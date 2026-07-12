//! Async export jobs: enqueue → background build → download link, plus RBAC and
//! audit. The scheduler is spawned in-test so the durable Export task runs. A
//! chat + messages are seeded directly (no LLM). Gated on PAI_E2E=1.
//! alice=admin, carol=user (dev realm).

use std::sync::Arc;
use std::time::Duration;

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{auth, cache, db, http, scheduler};
use tokio::sync::watch;
use uuid::Uuid;

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

async fn setup() -> (sqlx::PgPool, u16, watch::Sender<bool>) {
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.keycloak.url = "http://localhost:8081".into();
    boot.keycloak.realm = "fosnie".into();
    boot.keycloak.client_id = "fosnie".into();
    boot.server.static_dir = "___no_spa___".into();
    boot.scheduler.poll_interval_secs = 1;
    boot.storage.exports_dir =
        std::env::temp_dir().join("pai_test_exports").to_string_lossy().into();
    let cfg = boot.scheduler.clone();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));

    let (sd_tx, sd_rx) = watch::channel(false);
    scheduler::spawn(state.clone(), cfg, sd_rx);

    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (pg, port, sd_tx)
}

async fn whoami(api: &reqwest::Client, base: &str, tok: &str) -> Uuid {
    let v: serde_json::Value =
        api.get(format!("{base}/api/whoami")).bearer_auth(tok).send().await.unwrap().json().await.unwrap();
    Uuid::parse_str(v["user_id"].as_str().unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn async_chat_export_builds_and_downloads() {
    if !enabled() {
        return;
    }
    let (pg, port, _sd) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let alice_id = whoami(&api, &base, &alice).await;

    // Seed an Agent + chat + a couple of messages.
    let agent: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "Exp", "system_prompt": "x" }))
        .send().await.unwrap().json().await.unwrap();
    let agent_id = Uuid::parse_str(agent["id"].as_str().unwrap()).unwrap();
    let chat_id = db::new_id();
    sqlx::query("INSERT INTO chats (id, owner_user_id, agent_id, title) VALUES ($1,$2,$3,'exported')")
        .bind(chat_id).bind(alice_id).bind(agent_id).execute(&pg).await.unwrap();
    sqlx::query("INSERT INTO messages (id, chat_id, role, sequence_number, content) VALUES ($1,$2,'user',1,'q')")
        .bind(db::new_id()).bind(chat_id).execute(&pg).await.unwrap();
    sqlx::query("INSERT INTO messages (id, chat_id, role, sequence_number, content) VALUES ($1,$2,'assistant',2,'a')")
        .bind(db::new_id()).bind(chat_id).execute(&pg).await.unwrap();

    // Enqueue an async JSON export of the chat.
    let created: serde_json::Value = api
        .post(format!("{base}/api/exports"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "kind": "chat", "target_id": chat_id, "format": "json" }))
        .send().await.unwrap().json().await.unwrap();
    let export_id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["status"], "queued");

    // Poll until the worker reports ready.
    let mut ready = false;
    for _ in 0..40 {
        let st: serde_json::Value = api
            .get(format!("{base}/api/exports/{export_id}"))
            .bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
        match st["status"].as_str() {
            Some("ready") => { ready = true; break; }
            Some("failed") => panic!("export failed: {:?}", st["error"]),
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    assert!(ready, "export should reach ready");

    // Download → the built JSON carries the seeded messages.
    let dl: serde_json::Value = api
        .get(format!("{base}/api/exports/{export_id}/download"))
        .bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert!(dl["messages"].as_array().map(|a| a.len() == 2).unwrap_or(false), "two messages exported");

    // RBAC: another user cannot see or download someone else's export.
    if let Some(carol) = token("carol").await {
        let forbidden = api
            .get(format!("{base}/api/exports/{export_id}"))
            .bearer_auth(&carol).send().await.unwrap();
        assert_eq!(forbidden.status().as_u16(), 403);
    }

    // Audit captured completion; chain intact.
    let done: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'export.completed' AND resource_id = $1",
    )
    .bind(Uuid::parse_str(&export_id).unwrap())
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(done >= 1, "export.completed audited");
    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}
