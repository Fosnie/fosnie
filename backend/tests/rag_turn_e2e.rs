//! Live wiring proof for the native tool authorisation seam: a real retrieval turn
//! must not break. Drives `chat::run_turn` in-process against an existing knowledge
//! base, with a real model, and asserts the turn completes with a grounded answer
//! and that `search_library` (injected per turn, never in `agent_tools`) is never
//! refused as an unauthorised grant. If the model calls the top-up tool, that call
//! passing is the end-to-end proof; if it does not, the turn still proves the seam
//! did not break retrieval.
//!
//! Gated on `PAI_E2E=1`; needs Postgres + Qdrant + the ML service up and a live LLM
//! reachable via `OPENAI_TEST_KEY` (an OpenAI key; model `gpt-5.4`). The key is read
//! from the environment and never written to disk.

use std::sync::Arc;
use std::time::Duration;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::runtime::{self as cfg, ConfigValueType};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{cache, chat, db};
use tokio::sync::mpsc;
use tokio::sync::Notify;
use uuid::Uuid;

// base64(32 bytes of 0x07) — a fixed test-only DEK so the provider row we insert
// below decrypts under this AppState's keyring. NOT a deployment key.
const TEST_KEY_B64: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";
const TEST_KEY_BYTES: [u8; 32] = [7u8; 32];

// An existing knowledge base in the dev database that holds the synthetic
// `atlantis.txt` fixture, and its owner (a client-admin who can read it). Synthetic
// facts force the model to retrieve rather than answer from training.
const KB_ID: &str = "019e7d35-1a32-7d70-aa78-6a25d1a6bded";
const OWNER_ID: &str = "85c9882a-3828-4b68-bc3b-06a341f40f7e";

fn enabled() -> bool {
    std::env::var("PAI_E2E").as_deref() == Ok("1")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_rag_turn_authorises_search_library() {
    if !enabled() {
        eprintln!("skip: set PAI_E2E=1 with the stack up");
        return;
    }
    let Ok(api_key) = std::env::var("OPENAI_TEST_KEY") else {
        eprintln!("skip: OPENAI_TEST_KEY unset");
        return;
    };
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let ml_url =
        std::env::var("PAI__ML__BASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());

    let pg = db::connect(&db_url, 5).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig {
        database_url: db_url,
        redis_url,
        message_encryption_key: TEST_KEY_B64.to_string(),
        ..BootConfig::default()
    };
    boot.ml.base_url = ml_url;
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));

    let owner = Uuid::parse_str(OWNER_ID).unwrap();
    let kb_id = Uuid::parse_str(KB_ID).unwrap();

    // A user-scoped default llm row for THIS owner only (wins over any deployment
    // default via resolve_llm's ordering, touches no other user). gpt-5.4 needs
    // reasoning_mode='none' to emit tool calls at all.
    let enc_key = fosnie_backend::crypto::encrypt(&TEST_KEY_BYTES, &api_key).unwrap();
    let provider_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO provider_configs (id, role, scope, scope_id, base_url, model, \
         api_key_encrypted, enabled, reasoning_mode, is_default) \
         VALUES ($1,'llm','user',$2,'https://api.openai.com/v1','gpt-5.4',$3,true,'none',true)",
    )
    .bind(provider_id)
    .bind(owner)
    .bind(&enc_key)
    .execute(&pg)
    .await
    .unwrap();

    // Advertise the top-up tool regardless of first-pass gaps.
    let prev_mode = set_cfg(&pg, "rag.model_search_mode", "always").await;
    let prev_locus = set_cfg(&pg, "rag.model_search_locus", "loop").await;

    let chat_id = Uuid::now_v7();
    sqlx::query("INSERT INTO chats (id, owner_user_id, title) VALUES ($1,$2,'authz rag e2e')")
        .bind(chat_id)
        .bind(owner)
        .execute(&pg)
        .await
        .unwrap();

    let ctx = AuthContext {
        user_id: Some(owner),
        email: None,
        display_name: None,
        role: PlatformRole::ClientAdmin,
        break_glass: false,
        mfa_enroll_only: false,
    };

    let question = "Using ONLY the library, summarise what is known about Atlantis: its \
         location, who governs it, and any notable events. Search the library again if \
         the first pass leaves any of these unanswered.";

    // Leg 1: the pre-answer tool loop (locus = loop), the run_one_call dispatch site.
    let denied_before = count_sl_grant_denials(&pg).await;
    let r1 = drive_turn(&state, &ctx, chat_id, kb_id, question).await;
    let denied_after_1 = count_sl_grant_denials(&pg).await;
    eprintln!(
        "leg 1 (loop):     completed={} error={:?} search_library_called={} answer_len={} sl_grant_denials(+{})",
        r1.completed, r1.errored, r1.search_library_called, r1.answer_len, denied_after_1 - denied_before
    );

    // Leg 2: the mid-stream locus, where search_library is withheld from the loop and
    // handed to the streaming answer (a separate dispatch site). Needs unified synthesis.
    let prev_locus2 = set_cfg(&pg, "rag.model_search_locus", "midstream").await;
    let prev_synth = set_cfg(&pg, "rag.synthesis_mode", "unified").await;
    let chat2 = Uuid::now_v7();
    sqlx::query("INSERT INTO chats (id, owner_user_id, title) VALUES ($1,$2,'authz rag e2e midstream')")
        .bind(chat2)
        .bind(owner)
        .execute(&pg)
        .await
        .unwrap();
    let r2 = drive_turn(&state, &ctx, chat2, kb_id, question).await;
    let denied_after_2 = count_sl_grant_denials(&pg).await;
    eprintln!(
        "leg 2 (midstream): completed={} error={:?} search_library_called={} answer_len={} sl_grant_denials(+{})",
        r2.completed, r2.errored, r2.search_library_called, r2.answer_len, denied_after_2 - denied_after_1
    );

    // Restore global config and remove the provider row.
    restore_cfg(&pg, "rag.synthesis_mode", prev_synth).await;
    restore_cfg(&pg, "rag.model_search_locus", prev_locus2).await;
    restore_cfg(&pg, "rag.model_search_locus", prev_locus).await;
    restore_cfg(&pg, "rag.model_search_mode", prev_mode).await;
    let _ = sqlx::query("DELETE FROM provider_configs WHERE id = $1")
        .bind(provider_id)
        .execute(&pg)
        .await;

    // Both legs: the turn completed with a grounded answer and search_library was
    // never refused as an unauthorised grant. A missing offered-set entry would have
    // produced exactly that denial when the model called the advertised tool.
    for (leg, r, before, after) in [
        ("loop", &r1, denied_before, denied_after_1),
        ("midstream", &r2, denied_after_1, denied_after_2),
    ] {
        assert!(r.errored.is_none(), "{leg}: turn errored: {:?}", r.errored);
        assert!(r.completed, "{leg}: turn did not complete within the deadline");
        assert!(r.answer_len > 0, "{leg}: turn produced no answer");
        assert_eq!(before, after, "{leg}: search_library must never be refused reason=grant");
    }
}

struct TurnResult {
    completed: bool,
    errored: Option<String>,
    search_library_called: bool,
    answer_len: usize,
}

/// Drive one turn in-process and collect the outcome from the frame stream.
async fn drive_turn(
    state: &AppState,
    ctx: &AuthContext,
    chat_id: Uuid,
    kb_id: Uuid,
    question: &str,
) -> TurnResult {
    let (tx, mut rx) = mpsc::channel::<ServerFrame>(256);
    let cancel = Arc::new(Notify::new());
    let turn_id = Uuid::now_v7();
    let st = state.clone();
    let cx = ctx.clone();
    let q = question.to_string();
    let handle = tokio::spawn(async move {
        chat::run_turn(
            &st, &cx, turn_id, Some(chat_id), None, None, q, Vec::new(), vec![kb_id], false,
            None, None, None, None, &tx, cancel,
        )
        .await;
    });

    let mut answer_len = 0usize;
    let mut completed = false;
    let mut errored: Option<String> = None;
    let mut search_library_called = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(ServerFrame::ChatToken { delta, .. })) => answer_len += delta.len(),
            Ok(Some(ServerFrame::ChatTool { name, .. })) => {
                if name == "search_library" {
                    search_library_called = true;
                }
            }
            Ok(Some(ServerFrame::ChatCompleted { .. })) => {
                completed = true;
                break;
            }
            Ok(Some(ServerFrame::ChatError { message, .. })) => {
                errored = Some(message);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    let _ = handle.await;
    TurnResult { completed, errored, search_library_called, answer_len }
}

/// Count `tool.denied` audit rows whose reason is a grant miss for `search_library`.
async fn count_sl_grant_denials(pg: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_events \
         WHERE action_type = 'tool.denied' \
           AND payload->>'denied' = 'grant' \
           AND payload->>'tool' = 'search_library'",
    )
    .fetch_one(pg)
    .await
    .unwrap()
}

/// Set a `config_settings` key via the app's own validated setter, returning the
/// previous value (if any) for restore.
async fn set_cfg(pg: &sqlx::PgPool, key: &str, value: &str) -> Option<String> {
    let prev = cfg::get(pg, key).await.unwrap().map(|e| e.value);
    cfg::set(pg, key, value, ConfigValueType::String, "deployment", None, "system")
        .await
        .unwrap();
    prev
}

async fn restore_cfg(pg: &sqlx::PgPool, key: &str, prev: Option<String>) {
    match prev {
        Some(v) => {
            let _ = cfg::set(pg, key, &v, ConfigValueType::String, "deployment", None, "system").await;
        }
        None => {
            let _ = sqlx::query("DELETE FROM config_settings WHERE key = $1")
                .bind(key)
                .execute(pg)
                .await;
        }
    }
}
