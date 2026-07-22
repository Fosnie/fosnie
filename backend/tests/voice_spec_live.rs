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

//! Speculative search driven through a real live-voice session against a stand-in
//! ML service: what it actually sends, and what happens to it when the speaker
//! interrupts.
//!
//! Both properties here are invisible from the outside. That a search reads the same
//! scope the turn would is a question about the arguments it sent, and that
//! interrupting stops it is a question about whether the upstream request went away
//! rather than merely being ignored. So the mock records the one and observes the
//! other.
//!
//! Needs a reachable Postgres; skips when `DATABASE_URL` is unset.

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::PgPool;
use tokio::sync::{mpsc, Notify};
use uuid::Uuid;

use fosnie_backend::auth::rbac::{Permission, ResourceType};
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::Result;
use fosnie_backend::ext::RbacPolicy;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::voice::Session;
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{cache, chat, db};

use common::mock_ml::{self, MlScript};

const QUESTION: &str = "what is the holiday allowance for contractors";

/// Denies a fixed set of documents, whatever is asked.
///
/// The open edition denies nothing, so without this both paths would agree on an
/// empty list and the comparison would prove nothing. This is the seam an edition
/// with document-level entitlements plugs into, standing in for one here.
struct DenyFixed(Vec<Uuid>);

#[async_trait]
impl RbacPolicy for DenyFixed {
    async fn can(
        &self,
        _pool: &PgPool,
        _ctx: &AuthContext,
        _rt: ResourceType,
        _id: Uuid,
        _perm: Permission,
    ) -> Result<bool> {
        Ok(true)
    }
    async fn may_grant(&self, _pool: &PgPool, _g: &AuthContext, _rt: ResourceType, _id: Uuid) -> Result<bool> {
        Ok(true)
    }
    async fn denied_kb_doc_ids(&self, _pool: &PgPool, _ctx: &AuthContext, _kbs: &[Uuid]) -> Result<Vec<Uuid>> {
        Ok(self.0.clone())
    }
}

async fn harness(ml_base_url: &str, rbac: Option<Arc<dyn RbacPolicy>>) -> Option<(PgPool, AppState)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url = std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    // Skipping is for "no database configured". A database that IS configured but
    // cannot be reached is an environment fault, and quietly reporting a pass for it
    // is how an untested change looks tested.
    let pg = db::connect(&db_url, 5)
        .await
        .unwrap_or_else(|e| panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}"));
    let redis = cache::create_pool(&redis_url).expect("redis pool");
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.ml.base_url = ml_base_url.to_string();
    let mut b = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot));
    if let Some(p) = rbac {
        b = b.with_rbac(p);
    }
    Some((pg, b.build()))
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

async fn mk_user(pg: &PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'T', $2, 'user')")
        .bind(id)
        .bind(format!("{id}@example.test"))
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn mk_chat(pg: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO chats (id, project_id, owner_user_id, title) VALUES ($1, NULL, $2, 'C')")
        .bind(id)
        .bind(owner)
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn mk_attached_kb(pg: &PgPool, owner: Uuid, chat: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO knowledge_bases \
           (id, name, owner_id, visibility, embedding_model_id, embedding_dimension, status) \
         VALUES ($1, 'KB', $2, 'shared', 'test-model', 1024, 'ready')",
    )
    .bind(id)
    .bind(owner)
    .execute(pg)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO kb_access_grants (id, kb_id, principal_type, principal_id, permission, granted_by) \
         VALUES ($1, $2, 'user', $3, 'read'::kb_permission, $3)",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(owner)
    .execute(pg)
    .await
    .unwrap();
    sqlx::query("INSERT INTO chat_kb_links (chat_id, kb_id) VALUES ($1, $2)")
        .bind(chat)
        .bind(id)
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn cleanup(pg: &PgPool, user: Uuid, chat: Uuid, kb: Uuid) {
    sqlx::query("DELETE FROM messages WHERE chat_id = $1").bind(chat).execute(pg).await.ok();
    sqlx::query("DELETE FROM chat_kb_links WHERE chat_id = $1").bind(chat).execute(pg).await.ok();
    sqlx::query("DELETE FROM chats WHERE id = $1").bind(chat).execute(pg).await.ok();
    sqlx::query("DELETE FROM kb_access_grants WHERE kb_id = $1").bind(kb).execute(pg).await.ok();
    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1").bind(kb).execute(pg).await.ok();
    sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(pg).await.ok();
}

/// A live-voice session on this chat, with a socket that goes nowhere.
async fn session_for(st: &AppState, user: Uuid, chat: Uuid) -> Arc<Session> {
    let (tx, rx) = mpsc::channel::<ServerFrame>(256);
    // Keep the receiver alive for the session's lifetime, or its sends start failing.
    std::mem::forget(rx);
    Session::start(st.clone(), ctx(user), Uuid::now_v7(), tx, Some(chat), None, None, Some("ptt".into()), true).await
}

/// The scope a speculative search reads must be exactly the scope the committed turn
/// reads: the same Libraries, and the same documents held back within them. Speaking
/// is not an authorisation event, so speculation must not be able to see one document
/// more than the turn it is speculating for.
#[tokio::test]
async fn speculation_searches_exactly_what_the_turn_would() {
    let denied = vec![Uuid::now_v7(), Uuid::now_v7()];
    let ml = mock_ml::spawn(MlScript::default()).await;
    let Some((pg, st)) = harness(&ml.base_url, Some(Arc::new(DenyFixed(denied.clone())))).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&pg).await;
    let chat = mk_chat(&pg, user).await;
    let kb = mk_attached_kb(&pg, user, chat).await;

    // One speculative search, fired as the speaker would have triggered it.
    let session = session_for(&st, user, chat).await;
    session.spec_fire(QUESTION.to_string(), true).await;
    for _ in 0..100 {
        if !ml.calls.retrieve_args().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    session.shutdown().await;

    // Then the ordinary turn, retrieving for itself.
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(256);
    let cancel = Arc::new(Notify::new());
    let st2 = st.clone();
    let cx = ctx(user);
    tokio::spawn(async move {
        chat::run_turn(
            &st2, chat::origin::TurnContext::web(&cx), Uuid::now_v7(), Some(chat), None, None, QUESTION.into(), Vec::new(), Vec::new(),
            false, None, None, None, None, &tx, cancel,
        )
        .await;
    });
    while let Ok(Some(f)) = tokio::time::timeout(Duration::from_secs(60), rx.recv()).await {
        if matches!(f, ServerFrame::ChatCompleted { .. } | ServerFrame::ChatError { .. }) {
            break;
        }
    }

    let calls = ml.calls.retrieve_args();
    assert_eq!(calls.len(), 2, "one speculative search and one from the turn: {calls:?}");
    let (spec, turn) = (&calls[0], &calls[1]);

    assert_eq!(spec.kb_ids, turn.kb_ids, "speculation must search the same Libraries as the turn");
    assert!(!spec.kb_ids.is_empty(), "the fixture must actually put a Library in scope");
    assert_eq!(
        spec.deny_doc_ids, turn.deny_doc_ids,
        "and hold back the same documents within them"
    );
    let mut expected: Vec<String> = denied.iter().map(|d| d.to_string()).collect();
    expected.sort();
    assert_eq!(spec.deny_doc_ids, expected, "the withheld documents actually reached the search");

    cleanup(&pg, user, chat, kb).await;
}

/// Interrupting mid-answer must STOP a search the turn is waiting on, not just stop
/// waiting for it. A search left running holds its upstream request open, burns the
/// budget of a turn that no longer exists, and can surface its result afterwards.
#[tokio::test]
async fn interrupting_stops_a_search_the_turn_is_waiting_on() {
    // The search hangs until released, so it is reliably still running when the
    // speaker interrupts — no racing a sleep against a search that may be done.
    let latch = Arc::new(Notify::new());
    let script = MlScript { retrieve_latch: Some(latch.clone()), ..MlScript::default() };
    let ml = mock_ml::spawn(script).await;
    let Some((pg, st)) = harness(&ml.base_url, None).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&pg).await;
    let chat = mk_chat(&pg, user).await;
    let kb = mk_attached_kb(&pg, user, chat).await;

    let session = session_for(&st, user, chat).await;
    session.spec_fire(QUESTION.to_string(), true).await;
    for _ in 0..100 {
        if ml.calls.retrieves_in_flight() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(ml.calls.retrieves_in_flight(), 1, "the search must be running before we interrupt it");

    // The speaker finishes with the same words, so the turn decides the search in
    // flight is the right one and waits for it...
    session.start_turn(QUESTION.to_string()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // ...and then interrupts.
    session.barge_in().await;

    let mut stopped = false;
    for _ in 0..100 {
        if ml.calls.retrieves_in_flight() == 0 {
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        stopped,
        "the interrupted search must be stopped: its upstream request is still open, so it was \
         detached rather than aborted"
    );

    latch.notify_waiters(); // nothing should be listening
    session.shutdown().await;
    cleanup(&pg, user, chat, kb).await;
}
