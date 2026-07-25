// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Cross-client settle over real WebSockets: one person with two windows open,
//! a decision taken through the HTTP API, and the resolution arriving on BOTH
//! sockets at once. This is the "answered on one device, settled on the others"
//! path proved end to end — a live server, two real connections, the wire bytes —
//! rather than at the hub. Needs Postgres (:5433) + Redis; skips without them.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::request::Parts;
use futures_util::StreamExt;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::AppError;
use fosnie_backend::ext::AuthProvider;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::{cache, db, http};

/// Authenticates as whoever `X-Test-User` names — the same shim the device tests
/// use, so a session-authenticated route needs no real login here.
struct HeaderAuthProvider;

#[async_trait]
impl AuthProvider for HeaderAuthProvider {
    async fn authenticate(&self, parts: &mut Parts, _state: &AppState) -> Result<AuthContext, AppError> {
        let uid = parts
            .headers
            .get("x-test-user")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| AppError::Unauthorized("no test user".into()))?;
        Ok(AuthContext {
            user_id: Some(uid),
            email: None,
            display_name: None,
            role: PlatformRole::User,
            break_glass: false,
            mfa_enroll_only: false,
        })
    }
}

async fn setup() -> Option<(AppState, u16)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into();
    let state = AppStateBuilder::new(pg, redis, Arc::new(boot))
        .with_auth(Arc::new(HeaderAuthProvider))
        .build();
    let app = http::router(state.clone(), None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((state, port))
}

async fn mk_user(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'sync', $2, 'user')")
        .bind(id)
        .bind(format!("sync-{}@local.test", id.simple()))
        .execute(pg)
        .await
        .unwrap();
    id
}

/// A browser-shaped ticket for `user` (session path, no device token).
async fn session_ticket(api: &reqwest::Client, base: &str, user: Uuid) -> String {
    let ticket: serde_json::Value = api
        .post(format!("{base}/api/ws-ticket"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    ticket["ticket"].as_str().expect("a ticket").to_string()
}

type Sock = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn open_ws(port: u16, ticket: &str) -> Sock {
    let (socket, _) = connect_async(format!("ws://127.0.0.1:{port}/ws?ticket={ticket}"))
        .await
        .expect("the socket connects");
    socket
}

/// Read frames off a socket until an `agent.approval.resolved` for `run_id`
/// arrives (returning its `approved`), or give up. The opening `hello` and any
/// other frames are skipped.
async fn await_resolved(socket: &mut Sock, run_id: Uuid) -> Option<bool> {
    let want = run_id.to_string();
    loop {
        let next = timeout(Duration::from_secs(5), socket.next()).await;
        let msg = match next {
            Ok(Some(Ok(Message::Text(t)))) => t,
            Ok(Some(Ok(_))) => continue, // ping/binary — keep reading
            _ => return None,            // closed or timed out
        };
        let v: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v["type"] == "agent.approval.resolved" && v["run_id"] == want {
            return v["approved"].as_bool();
        }
    }
}

async fn awaiting_run(st: &AppState, uid: Uuid) -> Uuid {
    let run_id = fosnie_backend::agent::start_run(
        st, None, Some(uid), PlatformRole::User.as_str(), Some(Uuid::now_v7()), Uuid::now_v7(), None, None, 600,
    )
    .await
    .expect("start a run");
    fosnie_backend::agent::request_approval(st, run_id, Some(uid), PlatformRole::User.as_str(), "web_search", &serde_json::json!({}), 0)
        .await
        .expect("pause on approval");
    run_id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_decision_settles_both_of_a_user_s_open_windows() {
    let Some((state, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let uid = mk_user(&state.pg).await;

    // Two windows of the same person, both connected and listening.
    let t1 = session_ticket(&api, &base, uid).await;
    let t2 = session_ticket(&api, &base, uid).await;
    let mut win_a = open_ws(port, &t1).await;
    let mut win_b = open_ws(port, &t2).await;
    // Let both register in the hub before the decision is taken.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let run_id = awaiting_run(&state, uid).await;

    // The decision is taken through the HTTP API, as a click would.
    let resp = api
        .post(format!("{base}/api/agent-runs/{run_id}/approve"))
        .header("x-test-user", uid.to_string())
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "approve succeeds: {}", resp.status());

    // Both windows hear it, on the wire.
    let (a, b) = tokio::join!(await_resolved(&mut win_a, run_id), await_resolved(&mut win_b, run_id));
    assert_eq!(a, Some(true), "window A settles");
    assert_eq!(b, Some(true), "window B settles too");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_user_s_window_hears_nothing() {
    let Some((state, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let owner = mk_user(&state.pg).await;
    let stranger = mk_user(&state.pg).await;

    let owner_ticket = session_ticket(&api, &base, owner).await;
    let stranger_ticket = session_ticket(&api, &base, stranger).await;
    let mut owner_win = open_ws(port, &owner_ticket).await;
    let mut stranger_win = open_ws(port, &stranger_ticket).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let run_id = awaiting_run(&state, owner).await;
    let resp = api
        .post(format!("{base}/api/agent-runs/{run_id}/reject"))
        .header("x-test-user", owner.to_string())
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    assert_eq!(await_resolved(&mut owner_win, run_id).await, Some(false), "the owner is told");
    assert_eq!(await_resolved(&mut stranger_win, run_id).await, None, "a stranger never is");
}
