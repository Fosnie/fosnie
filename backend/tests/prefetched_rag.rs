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

//! A turn handed a retrieval performed before it started must use that retrieval and
//! not repeat it — and a turn handed nothing must behave exactly as it always has.
//!
//! Live voice searches the knowledge base from the partial transcript while the
//! speaker is still talking, then hands the result to the turn. "The turn did not
//! search again" is not visible in anything the turn emits, so it is asserted here
//! against a stand-in ML service that counts its calls.
//!
//! Needs a reachable Postgres; skips when `DATABASE_URL` is unset. Never needs the
//! real ML service, Qdrant or an LLM.

mod common;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Notify};
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::chat::prefetch::PrefetchedRag;
use fosnie_backend::config::BootConfig;
use fosnie_backend::ml;
use fosnie_backend::state::AppState;
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{cache, chat, db};

use common::mock_ml::{self, MlScript};

const PREFETCH_CONTEXT: &str = "[D1] Contractors accrue holiday pro rata.";
const PREFETCH_QUOTE: &str = "accrue holiday pro rata";
const QUESTION: &str = "what is the holiday allowance for contractors";

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

async fn harness(ml_base_url: &str) -> Option<(sqlx::PgPool, AppState)> {
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
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));
    Some((pg, state))
}

async fn mk_user(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'T', $2, 'user')")
        .bind(id)
        .bind(format!("{id}@example.test"))
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn mk_chat(pg: &sqlx::PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO chats (id, project_id, owner_user_id, title) VALUES ($1, NULL, $2, 'C')")
        .bind(id)
        .bind(owner)
        .execute(pg)
        .await
        .unwrap();
    id
}

/// A ready KB the user can read, attached to their chat — so the turn's allow-list
/// is non-empty and it takes the retrieval path at all.
async fn mk_attached_kb(pg: &sqlx::PgPool, owner: Uuid, chat: Uuid) -> Uuid {
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

async fn cleanup(pg: &sqlx::PgPool, user: Uuid, chat: Uuid, kb: Uuid) {
    sqlx::query("DELETE FROM messages WHERE chat_id = $1").bind(chat).execute(pg).await.ok();
    sqlx::query("DELETE FROM chat_kb_links WHERE chat_id = $1").bind(chat).execute(pg).await.ok();
    sqlx::query("DELETE FROM chats WHERE id = $1").bind(chat).execute(pg).await.ok();
    sqlx::query("DELETE FROM kb_access_grants WHERE kb_id = $1").bind(kb).execute(pg).await.ok();
    sqlx::query("DELETE FROM knowledge_bases WHERE id = $1").bind(kb).execute(pg).await.ok();
    sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(pg).await.ok();
}

struct TurnResult {
    citations: Vec<String>,
    tools: Vec<String>,
    errored: Option<String>,
}

/// Drive one turn in-process and collect what reached the client.
async fn drive_turn(
    state: &AppState,
    user: Uuid,
    chat_id: Uuid,
    question: &str,
    prefetched: Option<PrefetchedRag>,
) -> TurnResult {
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(256);
    let cancel = Arc::new(Notify::new());
    let st = state.clone();
    let cx = ctx(user);
    let q = question.to_string();
    tokio::spawn(async move {
        chat::run_turn(
            &st, &cx, Uuid::now_v7(), Some(chat_id), None, None, q, Vec::new(), Vec::new(), false,
            None, None, None, prefetched, &tx, cancel,
        )
        .await;
    });

    let mut out = TurnResult { citations: Vec::new(), tools: Vec::new(), errored: None };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ServerFrame::ChatCitations { citations, .. })) => {
                out.citations = citations.iter().map(|c| c.quote_text.clone()).collect();
            }
            Ok(Some(ServerFrame::ChatTool { name, .. })) => {
                if !out.tools.contains(&name) {
                    out.tools.push(name);
                }
            }
            Ok(Some(ServerFrame::ChatError { message, .. })) => {
                out.errored = Some(message);
                break;
            }
            Ok(Some(ServerFrame::ChatCompleted { .. })) => break,
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    out
}

fn prefetched() -> PrefetchedRag {
    PrefetchedRag {
        context: PREFETCH_CONTEXT.into(),
        citations: vec![ml::Citation {
            doc_id: None,
            chunk_index: None,
            page_number: None,
            clause_section_ref: None,
            quote_text: PREFETCH_QUOTE.into(),
        }],
        parts: Vec::new(),
        debug: ml::RetrieveDebug::default(),
        source_query: "what is the holiday allowance for contractors".into(),
    }
}

/// The whole point of the feature: a turn given a retrieval does not perform one.
/// The evidence that reaches the client is the prefetched evidence, not the ML
/// service's — which is what proves the injected result is the one actually used,
/// rather than being quietly overwritten by a second search.
#[tokio::test]
async fn a_prefetched_turn_does_not_retrieve_again() {
    let ml = mock_ml::spawn(MlScript::default()).await;
    let Some((pg, st)) = harness(&ml.base_url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&pg).await;
    let chat = mk_chat(&pg, user).await;
    let kb = mk_attached_kb(&pg, user, chat).await;

    let r = drive_turn(&st, user, chat, "what is the holiday allowance for contractors", Some(prefetched())).await;

    assert_eq!(ml.calls.retrieves(), 0, "a turn handed a retrieval must not perform one");
    assert!(r.errored.is_none(), "turn errored: {:?}", r.errored);
    assert_eq!(r.citations, vec![PREFETCH_QUOTE.to_string()], "the prefetched evidence is what reaches the client");
    assert!(r.tools.contains(&"retrieve".to_string()), "the turn still reports that the library was searched");

    cleanup(&pg, user, chat, kb).await;
}

/// The other half of the invariant, and the one that protects every existing user:
/// with nothing handed over, the turn retrieves exactly as it always did.
#[tokio::test]
async fn a_turn_without_a_prefetch_retrieves_as_before() {
    let ml = mock_ml::spawn(MlScript::default()).await;
    let Some((pg, st)) = harness(&ml.base_url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&pg).await;
    let chat = mk_chat(&pg, user).await;
    let kb = mk_attached_kb(&pg, user, chat).await;

    let r = drive_turn(&st, user, chat, "what is the holiday allowance for contractors", None).await;

    assert_eq!(ml.calls.retrieves(), 1, "the turn performs its own retrieval");
    assert!(r.errored.is_none(), "turn errored: {:?}", r.errored);
    assert_eq!(
        r.citations,
        vec!["retrieved by the turn itself".to_string()],
        "the evidence is the one the turn retrieved"
    );

    cleanup(&pg, user, chat, kb).await;
}

/// Speculation runs a light search on a sentence that was still being spoken, so it
/// may not have covered the whole question. The safety net is the model's own
/// library-search tool: a prefetched turn is always offered it, even though the
/// retrieval reported no gaps, so the model can fill in whatever was missed.
#[tokio::test]
async fn a_prefetched_turn_is_always_offered_the_top_up_tool() {
    // The model asks to search the library. A tool the turn never offered is refused
    // at dispatch and its implementation is never reached — so the search actually
    // happening is the proof that the tool was in this turn's offered set.
    let script = MlScript {
        generate_tool_call: Some((
            "search_library".into(),
            serde_json::json!({ "query": "notice period for contractors" }),
        )),
        ..MlScript::default()
    };
    let ml = mock_ml::spawn(script).await;
    let Some((pg, st)) = harness(&ml.base_url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&pg).await;
    let chat = mk_chat(&pg, user).await;
    let kb = mk_attached_kb(&pg, user, chat).await;

    // The prefetched result reports no gaps at all. Under the default 'gaps_only'
    // policy that would normally withhold the tool, so if it turns out to be
    // available, it is the prefetch that made it so.
    let p = prefetched();
    assert_eq!(p.debug.gap_stop_reason, "", "the fixture must report no gaps for this test to mean anything");
    let r = drive_turn(&st, user, chat, "what is the holiday allowance for contractors", Some(p)).await;

    assert!(r.errored.is_none(), "turn errored: {:?}", r.errored);
    assert!(
        ml.calls.was_offered("search_library"),
        "a prefetched turn must offer the library-search tool as its safety net; offered: {:?}",
        ml.calls.offered()
    );

    cleanup(&pg, user, chat, kb).await;
}

/// The injection invariant, stated exactly: a turn HANDED a retrieval must compose
/// the same prompt as a turn that PERFORMED that same retrieval itself.
///
/// The behavioural tests above show the prefetched evidence reaching the client, but
/// the prompt is what the model actually reads, and it is assembled through budget
/// allocation, trimming and a seven-layer compose. A divergence anywhere in that
/// chain would be invisible in the frames and would change every prefetched answer.
/// So the two prompts are compared whole, byte for byte.
#[tokio::test]
async fn a_prefetched_turn_composes_the_same_prompt_as_one_that_retrieved() {
    // The mock returns exactly what the prefetch carries, so the only difference
    // between the two runs is which route the identical evidence arrived by.
    let script = MlScript {
        retrieve_context: PREFETCH_CONTEXT.into(),
        retrieve_quote: PREFETCH_QUOTE.into(),
        ..MlScript::default()
    };
    let ml = mock_ml::spawn(script).await;
    let Some((pg, st)) = harness(&ml.base_url).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };

    // Diagnostics off: the Coverage line is the one divergence the prefetched path is
    // allowed itself, and it must not be in play or the comparison would not be
    // strict. Whatever the deployment had is put back at the end.
    use fosnie_backend::config::runtime::{self as cfg, ConfigValueType};
    let previous = cfg::get(&pg, "rag.show_diagnostics").await.ok().flatten().map(|e| e.value);
    cfg::set(&pg, "rag.show_diagnostics", "false", ConfigValueType::Bool, "global", None, "test")
        .await
        .expect("diagnostics off");

    let user = mk_user(&pg).await;
    // Two fresh chats: the same question in the same chat twice would put the first
    // exchange into the second turn's history and diverge for an unrelated reason.
    let chat_a = mk_chat(&pg, user).await;
    let kb_a = mk_attached_kb(&pg, user, chat_a).await;
    let chat_b = mk_chat(&pg, user).await;
    let kb_b = mk_attached_kb(&pg, user, chat_b).await;

    let r1 = drive_turn(&st, user, chat_a, QUESTION, Some(prefetched())).await;
    let r2 = drive_turn(&st, user, chat_b, QUESTION, None).await;

    assert!(r1.errored.is_none(), "prefetched turn errored: {:?}", r1.errored);
    assert!(r2.errored.is_none(), "retrieving turn errored: {:?}", r2.errored);

    // A turn generates more than once (naming a new chat is a generation too), so the
    // answer prompts are picked out by the evidence fence rather than by position.
    let prompts = ml.calls.system_prompts_containing("<retrieved-context>");
    assert_eq!(prompts.len(), 2, "one answer prompt per turn, each carrying its evidence");
    assert!(
        prompts[0].contains("Contractors accrue holiday"),
        "the fixture must actually reach the prompt, or this compares two empty strings"
    );
    assert_eq!(
        prompts[0], prompts[1],
        "a handed-over retrieval must compose byte-identically to one the turn performed"
    );

    cleanup(&pg, user, chat_a, kb_a).await;
    cleanup(&pg, user, chat_b, kb_b).await;
    match previous {
        Some(v) => {
            cfg::set(&pg, "rag.show_diagnostics", &v, ConfigValueType::Bool, "global", None, "test").await.ok();
        }
        None => {
            cfg::unset(&pg, "rag.show_diagnostics", "test").await.ok();
        }
    }
}
