//! The instance's half of working in a folder: what it sends, what it refuses to
//! send, and what happens when the machine at the other end does not answer.
//!
//! There is no desktop client here. A channel stands in for the socket, which is
//! exactly what a socket is at this level — so a case can take the request off
//! it, answer it, and watch the tool call come back with what the answer said.
//! Skips when DATABASE_URL is unset (the state needs the pools).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::tools::desktop::{self, DesktopToolCtx, Tier, Workspace};
use fosnie_backend::tools::{self, DesktopReply};
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{cache, db};

async fn state() -> Option<AppState> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    Some(AppState::new(pg, redis, Arc::new(BootConfig::default())))
}

fn ctx() -> AuthContext {
    AuthContext {
        user_id: Some(Uuid::now_v7()),
        email: None,
        display_name: None,
        role: PlatformRole::User,
        break_glass: false,
        mfa_enroll_only: false,
    }
}

fn folder(tier: Tier, prefixes: &[&str]) -> DesktopToolCtx {
    DesktopToolCtx {
        workspace: Workspace {
            id: Uuid::now_v7(),
            device_id: Uuid::now_v7(),
            path: "C:\\work\\demo".into(),
            label: "demo".into(),
            tier,
        },
        command_prefixes: prefixes.iter().map(|p| p.to_string()).collect(),
    }
}

/// Mint the authorisation witness through the real seam and dispatch, exactly as
/// a turn does. Nothing can reach `dispatch` any other way.
async fn dispatch(
    st: &AppState,
    tx: &mpsc::Sender<ServerFrame>,
    d: &DesktopToolCtx,
    tool: &str,
    args: &Value,
) -> Result<String, fosnie_backend::AppError> {
    let ctx = ctx();
    let chat_id = Uuid::now_v7();
    let custom: HashMap<String, tools::custom::CustomToolRow> = HashMap::new();
    let authorised =
        tools::AuthorisedTools::build(&[tool.to_string()], &[tool.to_string()], false, &custom);
    let overrides = HashMap::new();
    match tools::authorize_native_call(st, &ctx, chat_id, &authorised, &overrides, tool, None).await
    {
        tools::NativeDecision::Allowed(w) => {
            tools::dispatch(
                st,
                &ctx,
                chat_id,
                Uuid::now_v7(),
                tx,
                None,
                None,
                Some(d),
                &[],
                &custom,
                &w,
                args,
            )
            .await
        }
        tools::NativeDecision::Recoverable(m) => Ok(m),
        tools::NativeDecision::Denied(e) => Err(e),
    }
}

/// Take the request off the stand-in socket and answer it as `device` — the
/// machine the call was sent to.
async fn answer(
    st: &AppState,
    rx: &mut mpsc::Receiver<ServerFrame>,
    device: Uuid,
    ok: bool,
    result: Value,
) -> (String, Value) {
    let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a request reaches the machine")
        .expect("a frame");
    let ServerFrame::DesktopToolCall { call_id, tool, args, .. } = frame else {
        panic!("expected a request to the machine, got something else");
    };
    assert!(st.desktop_calls.turn_of(call_id, device).is_some(), "the turn is waiting on it");
    st.desktop_calls.resolve(call_id, device, DesktopReply { ok, result });
    (tool, args)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_goes_out_resolved_against_the_folder_and_its_answer_comes_back() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(8);
    let d = folder(Tier::ReadWrite, &[]);
    let st2 = st.clone();

    let call = tokio::spawn({
        let d = d.clone();
        async move { dispatch(&st2, &tx, &d, desktop::FS_READ, &json!({ "path": "notes.md" })).await }
    });

    let (tool, args) = answer(&st, &mut rx, d.workspace.device_id, true, json!({ "content": "the file's text" })).await;
    assert_eq!(tool, desktop::FS_READ);
    assert_eq!(args["full_path"], "C:\\work\\demo\\notes.md", "joined onto the folder");
    assert_eq!(args["workspace_path"], "C:\\work\\demo");

    let out = call.await.unwrap().expect("the read succeeds");
    assert_eq!(out, "the file's text");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_comes_back_with_its_output_and_its_exit_code() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(8);
    let d = folder(Tier::ReadWrite, &[]);
    let st2 = st.clone();

    let call = tokio::spawn({
        let d = d.clone();
        async move {
            dispatch(
                &st2,
                &tx,
                &d,
                desktop::TERMINAL_RUN,
                &json!({ "command": "npm test", "timeout_secs": 9999 }),
            )
            .await
        }
    });

    let (_tool, args) = answer(
        &st,
        &mut rx,
        d.workspace.device_id,
        true,
        json!({ "stdout": "2 passing\n", "stderr": "", "exit_code": 0 }),
    )
    .await;
    assert_eq!(args["timeout_secs"], 600, "a caller cannot ask for an unbounded run");

    let out = call.await.unwrap().expect("the command runs");
    assert!(out.contains("exit code 0") && out.contains("2 passing"), "{out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_machine_the_call_was_sent_to_may_answer_it() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    // A call bound to one device. Another device — a second machine of the same
    // user, or someone else's — must not be able to forge its answer even if it
    // learns the call id.
    let call_id = Uuid::now_v7();
    let turn = Uuid::now_v7();
    let target = Uuid::now_v7();
    let stranger = Uuid::now_v7();
    let rx = st.desktop_calls.register(call_id, turn, target);

    // The stranger's reply is refused and the waiter is left pending.
    assert!(!st.desktop_calls.resolve(call_id, stranger, DesktopReply { ok: true, result: json!({ "content": "forged" }) }));
    assert!(st.desktop_calls.turn_of(call_id, stranger).is_none(), "a stranger cannot even read the turn");
    assert_eq!(st.desktop_calls.turn_of(call_id, target), Some(turn));

    // The real device's reply lands.
    assert!(st.desktop_calls.resolve(call_id, target, DesktopReply { ok: true, result: json!({ "content": "real" }) }));
    let got = rx.await.expect("the waiter got the real answer");
    assert_eq!(got.result["content"], "real");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_from_the_machine_is_a_tool_error_not_a_result() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(8);
    let d = folder(Tier::ReadWrite, &[]);
    let st2 = st.clone();

    let call = tokio::spawn({
        let d = d.clone();
        async move { dispatch(&st2, &tx, &d, desktop::FS_READ, &json!({ "path": "gone.md" })).await }
    });
    answer(&st, &mut rx, d.workspace.device_id, false, json!({ "error": "no such path: gone.md" })).await;
    let err = call.await.unwrap().expect_err("a refusal is an error");
    assert!(err.to_string().contains("no such path"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_out_of_the_folder_never_leaves_the_instance() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(8);
    let d = folder(Tier::ReadWrite, &[]);

    for escape in ["../secrets.txt", "..\\secrets.txt", "D:\\other\\file", "/etc/passwd"] {
        let err = dispatch(&st, &tx, &d, desktop::FS_READ, &json!({ "path": escape }))
            .await
            .expect_err("{escape} should be refused");
        assert!(err.to_string().contains("outside the connected folder"), "{escape}: {err}");
    }
    assert!(rx.try_recv().is_err(), "nothing was ever sent to the machine");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_level_of_trust_decides_before_anything_is_sent() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(8);

    let read_only = folder(Tier::ReadOnly, &[]);
    for (tool, args) in [
        (desktop::FS_WRITE, json!({ "path": "a.txt", "new_content": "x" })),
        (desktop::FS_DELETE, json!({ "path": "a.txt" })),
        (desktop::TERMINAL_RUN, json!({ "command": "npm test" })),
    ] {
        let err = dispatch(&st, &tx, &read_only, tool, &args).await.expect_err("refused");
        assert!(err.to_string().contains("read only"), "{tool}: {err}");
    }

    let no_delete = folder(Tier::ReadWriteNoDelete, &[]);
    let err = dispatch(&st, &tx, &no_delete, desktop::FS_DELETE, &json!({ "path": "a.txt" }))
        .await
        .expect_err("refused");
    assert!(err.to_string().contains("does not allow deleting"), "{err}");

    assert!(rx.try_recv().is_err(), "nothing was sent to the machine");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_machine_that_goes_away_ends_the_call_rather_than_the_turn() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let (tx, rx) = mpsc::channel::<ServerFrame>(8);
    let d = folder(Tier::ReadWrite, &[]);
    let st2 = st.clone();

    let call = tokio::spawn({
        let d = d.clone();
        async move { dispatch(&st2, &tx, &d, desktop::FS_READ, &json!({ "path": "notes.md" })).await }
    });
    // The socket closes with the request still outstanding.
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(rx);

    let err = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("the call does not hang on a machine that is gone")
        .unwrap()
        .expect_err("a lost machine is an error");
    assert!(err.to_string().contains("disconnected") || err.to_string().contains("no longer connected"), "{err}");
}

#[tokio::test]
async fn deleting_is_never_covered_by_an_agreed_command() {
    // The allowlist is consulted for commands and nothing else — there is no
    // argument shape that turns a deletion into one.
    let d = folder(Tier::ReadWrite, &["rm", "npm test"]);
    assert_eq!(d.allowed_prefix("npm test"), Some("npm test"));
    assert_eq!(d.allowed_prefix("npm test --watch"), Some("npm test"));
    assert_eq!(d.allowed_prefix("npm test && rm -rf ."), None);
    // Even a prefix that names a deleting command only ever covers a command:
    // `desktop.fs_delete` does not go through this path at all.
    assert!(matches!(
        tools::effect(desktop::FS_DELETE),
        tools::ToolEffect::RequiresRun
    ));
}

#[test]
fn a_tool_the_trust_level_forbids_is_not_offered_at_all() {
    // The refusal in `prepare` is the backstop. The rule that matters to the
    // person is this one: a folder connected read only does not put a write tool
    // in front of the model, so nobody is ever asked to approve a write that was
    // going to be refused anyway.
    let offer = |tier: Tier| -> Vec<&'static str> {
        desktop::ALL
            .iter()
            .copied()
            .filter(|t| desktop::tier_allows(tier, t).is_ok())
            .collect()
    };
    assert_eq!(offer(Tier::ReadOnly), vec![desktop::FS_LIST, desktop::FS_READ]);
    assert_eq!(
        offer(Tier::ReadWriteNoDelete),
        vec![desktop::FS_LIST, desktop::FS_READ, desktop::FS_WRITE, desktop::TERMINAL_RUN]
    );
    assert_eq!(offer(Tier::ReadWrite).len(), desktop::ALL.len());
}

#[test]
fn the_family_is_a_host_capability_that_can_be_switched_off() {
    let mut features = BootConfig::default().features;
    features.desktop_execution = true;
    for tool in desktop::ALL {
        assert!(tools::host_enabled(tool, &features), "{tool} on");
        assert_eq!(tools::capability(tool), Some("desktop_execution"));
    }
    features.desktop_execution = false;
    for tool in desktop::ALL {
        assert!(!tools::host_enabled(tool, &features), "{tool} off");
    }
}
