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

//! Reaching an outside system from a telephone call, and telling somebody outside about it.
//!
//! Two properties carry this slice, and neither is visible from a unit test because both
//! are about what a whole turn does with a tool.
//!
//! **Nothing on a call waits for a person.** Everywhere else, a tool that changes something
//! outside this deployment is held for approval. On a call that would be minutes of silence
//! on a live line, so the decision is made in advance by configuration: an unmarked tool is
//! refused in words the caller can be given, and a marked one runs. The test drives a real
//! turn against a real local server and reads both what came back to the model and whether
//! that server was ever touched.
//!
//! **What leaves says who rang and what about, and nothing else.** Delivery is driven here
//! against a receiver that records exactly what arrived.
//!
//! Needs a reachable Postgres and Redis; skips when `DATABASE_URL` is unset.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State as AxState;
use axum::Json;
use common::mock_ml::{self, MlScript};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::mpsc;
use tokio::sync::Notify;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::runtime::{self, ConfigValueType};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::telephony::notify::{self, Event};
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{cache, chat, db};

/// The tool this file registers, named per run.
///
/// Tool names are unique across the deployment, so a fixed one would make a run that fell
/// over before its clean-up block every run after it, and two files could never overlap.
fn tool_name() -> String {
    format!("book_outside_{:x}", Uuid::now_v7().as_u128() % 0xffff_ffff)
}

/// What a local stand-in for somebody else's system recorded.
#[derive(Clone, Default)]
struct Recorder {
    hits: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<Value>>>,
    /// How long each request should take before answering, so a slow service can be tested
    /// without one.
    delay: Arc<Mutex<Duration>>,
}

impl Recorder {
    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }
    fn hold_for(&self, d: Duration) {
        *self.delay.lock().unwrap() = d;
    }
}

/// A server that stands in for whatever a practice actually runs on.
async fn outside() -> (String, Recorder) {
    let rec = Recorder::default();
    let app = axum::Router::new()
        .route(
            "/hook",
            // Bytes rather than a typed body: a real service takes what it is sent, and a
            // stand-in that refuses an empty or untyped request would fail the very calls
            // this file exists to observe.
            axum::routing::post(|AxState(r): AxState<Recorder>, body: axum::body::Bytes| async move {
                let wait = { *r.delay.lock().unwrap() };
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
                r.hits.fetch_add(1, Ordering::SeqCst);
                let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                r.bodies.lock().unwrap().push(parsed);
                Json(json!({ "ok": true }))
            }),
        )
        .with_state(rec.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://127.0.0.1:{port}/hook"), rec)
}

struct Cast {
    state: AppState,
    ml: mock_ml::MockMl,
    pg: PgPool,
    owner: Uuid,
    agent: Uuid,
    chat: Uuid,
    line: Uuid,
    call: Uuid,
    tool: Uuid,
    tool_name: String,
    /// What was there before this test touched the deployment-wide connector flags.
    borrowed: Vec<(String, Option<String>)>,
}

fn ctx_for(user: Uuid) -> AuthContext {
    AuthContext {
        user_id: Some(user),
        email: None,
        display_name: None,
        role: PlatformRole::User,
        break_glass: false,
        mfa_enroll_only: false,
    }
}

/// The connectors this file switches on, and therefore has to put back: they are
/// deployment-wide, and a developer's database is not this test's to rearrange.
const BORROWED: [&str; 2] = ["integration.custom_tool.enabled", "integration.notify.enabled"];

/// One test at a time in this file.
///
/// Those two flags are deployment-wide and every test here writes them. Overlapping, one
/// would put them back while another was mid-turn, and the symptom is a connector that is
/// dormant halfway through a test that switched it on. The flags really are shared, so
/// serialising is the honest fix rather than pretending otherwise.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn cast(url: &str) -> Option<Cast> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:16379".into());
    let pg = db::connect(&db_url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    });
    let redis = cache::create_pool(&redis_url).expect("redis pool");
    let name = tool_name();
    // A model that asks for the tool as soon as it is offered one, and then answers.
    let ml = mock_ml::spawn(MlScript {
        generate_tool_call: Some((name.clone(), json!({ "when": "Tuesday" }))),
        generate_tokens: vec!["Done.".into()],
        ..MlScript::default()
    })
    .await;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.ml.base_url = ml.base_url.clone();
    boot.features.telephony = true;
    boot.message_encryption_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".into();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot)).build();

    let mut borrowed = Vec::new();
    for key in BORROWED {
        borrowed.push((key.to_string(), runtime::get(&pg, key).await.ok().flatten().map(|e| e.value)));
    }
    for key in BORROWED {
        runtime::set(&pg, key, "true", ConfigValueType::Bool, "global", None, "system")
            .await
            .expect("switch the connector on");
    }

    let owner = mk_user(&pg).await;
    let tool = mk_tool(&pg, url, &name).await;
    let agent = mk_agent(&pg, owner, &name).await;
    let line = mk_line(&pg, owner, agent).await;
    let call = mk_call(&pg, owner, agent, line).await;
    let chat = mk_chat(&pg, owner, agent).await;
    Some(Cast { state, ml, pg, owner, agent, chat, line, call, tool, tool_name: name, borrowed })
}

async fn clear_up(c: &Cast) {
    for (key, was) in &c.borrowed {
        match was {
            Some(v) => {
                let _ =
                    runtime::set(&c.pg, key, v, ConfigValueType::Bool, "global", None, "system").await;
            }
            None => {
                let _ = runtime::unset(&c.pg, key, "system").await;
            }
        }
    }
    for sql in [
        "DELETE FROM notify_targets WHERE owner_user_id = $1",
        "DELETE FROM enquiries WHERE owner_user_id = $1",
        "DELETE FROM calls WHERE owner_user_id = $1",
        "DELETE FROM phone_numbers WHERE owner_user_id = $1",
        "DELETE FROM messages WHERE chat_id IN (SELECT id FROM chats WHERE owner_user_id = $1)",
        "DELETE FROM chats WHERE owner_user_id = $1",
        "DELETE FROM agent_tools WHERE agent_id IN (SELECT id FROM agents WHERE created_by = $1)",
        "DELETE FROM agents WHERE created_by = $1",
        "DELETE FROM users WHERE id = $1",
    ] {
        let _ = sqlx::query(sql).bind(c.owner).execute(&c.pg).await;
    }
    let _ = sqlx::query("DELETE FROM tasks WHERE task_type = 'notify_deliver'").execute(&c.pg).await;
    let _ = sqlx::query("DELETE FROM custom_tools WHERE id = $1").bind(c.tool).execute(&c.pg).await;
}

async fn mk_user(pg: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'Line owner', $2, 'user')")
        .bind(id)
        .bind(format!("{id}@example.test"))
        .execute(pg)
        .await
        .unwrap();
    id
}

/// A tool that changes something in somebody else's system: exactly the shape this slice is
/// about. Approved and enabled, so the only question left is the one being tested.
async fn mk_tool(pg: &PgPool, url: &str, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO custom_tools \
           (id, name, display_name, description, kind, params_schema, config, \
            requires_egress, side_effecting, enabled, approved_version, version) \
         VALUES ($1, $2, 'Book outside', 'Book a time in the practice system', 'http', $3, $4, \
                 false, true, true, 1, 1)",
    )
    .bind(id)
    .bind(name)
    .bind(json!({ "type": "object", "properties": { "when": { "type": "string" } } }))
    .bind(json!({ "method": "POST", "url": url }))
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn mk_agent(pg: &PgPool, owner: Uuid, tool: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO agents (id, name, description, system_prompt, created_by, modes) \
         VALUES ($1, 'Reception', '', 'Answer the telephone.', $2, ARRAY['general'])",
    )
    .bind(id)
    .bind(owner)
    .execute(pg)
    .await
    .unwrap();
    sqlx::query("INSERT INTO agent_tools (agent_id, tool_name) VALUES ($1, $2)")
        .bind(id)
        .bind(tool)
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn mk_line(pg: &PgPool, owner: Uuid, agent: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO phone_numbers (id, e164, provider, owner_user_id, agent_id, enabled) \
         VALUES ($1, $2, 'twilio', $3, $4, true)",
    )
    .bind(id)
    .bind(format!("+44131555{:05}", Uuid::now_v7().as_u128() % 100_000))
    .bind(owner)
    .bind(agent)
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn mk_call(pg: &PgPool, owner: Uuid, agent: Uuid, line: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO calls (id, phone_number_id, provider, provider_call_id, to_e164, from_e164, \
                            owner_user_id, agent_id) \
         VALUES ($1, $2, 'twilio', $3, '+441315550000', '+447700900123', $4, $5)",
    )
    .bind(id)
    .bind(line)
    .bind(format!("CA{}", id.simple()))
    .bind(owner)
    .bind(agent)
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn mk_chat(pg: &PgPool, owner: Uuid, agent: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO chats (id, owner_user_id, agent_id, title, origin) \
         VALUES ($1, $2, $3, 'A call', 'phone')",
    )
    .bind(id)
    .bind(owner)
    .bind(agent)
    .execute(pg)
    .await
    .unwrap();
    id
}

/// Mark the tool as usable while somebody is on the telephone, or not.
async fn mark_for_calls(c: &Cast, allowed: bool) {
    sqlx::query("UPDATE custom_tools SET allow_on_call = $2 WHERE id = $1")
        .bind(c.tool)
        .bind(allowed)
        .execute(&c.pg)
        .await
        .unwrap();
}

/// Drive one turn, on a call or not, and return how long it took.
///
/// The turn is run to completion rather than sampled: what is being tested is what the
/// whole turn does with the tool, and a turn that is still going has not decided yet.
async fn one_turn(c: &Cast, on_call: bool) -> Duration {
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(256);
    let cancel = Arc::new(Notify::new());
    let st = c.state.clone();
    let cx = ctx_for(c.owner);
    let chat_id = c.chat;
    let agent = c.agent;
    let call = if on_call { Some(c.call) } else { None };
    let started = tokio::time::Instant::now();
    tokio::spawn(async move {
        let turn = chat::origin::TurnContext::new(
            &cx,
            if call.is_some() {
                chat::origin::ChatOrigin::Phone
            } else {
                chat::origin::ChatOrigin::Web
            },
        )
        .with_call(call);
        chat::run_turn(
            &st,
            turn,
            Uuid::now_v7(),
            Some(chat_id),
            None,
            Some(agent),
            "Please book me in".into(),
            Vec::new(),
            Vec::new(),
            false,
            None,
            None,
            None,
            None,
            &tx,
            cancel,
        )
        .await;
    });
    // Drain until the turn is done or the socket closes.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, rx.recv()).await {
            Ok(Some(ServerFrame::ChatCompleted { .. })) | Ok(None) => break,
            Ok(Some(_)) => continue,
            Err(_) => break,
        }
    }
    started.elapsed()
}

async fn audited(c: &Cast, action: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_events WHERE action_type = $1")
        .bind(action)
        .fetch_one(&c.pg)
        .await
        .unwrap_or(0)
}

/// A tool that changes something outside, asked for while somebody is on the telephone, on
/// a definition nobody marked for that: refused, in words, without touching anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unmarked_tool_is_refused_on_a_call_and_nothing_is_touched() {
    let _one_at_a_time = ONE_AT_A_TIME.lock().await;
    let (url, rec) = outside().await;
    let Some(c) = cast(&url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let before = audited(&c, "telephony.tool.refused").await;
    mark_for_calls(&c, false).await;
    one_turn(&c, true).await;
    let results = c.ml.calls.tool_replies();
    let hits = rec.hits();
    let after = audited(&c, "telephony.tool.refused").await;
    clear_up(&c).await;

    assert_eq!(hits, 0, "an unmarked tool reached the outside system anyway");
    assert!(!results.is_empty(), "the model was told nothing at all");
    assert!(
        results.iter().any(|r| r.contains("take a message")),
        "the caller was not offered anything: {results:?}"
    );
    assert!(after > before, "the refusal was not recorded");
}

/// The same tool, marked: it runs, and the fact that an anonymous caller caused a change
/// outside this deployment is in the trail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_marked_tool_runs_on_a_call_and_is_recorded() {
    let _one_at_a_time = ONE_AT_A_TIME.lock().await;
    let (url, rec) = outside().await;
    let Some(c) = cast(&url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let before = audited(&c, "telephony.tool.allowed").await;
    mark_for_calls(&c, true).await;
    one_turn(&c, true).await;
    let hits = rec.hits();
    let results = c.ml.calls.tool_replies();
    let after = audited(&c, "telephony.tool.allowed").await;
    clear_up(&c).await;

    assert_eq!(hits, 1, "a marked tool did not reach the outside system; it was told {results:?}");
    assert!(after > before, "a change made for a caller was not recorded");
}

/// A service that does not answer must not become silence on a live line.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_service_gives_the_caller_an_answer_instead_of_silence() {
    let _one_at_a_time = ONE_AT_A_TIME.lock().await;
    let (url, rec) = outside().await;
    let Some(c) = cast(&url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    mark_for_calls(&c, true).await;
    // Well past the ceiling this test sets, so the only way the turn finishes quickly is
    // the ceiling being honoured.
    rec.hold_for(Duration::from_secs(30));
    runtime::set(&c.pg, "telephony.tool_timeout_secs", "2", ConfigValueType::Int, "global", None, "system")
        .await
        .expect("set the ceiling");

    let took = one_turn(&c, true).await;
    let results = c.ml.calls.tool_replies();
    let timeouts = audited(&c, "telephony.tool.timeout").await;
    let _ = runtime::unset(&c.pg, "telephony.tool_timeout_secs", "system").await;
    clear_up(&c).await;

    assert!(
        results.iter().any(|r| r.contains("take a message")),
        "the caller was left with nothing to be told: {results:?}"
    );
    assert!(timeouts > 0, "the wait was not recorded");
    assert!(took < Duration::from_secs(25), "the caller waited {took:?} on a two second ceiling");
}

/// Off a call nothing about any of this applies: the same tool still takes the approval
/// path, which is what it did before this existed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn off_a_call_the_same_tool_still_waits_for_a_person() {
    let _one_at_a_time = ONE_AT_A_TIME.lock().await;
    let (url, rec) = outside().await;
    let Some(c) = cast(&url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    // Not marked, which on a call would be a refusal. Off one it is simply held.
    mark_for_calls(&c, false).await;
    let pending_before =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM agent_runs WHERE status = 'awaiting_approval'")
            .fetch_one(&c.pg)
            .await
            .unwrap_or(0);
    // The approval nobody gives times out; a short one keeps the test honest and quick.
    sqlx::query("UPDATE agents SET params = jsonb_set(COALESCE(params, '{}'::jsonb), '{approval_timeout_secs}', '3') WHERE id = $1")
        .bind(c.agent)
        .execute(&c.pg)
        .await
        .unwrap();

    one_turn(&c, false).await;
    let hits = rec.hits();
    let results = c.ml.calls.tool_replies();
    let pending_after =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM agent_runs WHERE status = 'awaiting_approval'")
            .fetch_one(&c.pg)
            .await
            .unwrap_or(0);
    clear_up(&c).await;

    assert_eq!(hits, 0, "an unapproved tool ran off a call");
    assert!(
        !results.iter().any(|r| r.contains("take a message")),
        "the telephone refusal was given to somebody who is not on a telephone: {results:?}"
    );
    assert!(
        pending_after > pending_before || results.iter().any(|r| r.contains("approv")),
        "the call was neither held for approval nor recorded as one: {results:?}"
    );
}

/// A message taken queues one line for every target that wants it, and none for one that
/// does not, and what is posted says who rang and what about and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_reaches_the_targets_that_asked_for_it() {
    let _one_at_a_time = ONE_AT_A_TIME.lock().await;
    let (url, rec) = outside().await;
    let Some(c) = cast(&url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = run_notify(&c, &url, &rec).await;
    clear_up(&c).await;
    outcome.expect("the notification was wrong");
}

async fn run_notify(c: &Cast, url: &str, rec: &Recorder) -> Result<(), String> {
    let wants = mk_target(c, url, &["message_taken"], true).await;
    let _elsewhere = mk_target(c, url, &["appointment_booked"], true).await;
    let _off = mk_target(c, url, &["message_taken"], false).await;

    let queued = notify::fire(
        &c.state,
        c.owner,
        Event::MessageTaken,
        "+447700900123",
        "Wants a call back about the survey",
    )
    .await;
    if queued != 1 {
        return Err(format!("{queued} deliveries were queued, not one"));
    }

    // Deliver it the way the worker does.
    notify::deliver(
        &c.state,
        &json!({
            "target_id": wants,
            "event": "message_taken",
            "text": notify::line(Event::MessageTaken, "+447700900123", "Wants a call back about the survey"),
        }),
    )
    .await
    .map_err(|e| format!("delivery failed: {e}"))?;

    if rec.hits() != 1 {
        return Err(format!("the service was posted to {} times", rec.hits()));
    }
    let body = rec.bodies().pop().ok_or("nothing was posted")?;
    let text = body["text"].as_str().unwrap_or_default();
    if !text.contains("+447700900123") || !text.contains("survey") {
        return Err(format!("the line does not say who rang or what about: {text}"));
    }
    if text.contains("Please book me in") {
        return Err("what the caller said left the deployment".into());
    }
    Ok(())
}

/// Dormant is dormant: with the connector switched off nothing leaves, and the attempt is
/// in the trail rather than lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_leaves_while_the_connector_is_dormant() {
    let _one_at_a_time = ONE_AT_A_TIME.lock().await;
    let (url, rec) = outside().await;
    let Some(c) = cast(&url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let target = mk_target(&c, &url, &["message_taken"], true).await;
    runtime::set(&c.pg, "integration.notify.enabled", "false", ConfigValueType::Bool, "global", None, "system")
        .await
        .expect("switch it off");
    let blocked_before = audited(&c, "integration.blocked").await;
    let refused = notify::deliver(
        &c.state,
        &json!({ "target_id": target, "event": "message_taken", "text": "hello" }),
    )
    .await;
    let blocked_after = audited(&c, "integration.blocked").await;
    let hits = rec.hits();
    clear_up(&c).await;

    assert!(refused.is_err(), "a dormant connector posted anyway");
    assert_eq!(hits, 0, "something left a deployment that has notifications switched off");
    assert!(blocked_after > blocked_before, "the blocked attempt was not recorded");
}

/// Somebody else's arrangements are not readable, writable or testable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stranger_cannot_reach_another_practices_targets() {
    let _one_at_a_time = ONE_AT_A_TIME.lock().await;
    use axum::extract::{Path, Query, State};
    use fosnie_backend::auth::keycloak::AuthUser;
    use fosnie_backend::http::notify_targets::{self, WhoseTargets};

    let (url, _rec) = outside().await;
    let Some(c) = cast(&url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let target = mk_target(&c, &url, &["message_taken"], true).await;
    let stranger = mk_user(&c.pg).await;
    let _ = &c.tool_name;

    let read = notify_targets::list(
        State(c.state.clone()),
        AuthUser(ctx_for(stranger)),
        Query(WhoseTargets { owner_user_id: Some(c.owner) }),
    )
    .await;
    let tested = notify_targets::test(
        State(c.state.clone()),
        AuthUser(ctx_for(stranger)),
        Path(target),
    )
    .await;
    let removed = notify_targets::remove(
        State(c.state.clone()),
        AuthUser(ctx_for(stranger)),
        Path(target),
    )
    .await;
    // The account's own reading still works, which is what makes the refusals above about
    // who is asking rather than about something being broken.
    let mine = notify_targets::list(
        State(c.state.clone()),
        AuthUser(ctx_for(c.owner)),
        Query(WhoseTargets { owner_user_id: None }),
    )
    .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(stranger).execute(&c.pg).await;
    let listed = mine.map(|j| j.0.len()).unwrap_or(0);
    let shown_host = notify_targets::list(
        State(c.state.clone()),
        AuthUser(ctx_for(c.owner)),
        Query(WhoseTargets { owner_user_id: None }),
    )
    .await
    .map(|j| j.0.first().map(|t| t.host.clone()).unwrap_or_default())
    .unwrap_or_default();
    clear_up(&c).await;

    assert!(read.is_err(), "a stranger read another practice's targets");
    assert!(tested.is_err(), "a stranger made another practice's target post something");
    assert!(removed.is_err(), "a stranger deleted another practice's target");
    assert_eq!(listed, 1, "the account could not read its own");
    assert_eq!(shown_host, "127.0.0.1", "the host is what a reader sees");
}

async fn mk_target(c: &Cast, url: &str, events: &[&str], enabled: bool) -> Uuid {
    let id = Uuid::now_v7();
    let enc = fosnie_backend::crypto::encrypt_at_rest(url).expect("encrypts");
    let events: Vec<String> = events.iter().map(|e| e.to_string()).collect();
    sqlx::query(
        "INSERT INTO notify_targets (id, owner_user_id, label, kind, url_enc, events, enabled) \
         VALUES ($1, $2, 'A channel', 'webhook', $3, $4, $5)",
    )
    .bind(id)
    .bind(c.owner)
    .bind(enc)
    .bind(&events)
    .bind(enabled)
    .execute(&c.pg)
    .await
    .unwrap();
    id
}
