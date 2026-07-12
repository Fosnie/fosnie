//! Tabular review over the real stack: create a review (2 workspace docs × 2
//! columns) → run it on the background scheduler → cells stream back (WS
//! tabular.cell + tabular.complete; persisted in Postgres) → export to xlsx →
//! review-scoped chat answers via the read_table_cells tool. Gated on
//! `PAI_E2E=1` (needs Postgres + Redis + Keycloak + ML + Ollama up).

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

fn fixture() -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/sample.docx", env!("CARGO_MANIFEST_DIR")))
        .expect("sample.docx fixture")
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

/// Boot the server AND the background scheduler (which runs the generate task).
/// Returns the shutdown sender — keep it alive for the test's duration.
async fn setup() -> (sqlx::PgPool, u16, String, watch::Sender<bool>) {
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
    boot.scheduler.poll_interval_secs = 1; // pick up the generate task quickly
    boot.storage.workspace_dir =
        std::env::temp_dir().join("pai_test_workspace_tab").to_string_lossy().into();
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
    (pg, port, tok, sd_tx)
}

async fn upload_doc(api: &reqwest::Client, base: &str, tok: &str, project_id: &str, name: &str) -> String {
    let up: serde_json::Value = api
        .post(format!("{base}/api/projects/{project_id}/workspace/documents?filename={name}"))
        .bearer_auth(tok)
        .header("content-type", "application/octet-stream")
        .body(fixture())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    up["document_id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tabular_review_end_to_end() {
    if !enabled() {
        eprintln!("skipping tabular review (set PAI_E2E=1 with full stack up)");
        return;
    }
    let (pg, port, tok, _shutdown) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    // Project + two workspace documents.
    let project: serde_json::Value = api
        .post(format!("{base}/api/projects"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "name": "Bundle", "sector": "legal" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let project_id = project["id"].as_str().unwrap().to_string();
    let doc_a = upload_doc(&api, &base, &tok, &project_id, "a.docx").await;
    let doc_b = upload_doc(&api, &base, &tok, &project_id, "b.docx").await;

    // Create a review: 2 docs × 2 columns (a yes_no + a text).
    let review: serde_json::Value = api
        .post(format!("{base}/api/tabular-reviews"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({
            "project_id": project_id,
            "name": "Fee review",
            "document_ids": [doc_a, doc_b],
            "columns": [
                { "key": "has_fee", "name": "Mentions a fee?", "format": "yes_no", "prompt": "Does this document mention a fee?" },
                { "key": "summary", "name": "Summary", "format": "text", "prompt": "In one sentence, what is this document about?" }
            ]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let review_id = review["id"].as_str().unwrap().to_string();
    let rid = uuid::Uuid::parse_str(&review_id).unwrap();

    // Connect WS BEFORE running so we capture cell/complete frames.
    let (mut socket, _) =
        connect_async(format!("ws://127.0.0.1:{port}/ws?token={tok}")).await.expect("ws connect");
    let _hello = next_json(&mut socket).await.expect("hello");

    // Run.
    let run = api
        .post(format!("{base}/api/tabular-reviews/{review_id}/run"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(run.status().is_success());

    // Collect WS frames until the review completes.
    let mut cell_frames = 0;
    let mut complete = false;
    loop {
        let Ok(Some(frame)) = timeout(Duration::from_secs(180), next_json(&mut socket)).await else {
            break;
        };
        match frame["type"].as_str() {
            Some("tabular.cell") if frame["review_id"] == review["id"] => cell_frames += 1,
            Some("tabular.complete") if frame["review_id"] == review["id"] => {
                complete = true;
                break;
            }
            _ => {}
        }
    }
    assert!(complete, "expected a tabular.complete frame");
    assert!(cell_frames >= 1, "expected at least one tabular.cell frame");

    // All 4 cells persisted as done with a value.
    let done: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tabular_cells WHERE review_id = $1 AND status = 'done' AND value IS NOT NULL",
    )
    .bind(rid)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(done, 4, "2 docs × 2 columns all done with values");

    // Export to xlsx.
    let export = api
        .get(format!("{base}/api/tabular-reviews/{review_id}/export"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(export.status().is_success());
    let xlsx = export.bytes().await.unwrap();
    assert!(xlsx.starts_with(b"PK"), "xlsx is a zip");

    // Any cell citation must be version-pinned (carries version_id).
    let cit: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT citations FROM tabular_cells \
         WHERE review_id = $1 AND jsonb_typeof(citations) = 'array' AND jsonb_array_length(citations) > 0 LIMIT 1",
    )
    .bind(rid)
    .fetch_optional(&pg)
    .await
    .unwrap();
    if let Some(c) = cit {
        assert!(c[0]["version_id"].is_string(), "workspace-doc citation carries version_id");
    }

    // Re-run a single cell → it regenerates to done.
    let rr = api
        .post(format!("{base}/api/tabular-reviews/{review_id}/cells/{doc_a}/summary/rerun"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(rr.status().is_success());
    let did_a = uuid::Uuid::parse_str(&doc_a).unwrap();
    let mut rerun_done = false;
    for _ in 0..60 {
        let s: String = sqlx::query_scalar(
            "SELECT status::text FROM tabular_cells WHERE review_id = $1 AND document_id = $2 AND column_key = 'summary'",
        )
        .bind(rid)
        .bind(did_a)
        .fetch_one(&pg)
        .await
        .unwrap();
        if s == "done" {
            rerun_done = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(rerun_done, "re-run cell should complete");

    // Cancel endpoint is accepted (marks the review cancelled).
    let cancel = api
        .post(format!("{base}/api/tabular-reviews/{review_id}/cancel"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(cancel.status().is_success());

    // Review-scoped chat: an Agent that can read the cells.
    let agent: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({
            "name": "Reviewer",
            "system_prompt": "You answer questions about a tabular review. Call read_table_cells to see the extracted cells, then answer.",
            "tools": ["read_table_cells"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let agent_id = agent["id"].as_str().unwrap().to_string();

    let chat: serde_json::Value = api
        .post(format!("{base}/api/tabular-reviews/{review_id}/chat"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "agent_id": agent_id }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let chat_id = chat["chat_id"].as_str().unwrap().to_string();

    socket
        .send(Message::Text(
            serde_json::json!({
                "version": 1, "type": "chat.send",
                "chat_id": chat_id,
                "content": "Use read_table_cells and tell me how many documents were reviewed."
            })
            .to_string(),
        ))
        .await
        .unwrap();

    let mut saw_tool = false;
    let mut completed = false;
    loop {
        let Ok(Some(frame)) = timeout(Duration::from_secs(120), next_json(&mut socket)).await else {
            break;
        };
        match frame["type"].as_str() {
            Some("chat.tool") if frame["name"] == "read_table_cells" => saw_tool = true,
            Some("chat.completed") => {
                completed = true;
                break;
            }
            Some("chat.error") => panic!("chat.error: {}", frame["message"]),
            _ => {}
        }
    }
    assert!(completed, "review chat turn should complete");
    assert!(saw_tool, "expected a read_table_cells tool call");

    // Audit + chain.
    for action in ["review.created", "cell.generated", "review.exported"] {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action_type = $1 AND resource_id = $2",
        )
        .bind(action)
        .bind(rid)
        .fetch_one(&pg)
        .await
        .unwrap();
        assert!(n >= 1, "missing audit event {action}");
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
