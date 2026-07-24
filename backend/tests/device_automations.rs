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

//! Device automations: the parts that decide outcomes without ever running an
//! LLM — the reconnect catch-up, the approval-expiry sweep, and create/update
//! validation. Each is exercised against a real Postgres. The whole file skips
//! when DATABASE_URL is unset, so it is inert in an environment without one.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::Json;
use serde_json::json;
use time::OffsetDateTime;
use tokio::sync::mpsc;
use uuid::Uuid;

use fosnie_backend::tools::desktop::{self, DesktopSink, DesktopToolCtx, Tier, Workspace};
use fosnie_backend::tools::DesktopReply;
use fosnie_backend::ws::protocol::ServerFrame;

use fosnie_backend::auth::keycloak::AuthUser;
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::http::automations::{
    create_automation, update_automation, CreateAutomation, UpdateAutomation,
};
use fosnie_backend::chat::origin::{ChatOrigin, TurnContext};
use fosnie_backend::scheduler::AutomationTarget;
use fosnie_backend::state::AppState;
use fosnie_backend::{cache, db, scheduler};

async fn state() -> Option<AppState> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    Some(AppState::new(pg, redis, Arc::new(BootConfig::default())))
}

fn ctx(user_id: Uuid) -> AuthContext {
    AuthContext {
        user_id: Some(user_id),
        email: None,
        display_name: None,
        role: PlatformRole::User,
        break_glass: false,
        mfa_enroll_only: false,
    }
}

async fn seed_user(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO users (id, display_name, email, role) VALUES ($1, 'Test', $2, 'user')",
        id,
        format!("dev-auto-{id}@example.test"),
    )
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn seed_workspace(pg: &sqlx::PgPool, user: Uuid) -> (Uuid, Uuid) {
    let device = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO devices (id, user_id, name, platform) VALUES ($1, $2, 'Box', 'windows')",
        device,
        user,
    )
    .execute(pg)
    .await
    .unwrap();
    let ws = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO device_workspaces (id, device_id, user_id, path, label, tier) \
         VALUES ($1, $2, $3, 'C:\\work', 'work', 'rw')",
        ws,
        device,
        user,
    )
    .execute(pg)
    .await
    .unwrap();
    (device, ws)
}

async fn seed_automation(pg: &sqlx::PgPool, owner: Uuid, ws: Option<Uuid>, run_when_back: bool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO automations (id, owner_user_id, name, schedule, prompt, next_run_at, workspace_id, run_when_back) \
         VALUES ($1, $2, 'A', '0 0 9 * * *', 'do it', now(), $3, $4)",
        id, owner, ws, run_when_back,
    )
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn insert_run(pg: &sqlx::PgPool, automation: Uuid, status: &str, reason: Option<&str>, started: OffsetDateTime) {
    sqlx::query(
        "INSERT INTO automation_runs (id, automation_id, status, reason, started_at) \
         VALUES ($1, $2, ($3::text)::automation_run_status, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(automation)
    .bind(status)
    .bind(reason)
    .bind(started)
    .execute(pg)
    .await
    .unwrap();
}

async fn seed_chat(pg: &sqlx::PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query!("INSERT INTO chats (id, owner_user_id) VALUES ($1, $2)", id, owner)
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn bind_chat(pg: &sqlx::PgPool, chat: Uuid, ws: Uuid) {
    sqlx::query!(
        "INSERT INTO chat_workspace (chat_id, workspace_id) VALUES ($1, $2)",
        chat, ws,
    )
    .execute(pg)
    .await
    .unwrap();
}

async fn run_status(pg: &sqlx::PgPool, run_id: Uuid) -> String {
    sqlx::query_scalar::<_, String>("SELECT status::text FROM agent_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(pg)
        .await
        .unwrap()
}

async fn enqueued_for(pg: &sqlx::PgPool, automation: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM tasks WHERE task_type = 'automation_run' AND payload->>'automation_id' = $1",
    )
    .bind(automation.to_string())
    .fetch_one(pg)
    .await
    .unwrap()
}

// The server-versus-device decision resolves from the folder alone: a null folder
// is always Server (the pre-folder path, untouched), a live folder resolves to its
// machine, and a withdrawn folder is Withdrawn rather than silently server-run.
#[tokio::test]
async fn target_resolves_server_device_and_withdrawn() {
    let Some(st) = state().await else { return };
    assert_eq!(
        scheduler::resolve_automation_target(&st.pg, None).await.unwrap(),
        AutomationTarget::Server
    );

    let user = seed_user(&st.pg).await;
    let (device, ws) = seed_workspace(&st.pg, user).await;
    assert_eq!(
        scheduler::resolve_automation_target(&st.pg, Some(ws)).await.unwrap(),
        AutomationTarget::Device { device_id: device, workspace_id: ws }
    );

    sqlx::query!("UPDATE device_workspaces SET revoked_at = now() WHERE id = $1", ws)
        .execute(&st.pg)
        .await
        .unwrap();
    assert_eq!(
        scheduler::resolve_automation_target(&st.pg, Some(ws)).await.unwrap(),
        AutomationTarget::Withdrawn { workspace_id: ws }
    );
}

// A machine's declared boundary is recorded against the device on connecting,
// defaults to the weaker tier, only rises to the full tier on an explicit claim,
// and reads back into the folder context a turn works from.
#[tokio::test]
async fn sandbox_tier_is_recorded_and_reads_back_into_the_folder_context() {
    let Some(st) = state().await else { return };
    let user = seed_user(&st.pg).await;
    let (device, ws) = seed_workspace(&st.pg, user).await;

    async fn stored(pg: &sqlx::PgPool, device: Uuid) -> String {
        sqlx::query_scalar::<_, String>("SELECT sandbox_tier FROM devices WHERE id = $1")
            .bind(device)
            .fetch_one(pg)
            .await
            .unwrap()
    }

    // A freshly paired device has said nothing yet, so it keeps the weaker tier.
    assert_eq!(stored(&st.pg, device).await, "lifecycle");

    // An explicit full claim is recorded; the presence of other capabilities does
    // not change that.
    desktop::record_sandbox_tier(&st.pg, device, &["folder".into(), "sandbox:full".into()]).await;
    assert_eq!(stored(&st.pg, device).await, "full");

    // A later connection that no longer claims it drops the device back down: a
    // machine is only ever trusted as much as it currently says.
    desktop::record_sandbox_tier(&st.pg, device, &["sandbox:lifecycle".into()]).await;
    assert_eq!(stored(&st.pg, device).await, "lifecycle");

    // The stored tier is what a turn's folder context carries.
    desktop::record_sandbox_tier(&st.pg, device, &["sandbox:full".into()]).await;
    let chat = seed_chat(&st.pg, user).await;
    bind_chat(&st.pg, chat, ws).await;
    let ctx = desktop::load_ctx(&st.pg, chat, Some(device)).await.expect("folder context");
    assert_eq!(ctx.sandbox_tier, "full");
}

// An agreement made with the network covers a command that needs it; a plain one
// does not, and the distinction survives the round trip through the database.
#[tokio::test]
async fn a_network_agreement_covers_a_network_command_but_a_plain_one_does_not() {
    let Some(st) = state().await else { return };
    let user = seed_user(&st.pg).await;
    let (device, ws) = seed_workspace(&st.pg, user).await;
    sqlx::query!(
        "INSERT INTO workspace_command_prefixes (id, workspace_id, prefix, with_network, added_by) \
         VALUES ($1, $2, 'git', true, $3)",
        Uuid::now_v7(),
        ws,
        user,
    )
    .execute(&st.pg)
    .await
    .unwrap();
    sqlx::query!(
        "INSERT INTO workspace_command_prefixes (id, workspace_id, prefix, with_network, added_by) \
         VALUES ($1, $2, 'ls', false, $3)",
        Uuid::now_v7(),
        ws,
        user,
    )
    .execute(&st.pg)
    .await
    .unwrap();

    let chat = seed_chat(&st.pg, user).await;
    bind_chat(&st.pg, chat, ws).await;
    let ctx = desktop::load_ctx(&st.pg, chat, Some(device)).await.expect("folder context");

    // Needing the network, only the network agreement covers the command.
    assert!(ctx.allowed_prefix("git pull", true).is_some(), "network agreement covers a network command");
    assert!(ctx.allowed_prefix("ls -la", true).is_none(), "a plain agreement does not");
    // Not needing it, either agreement covers.
    assert!(ctx.allowed_prefix("git status", false).is_some());
    assert!(ctx.allowed_prefix("ls -la", false).is_some());
}

// A server automation's turn carries the job it came from but no machine and no
// folder, so it takes the identical path a device-less run always did.
#[test]
fn a_server_automation_turn_has_no_device_routing() {
    let auth = AuthContext {
        user_id: Some(Uuid::now_v7()),
        email: None,
        display_name: None,
        role: PlatformRole::User,
        break_glass: false,
        mfa_enroll_only: false,
    };
    let automation = Uuid::now_v7();
    let turn = TurnContext::web(&auth).with_automation(Some(automation));
    assert_eq!(turn.origin, ChatOrigin::Web);
    assert_eq!(turn.device_id, None);
    assert_eq!(turn.workspace_id, None);
    assert_eq!(turn.automation_id, Some(automation));
}

// A machine reconnecting makes up exactly one missed occurrence that fell in the
// window, and the missed row is consumed so a second reconnect makes nothing up.
#[tokio::test]
async fn catchup_claims_one_missed_run_in_window() {
    let Some(st) = state().await else { return };
    let user = seed_user(&st.pg).await;
    let (device, ws) = seed_workspace(&st.pg, user).await;
    let auto = seed_automation(&st.pg, user, Some(ws), true).await;
    insert_run(&st.pg, auto, "missed", Some("offline"), OffsetDateTime::now_utc()).await;

    scheduler::catchup_device(&st, user, device).await;
    assert_eq!(enqueued_for(&st.pg, auto).await, 1, "exactly one make-up run");
    let missed_left: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM automation_runs WHERE automation_id = $1 AND status = 'missed'",
    )
    .bind(auto)
    .fetch_one(&st.pg)
    .await
    .unwrap();
    assert_eq!(missed_left, 0, "the missed row was claimed");

    // A second reconnect finds nothing left to claim.
    scheduler::catchup_device(&st, user, device).await;
    assert_eq!(enqueued_for(&st.pg, auto).await, 1, "no second make-up run");
}

// A miss older than the window, or one whose automation opted out of make-up,
// is left alone.
#[tokio::test]
async fn catchup_ignores_stale_and_opted_out() {
    let Some(st) = state().await else { return };
    let user = seed_user(&st.pg).await;
    let (device, ws) = seed_workspace(&st.pg, user).await;

    let old = seed_automation(&st.pg, user, Some(ws), true).await;
    insert_run(&st.pg, old, "missed", Some("offline"), OffsetDateTime::now_utc() - time::Duration::days(2)).await;
    let opted_out = seed_automation(&st.pg, user, Some(ws), false).await;
    insert_run(&st.pg, opted_out, "missed", Some("offline"), OffsetDateTime::now_utc()).await;

    scheduler::catchup_device(&st, user, device).await;
    assert_eq!(enqueued_for(&st.pg, old).await, 0, "stale miss is not made up");
    assert_eq!(enqueued_for(&st.pg, opted_out).await, 0, "opted-out is not made up");
}

// An approval left unanswered past its window fails the run and its automation's
// record, and releases the card.
#[tokio::test]
async fn expired_approval_fails_the_run() {
    let Some(st) = state().await else { return };
    let user = seed_user(&st.pg).await;
    let (_device, ws) = seed_workspace(&st.pg, user).await;
    let auto = seed_automation(&st.pg, user, Some(ws), false).await;
    // A run paused two days ago (past the 24h default) and its automation record.
    let run_id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO agent_runs (id, acting_user_id, automation_id, status, pending_tool, updated_at) \
         VALUES ($1, $2, $3, 'awaiting_approval', 'desktop.fs_write', now() - interval '2 days')",
        run_id, user, auto,
    )
    .execute(&st.pg)
    .await
    .unwrap();
    insert_run(&st.pg, auto, "needs_approval", None, OffsetDateTime::now_utc()).await;

    let n = scheduler::scan_expired_approvals(&st).await.unwrap();
    assert!(n >= 1);
    let run_status: String =
        sqlx::query_scalar::<_, String>("SELECT status::text FROM agent_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&st.pg)
            .await
            .unwrap();
    assert_eq!(run_status, "failed");
    let auto_failed: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM automation_runs WHERE automation_id = $1 AND status = 'failed' AND error = 'approval expired'",
    )
    .bind(auto)
    .fetch_one(&st.pg)
    .await
    .unwrap();
    assert_eq!(auto_failed, 1);
}

// A folder call from a server-run automation turn is addressed to the machine's
// live socket through the hub, never down the turn's own throwaway channel, and
// its answer comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_route_reaches_the_machine_over_the_hub_not_the_turn_channel() {
    let Some(st) = state().await else { return };
    let user = Uuid::now_v7();
    let device = Uuid::now_v7();
    // The machine's socket on the hub.
    let (sock_tx, mut sock_rx) = mpsc::channel::<ServerFrame>(8);
    st.hub.register(Uuid::now_v7(), user, Some(device), sock_tx);
    // The scheduled turn's throwaway channel: it must carry nothing.
    let (dead_tx, mut dead_rx) = mpsc::channel::<ServerFrame>(8);

    let d = DesktopToolCtx {
        workspace: Workspace {
            id: Uuid::now_v7(),
            device_id: device,
            path: "C:\\work".into(),
            label: "w".into(),
            tier: Tier::ReadWrite,
        },
        command_prefixes: vec![],
        sandbox_tier: "lifecycle".into(),
        route: DesktopSink::DeviceRoute { user_id: user, device_id: device },
    };
    let auth = ctx(user);
    let st2 = st.clone();
    let call = tokio::spawn(async move {
        desktop::execute(
            &st2,
            &auth,
            Uuid::now_v7(),
            &dead_tx,
            &d,
            Uuid::now_v7(),
            desktop::FS_READ,
            &json!({ "path": "notes.md" }),
        )
        .await
    });

    let frame = tokio::time::timeout(Duration::from_secs(5), sock_rx.recv())
        .await
        .expect("the request reaches the machine")
        .expect("a frame");
    let ServerFrame::DesktopToolCall { call_id, .. } = frame else {
        panic!("expected a tool call to the machine");
    };
    assert!(st.desktop_calls.resolve(call_id, device, DesktopReply { ok: true, result: json!({ "content": "hi" }) }));

    let out = call.await.unwrap().expect("the read succeeds");
    assert_eq!(out, "hi");
    assert!(dead_rx.try_recv().is_err(), "the turn's own channel carried nothing");
}

// An approved resume that finds the machine gone parks the run again rather than
// failing it; when the machine returns and it is approved once more, it runs there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resume_defers_while_the_machine_is_away_then_runs_when_it_returns() {
    let Some(st) = state().await else { return };
    let user = seed_user(&st.pg).await;
    let (device, ws) = seed_workspace(&st.pg, user).await;
    let chat = seed_chat(&st.pg, user).await;
    bind_chat(&st.pg, chat, ws).await;

    let run_id = Uuid::now_v7();
    let turn_id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO agent_runs (id, acting_user_id, chat_id, turn_id, status, pending_tool, pending_args) \
         VALUES ($1, $2, $3, $4, 'approved', 'desktop.fs_write', $5)",
        run_id, user, chat, turn_id, json!({ "path": "out.txt", "new_content": "hi" }),
    )
    .execute(&st.pg)
    .await
    .unwrap();

    // Machine offline: the resume defers and the run returns to awaiting approval.
    fosnie_backend::agent::execute_approved(&st, run_id).await.unwrap();
    assert_eq!(run_status(&st.pg, run_id).await, "awaiting_approval");

    // The owner approves again (REST would CAS it back to approved) and the machine
    // is now connected: the pending write runs on it and the run completes.
    sqlx::query!("UPDATE agent_runs SET status = 'approved' WHERE id = $1", run_id)
        .execute(&st.pg)
        .await
        .unwrap();
    let (sock_tx, mut sock_rx) = mpsc::channel::<ServerFrame>(8);
    st.hub.register(Uuid::now_v7(), user, Some(device), sock_tx);
    // Localise: the machine must read as online and its folder must resolve, or the
    // resume would refuse before it ever sends anything.
    assert!(st.hub.is_device_online(user, device), "the machine is registered online");
    assert!(
        desktop::load_ctx(&st.pg, chat, Some(device)).await.is_some(),
        "the chat's folder resolves for the resume"
    );
    let st2 = st.clone();
    let resume = tokio::spawn(async move { fosnie_backend::agent::execute_approved(&st2, run_id).await });

    let frame = tokio::time::timeout(Duration::from_secs(5), sock_rx.recv())
        .await
        .expect("the write reaches the machine")
        .expect("a frame");
    let ServerFrame::DesktopToolCall { call_id, .. } = frame else {
        panic!("expected the write to reach the machine");
    };
    assert!(st.desktop_calls.resolve(call_id, device, DesktopReply { ok: true, result: json!({}) }));

    resume.await.unwrap().expect("the resume completes");
    assert_eq!(run_status(&st.pg, run_id).await, "completed");
}

// The device-only options mean nothing without a folder, and a folder must be
// one the caller actually holds.
#[tokio::test]
async fn create_validates_device_target() {
    let Some(st) = state().await else { return };
    let user = seed_user(&st.pg).await;
    let (_device, ws) = seed_workspace(&st.pg, user).await;

    // Pre-approved writes with no folder is refused.
    let body = CreateAutomation {
        name: "x".into(),
        schedule: "0 0 9 * * *".into(),
        prompt: "do".into(),
        agent_id: None,
        project_id: None,
        kb_ids: vec![],
        deliver_group_chat_id: None,
        workspace_id: None,
        pre_approved_writes: true,
        run_when_back: false,
    };
    let denied = create_automation(State(st.clone()), AuthUser(ctx(user)), Json(body)).await;
    assert!(denied.is_err(), "flags without a folder are refused");

    // A folder the caller does not own is refused.
    let stranger_ws = {
        let other = seed_user(&st.pg).await;
        seed_workspace(&st.pg, other).await.1
    };
    let body = CreateAutomation {
        name: "x".into(),
        schedule: "0 0 9 * * *".into(),
        prompt: "do".into(),
        agent_id: None,
        project_id: None,
        kb_ids: vec![],
        deliver_group_chat_id: None,
        workspace_id: Some(stranger_ws),
        pre_approved_writes: false,
        run_when_back: false,
    };
    let denied = create_automation(State(st.clone()), AuthUser(ctx(user)), Json(body)).await;
    assert!(denied.is_err(), "a folder you do not own is refused");

    // The owner's own folder, with the option, is accepted.
    let body = CreateAutomation {
        name: "x".into(),
        schedule: "0 0 9 * * *".into(),
        prompt: "do".into(),
        agent_id: None,
        project_id: None,
        kb_ids: vec![],
        deliver_group_chat_id: None,
        workspace_id: Some(ws),
        pre_approved_writes: true,
        run_when_back: true,
    };
    let ok = create_automation(State(st.clone()), AuthUser(ctx(user)), Json(body)).await;
    let id = ok.expect("accepted").0.id;

    // Clearing the folder also clears the two options.
    let upd = UpdateAutomation {
        name: None,
        schedule: None,
        prompt: None,
        status: None,
        project_id: None,
        kb_ids: None,
        deliver_group_chat_id: None,
        workspace_id: Some(None),
        pre_approved_writes: None,
        run_when_back: None,
    };
    let _ = update_automation(State(st.clone()), AuthUser(ctx(user)), Path(id), Json(upd)).await.expect("update");
    let row = sqlx::query!(
        "SELECT workspace_id, pre_approved_writes, run_when_back FROM automations WHERE id = $1",
        id
    )
    .fetch_one(&st.pg)
    .await
    .unwrap();
    assert!(row.workspace_id.is_none());
    assert!(!row.pre_approved_writes);
    assert!(!row.run_when_back);
}
