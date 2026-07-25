//! Connecting a folder on a paired machine, and what may be done about it
//! afterwards.
//!
//! The same header-driven fake `AuthProvider` as the pairing suite: it yields a
//! chosen user when `X-Test-User` is present and fails otherwise, which is the
//! condition under which a device token falls through to its own resolution. So
//! both callers in these cases are real — a web session and a paired machine —
//! and the rules about which of them may do what are exercised as written.
//!
//! Needs Postgres (:5433) + Redis; skips if DATABASE_URL is unset.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::request::Parts;
use serde_json::{json, Value};
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

async fn setup() -> Option<(sqlx::PgPool, u16)> {
    setup_with(true).await
}

/// Spin an instance up with the folder family either on or off, so a case can
/// prove the switch actually refuses.
async fn setup_with(desktop_execution: bool) -> Option<(sqlx::PgPool, u16)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into();
    boot.features.desktop_execution = desktop_execution;
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_auth(Arc::new(HeaderAuthProvider))
        .build();
    let app = http::router(state, None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((pg, port))
}

async fn mk_user(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'folder', $2, 'user')")
        .bind(id)
        .bind(format!("folder-{}@local.test", id.simple()))
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn mk_chat(pg: &sqlx::PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO chats (id, owner_user_id, title) VALUES ($1, $2, 'folder chat')")
        .bind(id)
        .bind(owner)
        .execute(pg)
        .await
        .unwrap();
    id
}

fn fresh_ip() -> String {
    Uuid::now_v7().to_string()
}

/// Pair a machine to `user`, returning its device token.
async fn pair(api: &reqwest::Client, base: &str, user: Uuid) -> String {
    let code: Value = api
        .post(format!("{base}/api/me/devices/pairing-code"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let paired: Value = api
        .post(format!("{base}/api/device/pair"))
        .header("x-forwarded-for", fresh_ip())
        .json(&json!({
            "code": code["code"].as_str().unwrap(),
            "name": "Work laptop",
            "platform": "windows",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    paired["token"].as_str().expect("a device token").to_string()
}

/// Connect a folder as the machine holding `token`.
async fn connect(
    api: &reqwest::Client,
    base: &str,
    token: &str,
    path: &str,
    tier: &str,
) -> reqwest::Response {
    api.post(format!("{base}/api/me/workspaces"))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "path": path, "label": "", "tier": tier }))
        .send()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_is_connected_from_the_machine_that_has_it() {
    let Some((pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let token = pair(&api, &base, user).await;

    // From the machine: accepted, and the path comes back folded to its stored
    // form (the trailing separator and the mixed slashes are the same folder).
    let created = connect(&api, &base, &token, "c:/work/demo/", "rw").await;
    assert_eq!(created.status(), 201);
    let created: Value = created.json().await.unwrap();
    assert_eq!(created["path"], "C:\\work\\demo");
    assert_eq!(created["tier"], "rw");

    // From a web session: refused. The browser was never in a position to ask
    // anybody about a folder it cannot see.
    let from_web = api
        .post(format!("{base}/api/me/workspaces"))
        .header("x-test-user", user.to_string())
        .json(&json!({ "path": "C:\\work\\other", "tier": "rw" }))
        .send()
        .await
        .unwrap();
    assert_eq!(from_web.status(), 403, "a folder is connected from the desktop, not the web");

    // Connecting the same folder again is the same grant, at the newly agreed
    // level — not a second one that a single withdrawal would leave behind.
    let again: Value = connect(&api, &base, &token, "C:\\work\\demo", "ro")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(again["id"], created["id"], "one folder, one grant");
    assert_eq!(again["tier"], "ro", "re-connecting agrees the new level");

    // Seeing them is a web thing as much as a machine thing.
    let listed: Value = api
        .get(format!("{base}/api/me/workspaces"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["device_name"], "Work laptop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_that_is_not_a_folder_to_work_in_is_refused() {
    let Some((pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let token = pair(&api, &base, user).await;

    for (path, why) in [
        ("work/demo", "relative"),
        ("C:\\work\\..\\secrets", "climbs out"),
        ("\\\\server\\share", "a network share"),
        ("", "empty"),
    ] {
        let res = connect(&api, &base, &token, path, "rw").await;
        assert_eq!(res.status(), 400, "{path} ({why}) should be refused");
    }
    // And an unknown level of trust is not a level of trust.
    assert_eq!(connect(&api, &base, &token, "C:\\work\\demo", "root").await.status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conversation_works_in_one_folder_and_only_its_owner_can_choose_it() {
    let Some((pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let owner = mk_user(&pg).await;
    let stranger = mk_user(&pg).await;
    let token = pair(&api, &base, owner).await;
    let chat = mk_chat(&pg, owner).await;

    let first: Value = connect(&api, &base, &token, "C:\\work\\one", "rw").await.json().await.unwrap();
    let second: Value = connect(&api, &base, &token, "C:\\work\\two", "rw").await.json().await.unwrap();

    let bind = |user: Uuid, ws: &str| {
        let api = api.clone();
        let base = base.clone();
        let ws = ws.to_string();
        async move {
            api.put(format!("{base}/api/chats/{chat}/workspace"))
                .header("x-test-user", user.to_string())
                .json(&json!({ "workspace_id": ws }))
                .send()
                .await
                .unwrap()
        }
    };

    assert_eq!(bind(owner, first["id"].as_str().unwrap()).await.status(), 204);
    // A second binding replaces the first: one folder at a time, so an approval
    // never has to explain which folder it meant.
    assert_eq!(bind(owner, second["id"].as_str().unwrap()).await.status(), 204);
    let bound: Value = api
        .get(format!("{base}/api/chats/{chat}/workspace"))
        .header("x-test-user", owner.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bound["path"], "C:\\work\\two");

    // Somebody else's conversation is not theirs to point at their folder, and
    // is reported as missing rather than forbidden.
    assert_eq!(bind(stranger, first["id"].as_str().unwrap()).await.status(), 404);

    // Withdrawing the folder leaves nothing bound to it.
    let revoked = api
        .delete(format!("{base}/api/me/workspaces/{}", second["id"].as_str().unwrap()))
        .header("x-test-user", owner.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 204);
    let after: Value = api
        .get(format!("{base}/api/chats/{chat}/workspace"))
        .header("x-test-user", owner.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(after.is_null(), "a withdrawn folder is not the folder a chat works in");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agreeing_to_a_command_is_recorded_and_cannot_smuggle_a_second_one() {
    let Some((pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let token = pair(&api, &base, user).await;
    let ws: Value = connect(&api, &base, &token, "C:\\work\\demo", "rw").await.json().await.unwrap();
    let ws_id = ws["id"].as_str().unwrap().to_string();

    // From the machine, which is where the card offering it is shown.
    let added = api
        .post(format!("{base}/api/workspaces/{ws_id}/command-prefixes"))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "prefix": "  npm   test " }))
        .send()
        .await
        .unwrap();
    assert_eq!(added.status(), 201);
    let added: Value = added.json().await.unwrap();
    assert_eq!(added["prefix"], "npm test", "stored in the form it is matched in");

    // A prefix that could chain is refused where it is agreed, not quietly
    // ignored when it is matched.
    let chained = api
        .post(format!("{base}/api/workspaces/{ws_id}/command-prefixes"))
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({ "prefix": "npm test && rm -rf ." }))
        .send()
        .await
        .unwrap();
    assert_eq!(chained.status(), 400);

    // It is recorded, with the folder and the machine it belongs to.
    let logged: Option<Value> = sqlx::query_scalar(
        "SELECT payload FROM audit_events WHERE action_type = 'workspace.command_allowed' \
         AND resource_id = $1 ORDER BY occurred_at DESC LIMIT 1",
    )
    .bind(Uuid::parse_str(&ws_id).unwrap())
    .fetch_optional(&pg)
    .await
    .unwrap()
    .flatten();
    let logged = logged.expect("agreeing to a command is recorded");
    assert_eq!(logged["prefix"], "npm test");
    assert!(logged["device_id"].is_string());

    // And it can be taken back.
    let removed = api
        .delete(format!(
            "{base}/api/workspaces/{ws_id}/command-prefixes/{}",
            added["id"].as_str().unwrap()
        ))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), 204);
    let left: Value = api
        .get(format!("{base}/api/workspaces/{ws_id}/command-prefixes"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(left.as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn switching_the_family_off_refuses_new_connections() {
    let Some((pg, port)) = setup_with(false).await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let token = pair(&api, &base, user).await;

    // With the capability off, registering a folder is refused rather than left to
    // record metadata for tools that can never run.
    let res = connect(&api, &base, &token, "C:\\work\\demo", "rw").await;
    assert_eq!(res.status(), 403, "connecting a folder is refused when the family is off");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_users_folder_is_not_reachable() {
    let Some((pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let owner = mk_user(&pg).await;
    let stranger = mk_user(&pg).await;
    let token = pair(&api, &base, owner).await;
    let ws: Value = connect(&api, &base, &token, "C:\\work\\private", "rw").await.json().await.unwrap();
    let ws_id = ws["id"].as_str().unwrap();

    for res in [
        api.get(format!("{base}/api/workspaces/{ws_id}/command-prefixes"))
            .header("x-test-user", stranger.to_string())
            .send()
            .await
            .unwrap(),
        api.post(format!("{base}/api/workspaces/{ws_id}/command-prefixes"))
            .header("x-test-user", stranger.to_string())
            .json(&json!({ "prefix": "npm test" }))
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(res.status(), 404, "somebody else's folder is not there to be found");
    }

    // Withdrawing it is likewise not theirs to do: the folder is untouched.
    let revoked = api
        .delete(format!("{base}/api/me/workspaces/{ws_id}"))
        .header("x-test-user", stranger.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), 204, "an indistinguishable no-op");
    let still: Option<Option<time::OffsetDateTime>> =
        sqlx::query_scalar("SELECT revoked_at FROM device_workspaces WHERE id = $1")
            .bind(Uuid::parse_str(ws_id).unwrap())
            .fetch_optional(&pg)
            .await
            .unwrap();
    assert!(still.flatten().is_none(), "the owner's folder is still connected");
}
