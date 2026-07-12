//! Code-interpreter seam: the cross-platform execute→artefact→audit core via a
//! mock executor (no KVM), plus the capability-gate refusal when the feature is
//! off. The real Firecracker backend is verified separately on a Linux+KVM box
//! (tests/firecracker.rs, PAI_FIRECRACKER=1). Gated on PAI_E2E=1. alice=admin.

use std::sync::Arc;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::code_interpreter::{
    self, CodeExecutor, ExecRequest, ExecResult, Limits, OutputFile,
};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{auth, cache, db, http, tools};
use tokio::sync::mpsc;
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

async fn setup(code_interpreter: bool) -> (sqlx::PgPool, AppState, u16) {
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
    boot.features.code_interpreter = code_interpreter;
    boot.storage.artefacts_dir =
        std::env::temp_dir().join("pai_test_ci_artefacts").to_string_lossy().into();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));

    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app2 = app.clone();
    tokio::spawn(async move { axum::serve(listener, app2).await.unwrap() });
    (pg, state, port)
}

async fn whoami(api: &reqwest::Client, base: &str, tok: &str) -> Uuid {
    let v: serde_json::Value =
        api.get(format!("{base}/api/whoami")).bearer_auth(tok).send().await.unwrap().json().await.unwrap();
    Uuid::parse_str(v["user_id"].as_str().unwrap()).unwrap()
}

async fn count_action(pg: &sqlx::PgPool, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type = $1")
        .bind(action)
        .fetch_one(pg)
        .await
        .unwrap()
}

/// A deterministic stand-in for the Firecracker backend.
struct MockExecutor;

#[async_trait::async_trait]
impl CodeExecutor for MockExecutor {
    async fn execute(&self, _req: ExecRequest, _limits: &Limits) -> Result<ExecResult, fosnie_backend::AppError> {
        Ok(ExecResult {
            stdout: "hello from sandbox\n".into(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 7,
            files: vec![
                OutputFile { name: "out.csv".into(), bytes: b"a,b\n1,2\n".to_vec(), mime: "text/csv".into() },
                OutputFile { name: "chart.png".into(), bytes: vec![0x89, 0x50, 0x4e, 0x47], mime: "image/png".into() },
            ],
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_and_store_persists_artefacts_and_audits() {
    if !enabled() {
        return;
    }
    let (pg, state, port) = setup(true).await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let alice_id = whoami(&api, &base, &alice).await;

    // Seed an Agent + chat to own the output artefacts.
    let agent: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "Coder", "system_prompt": "x" }))
        .send().await.unwrap().json().await.unwrap();
    let agent_id = Uuid::parse_str(agent["id"].as_str().unwrap()).unwrap();
    let chat_id = db::new_id();
    sqlx::query("INSERT INTO chats (id, owner_user_id, agent_id, title) VALUES ($1,$2,$3,'coded')")
        .bind(chat_id).bind(alice_id).bind(agent_id).execute(&pg).await.unwrap();

    let ctx = AuthContext {
        user_id: Some(alice_id),
        email: None,
        display_name: None,
        role: PlatformRole::ClientAdmin,
        break_glass: false, mfa_enroll_only: false,
    };
    let req = ExecRequest { language: "python".into(), code: "print('hi')".into(), inputs: vec![] };

    let inv_before = count_action(&pg, "code_interpreter.invoked").await;
    let done_before = count_action(&pg, "code_interpreter.completed").await;

    let turn_id = db::new_id();
    let out = code_interpreter::run_and_store(&state, &ctx, chat_id, turn_id, &MockExecutor, req)
        .await
        .expect("run_and_store");
    assert!(out.contains("exit_code: 0"));
    assert!(out.contains("out.csv") && out.contains("chart.png"));

    // Two output files became chat-scoped 'file' artefacts.
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, title FROM generated_artefacts WHERE chat_id = $1 AND kind = 'file' ORDER BY title",
    )
    .bind(chat_id)
    .fetch_all(&pg)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "two artefacts persisted");

    // Each downloads with its bytes.
    for (id, title) in &rows {
        let resp = api
            .get(format!("{base}/api/artefacts/{id}/download"))
            .bearer_auth(&alice)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success(), "download {title}");
        let body = resp.bytes().await.unwrap();
        assert!(!body.is_empty());
    }

    // Both audit events fired; chain intact.
    assert!(count_action(&pg, "code_interpreter.invoked").await > inv_before);
    assert!(count_action(&pg, "code_interpreter.completed").await > done_before);
    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabled_capability_is_refused() {
    if !enabled() {
        return;
    }
    // Feature OFF (default): dispatch must refuse, no executor runs.
    let (_pg, state, _port) = setup(false).await;
    let ctx = AuthContext {
        user_id: Some(Uuid::now_v7()),
        email: None,
        display_name: None,
        role: PlatformRole::ClientAdmin,
        break_glass: false, mfa_enroll_only: false,
    };
    let (tx, _rx) = mpsc::channel::<ServerFrame>(8);
    let res = tools::dispatch(
        &state,
        &ctx,
        None,
        Uuid::now_v7(),
        Uuid::now_v7(),
        &tx,
        None,
        &[],
        &std::collections::HashMap::new(),
        "code_interpreter",
        &serde_json::json!({ "code": "print(1)" }),
    )
    .await;
    match res {
        Err(fosnie_backend::AppError::Validation(m)) => assert!(m.contains("disabled")),
        other => panic!("expected disabled-capability refusal, got {other:?}"),
    }
}
