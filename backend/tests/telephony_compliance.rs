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

//! Throwing away what a line has finished with, and being able to say what it keeps.
//!
//! Deletion is the one behaviour where a test is the only honest form of proof: a sweep that
//! deletes too little leaves a practice in breach of its own policy, and one that deletes too
//! much destroys records somebody is relying on. Both look identical from the outside until
//! the day they matter, so both are driven here against real rows.
//!
//! The three things this file exists to pin down: a period that is set is honoured, a period
//! that is not set changes nothing, and the sweep never reaches past the call into the
//! practice's own records. An appointment somebody is expecting to be kept must survive the
//! deletion of the call it was arranged on.
//!
//! Nothing here touches deployment-wide settings, so it runs alongside the rest of the suite.
//! The sweep itself, though, is deployment-wide by nature: it walks every line there is. So the
//! tests that run one take a lock and read back their own rows rather than the totals, because a
//! total is a fact about the whole database and these tests each own only part of it.
//!
//! Needs a reachable Postgres and Redis; skips when `DATABASE_URL` is unset.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::auth::keycloak::AuthUser;
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::http::telephony_compliance::{self, WhoseRecord};
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::telephony::retention;
use fosnie_backend::{cache, db};

/// One sweep at a time, and no reading of totals.
///
/// A sweep is deployment-wide, so two of them at once would each be deleting rows the other
/// is about to make an assertion about.
static SWEEP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Cast {
    state: AppState,
    pg: PgPool,
    owner: Uuid,
    agent: Uuid,
    line: Uuid,
}

fn ctx_for(user: Uuid, role: PlatformRole) -> AuthContext {
    AuthContext {
        user_id: Some(user),
        email: None,
        display_name: None,
        role,
        break_glass: false,
        mfa_enroll_only: false,
    }
}

async fn cast() -> Option<Cast> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:16379".into());
    let pg = db::connect(&db_url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    });
    let redis = cache::create_pool(&redis_url).expect("redis pool");
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.features.telephony = true;
    boot.message_encryption_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".into();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot)).build();

    let owner = mk_user(&pg).await;
    let agent = mk_agent(&pg, owner).await;
    let line = mk_line(&pg, owner, agent).await;
    Some(Cast { state, pg, owner, agent, line })
}

async fn clear_up(c: &Cast) {
    for sql in [
        "DELETE FROM appointments WHERE owner_user_id = $1",
        "DELETE FROM diaries WHERE owner_user_id = $1",
        "DELETE FROM conflict_names WHERE owner_user_id = $1",
        "DELETE FROM enquiries WHERE owner_user_id = $1",
        "DELETE FROM calls WHERE owner_user_id = $1",
        "DELETE FROM phone_numbers WHERE owner_user_id = $1",
        "DELETE FROM messages WHERE chat_id IN (SELECT id FROM chats WHERE owner_user_id = $1)",
        "DELETE FROM chats WHERE owner_user_id = $1",
        "DELETE FROM agents WHERE created_by = $1",
        "DELETE FROM users WHERE id = $1",
    ] {
        let _ = sqlx::query(sql).bind(c.owner).execute(&c.pg).await;
    }
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

async fn mk_agent(pg: &PgPool, owner: Uuid) -> Uuid {
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

/// How long this line keeps things. Both nought by default, which is what a line does
/// until an operator decides otherwise.
async fn set_periods(c: &Cast, transcript_days: i32, log_days: i32) {
    sqlx::query("UPDATE phone_numbers SET transcript_days = $2, log_days = $3 WHERE id = $1")
        .bind(c.line)
        .bind(transcript_days)
        .bind(log_days)
        .execute(&c.pg)
        .await
        .unwrap();
}

/// A conversation, as a call produces one.
async fn mk_chat(c: &Cast) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO chats (id, owner_user_id, agent_id, title, origin) \
         VALUES ($1, $2, $3, 'A call', 'phone')",
    )
    .bind(id)
    .bind(c.owner)
    .bind(c.agent)
    .execute(&c.pg)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO messages (id, chat_id, role, content, sequence_number)          VALUES ($1, $2, 'user', 'Hello there', 1)",
    )
    .bind(Uuid::now_v7())
    .bind(id)
    .execute(&c.pg)
    .await
    .unwrap();
    id
}

/// A call that ended `days_ago`, with a conversation attached.
async fn mk_aged_call(c: &Cast, days_ago: i32) -> (Uuid, Uuid) {
    let chat = mk_chat(c).await;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO calls (id, phone_number_id, provider, provider_call_id, to_e164, from_e164, \
                            owner_user_id, agent_id, chat_id, outcome, started_at, ended_at, \
                            notice_at, notice_text) \
         VALUES ($1, $2, 'twilio', $3, '+441315550000', '+447700900123', $4, $5, $6, 'completed', \
                 now() - make_interval(days => $7), now() - make_interval(days => $7), \
                 now() - make_interval(days => $7), 'You are speaking to an automated assistant.')",
    )
    .bind(id)
    .bind(c.line)
    .bind(format!("CA{}", id.simple()))
    .bind(c.owner)
    .bind(c.agent)
    .bind(chat)
    .bind(days_ago)
    .execute(&c.pg)
    .await
    .unwrap();
    (id, chat)
}

async fn call_row(c: &Cast, call: Uuid) -> Option<(Option<Uuid>, bool)> {
    sqlx::query_as::<_, (Option<Uuid>, bool)>(
        "SELECT chat_id, transcript_deleted_at IS NOT NULL FROM calls WHERE id = $1",
    )
    .bind(call)
    .fetch_optional(&c.pg)
    .await
    .unwrap()
}

async fn chat_exists(c: &Cast, chat: Uuid) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM chats WHERE id = $1)")
        .bind(chat)
        .fetch_one(&c.pg)
        .await
        .unwrap()
}

/// A line that keeps its conversations for ninety days loses the older ones and nothing
/// else: the call is still in the log, with a note that its words have gone.
#[tokio::test]
async fn a_line_that_keeps_words_for_a_time_stops_keeping_the_older_ones() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = run_transcript_sweep(&c).await;
    clear_up(&c).await;
    outcome.expect("the sweep was wrong");
}

async fn run_transcript_sweep(c: &Cast) -> Result<(), String> {
    let _one_at_a_time = SWEEP.lock().await;
    set_periods(c, 90, 0).await;
    let (old, old_chat) = mk_aged_call(c, 120).await;
    let (recent, recent_chat) = mk_aged_call(c, 10).await;

    let swept = retention::sweep(&c.state).await.map_err(|e| e.to_string())?;
    if swept.refused != 0 {
        return Err(format!("the sweep could not finish what it started: {swept:?}"));
    }

    // The old one: words gone, call kept, and the log says which it is.
    match call_row(c, old).await {
        Some((None, true)) => {}
        other => return Err(format!("the aged call reads as {other:?}")),
    }
    if chat_exists(c, old_chat).await {
        return Err("the aged conversation is still there".into());
    }
    // The recent one: untouched.
    match call_row(c, recent).await {
        Some((Some(chat), false)) if chat == recent_chat => {}
        other => return Err(format!("a call inside the period was disturbed: {other:?}")),
    }
    if !chat_exists(c, recent_chat).await {
        return Err("a conversation inside the period was deleted".into());
    }

    // Running again changes nothing: the sweep has to be safe to run every night, and a
    // second pass must not start taking the calls it has already tidied.
    retention::sweep(&c.state).await.map_err(|e| e.to_string())?;
    match call_row(c, old).await {
        Some((None, true)) => {}
        other => return Err(format!("a second sweep changed the tidied call: {other:?}")),
    }
    match call_row(c, recent).await {
        Some((Some(_), false)) => {}
        other => return Err(format!("a second sweep reached a call inside the period: {other:?}")),
    }
    Ok(())
}

/// The record of the call goes too, once its own period is past, and what the practice
/// wrote down because of that call does not.
#[tokio::test]
async fn deleting_the_record_of_a_call_leaves_the_practice_its_own_records() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = run_log_sweep(&c).await;
    clear_up(&c).await;
    outcome.expect("the sweep was wrong");
}

async fn run_log_sweep(c: &Cast) -> Result<(), String> {
    let _one_at_a_time = SWEEP.lock().await;
    set_periods(c, 90, 365).await;
    let (ancient, ancient_chat) = mk_aged_call(c, 400).await;

    // What the caller left behind on that call: a message to pass on, and an appointment.
    // Both point at the call, and both are the practice's own record of somebody asking
    // for something rather than a by-product of the call.
    let enquiry = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO enquiries (id, kind, owner_user_id, call_id, subject, body, caller_name) \
         VALUES ($1, 'message', $2, $3, 'Please ring back', 'About the survey', 'Alex Fraser')",
    )
    .bind(enquiry)
    .bind(c.owner)
    .bind(ancient)
    .execute(&c.pg)
    .await
    .map_err(|e| e.to_string())?;
    let appointment = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO appointments \
           (id, owner_user_id, starts_at, ends_at, reference, caller_name, subject, call_id) \
         VALUES ($1, $2, now() + interval '7 days', now() + interval '7 days 30 minutes', \
                 $3, 'Alex Fraser', 'First meeting', $4)",
    )
    .bind(appointment)
    .bind(c.owner)
    .bind(format!("R{}", &appointment.simple().to_string()[..6]))
    .bind(ancient)
    .execute(&c.pg)
    .await
    .map_err(|e| e.to_string())?;

    let swept = retention::sweep(&c.state).await.map_err(|e| e.to_string())?;
    if swept.refused != 0 {
        return Err(format!("the sweep could not finish what it started: {swept:?}"));
    }
    if call_row(c, ancient).await.is_some() {
        return Err("the call outlived its own period".into());
    }
    if chat_exists(c, ancient_chat).await {
        return Err("the conversation outlived the call it belonged to".into());
    }

    // The two things that must have survived, with their reference to the call let go
    // rather than taking them with it.
    let left = sqlx::query_as::<_, (Option<Uuid>,)>("SELECT call_id FROM enquiries WHERE id = $1")
        .bind(enquiry)
        .fetch_optional(&c.pg)
        .await
        .map_err(|e| e.to_string())?;
    match left {
        Some((None,)) => {}
        other => return Err(format!("the message a caller left reads as {other:?}")),
    }
    let booked =
        sqlx::query_as::<_, (String, Option<Uuid>)>("SELECT status, call_id FROM appointments WHERE id = $1")
            .bind(appointment)
            .fetch_optional(&c.pg)
            .await
            .map_err(|e| e.to_string())?;
    match booked {
        Some((status, None)) if status == "booked" => {}
        other => return Err(format!("the appointment somebody is expecting reads as {other:?}")),
    }
    Ok(())
}

/// A line nobody has given a period keeps everything, which is what every line did before
/// any of this existed.
#[tokio::test]
async fn a_line_with_no_period_deletes_nothing() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = run_no_period(&c).await;
    clear_up(&c).await;
    outcome.expect("something was deleted that nobody asked to have deleted");
}

async fn run_no_period(c: &Cast) -> Result<(), String> {
    let _one_at_a_time = SWEEP.lock().await;
    set_periods(c, 0, 0).await;
    // Older than any period anybody could set, so only the absence of one can save it.
    let (ancient, chat) = mk_aged_call(c, 4_000).await;
    retention::sweep(&c.state).await.map_err(|e| e.to_string())?;
    match call_row(c, ancient).await {
        Some((Some(_), false)) => {}
        other => return Err(format!("a call on a line with no period reads as {other:?}")),
    }
    if !chat_exists(c, chat).await {
        return Err("a conversation on a line with no period was deleted".into());
    }
    Ok(())
}

/// Somebody asking for their words to be removed does not wait for the nightly sweep, and
/// a stranger asking about the same call is told there is no such call.
#[tokio::test]
async fn a_conversation_can_be_thrown_away_on_request_by_the_practice_alone() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = run_by_hand(&c).await;
    let stranger = sqlx::query_scalar::<_, Option<Uuid>>("SELECT id FROM users WHERE display_name = 'Stranger' AND email LIKE $1")
        .bind(format!("%{}%", c.owner))
        .fetch_optional(&c.pg)
        .await
        .ok()
        .flatten()
        .flatten();
    if let Some(id) = stranger {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(id).execute(&c.pg).await;
    }
    clear_up(&c).await;
    outcome.expect("deleting a conversation by hand was wrong");
}

async fn run_by_hand(c: &Cast) -> Result<(), String> {
    set_periods(c, 0, 0).await;
    let (call, chat) = mk_aged_call(c, 1).await;

    // Somebody else's request about somebody else's call: refused, and refused as a call
    // that does not exist rather than as one they may not touch.
    let stranger = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'Stranger', $2, 'user')")
        .bind(stranger)
        .bind(format!("stranger-{}-{}@example.test", c.owner, stranger))
        .execute(&c.pg)
        .await
        .map_err(|e| e.to_string())?;
    let refused = telephony_compliance::delete_transcript(
        State(c.state.clone()),
        AuthUser(ctx_for(stranger, PlatformRole::User)),
        Path(call),
    )
    .await;
    match refused {
        Err(fosnie_backend::error::AppError::NotFound(_)) => {}
        other => return Err(format!("a stranger was allowed to delete a conversation: {:?}", other.is_ok())),
    }
    if !chat_exists(c, chat).await {
        return Err("a stranger's request deleted the conversation anyway".into());
    }
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(stranger).execute(&c.pg).await;

    // The account's own request: done at once.
    let _ = telephony_compliance::delete_transcript(
        State(c.state.clone()),
        AuthUser(ctx_for(c.owner, PlatformRole::User)),
        Path(call),
    )
    .await
    .map_err(|e| format!("the account could not delete its own call's conversation: {e}"))?;
    if chat_exists(c, chat).await {
        return Err("the conversation is still there".into());
    }
    match call_row(c, call).await {
        Some((None, true)) => {}
        other => return Err(format!("the call reads as {other:?} after its words were removed")),
    }

    // Asking again is not an error: being told it has already gone is the answer that was
    // wanted.
    let _ = telephony_compliance::delete_transcript(
        State(c.state.clone()),
        AuthUser(ctx_for(c.owner, PlatformRole::User)),
        Path(call),
    )
    .await
    .map_err(|e| format!("asking twice failed: {e}"))?;
    Ok(())
}

/// The record an assessment is written from: the practice's own, nobody else's, and true of
/// the settings as they stand rather than as somebody wrote them down once.
#[tokio::test]
async fn the_record_states_what_this_line_says_and_keeps() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = run_record(&c).await;
    clear_up(&c).await;
    outcome.expect("the record was wrong");
}

async fn run_record(c: &Cast) -> Result<(), String> {
    set_periods(c, 90, 365).await;
    let (_call, _chat) = mk_aged_call(c, 2).await;

    let record = telephony_compliance::record(
        State(c.state.clone()),
        AuthUser(ctx_for(c.owner, PlatformRole::User)),
        Query(WhoseRecord { owner_user_id: None }),
    )
    .await
    .map_err(|e| format!("the account could not read its own record: {e}"))?
    .0;

    let line = record
        .lines
        .iter()
        .find(|l| l.id == c.line)
        .ok_or("the record does not mention the line")?;
    // The words a caller hears, not the words somebody typed: this line has no notice of
    // its own, so it is the standard one, said in full.
    if !line.spoken_to_callers.contains("automated assistant") || !line.notice_is_standard {
        return Err(format!("the record does not say what a caller hears: {line:?}", line = &line.spoken_to_callers));
    }
    if line.transcript_days != 90 || line.log_days != 365 {
        return Err(format!("the periods read as {} and {}", line.transcript_days, line.log_days));
    }
    if line.calls != 1 || line.calls_with_notice != 1 {
        return Err(format!(
            "the record counts {} calls and {} told what they were speaking to",
            line.calls, line.calls_with_notice
        ));
    }
    if !record.no_audio_is_kept {
        return Err("the record does not state that no audio is kept".into());
    }
    // The periods in force, in words, because nought is not a length of time.
    let words = record
        .holdings
        .iter()
        .find(|h| h.held == "Conversations")
        .ok_or("the record says nothing about conversations")?;
    if !words.kept.contains("90 days") {
        return Err(format!("conversations are said to be kept: {}", words.kept));
    }

    // Somebody else's practice: not found, rather than an empty record, which would itself
    // say whether that account answers a telephone.
    let stranger = mk_user(&c.pg).await;
    let refused = telephony_compliance::record(
        State(c.state.clone()),
        AuthUser(ctx_for(stranger, PlatformRole::User)),
        Query(WhoseRecord { owner_user_id: Some(c.owner) }),
    )
    .await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(stranger).execute(&c.pg).await;
    match refused {
        Err(fosnie_backend::error::AppError::NotFound(_)) => {}
        other => return Err(format!("a stranger read the record: {}", other.is_ok())),
    }
    Ok(())
}
