//! Automations: create + calendar + the due-scan enqueue (no LLM), and a
//! run-on-demand that executes a headless chat (LLM). Gated on PAI_E2E=1.

use std::sync::Arc;
use std::time::Duration;

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{auth, cache, db, http, scheduler};
use tokio::sync::watch;

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

/// Boot a server; optionally spawn the background scheduler. Returns the shared
/// state (for calling scan directly) + port + shutdown guard.
async fn setup(with_scheduler: bool) -> (sqlx::PgPool, AppState, u16, watch::Sender<bool>) {
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
    boot.scheduler.poll_interval_secs = 1;
    let cfg = boot.scheduler.clone();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));

    let (sd_tx, sd_rx) = watch::channel(false);
    if with_scheduler {
        scheduler::spawn(state.clone(), cfg, sd_rx);
    }

    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app2 = app.clone();
    tokio::spawn(async move { axum::serve(listener, app2).await.unwrap() });
    (pg, state, port, sd_tx)
}

async fn whoami(api: &reqwest::Client, base: &str, tok: &str) -> uuid::Uuid {
    let v: serde_json::Value =
        api.get(format!("{base}/api/whoami")).bearer_auth(tok).send().await.unwrap().json().await.unwrap();
    uuid::Uuid::parse_str(v["user_id"].as_str().unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_calendar_and_scan() {
    if !enabled() {
        return;
    }
    let (pg, state, port, _sd) = setup(false).await; // no scheduler — call scan directly
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let carol = token("carol").await.expect("carol");

    let created: serde_json::Value = api
        .post(format!("{base}/api/automations"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "Daily brief", "schedule": "0 0 9 * * *", "prompt": "Summarise." }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let aid = uuid::Uuid::parse_str(&id).unwrap();

    let got: serde_json::Value =
        api.get(format!("{base}/api/automations/{id}")).bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert!(got["next_run_at"].is_string(), "next_run_at scheduled");

    // Calendar over 7 days → ~7 daily occurrences for this automation.
    let cal: Vec<serde_json::Value> = api
        .get(format!("{base}/api/automations/calendar"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mine = cal.iter().filter(|e| e["automation_id"] == created["id"]).count();
    assert!((6..=8).contains(&mine), "≈7 daily occurrences in the default 7-day window, got {mine}");

    // Force it due, scan directly → a durable AutomationRun task is enqueued and
    // next_run_at advances. (No scheduler running, so it is not executed here.)
    sqlx::query("UPDATE automations SET next_run_at = now() - interval '1 minute' WHERE id = $1")
        .bind(aid).execute(&pg).await.unwrap();
    let fired = scheduler::scan_due_automations(&state).await.unwrap();
    assert!(fired >= 1);
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tasks WHERE task_type = 'automation_run' AND payload->>'automation_id' = $1",
    )
    .bind(id.clone())
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(queued >= 1, "scan enqueues an automation_run task");
    let next: Option<time::OffsetDateTime> =
        sqlx::query_scalar("SELECT next_run_at FROM automations WHERE id = $1").bind(aid).fetch_one(&pg).await.unwrap();
    assert!(next.map(|t| t > time::OffsetDateTime::now_utc()).unwrap_or(false), "next_run_at advanced");

    // RBAC.
    let _ = whoami(&api, &base, &carol).await;
    let forbidden = api.get(format!("{base}/api/automations/{id}")).bearer_auth(&carol).send().await.unwrap();
    assert_eq!(forbidden.status().as_u16(), 403);
    let carol_create = api
        .post(format!("{base}/api/automations"))
        .bearer_auth(&carol)
        .json(&serde_json::json!({ "name": "mine", "schedule": "0 0 8 * * *", "prompt": "hi" }))
        .send()
        .await
        .unwrap();
    assert!(carol_create.status().is_success());

    // Tidy: pause so the shared-DB scheduler in other tests won't fire it.
    sqlx::query("UPDATE automations SET status = 'paused' WHERE id = $1").bind(aid).execute(&pg).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn caps_reject_frequent_and_over_count() {
    if !enabled() {
        return;
    }
    use fosnie_backend::config::runtime::{self, ConfigValueType};
    let (pg, _state, port, _sd) = setup(false).await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");

    // A sane daily schedule is accepted.
    let ok = api
        .post(format!("{base}/api/automations"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "daily", "schedule": "0 0 9 * * *", "prompt": "x" }))
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success());
    let created: serde_json::Value = ok.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // A sub-interval schedule (every second) is rejected (default min 300s).
    let frequent = api
        .post(format!("{base}/api/automations"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "spam", "schedule": "* * * * * *", "prompt": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(frequent.status().as_u16(), 400, "every-second schedule rejected");

    // Count cap: force max to 0 → any create is over the limit.
    runtime::set(&pg, "automation.max_per_user", "0", ConfigValueType::Int, "global", None, "system")
        .await
        .unwrap();
    let over = api
        .post(format!("{base}/api/automations"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "extra", "schedule": "0 0 9 * * *", "prompt": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(over.status().as_u16(), 400, "over the per-user count cap");

    // Restore a generous cap and tidy.
    runtime::set(&pg, "automation.max_per_user", "1000", ConfigValueType::Int, "global", None, "system")
        .await
        .unwrap();
    sqlx::query("DELETE FROM automations WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&id).unwrap())
        .execute(&pg)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_on_demand_produces_chat() {
    if !enabled() {
        return;
    }
    let (pg, _state, port, _sd) = setup(true).await; // scheduler runs the task
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let alice_id = whoami(&api, &base, &alice).await;

    // Paused so the periodic scan never double-fires; run-on-demand still works.
    let created: serde_json::Value = api
        .post(format!("{base}/api/automations"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "On demand", "schedule": "0 0 9 * * *", "prompt": "Say a one-line hello." }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    let aid = uuid::Uuid::parse_str(&id).unwrap();
    sqlx::query("UPDATE automations SET status = 'paused' WHERE id = $1").bind(aid).execute(&pg).await.unwrap();

    let run = api.post(format!("{base}/api/automations/{id}/run")).bearer_auth(&alice).send().await.unwrap();
    assert!(run.status().is_success());

    // Poll the run history until the headless chat completes.
    let mut output_chat: Option<uuid::Uuid> = None;
    for _ in 0..50 {
        let runs: Vec<serde_json::Value> = api
            .get(format!("{base}/api/automations/{id}/runs"))
            .bearer_auth(&alice)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let Some(r) = runs.iter().find(|r| r["status"] == "succeeded") {
            output_chat = r["output_chat_id"].as_str().and_then(|s| uuid::Uuid::parse_str(s).ok());
            break;
        }
        if runs.iter().any(|r| r["status"] == "failed") {
            panic!("automation run failed: {:?}", runs);
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let cid = output_chat.expect("a succeeded run with an output chat");
    let owner: uuid::Uuid =
        sqlx::query_scalar("SELECT owner_user_id FROM chats WHERE id = $1").bind(cid).fetch_one(&pg).await.unwrap();
    assert_eq!(owner, alice_id, "output chat owned by the automation owner");
    let ran: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'automation.ran' AND resource_id = $1",
    )
    .bind(aid)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(ran >= 1);
    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}
