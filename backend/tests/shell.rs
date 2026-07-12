//! App-shell endpoints: chat list, chat message history (RBAC), project list
//! (owned + shared-with-me). Gated on PAI_E2E=1. No LLM — chats/messages/grants
//! seeded directly. alice=admin, bob=power_user, carol=user (dev realm).

use std::sync::Arc;

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{auth, cache, db, http};
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

async fn uid(api: &reqwest::Client, base: &str, tok: &str) -> Uuid {
    let v: serde_json::Value =
        api.get(format!("{base}/api/whoami")).bearer_auth(tok).send().await.unwrap().json().await.unwrap();
    Uuid::parse_str(v["user_id"].as_str().unwrap()).unwrap()
}

async fn projects(api: &reqwest::Client, base: &str, tok: &str) -> Vec<serde_json::Value> {
    api.get(format!("{base}/api/projects")).bearer_auth(tok).send().await.unwrap().json().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chats_messages_and_project_visibility() {
    if !enabled() {
        return;
    }
    let (pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let bob = token("bob").await.expect("bob");
    let carol = token("carol").await.expect("carol");
    let alice_id = uid(&api, &base, &alice).await;
    let bob_id = uid(&api, &base, &bob).await;

    // alice creates a project.
    let created: serde_json::Value = api
        .post(format!("{base}/api/projects"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "Matter Z", "sector": "legal" }))
        .send().await.unwrap().json().await.unwrap();
    let pid = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();

    // Owner sees it; bob (no grant) does not.
    assert!(projects(&api, &base, &alice).await.iter().any(|p| p["id"] == created["id"]));
    assert!(!projects(&api, &base, &bob).await.iter().any(|p| p["id"] == created["id"]));

    // Grant bob Read → now visible to bob.
    sqlx::query(
        "INSERT INTO access_grants (id, resource_type, resource_id, principal_type, principal_id, permission, created_by) \
         VALUES ($1,'project',$2,'user',$3,'read',$4)",
    )
    .bind(db::new_id()).bind(pid).bind(bob_id).bind(alice_id)
    .execute(&pg).await.unwrap();
    assert!(projects(&api, &base, &bob).await.iter().any(|p| p["id"] == created["id"]));

    // Seed a chat (owner alice, no project) + two messages.
    let chat_id = db::new_id();
    sqlx::query("INSERT INTO chats (id, owner_user_id, title) VALUES ($1,$2,'History test')")
        .bind(chat_id).bind(alice_id).execute(&pg).await.unwrap();
    sqlx::query("INSERT INTO messages (id, chat_id, role, sequence_number, content) VALUES ($1,$2,'user',1,'hello')")
        .bind(db::new_id()).bind(chat_id).execute(&pg).await.unwrap();
    sqlx::query("INSERT INTO messages (id, chat_id, role, sequence_number, content) VALUES ($1,$2,'assistant',2,'hi there')")
        .bind(db::new_id()).bind(chat_id).execute(&pg).await.unwrap();

    // List chats (alice) includes it.
    let chats: Vec<serde_json::Value> =
        api.get(format!("{base}/api/chats")).bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    let cid = chat_id.to_string();
    assert!(chats.iter().any(|c| c["id"] == serde_json::json!(cid)), "new chat in list");

    // Message history (alice) ordered.
    let msgs: Vec<serde_json::Value> = api
        .get(format!("{base}/api/chats/{chat_id}/messages"))
        .bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[1]["content"], "hi there");

    // carol (not owner / not admin / no project) → 403.
    let denied = api
        .get(format!("{base}/api/chats/{chat_id}/messages"))
        .bearer_auth(&carol).send().await.unwrap();
    assert_eq!(denied.status().as_u16(), 403);

    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}
