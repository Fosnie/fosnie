//! Message feedback: thumbs up/down + comment, per-Agent summary,
//! RBAC. Gated on PAI_E2E=1 (Keycloak + Postgres). No LLM — a chat + assistant
//! message are seeded directly.

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

async fn setup() -> (sqlx::PgPool, u16) {
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
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));
    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (pg, port)
}

async fn whoami(api: &reqwest::Client, base: &str, tok: &str) -> uuid::Uuid {
    let v: serde_json::Value =
        api.get(format!("{base}/api/whoami")).bearer_auth(tok).send().await.unwrap().json().await.unwrap();
    uuid::Uuid::parse_str(v["user_id"].as_str().unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn feedback_lifecycle_summary_and_rbac() {
    if !enabled() {
        return;
    }
    let (pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let carol = token("carol").await.expect("carol");
    let alice_id = whoami(&api, &base, &alice).await;

    // An Agent + a seeded chat with one assistant message (and one user message).
    let agent: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "Rated", "system_prompt": "x" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let agent_id = uuid::Uuid::parse_str(agent["id"].as_str().unwrap()).unwrap();

    let chat_id = db::new_id();
    sqlx::query("INSERT INTO chats (id, owner_user_id, agent_id, title) VALUES ($1,$2,$3,'rated')")
        .bind(chat_id).bind(alice_id).bind(agent_id)
        .execute(&pg).await.unwrap();
    let asst_id = db::new_id();
    let user_msg_id = db::new_id();
    sqlx::query("INSERT INTO messages (id, chat_id, role, sequence_number, content) VALUES ($1,$2,'user',1,'q')")
        .bind(user_msg_id).bind(chat_id).execute(&pg).await.unwrap();
    sqlx::query("INSERT INTO messages (id, chat_id, role, sequence_number, content) VALUES ($1,$2,'assistant',2,'a')")
        .bind(asst_id).bind(chat_id).execute(&pg).await.unwrap();

    // Submit up.
    let r = api.post(format!("{base}/api/messages/{asst_id}/feedback"))
        .bearer_auth(&alice).json(&serde_json::json!({ "rating": "up" })).send().await.unwrap();
    assert!(r.status().is_success());
    let got: serde_json::Value = api.get(format!("{base}/api/messages/{asst_id}/feedback"))
        .bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert_eq!(got["mine"]["rating"], "up");
    assert_eq!(got["up"], serde_json::json!(1));

    // Re-submit down with a comment → upsert (still one row).
    let r = api.post(format!("{base}/api/messages/{asst_id}/feedback"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "rating": "down", "comment": "weak reasoning" }))
        .send().await.unwrap();
    assert!(r.status().is_success());
    let got: serde_json::Value = api.get(format!("{base}/api/messages/{asst_id}/feedback"))
        .bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert_eq!(got["mine"]["rating"], "down");
    assert_eq!(got["up"], serde_json::json!(0));
    assert_eq!(got["down"], serde_json::json!(1));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM feedback WHERE message_id = $1")
        .bind(asst_id).fetch_one(&pg).await.unwrap();
    assert_eq!(n, 1, "upsert keeps one row");

    // Submission is audited (the already-present half of the §B.18 trail).
    let submitted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'feedback.submitted' AND resource_id = $1",
    )
    .bind(asst_id).fetch_one(&pg).await.unwrap();
    assert!(submitted >= 1, "feedback.submitted audited");

    // Per-Agent summary (alice = admin) shows the negative + comment.
    let sum: serde_json::Value = api.get(format!("{base}/api/agents/{agent_id}/feedback/summary"))
        .bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert!(sum["down"].as_i64().unwrap() >= 1);
    assert!(sum["recent_negative"].as_array().unwrap().iter().any(|n| n["comment"] == "weak reasoning"));

    // RBAC: carol (not owner, not admin) cannot rate.
    let forbidden = api.post(format!("{base}/api/messages/{asst_id}/feedback"))
        .bearer_auth(&carol).json(&serde_json::json!({ "rating": "up" })).send().await.unwrap();
    assert_eq!(forbidden.status().as_u16(), 403);
    // carol (user) cannot read Agent analytics.
    let denied = api.get(format!("{base}/api/agents/{agent_id}/feedback/summary"))
        .bearer_auth(&carol).send().await.unwrap();
    assert_eq!(denied.status().as_u16(), 403);

    // Rating a non-assistant message → validation error.
    let bad = api.post(format!("{base}/api/messages/{user_msg_id}/feedback"))
        .bearer_auth(&alice).json(&serde_json::json!({ "rating": "up" })).send().await.unwrap();
    assert_eq!(bad.status().as_u16(), 400);

    // Delete clears it.
    let del = api.delete(format!("{base}/api/messages/{asst_id}/feedback"))
        .bearer_auth(&alice).send().await.unwrap();
    assert!(del.status().is_success());
    let after: serde_json::Value = api.get(format!("{base}/api/messages/{asst_id}/feedback"))
        .bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert!(after["mine"].is_null());
    assert_eq!(after["down"], serde_json::json!(0));

    // Deletion is now audited too — the trail is complete (create/update/delete).
    let deleted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'feedback.deleted' AND resource_id = $1",
    )
    .bind(asst_id).fetch_one(&pg).await.unwrap();
    assert!(deleted >= 1, "feedback.deleted audited");

    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}
