//! Team messaging (channels-messaging): chats, members, reliable messages with
//! WS live delivery + since-seq replay, shared notes (optimistic concurrency),
//! search, and the memory→project-chat system notification. Gated on PAI_E2E=1
//! (Postgres + Redis + Keycloak; no LLM, so these are quick).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

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
async fn live_delivery_replay_and_rbac() {
    if !enabled() {
        return;
    }
    let (pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let bob = token("bob").await.expect("bob");
    let carol = token("carol").await.expect("carol");
    let bob_id = whoami(&api, &base, &bob).await;
    let carol_id = whoami(&api, &base, &carol).await;

    // alice creates a group chat with bob.
    let chat: serde_json::Value = api
        .post(format!("{base}/api/group-chats"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "kind": "group", "name": "Team", "member_user_ids": [bob_id] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let chat_id = chat["id"].as_str().unwrap().to_string();

    // bob connects his socket; alice posts → bob receives a live group.message.
    let (mut sock, _) =
        connect_async(format!("ws://127.0.0.1:{port}/ws?token={bob}")).await.expect("bob ws");
    let _hello = next_json(&mut sock).await.expect("hello");

    let sent: serde_json::Value = api
        .post(format!("{base}/api/group-chats/{chat_id}/messages"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "content": "hello team" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sent["seq"], serde_json::json!(1));

    let mut delivered = false;
    for _ in 0..10 {
        let Ok(Some(f)) = timeout(Duration::from_secs(10), next_json(&mut sock)).await else { break };
        if f["type"] == "group.message" && f["content"] == "hello team" {
            assert_eq!(f["chat_id"].as_str().unwrap(), chat_id);
            delivered = true;
            break;
        }
    }
    assert!(delivered, "bob's socket should receive the live group.message");

    // Durable history.
    let hist: Vec<serde_json::Value> = api
        .get(format!("{base}/api/group-chats/{chat_id}/messages?since=0"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hist.len(), 1);

    // Replay-by-seq: bob "offline" (we just don't read the socket); alice posts 2.
    drop(sock);
    for n in ["second", "third"] {
        api.post(format!("{base}/api/group-chats/{chat_id}/messages"))
            .bearer_auth(&alice)
            .json(&serde_json::json!({ "content": n }))
            .send()
            .await
            .unwrap();
    }
    let caught: Vec<serde_json::Value> = api
        .get(format!("{base}/api/group-chats/{chat_id}/messages?since=1"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caught.len(), 2, "replay returns exactly the missed messages");
    assert_eq!(caught[0]["content"], "second");
    assert_eq!(caught[1]["content"], "third");
    assert_eq!(caught[0]["seq"], serde_json::json!(2));

    // RBAC: carol is not a member.
    let forbidden = api
        .get(format!("{base}/api/group-chats/{chat_id}/messages?since=0"))
        .bearer_auth(&carol)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status().as_u16(), 403);
    // alice adds carol → now allowed.
    let added = api
        .post(format!("{base}/api/group-chats/{chat_id}/members"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "user_id": carol_id }))
        .send()
        .await
        .unwrap();
    assert!(added.status().is_success());
    let ok = api
        .get(format!("{base}/api/group-chats/{chat_id}/messages?since=0"))
        .bearer_auth(&carol)
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success());

    // Search finds the phrase for a member.
    let hits: Vec<serde_json::Value> = api
        .get(format!("{base}/api/group-chats/search?q=third"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(hits.iter().any(|h| h["content"] == "third"));

    // Full-text stemming: "terminate" matches "terminated" (ILIKE would miss).
    api.post(format!("{base}/api/group-chats/{chat_id}/messages"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "content": "the contract was terminated" }))
        .send()
        .await
        .unwrap();
    let stem: Vec<serde_json::Value> = api
        .get(format!("{base}/api/group-chats/search?q=terminate"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(stem.iter().any(|h| h["content"] == "the contract was terminated"), "stemmed match");
    let absent: Vec<serde_json::Value> = api
        .get(format!("{base}/api/group-chats/search?q=zzznope"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(absent.is_empty(), "absent term yields no hits");

    // Audit + chain.
    let cid = uuid::Uuid::parse_str(&chat_id).unwrap();
    let created: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'chat.created' AND resource_id = $1",
    )
    .bind(cid)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(created >= 1);
    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn notes_optimistic_concurrency() {
    if !enabled() {
        return;
    }
    let (_pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");

    let chat: serde_json::Value = api
        .post(format!("{base}/api/group-chats"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "kind": "group", "name": "Notes" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let chat_id = chat["id"].as_str().unwrap().to_string();

    let note: serde_json::Value = api
        .post(format!("{base}/api/group-chats/{chat_id}/notes"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "content": "draft" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let note_id = note["id"].as_str().unwrap().to_string();
    assert_eq!(note["version"], serde_json::json!(1));

    // Correct version updates.
    let ok = api
        .put(format!("{base}/api/group-chats/{chat_id}/notes/{note_id}"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "content": "v2", "version": 1 }))
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success());

    // Stale version → 409 conflict.
    let stale = api
        .put(format!("{base}/api/group-chats/{chat_id}/notes/{note_id}"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "content": "v2-again", "version": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status().as_u16(), 409);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn project_memory_posts_system_message() {
    if !enabled() {
        return;
    }
    let (pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");

    // Creating a project auto-creates its project chat (owner = alice).
    let project: serde_json::Value = api
        .post(format!("{base}/api/projects"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "name": "Matter Z", "sector": "legal" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let project_id = uuid::Uuid::parse_str(project["id"].as_str().unwrap()).unwrap();

    // A project-scoped memory fact posts a system notice to the project chat.
    let r = api
        .post(format!("{base}/api/memory"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "content": "client prefers email", "scope": "project", "project_id": project_id }))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());

    let chat_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM group_chats WHERE project_id = $1 AND kind = 'project'")
            .bind(project_id)
            .fetch_one(&pg)
            .await
            .unwrap();
    let msgs: Vec<serde_json::Value> = api
        .get(format!("{base}/api/group-chats/{chat_id}/messages?since=0"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sys = msgs.iter().find(|m| m["message_type"] == "system");
    assert!(sys.is_some(), "a system message should be posted");
    let sys = sys.unwrap();
    assert!(sys["content"].as_str().unwrap().contains("client prefers email"));
    assert!(sys["sender_user_id"].is_null(), "system message has no sender");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_send_delivers_and_persists() {
    if !enabled() {
        return;
    }
    let (_pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let bob = token("bob").await.expect("bob");
    let carol = token("carol").await.expect("carol");
    let bob_id = whoami(&api, &base, &bob).await;

    let chat: serde_json::Value = api
        .post(format!("{base}/api/group-chats"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "kind": "group", "name": "WS", "member_user_ids": [bob_id] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let chat_id = chat["id"].as_str().unwrap().to_string();

    // alice + bob both connected; alice SENDS over the socket.
    let (mut asock, _) =
        connect_async(format!("ws://127.0.0.1:{port}/ws?token={alice}")).await.expect("alice ws");
    let _ = next_json(&mut asock).await; // hello
    let (mut bsock, _) =
        connect_async(format!("ws://127.0.0.1:{port}/ws?token={bob}")).await.expect("bob ws");
    let _ = next_json(&mut bsock).await; // hello

    asock
        .send(Message::Text(
            serde_json::json!({ "type": "group.send", "chat_id": chat_id, "content": "via socket" }).to_string(),
        ))
        .await
        .unwrap();

    // bob receives it; alice gets the echo (sender is a member).
    assert!(recv_group_msg(&mut bsock, "via socket").await, "bob receives the WS-sent message");
    assert!(recv_group_msg(&mut asock, "via socket").await, "sender gets the echo");

    // Persisted with a sequence number.
    let hist: Vec<serde_json::Value> = api
        .get(format!("{base}/api/group-chats/{chat_id}/messages?since=0"))
        .bearer_auth(&bob)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(hist.iter().any(|m| m["content"] == "via socket" && m["seq"].as_i64().unwrap() >= 1));

    // A non-member sending over the socket gets a chat.error, not delivery.
    let (mut csock, _) =
        connect_async(format!("ws://127.0.0.1:{port}/ws?token={carol}")).await.expect("carol ws");
    let _ = next_json(&mut csock).await; // hello
    csock
        .send(Message::Text(
            serde_json::json!({ "type": "group.send", "chat_id": chat_id, "content": "intruder" }).to_string(),
        ))
        .await
        .unwrap();
    let mut errored = false;
    for _ in 0..6 {
        let Ok(Some(f)) = timeout(Duration::from_secs(8), next_json(&mut csock)).await else { break };
        if f["type"] == "chat.error" {
            errored = true;
            break;
        }
    }
    assert!(errored, "non-member WS send should be rejected with chat.error");
}

async fn recv_group_msg(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    content: &str,
) -> bool {
    for _ in 0..10 {
        let Ok(Some(f)) = timeout(Duration::from_secs(10), next_json(socket)).await else { return false };
        if f["type"] == "group.message" && f["content"] == content {
            return true;
        }
    }
    false
}

/// Unread indicators (#12): a member's unread count reflects messages past their
/// read watermark; opening the chat (list_messages) clears it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unread_count_tracks_and_clears() {
    if !enabled() {
        return;
    }
    let (_pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let bob = token("bob").await.expect("bob");
    let bob_id = whoami(&api, &base, &bob).await;

    let chat: serde_json::Value = api
        .post(format!("{base}/api/group-chats"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "kind": "group", "name": "Unread", "member_user_ids": [bob_id] }))
        .send().await.unwrap().json().await.unwrap();
    let chat_id = chat["id"].as_str().unwrap().to_string();

    for n in ["first", "second"] {
        api.post(format!("{base}/api/group-chats/{chat_id}/messages"))
            .bearer_auth(&alice).json(&serde_json::json!({ "content": n })).send().await.unwrap();
    }

    // bob sees 2 unread in his chat list.
    let unread_of = |chats: &Vec<serde_json::Value>| -> i64 {
        chats.iter().find(|c| c["id"] == chat_id).and_then(|c| c["unread_count"].as_i64()).unwrap_or(-1)
    };
    let list1: Vec<serde_json::Value> =
        api.get(format!("{base}/api/group-chats")).bearer_auth(&bob).send().await.unwrap().json().await.unwrap();
    assert_eq!(unread_of(&list1), 2, "two unread before opening");

    // bob opens the chat → marks read.
    let _ = api.get(format!("{base}/api/group-chats/{chat_id}/messages?since=0")).bearer_auth(&bob).send().await.unwrap();

    let list2: Vec<serde_json::Value> =
        api.get(format!("{base}/api/group-chats")).bearer_auth(&bob).send().await.unwrap().json().await.unwrap();
    assert_eq!(unread_of(&list2), 0, "cleared after opening");
}

/// Shared-chats governance: a user lists and revokes their own chat shares; the
/// revoke cuts the target members' access and is audited.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_chat_list_revoke_and_access() {
    if !enabled() {
        return;
    }
    let (pg, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let bob = token("bob").await.expect("bob");
    let alice_id = whoami(&api, &base, &alice).await;
    let bob_id = whoami(&api, &base, &bob).await;

    // Seed an LLM chat owned by alice (chats are normally created over the WS).
    let llm_chat = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO chats (id, owner_user_id, title) VALUES ($1, $2, 'Shared brief')")
        .bind(llm_chat)
        .bind(alice_id)
        .execute(&pg)
        .await
        .unwrap();

    // alice creates a group with bob, then shares the LLM chat into it.
    let group: serde_json::Value = api
        .post(format!("{base}/api/group-chats"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "kind": "group", "name": "Team", "member_user_ids": [bob_id] }))
        .send().await.unwrap().json().await.unwrap();
    let group_id = group["id"].as_str().unwrap().to_string();

    let shared = api
        .post(format!("{base}/api/chats/{llm_chat}/share"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "group_chat_id": group_id }))
        .send().await.unwrap();
    assert!(shared.status().is_success(), "share: {}", shared.status());

    // bob (a group member) can now open the shared chat.
    let bob_read = api.get(format!("{base}/api/chats/{llm_chat}/messages")).bearer_auth(&bob).send().await.unwrap();
    assert!(bob_read.status().is_success(), "member reads shared chat while shared");

    // alice sees the share in her management list, with the friendly group name.
    let shares: Vec<serde_json::Value> =
        api.get(format!("{base}/api/chat-shares")).bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert!(shares.iter().any(|s| s["chat_id"] == llm_chat.to_string() && s["group_chat_name"] == "Team"));

    // bob (not the sharer) cannot revoke alice's share.
    let bob_revoke = api
        .delete(format!("{base}/api/chat-shares/{llm_chat}/{group_id}"))
        .bearer_auth(&bob).send().await.unwrap();
    assert_eq!(bob_revoke.status().as_u16(), 403, "non-sharer may not revoke");

    // alice revokes → 200, the list empties, and bob loses access.
    let revoke = api
        .delete(format!("{base}/api/chat-shares/{llm_chat}/{group_id}"))
        .bearer_auth(&alice).send().await.unwrap();
    assert!(revoke.status().is_success());
    let after: Vec<serde_json::Value> =
        api.get(format!("{base}/api/chat-shares")).bearer_auth(&alice).send().await.unwrap().json().await.unwrap();
    assert!(!after.iter().any(|s| s["chat_id"] == llm_chat.to_string()), "share gone from list");
    let bob_after = api.get(format!("{base}/api/chats/{llm_chat}/messages")).bearer_auth(&bob).send().await.unwrap();
    assert_eq!(bob_after.status().as_u16(), 403, "access cut after revoke");

    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);

    let _ = sqlx::query("DELETE FROM chats WHERE id = $1").bind(llm_chat).execute(&pg).await;
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
