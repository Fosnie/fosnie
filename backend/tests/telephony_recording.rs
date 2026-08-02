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

//! Keeping the sound of a call, and everything that follows from keeping it.
//!
//! The recorder's own arithmetic is settled in its unit tests. What can only be shown here
//! is the part that spans the whole feature, and it is the part that matters most: that a
//! line which records **says so to every caller**, that a line which does not records
//! nothing at all, and that the audio is reachable, deletable and swept exactly like the
//! words are.
//!
//! The one thing this file will not let pass is a recording that nobody was told about.
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
use fosnie_backend::telephony::{notice, retention};
use fosnie_backend::voice::telephony::record;
use fosnie_backend::{cache, db};

/// One sweep at a time: a sweep walks every line there is, so two at once would each be
/// deleting what the other is about to assert on.
static SWEEP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Cast {
    state: AppState,
    pg: PgPool,
    dir: tempfile::TempDir,
    owner: Uuid,
    agent: Uuid,
    line: Uuid,
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

async fn cast() -> Option<Cast> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:16379".into());
    let pg = db::connect(&db_url, 5).await.unwrap_or_else(|e| {
        panic!("DATABASE_URL is set but unreachable, so nothing here was tested: {e}")
    });
    let redis = cache::create_pool(&redis_url).expect("redis pool");
    // A directory of this run's own, so a test never reads or removes another's audio.
    let dir = tempfile::tempdir().expect("somewhere to keep recordings");
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.features.telephony = true;
    boot.message_encryption_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".into();
    boot.storage.recordings_dir = dir.path().to_string_lossy().to_string();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot)).build();

    let owner = mk_user(&pg).await;
    let agent = mk_agent(&pg, owner).await;
    let line = mk_line(&pg, owner, agent).await;
    Some(Cast { state, pg, dir, owner, agent, line })
}

async fn clear_up(c: &Cast) {
    for sql in [
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
    .bind(format!("+44131888{:05}", Uuid::now_v7().as_u128() % 100_000))
    .bind(owner)
    .bind(agent)
    .execute(pg)
    .await
    .unwrap();
    id
}

/// Set this line to record, and for how long the audio is kept.
async fn set_recording(c: &Cast, on: bool, days: i32) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE phone_numbers SET record_calls = $2, recording_days = $3 WHERE id = $1")
        .bind(c.line)
        .bind(on)
        .bind(days)
        .execute(&c.pg)
        .await
        .map(|_| ())
}

/// A finished call with a recording of its own on the disk.
async fn mk_recorded_call(c: &Cast, days_ago: i32) -> Uuid {
    let id = Uuid::now_v7();
    // A real recording, written by the real recorder, so what the tests read back is what a
    // call would have left behind.
    let (rec, task) = record::start(&c.state.boot.storage.recordings_dir, id)
        .await
        .expect("a recording starts");
    let caller: Vec<i16> = (0..1_600).map(|n| ((n % 16) as i16 - 8) * 900).collect();
    let line: Vec<i16> = (0..1_600).map(|n| ((n % 64) as i16 - 32) * 300).collect();
    rec.caller(&caller);
    rec.line(&line);
    drop(rec);
    let done = task.await.expect("the recording finishes");
    assert!(!done.failed, "the fixture could not write a recording: {done:?}");

    sqlx::query(
        "INSERT INTO calls (id, phone_number_id, provider, provider_call_id, to_e164, from_e164, \
                            owner_user_id, agent_id, outcome, started_at, ended_at, \
                            recording_path, recording_bytes, recording_seconds) \
         VALUES ($1, $2, 'twilio', $3, '+441315550000', '+447700900123', $4, $5, 'completed', \
                 now() - make_interval(days => $6), now() - make_interval(days => $6), \
                 $7, $8, $9)",
    )
    .bind(id)
    .bind(c.line)
    .bind(format!("CA{}", id.simple()))
    .bind(c.owner)
    .bind(c.agent)
    .bind(days_ago)
    .bind(&done.path)
    .bind(done.bytes as i64)
    .bind(done.seconds as i32)
    .execute(&c.pg)
    .await
    .unwrap();
    id
}

fn on_disk(c: &Cast, call: Uuid) -> bool {
    c.dir.path().join(record::relative_path(call)).exists()
}

/// The rule this whole feature turns on: a line that records tells every caller, and a line
/// that does not never claims it does. Asserted here as well as in the wording's own tests,
/// because this is where the line's setting and the spoken words meet.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_the_caller_is_told_follows_what_the_line_does() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    set_recording(&c, false, 0).await.expect("a line that does not record");
    let quiet = line_opening(&c).await;

    set_recording(&c, true, 30).await.expect("a line that records");
    let recording = line_opening(&c).await;
    clear_up(&c).await;

    assert!(!quiet.to_lowercase().contains("record"), "a line that keeps no audio said it does: {quiet}");
    assert!(
        recording.contains(notice::RECORDED_SENTENCE),
        "a line that keeps audio did not say so: {recording}"
    );
}

/// What this line would say, composed the way the answer composes it.
async fn line_opening(c: &Cast) -> String {
    let row = sqlx::query_as::<_, (Option<String>, Option<String>, bool)>(
        "SELECT greeting, notice, record_calls FROM phone_numbers WHERE id = $1",
    )
    .bind(c.line)
    .fetch_one(&c.pg)
    .await
    .unwrap();
    notice::opening(row.0.as_deref(), row.1.as_deref(), row.2)
}

/// A line cannot be set to record without saying how long the audio is kept: in the column,
/// so that no path anywhere can produce a recording nobody will ever delete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recording_cannot_be_switched_on_without_a_period() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let refused = set_recording(&c, true, 0).await;
    let allowed = set_recording(&c, true, 14).await;
    // And it cannot be left recording while the period is taken away, either.
    let taken_away = set_recording(&c, true, 0).await;
    clear_up(&c).await;

    assert!(refused.is_err(), "a line was set to record with no period");
    assert!(allowed.is_ok(), "a line with a period was refused");
    assert!(taken_away.is_err(), "the period was removed from a line that records");
}

/// The audio is reachable by the account whose line took the call, is what was written, and
/// is not reachable by anybody else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_can_be_listened_to_by_the_practice_and_nobody_else() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    set_recording(&c, true, 30).await.unwrap();
    let call = mk_recorded_call(&c, 0).await;
    let stranger = mk_user(&c.pg).await;

    let refused = telephony_compliance::play_recording(
        State(c.state.clone()),
        AuthUser(ctx_for(stranger)),
        Path(call),
    )
    .await;
    let played = telephony_compliance::play_recording(
        State(c.state.clone()),
        AuthUser(ctx_for(c.owner)),
        Path(call),
    )
    .await;
    let listened = audited(&c, "telephony.recording.played").await;
    let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(stranger).execute(&c.pg).await;
    clear_up(&c).await;

    assert!(refused.is_err(), "a stranger listened to a recording");
    let response = played.expect("the account could not listen to its own call");
    assert_eq!(response.status(), 200);
    let kind = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(kind, "audio/wav", "what came back is not something a player would open");
    assert!(listened > 0, "listening to a recording was not recorded");
}

/// Deleting what was said takes the sound with it. Somebody asking for their words to be
/// removed does not mean the text only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_what_was_said_takes_the_sound_too() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    set_recording(&c, true, 30).await.unwrap();
    let call = mk_recorded_call(&c, 0).await;
    assert!(on_disk(&c, call), "the fixture wrote no recording");

    let _ = telephony_compliance::delete_transcript(
        State(c.state.clone()),
        AuthUser(ctx_for(c.owner)),
        Path(call),
    )
    .await
    .expect("the account could not delete its own call's conversation");
    let left = on_disk(&c, call);
    let row = recording_row(&c, call).await;
    clear_up(&c).await;

    assert!(!left, "the sound of the call was left on the disk");
    assert_eq!(row, None, "the call still claims to have a recording");
}

/// And the sound can be taken on its own, keeping the words: a practice being careful
/// rather than inconsistent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sound_can_be_removed_on_its_own() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    set_recording(&c, true, 30).await.unwrap();
    let call = mk_recorded_call(&c, 0).await;

    let _ = telephony_compliance::delete_recording(
        State(c.state.clone()),
        AuthUser(ctx_for(c.owner)),
        Path(call),
    )
    .await
    .expect("the account could not delete its own recording");
    let left = on_disk(&c, call);
    let row = recording_row(&c, call).await;
    let still_a_call = sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM calls WHERE id = $1)")
        .bind(call)
        .fetch_one(&c.pg)
        .await
        .unwrap();
    clear_up(&c).await;

    assert!(!left, "the file is still there");
    assert_eq!(row, None);
    assert!(still_a_call, "removing the sound removed the call as well");
}

/// The sweep honours the line's own period: past it the file goes, inside it nothing moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sweep_takes_an_old_recording_and_leaves_a_recent_one() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let _one_at_a_time = SWEEP.lock().await;
    set_recording(&c, true, 30).await.unwrap();
    let old = mk_recorded_call(&c, 60).await;
    let recent = mk_recorded_call(&c, 2).await;

    retention::sweep(&c.state).await.expect("the sweep runs");
    let old_left = on_disk(&c, old);
    let recent_left = on_disk(&c, recent);
    let old_row = recording_row(&c, old).await;
    let recent_row = recording_row(&c, recent).await;
    clear_up(&c).await;

    assert!(!old_left, "a recording past its period was kept");
    assert_eq!(old_row, None, "the call still points at a deleted recording");
    assert!(recent_left, "a recording inside its period was deleted");
    assert!(recent_row.is_some(), "a recording inside its period was cleared from its call");
}

/// A recording whose call has gone is a voice recording nothing in the product can see,
/// retain or delete. Erasing an account is the ordinary way that happens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recording_whose_call_has_gone_is_swept_up() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let _one_at_a_time = SWEEP.lock().await;
    set_recording(&c, true, 30).await.unwrap();
    let call = mk_recorded_call(&c, 1).await;
    // The row goes the way erasure takes it: in the database, leaving the file behind.
    sqlx::query("DELETE FROM calls WHERE id = $1").bind(call).execute(&c.pg).await.unwrap();
    assert!(on_disk(&c, call), "the fixture is not set up");

    let swept = retention::sweep(&c.state).await.expect("the sweep runs");
    let left = on_disk(&c, call);
    clear_up(&c).await;

    assert!(!left, "a recording with no call was left on the disk");
    assert!(swept.orphans >= 1, "the sweep did not say it had taken one: {swept:?}");
}

/// The record an assessment is written from stops saying no audio is kept the moment some
/// is. This is the single line in that record most likely to be read and believed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_compliance_record_says_when_audio_is_kept() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    set_recording(&c, false, 0).await.unwrap();
    let quiet = read_record(&c).await;

    set_recording(&c, true, 30).await.unwrap();
    let _call = mk_recorded_call(&c, 0).await;
    let recording = read_record(&c).await;
    clear_up(&c).await;

    assert!(quiet.no_audio_is_kept, "a deployment that records nothing said otherwise");
    assert!(!recording.no_audio_is_kept, "a deployment that records said no audio is kept");
    let holding = recording
        .holdings
        .iter()
        .find(|h| h.held == "Recordings")
        .expect("the record says nothing about recordings");
    assert!(holding.kept.contains("30 days"), "the period is not stated: {}", holding.kept);
    assert_eq!(holding.rows, 1, "the recording was not counted");
    // And the line itself says it records, with the words its callers hear.
    let line = recording.lines.first().expect("no line in the record");
    assert!(line.records_calls);
    assert!(line.spoken_to_callers.contains("recorded"), "{}", line.spoken_to_callers);
}

async fn read_record(c: &Cast) -> telephony_compliance::ComplianceRecord {
    telephony_compliance::record(
        State(c.state.clone()),
        AuthUser(ctx_for(c.owner)),
        Query(WhoseRecord { owner_user_id: None }),
    )
    .await
    .expect("the account could not read its own record")
    .0
}

async fn recording_row(c: &Cast, call: Uuid) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT recording_path FROM calls WHERE id = $1")
        .bind(call)
        .fetch_optional(&c.pg)
        .await
        .unwrap()
        .flatten()
}

async fn audited(c: &Cast, action: &str) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_events WHERE action_type = $1")
        .bind(action)
        .fetch_one(&c.pg)
        .await
        .unwrap_or(0)
}
