//! Agent completions over the compatibility surface, and what conversation
//! origin means for the rest of the platform.
//!
//! Scope: everything up to and including the handover to the chat pipeline —
//! which agents a key may address, which conversation it may continue, that a
//! conversation it creates is marked and stays out of the chat lists, and that
//! the retention sweep is off unless configured. Driving a full turn needs a
//! real generation service and belongs to an end-to-end run.
//!
//! Needs Postgres (:5433) + Redis; skips if DATABASE_URL is unset.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::request::Parts;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::AppError;
use fosnie_backend::ext::AuthProvider;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::{cache, db, http};

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

async fn mk_user(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'apiagent', $2, 'user')")
        .bind(id)
        .bind(format!("apiagent-{}@local.test", id.simple()))
        .execute(pg)
        .await
        .unwrap();
    id
}

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

async fn mk_agent(pg: &sqlx::PgPool, owner: Option<Uuid>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO agents (id, name, description, system_prompt, created_by, modes) \
         VALUES ($1, 'Test agent', 'for tests', 'You are a test.', $2, ARRAY['general'])",
    )
    .bind(id)
    .bind(owner)
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn mk_chat(pg: &sqlx::PgPool, owner: Uuid, origin: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO chats (id, owner_user_id, title, origin) VALUES ($1, $2, 'existing', $3)")
        .bind(id)
        .bind(owner)
        .bind(origin)
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn setup() -> Option<(AppState, sqlx::PgPool, u16)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_auth(Arc::new(HeaderAuthProvider))
        .build();
    let app = http::router(state.clone(), None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((state, pg, port))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_agents_the_caller_can_see_are_addressable() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let owner = mk_user(&pg).await;
    let other = mk_user(&pg).await;
    let private_agent = mk_agent(&pg, Some(owner)).await;
    let other_key = mk_key(&pg, other).await;

    let res = api
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", format!("Bearer {other_key}"))
        .json(&serde_json::json!({
            "model": format!("agent/{private_agent}"),
            "messages": [{ "role": "user", "content": "hello" }],
        }))
        .send()
        .await
        .unwrap();
    // Refused as absent rather than forbidden: whether someone else's agent
    // exists is not something a key should be able to probe.
    assert_eq!(res.status(), 404);
    let err: serde_json::Value = res.json().await.unwrap();
    assert_eq!(err["error"]["code"], "model_not_found");

    // A malformed identifier is the same answer, not a server error.
    let malformed = api
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", format!("Bearer {other_key}"))
        .json(&serde_json::json!({
            "model": "agent/not-a-uuid",
            "messages": [{ "role": "user", "content": "hello" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_cannot_post_into_the_owners_own_conversations() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let stranger = mk_user(&pg).await;
    let key = mk_key(&pg, user).await;
    let agent = mk_agent(&pg, Some(user)).await;

    let web_chat = mk_chat(&pg, user, "web").await;
    let someone_elses = mk_chat(&pg, stranger, "api").await;

    for (label, chat) in [
        ("the owner's own application conversation", web_chat),
        ("another user's API conversation", someone_elses),
    ] {
        let res = api
            .post(format!("{base}/v1/chat/completions"))
            .header("authorization", format!("Bearer {key}"))
            .header("x-fosnie-chat-id", chat.to_string())
            .json(&serde_json::json!({
                "model": format!("agent/{agent}"),
                "messages": [{ "role": "user", "content": "hello" }],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 404, "{label} cannot be continued through a key");
    }

    // Nothing was written into the conversation it tried to reach.
    let messages = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM messages WHERE chat_id = $1")
        .bind(web_chat)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(messages, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_question_without_text_is_refused_before_anything_is_created() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let key = mk_key(&pg, user).await;
    let agent = mk_agent(&pg, Some(user)).await;

    let before = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chats WHERE owner_user_id = $1")
        .bind(user)
        .fetch_one(&pg)
        .await
        .unwrap();

    // Only an assistant turn: there is no question to answer.
    let res = api
        .post(format!("{base}/v1/chat/completions"))
        .header("authorization", format!("Bearer {key}"))
        .json(&serde_json::json!({
            "model": format!("agent/{agent}"),
            "messages": [{ "role": "assistant", "content": "I said something" }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let after = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chats WHERE owner_user_id = $1")
        .bind(user)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(before, after, "a refused request leaves no conversation behind");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn programmatic_conversations_stay_out_of_the_chat_lists() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;

    let web = mk_chat(&pg, user, "web").await;
    let desktop = mk_chat(&pg, user, "desktop").await;
    let programmatic = mk_chat(&pg, user, "api").await;

    let listed: serde_json::Value = api
        .get(format!("{base}/api/chats"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<String> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect();

    assert!(ids.contains(&web.to_string()), "application conversations are listed");
    assert!(
        ids.contains(&desktop.to_string()),
        "so are conversations from another first-class client: the rule excludes machine \
         traffic, it does not admit only the web"
    );
    assert!(!ids.contains(&programmatic.to_string()), "machine traffic is not");

    // The origin travels to the client so an interface can mark where a
    // conversation came from.
    let desktop_row = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == desktop.to_string())
        .unwrap();
    assert_eq!(desktop_row["origin"], "desktop");

    // Still readable directly, which is what makes an API conversation
    // debuggable rather than invisible.
    let direct = api
        .get(format!("{base}/api/chats/{programmatic}/messages"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(direct.status(), 200);
}
