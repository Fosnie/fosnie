//! Agent quality/trajectory eval harness.
//!
//! Runs a task set against the seeded General/Legal agents, k trials each, and
//! scores every trial by (a) **tool-call correctness** (expected tools ⊆ the tools
//! actually invoked, read from the hash-chain audit) and (b) an **LLM-as-judge**
//! over the answer vs a rubric. Reports per-task **pass^k** (all k trials pass) +
//! pass rate, prints a table, and writes a markdown summary. The client's real
//! ~20 tasks extend `fixtures/agent_eval_tasks.json` — data only.
//!
//! Heavy: needs the full stack (Postgres + Redis + ML + served LLM). Gated on
//! `PAI_E2E=1`; trials per task via `EVAL_K` (default 2). Run:
//!   PAI_E2E=1 DATABASE_URL=… cargo test --test agent_eval -- --nocapture

use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{cache, db};

// Seeded agents (migration 0035), by sector.
const GENERAL_AGENT: &str = "a9e70000-0000-4000-8000-000000000001";
const LEGAL_AGENT: &str = "a9e70000-0000-4000-8000-000000000002";
const ALICE: &str = "0a1ce000-0000-4000-8000-000000000001";

#[derive(Deserialize)]
struct Task {
    id: String,
    agent: String,
    prompt: String,
    expect_tools: Vec<String>,
    judge_rubric: String,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
}

fn enabled() -> bool {
    std::env::var("PAI_E2E").as_deref() == Ok("1")
}

struct TaskResult {
    id: String,
    k: usize,
    passes: usize,
    tool_passes: usize,
    judge_passes: usize,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_quality_eval() {
    if !enabled() {
        eprintln!("skipping agent_eval (set PAI_E2E=1 with the full stack up)");
        return;
    }
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url = std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let ml_url = std::env::var("PAI__ML__BASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());
    let k: usize = std::env::var("EVAL_K").ok().and_then(|s| s.parse().ok()).unwrap_or(2);

    let pg = db::connect(&db_url, 5).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.ml.base_url = ml_url;
    boot.server.static_dir = "___no_spa___".into();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));

    // The run acts under alice (client_admin) — broad access so the intersection
    // never blocks; the harness measures answer + tool quality, not RBAC.
    let alice = Uuid::parse_str(ALICE).unwrap();
    let ctx = AuthContext {
        user_id: Some(alice),
        email: Some("alice@example.com".into()),
        display_name: Some("Alice".into()),
        role: PlatformRole::ClientAdmin,
        break_glass: false, mfa_enroll_only: false,
    };

    let tasks: Vec<Task> = {
        let path = format!("{}/tests/fixtures/agent_eval_tasks.json", env!("CARGO_MANIFEST_DIR"));
        let raw = std::fs::read_to_string(&path).expect("read task fixture");
        serde_json::from_str(&raw).expect("parse task fixture")
    };

    let mut results: Vec<TaskResult> = Vec::new();
    for task in &tasks {
        let agent_id = Uuid::parse_str(if task.agent == "legal" { LEGAL_AGENT } else { GENERAL_AGENT }).unwrap();
        let mut passes = 0usize;
        let mut tool_passes = 0usize;
        let mut judge_passes = 0usize;
        for trial in 0..k {
            // Fresh project per trial → a clean chat + clean trajectory.
            let project_id = Uuid::now_v7();
            sqlx::query("INSERT INTO projects (id, name, owner_user_id) VALUES ($1, $2, $3)")
                .bind(project_id)
                .bind(format!("eval-{}-{}", task.id, trial))
                .bind(alice)
                .execute(&pg)
                .await
                .unwrap();

            let (output, chat_id, err) = run_once(&state, &ctx, project_id, agent_id, &task.prompt).await;
            if let Some(e) = &err {
                eprintln!("  [{}] trial {trial}: chat error: {e}", task.id);
            }

            // (a) tool-call correctness from the audit trajectory.
            let called = called_tools(&pg, chat_id).await;
            let tool_ok = task.expect_tools.iter().all(|t| called.iter().any(|c| c == t));
            if tool_ok {
                tool_passes += 1;
            }

            // (b) LLM-as-judge over the answer vs the rubric.
            let judge_ok = !output.trim().is_empty() && judge(&state, &output, &task.judge_rubric).await;
            if judge_ok {
                judge_passes += 1;
            }

            if tool_ok && judge_ok {
                passes += 1;
            }
            eprintln!(
                "  [{}] trial {trial}: tools={} judge={} (called: {:?})",
                task.id, tool_ok, judge_ok, called
            );
        }
        results.push(TaskResult { id: task.id.clone(), k, passes, tool_passes, judge_passes });
    }

    // ── Report ──────────────────────────────────────────────────────────────
    let mut md = String::from("# Agent eval report\n\n| task | pass^k | pass rate | tool-correct | judge |\n|---|---|---|---|---|\n");
    println!("\n=== AGENT EVAL (k={k}) ===");
    println!("{:<22} {:>7} {:>10} {:>6} {:>6}", "task", "pass^k", "pass", "tool", "judge");
    let mut any_pass = false;
    let mut control_passrate = 1.0f64;
    for r in &results {
        let passk = if r.passes == r.k { 1 } else { 0 };
        let rate = r.passes as f64 / r.k as f64;
        if r.id != "control-wrong-tool" && r.passes > 0 {
            any_pass = true;
        }
        if r.id == "control-wrong-tool" {
            control_passrate = rate;
        }
        println!("{:<22} {:>7} {:>9.0}% {:>5}/{} {:>5}/{}", r.id, passk, rate * 100.0, r.tool_passes, r.k, r.judge_passes, r.k);
        md.push_str(&format!(
            "| {} | {} | {:.0}% | {}/{} | {}/{} |\n",
            r.id, passk, rate * 100.0, r.tool_passes, r.k, r.judge_passes, r.k
        ));
    }
    let report_path = std::env::temp_dir().join("agent_eval_report.md");
    let _ = std::fs::write(&report_path, &md);
    println!("\nreport written to {}", report_path.display());

    // The harness must DISCRIMINATE: the control task expects a tool the General
    // agent cannot call, so its tool-correctness — and thus pass^k — is always 0.
    assert_eq!(control_passrate, 0.0, "control task must fail (tool-correctness discrimination)");
    // And the pipeline must actually exercise the agents (≥1 real task passed a trial).
    assert!(any_pass, "no real task passed any trial — the eval pipeline is not exercising the agents");
}

/// Drive one unattended turn directly, draining the answer + chat id + any error.
async fn run_once(
    state: &AppState,
    ctx: &AuthContext,
    project_id: Uuid,
    agent_id: Uuid,
    prompt: &str,
) -> (String, Option<Uuid>, Option<String>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ServerFrame>(256);
    let drain = tokio::spawn(async move {
        let (mut out, mut cid, mut err) = (String::new(), None, None);
        while let Some(f) = rx.recv().await {
            match f {
                ServerFrame::ChatToken { delta, .. } => out.push_str(&delta),
                ServerFrame::ChatCreated { chat_id } => cid = Some(chat_id),
                ServerFrame::ChatError { message, .. } => err = Some(message),
                _ => {}
            }
        }
        (out, cid, err)
    });
    let cancel = Arc::new(tokio::sync::Notify::new());
    fosnie_backend::chat::run_turn(
        state, ctx, Uuid::now_v7(), None, Some(project_id), Some(agent_id),
        prompt.to_string(), Vec::new(), Vec::new(), true, None, None, None, None, &tx, cancel,
    )
    .await;
    drop(tx);
    drain.await.unwrap()
}

/// The distinct tools invoked during the chat, from the hash-chain audit (the
/// trajectory — `tool.invoked` events tagged with the chat id).
async fn called_tools(pg: &sqlx::PgPool, chat_id: Option<Uuid>) -> Vec<String> {
    let Some(cid) = chat_id else { return vec![] };
    sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT payload->>'tool' FROM audit_events \
         WHERE action_type = 'tool.invoked' AND resource_id = $1 AND payload->>'tool' IS NOT NULL",
    )
    .bind(cid)
    .fetch_all(pg)
    .await
    .unwrap_or_default()
}

/// LLM-as-judge: PASS iff the answer satisfies the rubric. Conservative (any
/// non-PASS → false), so a flaky judge under-counts rather than over-counts.
async fn judge(state: &AppState, output: &str, rubric: &str) -> bool {
    let answer: String = output.chars().take(4000).collect();
    let sys = "You are a strict grader. Given a RUBRIC and an ANSWER, reply with exactly one word: PASS if the answer satisfies the rubric, otherwise FAIL. Output only PASS or FAIL.";
    let user = format!("RUBRIC:\n{rubric}\n\nANSWER:\n{answer}\n\nReply PASS or FAIL.");
    let req = fosnie_backend::ml::GenerateRequest {
        messages: vec![json!({ "role": "system", "content": sys }), json!({ "role": "user", "content": user })],
        sampling: Default::default(),
        model: None,
        tools: None,
        overrides: Default::default(),
    };
    let Ok(mut stream) = fosnie_backend::ml::generate(&state.http, &state.boot.ml.base_url, &req).await else {
        return false;
    };
    let mut txt = String::new();
    while let Some(ev) = stream.recv().await {
        match ev {
            fosnie_backend::ml::GenEvent::Token { delta } => txt.push_str(&delta),
            fosnie_backend::ml::GenEvent::Reasoning { .. } => {}
            fosnie_backend::ml::GenEvent::ToolCall { .. } => {}
            fosnie_backend::ml::GenEvent::Done { .. } => break,
            fosnie_backend::ml::GenEvent::Error { .. } => break,
        }
    }
    let up = txt.trim().to_uppercase();
    up.starts_with("PASS") || (up.contains("PASS") && !up.contains("FAIL"))
}
