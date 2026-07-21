//! Forward compatibility of the socket protocol.
//!
//! Clients are not upgraded in step with the server (the application ships with
//! the instance, anything installed does not), so a connection that says
//! something this build has never heard of must survive, silently. And the
//! opening handshake must be genuinely optional: a client that never sends one
//! behaves exactly as before it existed.
//!
//! Needs Postgres (:5433) + Redis; skips if DATABASE_URL is unset.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::request::Parts;
use futures_util::{SinkExt, StreamExt};
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

struct HeaderAuthProvider(Uuid);

#[async_trait]
impl AuthProvider for HeaderAuthProvider {
    async fn authenticate(&self, _parts: &mut Parts, _state: &AppState) -> Result<AuthContext, AppError> {
        Ok(AuthContext {
            user_id: Some(self.0),
            email: None,
            display_name: None,
            role: PlatformRole::User,
            break_glass: false,
            mfa_enroll_only: false,
        })
    }
}

async fn setup() -> Option<(sqlx::PgPool, u16, Uuid)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;

    let user = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'ws', $2, 'user')")
        .bind(user)
        .bind(format!("ws-{}@local.test", user.simple()))
        .execute(&pg)
        .await
        .ok()?;

    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_auth(Arc::new(HeaderAuthProvider(user)))
        .build();
    let app = http::router(state.clone(), None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((pg, port, user))
}

/// A socket, opened through a freshly minted single-use ticket.
async fn connect(
    port: u16,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let api = reqwest::Client::new();
    let ticket: serde_json::Value = api
        .post(format!("http://127.0.0.1:{port}/api/ws-ticket"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let t = ticket["ticket"].as_str().expect("ticket minted");
    let (socket, _) = connect_async(format!("ws://127.0.0.1:{port}/ws?ticket={t}"))
        .await
        .expect("ws connect");
    socket
}

async fn next_json<S>(socket: &mut S) -> Option<serde_json::Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(Ok(msg)) = socket.next().await {
        if let Message::Text(t) = msg {
            return serde_json::from_str(&t).ok();
        }
    }
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_frame_is_ignored_and_the_socket_lives_on() {
    let Some((_pg, port, _user)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let mut socket = connect(port).await;

    let hello = next_json(&mut socket).await.expect("hello");
    assert_eq!(hello["type"], "hello");

    // A frame from a client newer than this build.
    socket
        .send(Message::Text(
            serde_json::json!({
                "version": 1,
                "type": "desktop.something.not.invented.yet",
                "payload": { "anything": true },
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    // Followed by something this build does understand. If the unknown frame
    // had produced an error frame, it would arrive here instead of the pong —
    // and the application renders an error frame into the user's conversation,
    // which is why silence is the right answer rather than a complaint.
    socket
        .send(Message::Text(
            serde_json::json!({ "version": 1, "type": "ping" }).to_string().into(),
        ))
        .await
        .unwrap();

    let reply = timeout(Duration::from_secs(10), next_json(&mut socket))
        .await
        .expect("a reply arrives")
        .expect("the socket is still open");
    assert_eq!(reply["type"], "pong", "the next frame is the pong, not a complaint");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_json_does_not_break_the_socket_either() {
    let Some((_pg, port, _user)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let mut socket = connect(port).await;
    let _hello = next_json(&mut socket).await.expect("hello");

    socket
        .send(Message::Text("{not json at all".to_string().into()))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            serde_json::json!({ "version": 1, "type": "ping" }).to_string().into(),
        ))
        .await
        .unwrap();

    let reply = timeout(Duration::from_secs(10), next_json(&mut socket))
        .await
        .expect("a reply arrives")
        .expect("the socket is still open");
    assert_eq!(reply["type"], "pong");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handshake_is_optional_and_the_greeting_describes_the_instance() {
    let Some((_pg, port, user)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };

    // A connection that never introduces itself: exactly what every client did
    // before the handshake existed.
    let mut silent = connect(port).await;
    let hello = next_json(&mut silent).await.expect("hello");
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["user_id"], user.to_string());
    assert!(hello["resume_token"].as_str().is_some(), "still resumable");
    assert!(
        hello["server_version"].as_str().is_some_and(|v| !v.is_empty()),
        "the greeting says what it is talking to"
    );
    assert!(hello["features"].is_array(), "and what this instance can do");

    silent
        .send(Message::Text(
            serde_json::json!({ "version": 1, "type": "ping" }).to_string().into(),
        ))
        .await
        .unwrap();
    let reply = timeout(Duration::from_secs(10), next_json(&mut silent))
        .await
        .expect("a reply arrives")
        .expect("open");
    assert_eq!(reply["type"], "pong");

    // A connection that does introduce itself is answered the same way: nothing
    // is enforced on the handshake in this version.
    let mut speaking = connect(port).await;
    let _ = next_json(&mut speaking).await.expect("hello");
    speaking
        .send(Message::Text(
            serde_json::json!({
                "version": 1,
                "type": "client.hello",
                "client_kind": "desktop",
                "client_version": "9.9.9",
                "capabilities": ["local_execution"],
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    speaking
        .send(Message::Text(
            serde_json::json!({ "version": 1, "type": "ping" }).to_string().into(),
        ))
        .await
        .unwrap();
    let reply = timeout(Duration::from_secs(10), next_json(&mut speaking))
        .await
        .expect("a reply arrives")
        .expect("open");
    assert_eq!(reply["type"], "pong", "the handshake is accepted without comment");
}
