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

//! When a run's approval gate is decided, every one of the user's open clients
//! hears about it. This drives the real decision handlers against a real database
//! and a socket registered in the hub, and watches the resolution come out — the
//! wiring is proved, not taken on trust. Skips when DATABASE_URL is unset.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use serde_json::json;
use tokio::sync::mpsc;
use uuid::Uuid;

use fosnie_backend::auth::keycloak::AuthUser;
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
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

async fn seed_user(st: &AppState) -> Uuid {
    let uid = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'approver', $2, 'user')")
        .bind(uid)
        .bind(format!("{uid}@example.test"))
        .execute(&st.pg)
        .await
        .expect("seed a user");
    uid
}

fn ctx(uid: Uuid) -> AuthContext {
    AuthContext {
        user_id: Some(uid),
        email: None,
        display_name: None,
        role: PlatformRole::User,
        break_glass: false,
        mfa_enroll_only: false,
    }
}

/// Register a stand-in socket for a user and hand back its receiver — exactly what
/// the writer task holds behind a real connection.
fn socket(st: &AppState, uid: Uuid) -> mpsc::Receiver<ServerFrame> {
    let (tx, rx) = mpsc::channel::<ServerFrame>(8);
    st.hub.register(Uuid::now_v7(), uid, tx);
    rx
}

/// A run belonging to `uid`, paused on an approval gate — the state a person is
/// looking at when they click approve or reject.
async fn awaiting_run(st: &AppState, uid: Uuid) -> Uuid {
    let run_id = fosnie_backend::agent::start_run(
        st, None, Some(uid), PlatformRole::User.as_str(), Some(Uuid::now_v7()), Uuid::now_v7(), None, None, 600,
    )
    .await
    .expect("start a run");
    fosnie_backend::agent::request_approval(st, run_id, Some(uid), PlatformRole::User.as_str(), "web_search", &json!({}), 0)
        .await
        .expect("pause it on approval");
    run_id
}

/// The next resolution for `run_id` off a socket, or None if none arrives quickly.
async fn next_resolved(rx: &mut mpsc::Receiver<ServerFrame>, run_id: Uuid) -> Option<bool> {
    let deadline = Duration::from_secs(2);
    while let Ok(Some(frame)) = tokio::time::timeout(deadline, rx.recv()).await {
        if let ServerFrame::AgentApprovalResolved { run_id: r, approved } = frame {
            if r == run_id {
                return Some(approved);
            }
        }
    }
    None
}

async fn run_status(st: &AppState, run_id: Uuid) -> String {
    sqlx::query_scalar::<_, String>("SELECT status::text FROM agent_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(&st.pg)
        .await
        .expect("read the run status")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approving_settles_the_card_on_the_user_s_clients() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let uid = seed_user(&st).await;
    let mut rx = socket(&st, uid);
    let run_id = awaiting_run(&st, uid).await;

    let _ = fosnie_backend::http::agent_runs::approve_run(State(st.clone()), AuthUser(ctx(uid)), Path(run_id))
        .await
        .expect("approve succeeds");

    assert_eq!(next_resolved(&mut rx, run_id).await, Some(true), "an approval reaches the socket");
    assert_eq!(run_status(&st, run_id).await, "approved");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejecting_settles_the_card_and_closes_the_run() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let uid = seed_user(&st).await;
    let mut rx = socket(&st, uid);
    let run_id = awaiting_run(&st, uid).await;

    let _ = fosnie_backend::http::agent_runs::reject_run(State(st.clone()), AuthUser(ctx(uid)), Path(run_id))
        .await
        .expect("reject succeeds");

    assert_eq!(next_resolved(&mut rx, run_id).await, Some(false), "a rejection reaches the socket");
    assert_eq!(run_status(&st, run_id).await, "rejected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_run_settles_any_open_card() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let uid = seed_user(&st).await;
    let mut rx = socket(&st, uid);
    let run_id = awaiting_run(&st, uid).await;

    let _ = fosnie_backend::http::agent_runs::cancel_run(State(st.clone()), AuthUser(ctx(uid)), Path(run_id))
        .await
        .expect("cancel succeeds");

    assert_eq!(next_resolved(&mut rx, run_id).await, Some(false), "a cancel closes the card");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_decision_is_a_conflict_and_sends_nothing() {
    // The resolution is emitted only after the compare-and-set that actually moves
    // the run: a decision that lost the race (already decided elsewhere) must not
    // send a second frame. This is what stops a resolved card from flickering.
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let uid = seed_user(&st).await;
    let mut rx = socket(&st, uid);
    let run_id = awaiting_run(&st, uid).await;

    // First decision wins and emits.
    let _ = fosnie_backend::http::agent_runs::approve_run(State(st.clone()), AuthUser(ctx(uid)), Path(run_id))
        .await
        .expect("first approve wins");
    assert_eq!(next_resolved(&mut rx, run_id).await, Some(true));

    // Second decision loses the CAS → 409, and emits nothing.
    let again = fosnie_backend::http::agent_runs::reject_run(State(st.clone()), AuthUser(ctx(uid)), Path(run_id)).await;
    assert!(again.is_err(), "a second decision is a conflict");
    assert!(next_resolved(&mut rx, run_id).await.is_none(), "the losing decision sent no frame");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_owner_s_clients_are_told() {
    // The resolution goes to the run's owner, not to everyone connected.
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let owner = seed_user(&st).await;
    let stranger = seed_user(&st).await;
    let mut owner_rx = socket(&st, owner);
    let mut stranger_rx = socket(&st, stranger);
    let run_id = awaiting_run(&st, owner).await;

    let _ = fosnie_backend::http::agent_runs::approve_run(State(st.clone()), AuthUser(ctx(owner)), Path(run_id))
        .await
        .expect("owner approves");

    assert_eq!(next_resolved(&mut owner_rx, run_id).await, Some(true), "the owner hears it");
    assert!(next_resolved(&mut stranger_rx, run_id).await.is_none(), "a stranger never does");
}
