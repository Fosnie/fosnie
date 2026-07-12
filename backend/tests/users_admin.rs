//! Client-admin console: user (de)activation, group management, sharing
//! (AccessGrants) + the feedback project-access widening, and usage analytics.
//! Gated on PAI_E2E=1 (Keycloak + Postgres). No LLM — chats/messages seeded
//! directly. alice=admin, bob=power_user, carol=user (dev realm).

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
async fn user_management_and_analytics() {
    if !enabled() {
        return;
    }
    let (_pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let carol = token("carol").await.expect("carol");
    let alice_id = whoami(&api, &base, &alice).await;
    let carol_id = whoami(&api, &base, &carol).await;

    // Admin lists users.
    let users: Vec<serde_json::Value> =
        api.get(format!("{base}/api/admin/users")).bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert!(users.iter().any(|u| u["email"] == "carol@example.com"));
    // Non-admin denied.
    let denied = api.get(format!("{base}/api/admin/users")).bearer_auth(&carol).send().await.unwrap();
    assert_eq!(denied.status().as_u16(), 403);
    // Cannot deactivate self.
    let self_off = api.post(format!("{base}/api/admin/users/{alice_id}/deactivate")).bearer_auth(&alice).send().await.unwrap();
    assert_eq!(self_off.status().as_u16(), 400);

    // Deactivate carol → her next request is rejected at the auth boundary.
    let off = api.post(format!("{base}/api/admin/users/{carol_id}/deactivate")).bearer_auth(&alice).send().await.unwrap();
    assert!(off.status().is_success());
    let who = api.get(format!("{base}/api/whoami")).bearer_auth(&carol).send().await.unwrap();
    assert_eq!(who.status().as_u16(), 401, "deactivated user is rejected");
    // Reactivate → allowed again (leave carol active for other tests).
    let on = api.post(format!("{base}/api/admin/users/{carol_id}/reactivate")).bearer_auth(&alice).send().await.unwrap();
    assert!(on.status().is_success());
    assert!(api.get(format!("{base}/api/whoami")).bearer_auth(&carol).send().await.unwrap().status().is_success());

    // Analytics: shape (counts tolerant — shared DB may already hold real rows).
    let an: serde_json::Value =
        api.get(format!("{base}/api/admin/analytics")).bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert!(an["per_model"].is_array() && an["per_user"].is_array());
    assert!(an["total_answers"].is_i64());
    let an_denied = api.get(format!("{base}/api/admin/analytics")).bearer_auth(&carol).send().await.unwrap();
    assert_eq!(an_denied.status().as_u16(), 403);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_management() {
    if !enabled() {
        return;
    }
    let (_pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let bob = token("bob").await.expect("bob");
    let carol = token("carol").await.expect("carol");
    let carol_id = whoami(&api, &base, &carol).await;

    // power_user creates a group with carol.
    let g: serde_json::Value = api
        .post(format!("{base}/api/groups"))
        .bearer_auth(&bob)
        .json(&serde_json::json!({ "name": "Litigation", "member_user_ids": [carol_id] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let gid = g["id"].as_str().unwrap().to_string();
    let detail: serde_json::Value =
        api.get(format!("{base}/api/groups/{gid}")).bearer_auth(&bob).send().await.unwrap().json().await.unwrap();
    assert!(detail["members"].as_array().unwrap().iter().any(|m| m.as_str() == Some(carol_id.to_string().as_str())));

    // Ordinary user cannot create groups.
    let denied = api
        .post(format!("{base}/api/groups"))
        .bearer_auth(&carol)
        .json(&serde_json::json!({ "name": "nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status().as_u16(), 403);

    // Remove member + delete group.
    assert!(api.delete(format!("{base}/api/groups/{gid}/members/{carol_id}")).bearer_auth(&bob).send().await.unwrap().status().is_success());
    assert!(api.delete(format!("{base}/api/groups/{gid}")).bearer_auth(&bob).send().await.unwrap().status().is_success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sharing_grant_widens_feedback() {
    if !enabled() {
        return;
    }
    let (pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let carol = token("carol").await.expect("carol");
    let alice_id = whoami(&api, &base, &alice).await;
    let carol_id = whoami(&api, &base, &carol).await;

    // Project + a chat in it (owner alice) + an assistant message.
    let project: serde_json::Value = api
        .post(format!("{base}/api/projects"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "Shared", "sector": "legal" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let project_id = uuid::Uuid::parse_str(project["id"].as_str().unwrap()).unwrap();
    let chat_id = db::new_id();
    sqlx::query("INSERT INTO chats (id, owner_user_id, project_id, title) VALUES ($1,$2,$3,'shared')")
        .bind(chat_id).bind(alice_id).bind(project_id).execute(&pg).await.unwrap();
    let asst_id = db::new_id();
    sqlx::query("INSERT INTO messages (id, chat_id, role, sequence_number, content) VALUES ($1,$2,'assistant',1,'a')")
        .bind(asst_id).bind(chat_id).execute(&pg).await.unwrap();

    // carol (not owner) cannot rate yet.
    let before = api.post(format!("{base}/api/messages/{asst_id}/feedback"))
        .bearer_auth(&carol).json(&serde_json::json!({ "rating": "up" })).send().await.unwrap();
    assert_eq!(before.status().as_u16(), 403);

    // alice grants carol project-read → widening lets carol rate.
    let grant: serde_json::Value = api
        .post(format!("{base}/api/admin/grants"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({
            "resource_type": "project", "resource_id": project_id,
            "principal_type": "user", "principal_id": carol_id, "permission": "read"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let grant_id = grant["id"].as_str().unwrap().to_string();

    let after = api.post(format!("{base}/api/messages/{asst_id}/feedback"))
        .bearer_auth(&carol).json(&serde_json::json!({ "rating": "up" })).send().await.unwrap();
    assert!(after.status().is_success(), "project-read grant widens feedback access");

    // Revoke → access closes again.
    assert!(api.delete(format!("{base}/api/admin/grants/{grant_id}")).bearer_auth(&alice).send().await.unwrap().status().is_success());
    // carol already has feedback; deleting it then re-rating should 403.
    api.delete(format!("{base}/api/messages/{asst_id}/feedback")).bearer_auth(&carol).send().await.unwrap();
    let revoked = api.post(format!("{base}/api/messages/{asst_id}/feedback"))
        .bearer_auth(&carol).json(&serde_json::json!({ "rating": "up" })).send().await.unwrap();
    assert_eq!(revoked.status().as_u16(), 403);

    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}
