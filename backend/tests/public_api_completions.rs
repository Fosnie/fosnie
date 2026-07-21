//! The OpenAI-compatible completion surface: what a key may address, how a
//! passthrough completion streams, how usage is metered, and how the throttle
//! behaves. Needs Postgres (:5433) + Redis; skips if DATABASE_URL is unset.
//!
//! The generation service is stood in for by a socket that writes scripted
//! newline-delimited events with no content length, so the platform reads them
//! incrementally and the streaming assertions below are about real chunk
//! boundaries rather than one buffered blob.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use fosnie_backend::config::BootConfig;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::{cache, db, http};

async fn mk_user(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'apiv1', $2, 'user')")
        .bind(id)
        .bind(format!("apiv1-{}@local.test", id.simple()))
        .execute(pg)
        .await
        .unwrap();
    id
}

/// Mint a key directly, the way the management endpoint does.
async fn mk_key(pg: &sqlx::PgPool, user_id: Uuid) -> String {
    let (token, hash, prefix) = fosnie_backend::auth::api_key::mint();
    sqlx::query(
        "INSERT INTO api_keys (id, user_id, name, token_hash, display_prefix) \
         VALUES ($1, $2, 'test', $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(&hash)
    .bind(prefix)
    .execute(pg)
    .await
    .unwrap();
    token
}

async fn mk_provider(pg: &sqlx::PgPool, scope: &str, scope_id: Option<Uuid>, label: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO provider_configs (id, role, scope, scope_id, label, model, enabled) \
         VALUES ($1, 'llm', $2, $3, $4, 'test-model', true)",
    )
    .bind(id)
    .bind(scope)
    .bind(scope_id)
    .bind(label)
    .execute(pg)
    .await
    .unwrap();
    id
}

/// A stand-in generation service that replays scripted events line by line.
///
/// No `Content-Length`, connection closed at the end: the reader consumes to
/// end-of-file, so each line arrives as its own read exactly as it would from
/// the real service.
async fn spawn_mock_ml(lines: Vec<String>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let lines = lines.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await; // request line + headers
                let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n";
                if sock.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for l in lines {
                    if sock.write_all(format!("{l}\n").as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = sock.flush().await;
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

async fn setup(ml_base: Option<String>) -> Option<(AppState, sqlx::PgPool, u16)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into();
    if let Some(b) = ml_base {
        boot.ml.base_url = b;
    }
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot)).build();
    let app = http::router(state.clone(), None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((state, pg, port))
}

fn scripted() -> Vec<String> {
    vec![
        r#"{"type":"token","delta":"Hello"}"#.into(),
        r#"{"type":"reasoning","delta":"thinking"}"#.into(),
        r#"{"type":"token","delta":" world"}"#.into(),
        r#"{"type":"done","finish_reason":"stop","model":"test-model","usage":{"prompt_tokens":11,"completion_tokens":2}}"#.into(),
    ]
}

/// Split an event-stream body into its `data:` payloads.
fn sse_payloads(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|l| l.strip_prefix("data: ").or_else(|| l.strip_prefix("data:")))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn models_list_respects_who_owns_what() {
    let Some((_state, pg, port)) = setup(None).await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let owner = mk_user(&pg).await;
    let other = mk_user(&pg).await;
    let owner_key = mk_key(&pg, owner).await;
    let other_key = mk_key(&pg, other).await;

    let label = format!("private-{}", Uuid::now_v7().simple());
    mk_provider(&pg, "user", Some(owner), &label).await;

    let ids = |key: String| {
        let api = api.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = api
                .get(format!("{base}/v1/models"))
                .header("authorization", format!("Bearer {key}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(body["object"], "list");
            body["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["id"].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        }
    };

    let mine = ids(owner_key.clone()).await;
    assert!(mine.contains(&label), "the owner sees their own provider");
    let theirs = ids(other_key).await;
    assert!(!theirs.contains(&label), "nobody else does");

    // Anything listed can be sent straight back as `model`: the id round-trips.
    let unknown = api
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", format!("Bearer {owner_key}"))
        .json(&serde_json::json!({
            "model": "no-such-model-anywhere",
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
    let err: serde_json::Value = unknown.json().await.unwrap();
    assert_eq!(err["error"]["code"], "model_not_found");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passthrough_streams_and_aggregates_identically() {
    let ml = spawn_mock_ml(scripted()).await;
    let Some((_state, pg, port)) = setup(Some(ml)).await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let key = mk_key(&pg, user).await;
    let label = format!("mock-{}", Uuid::now_v7().simple());
    mk_provider(&pg, "user", Some(user), &label).await;

    // --- Streaming -----------------------------------------------------------
    let res = api
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", format!("Bearer {key}"))
        .json(&serde_json::json!({
            "model": label,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream")),
        "streamed as server-sent events"
    );
    let body = res.text().await.unwrap();
    let payloads = sse_payloads(&body);
    assert_eq!(payloads.last().map(String::as_str), Some("[DONE]"), "terminated with the sentinel");

    let chunks: Vec<serde_json::Value> = payloads[..payloads.len() - 1]
        .iter()
        .map(|p| serde_json::from_str(p).expect("each chunk is JSON"))
        .collect();
    assert!(chunks.iter().all(|c| c["object"] == "chat.completion.chunk"));

    let text: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(text, "Hello world");
    let reasoning: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["reasoning_content"].as_str())
        .collect();
    assert_eq!(reasoning, "thinking", "the reasoning channel stays out of the answer");

    let last = chunks.last().unwrap();
    assert_eq!(last["choices"][0]["finish_reason"], "stop");
    assert_eq!(last["usage"]["prompt_tokens"], 11);
    assert_eq!(last["usage"]["completion_tokens"], 2);
    assert_eq!(last["usage"]["total_tokens"], 13);

    // --- The same turn, not streamed -----------------------------------------
    let whole: serde_json::Value = api
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", format!("Bearer {key}"))
        .json(&serde_json::json!({
            "model": label,
            "messages": [{ "role": "user", "content": "hi" }],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(whole["object"], "chat.completion");
    assert_eq!(whole["choices"][0]["message"]["content"], "Hello world");
    assert_eq!(whole["choices"][0]["message"]["role"], "assistant");
    assert_eq!(whole["choices"][0]["finish_reason"], "stop");
    assert_eq!(whole["usage"]["completion_tokens"], 2);

    // --- Metering ------------------------------------------------------------
    let metered = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM audit_events \
         WHERE action_type = 'api.completion.finished' AND actor_user_id = $1 \
           AND (token_usage->>'prompt_tokens')::int = 11 \
           AND (token_usage->>'completion_tokens')::int = 2",
    )
    .bind(user)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(metered, 2, "both completions are metered with their token counts");

    // The dashboard rollup reads the metered actions by name; a completion that
    // is not in that set would be invisible to it.
    let visible = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(SUM((token_usage->>'completion_tokens')::bigint), 0)::bigint \
         FROM audit_events WHERE action_type = ANY($1) AND actor_user_id = $2",
    )
    .bind(&fosnie_backend::audit::METERED_COMPLETION_ACTIONS[..])
    .bind(user)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(visible, 4, "the usage rollup counts API traffic too");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_side_tools_and_multiple_completions_are_refused() {
    let Some((_state, pg, port)) = setup(None).await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let key = mk_key(&pg, user).await;
    let label = format!("mock-{}", Uuid::now_v7().simple());
    mk_provider(&pg, "user", Some(user), &label).await;

    let post = |body: serde_json::Value| {
        api.post(format!("{base}/v1/chat/completions"))
            .header("authorization", format!("Bearer {key}"))
            .json(&body)
            .send()
    };

    let tools = post(serde_json::json!({
        "model": label,
        "messages": [{ "role": "user", "content": "hi" }],
        "tools": [{ "type": "function", "function": { "name": "f" } }],
    }))
    .await
    .unwrap();
    assert_eq!(tools.status(), 400, "refused rather than silently ignored");
    let err: serde_json::Value = tools.json().await.unwrap();
    assert!(
        err["error"]["message"].as_str().unwrap().contains("tools are not supported"),
        "and says why: {err}"
    );

    let many = post(serde_json::json!({
        "model": label,
        "messages": [{ "role": "user", "content": "hi" }],
        "n": 3,
    }))
    .await
    .unwrap();
    assert_eq!(many.status(), 400);

    // An explicit single completion is the normal case and must pass validation
    // (it fails later, on the absent generation service, not here).
    let one = post(serde_json::json!({
        "model": label,
        "messages": [{ "role": "user", "content": "hi" }],
        "n": 1,
    }))
    .await
    .unwrap();
    assert_ne!(one.status(), 400, "n=1 is not a rejection");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_throttle_answers_in_the_shape_clients_parse() {
    let Some((_state, pg, port)) = setup(None).await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let key = mk_key(&pg, user).await;

    sqlx::query(
        "INSERT INTO config_settings (key, value, value_type, scope) \
         VALUES ('api.rate_per_min', '3', 'int', 'global') \
         ON CONFLICT (key) DO UPDATE SET value = '3'",
    )
    .execute(&pg)
    .await
    .unwrap();

    let mut throttled = None;
    for _ in 0..8 {
        let res = api
            .get(format!("{base}/v1/models"))
            .header("authorization", format!("Bearer {key}"))
            .send()
            .await
            .unwrap();
        if res.status() == 429 {
            throttled = Some(res);
            break;
        }
    }
    let res = throttled.expect("the throttle engages past its threshold");
    assert!(res.headers().contains_key("retry-after"), "and says when to come back");
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"]["type"], "rate_limit_error");

    sqlx::query("DELETE FROM config_settings WHERE key = 'api.rate_per_min'")
        .execute(&pg)
        .await
        .unwrap();
}
