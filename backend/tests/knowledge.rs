//! Project Knowledge: list KB + docs with status, create KB, upload a doc,
//! RBAC. Gated on PAI_E2E=1. No ML needed (ingest stays 'uploaded'). alice=admin,
//! carol=user.

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
    boot.storage.documents_dir =
        std::env::temp_dir().join("pai_test_kdocs").to_string_lossy().into();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn knowledge_base_create_upload_list_rbac() {
    if !enabled() {
        return;
    }
    let (pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let carol = token("carol").await.expect("carol");

    let created: serde_json::Value = api
        .post(format!("{base}/api/projects"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "KB project", "sector": "general" }))
        .send().await.unwrap().json().await.unwrap();
    let pid = created["id"].as_str().unwrap().to_string();

    // No KB yet (deterministic, no ML).
    let docs0: serde_json::Value =
        api.get(format!("{base}/api/projects/{pid}/documents")).bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert!(docs0["knowledge"].is_null());
    assert_eq!(docs0["documents"].as_array().unwrap().len(), 0);

    // RBAC: carol (no grant) → 403 (deterministic, no ML).
    let denied = api.get(format!("{base}/api/projects/{pid}/documents")).bearer_auth(&carol).send().await.unwrap();
    assert_eq!(denied.status().as_u16(), 403);
    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);

    // Create KB — needs ML (/embed-dimension) + Qdrant. Skip the rest if ML is down.
    let kb = api
        .post(format!("{base}/api/projects/{pid}/knowledge"))
        .bearer_auth(&alice).send().await.unwrap();
    if !kb.status().is_success() {
        eprintln!("skip KB-dependent asserts (ML/embeddings unavailable): {}", kb.status());
        return;
    }

    // Upload a tiny text doc.
    let up = api
        .post(format!("{base}/api/projects/{pid}/documents?filename=note.txt&mime=text/plain"))
        .bearer_auth(&alice)
        .header("content-type", "application/octet-stream")
        .body("the consultancy fee is 30 pounds".as_bytes().to_vec())
        .send().await.unwrap();
    assert!(up.status().is_success(), "upload: {}", up.status());

    // List shows the KB + the doc (status at least 'uploaded').
    let docs: serde_json::Value =
        api.get(format!("{base}/api/projects/{pid}/documents")).bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert!(docs["knowledge"]["id"].is_string());
    let list = docs["documents"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["filename"], "note.txt");
    assert!(["uploaded", "extracting", "indexing", "ready"].contains(&list[0]["status"].as_str().unwrap()));
}
