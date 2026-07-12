//! #13 — the `track_steps` tool forwards the model's checklist to the UI as a
//! `chat.steps` frame (stateless; full list each call). Drives `tools::dispatch`
//! directly. Skips when DATABASE_URL is unset (AppState needs the pools).

use std::sync::Arc;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{cache, db, tools};
use tokio::sync::mpsc;
use uuid::Uuid;

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
        break_glass: false, mfa_enroll_only: false,
    }
}

#[tokio::test]
async fn track_steps_emits_checklist_frame() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(8);
    let turn = Uuid::now_v7();
    let args = serde_json::json!({ "steps": [
        { "title": "Read the contract", "status": "done" },
        { "title": "Extract clauses", "status": "running" },
        { "title": "Summarise", "status": "pending" },
        { "title": "   ", "status": "pending" }
    ]});
    let out = tools::dispatch(&st, &ctx(), None, Uuid::now_v7(), turn, &tx, None, &[], &std::collections::HashMap::new(), "track_steps", &args)
        .await
        .unwrap();
    assert!(out.contains("3 step"), "blank-title step dropped → 3 recorded: {out}");

    match rx.try_recv().expect("a chat.steps frame") {
        ServerFrame::ChatSteps { turn_id, steps } => {
            assert_eq!(turn_id, turn);
            assert_eq!(steps.len(), 3);
            assert_eq!(steps[0].status, "done");
            assert_eq!(steps[1].status, "running");
            assert_eq!(steps[2].status, "pending");
        }
        other => panic!("expected ChatSteps, got {other:?}"),
    }
}

#[tokio::test]
async fn track_steps_rejects_empty() {
    let Some(st) = state().await else {
        return;
    };
    let (tx, _rx) = mpsc::channel::<ServerFrame>(8);
    let r = tools::dispatch(
        &st, &ctx(), None, Uuid::now_v7(), Uuid::now_v7(), &tx, None, &[], &std::collections::HashMap::new(), "track_steps",
        &serde_json::json!({ "steps": [] }),
    )
    .await;
    assert!(r.is_err(), "empty steps → error");
}
