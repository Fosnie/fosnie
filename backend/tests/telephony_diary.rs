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

//! Arranging a time with a caller.
//!
//! When a practice is open is decided by pure functions, tested where they live, including
//! both days a year when local time is not a function of the local calendar. What only a
//! database can show is the rest: that a time offered is a time free, that two callers
//! cannot take one slot, that a caller who cannot be identified changes nothing, and that
//! none of it happens before the caller has been checked.
//!
//! The tools are driven through the authorisation seam rather than around it, as everywhere
//! else in this feature: the witness `dispatch` demands cannot be built from a test crate,
//! so the only way in is the gate a real call meets.
//!
//! Needs a reachable Postgres and Redis; skips when `DATABASE_URL` is unset.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::telephony::diary;
use fosnie_backend::tools::phone::{self, CallToolCtx};
use fosnie_backend::tools::{self, AuthorisedTools, NativeDecision};
use fosnie_backend::{cache, db};

/// The zone every diary here keeps, so the expected local times are stated once.
const ZONE: &str = "Europe/London";

struct Cast {
    state: AppState,
    pg: PgPool,
    owner: Uuid,
    agent: Uuid,
    line: Uuid,
    call: Uuid,
    chat: Uuid,
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

    let owner = mk_user(&pg, "Practice").await;
    let agent = mk_agent(&pg, owner).await;
    let line = mk_line(&pg, owner, agent).await;
    let call = mk_call(&pg, owner, agent, line, "+447700900123").await;
    let chat = mk_chat(&pg, owner, agent).await;
    Some(Cast { state, pg, owner, agent, line, call, chat })
}

async fn clear_up(c: &Cast) {
    for sql in [
        "DELETE FROM appointments WHERE owner_user_id = $1",
        "DELETE FROM diaries WHERE owner_user_id = $1",
        "DELETE FROM conflict_names WHERE owner_user_id = $1",
        "DELETE FROM enquiries WHERE owner_user_id = $1",
        "DELETE FROM calls WHERE owner_user_id = $1",
        "DELETE FROM phone_numbers WHERE owner_user_id = $1",
        "DELETE FROM chats WHERE owner_user_id = $1",
        "DELETE FROM agents WHERE created_by = $1",
        "DELETE FROM users WHERE id = $1",
    ] {
        let _ = sqlx::query(sql).bind(c.owner).execute(&c.pg).await;
    }
}

async fn mk_user(pg: &PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, $2, $3, 'user')")
        .bind(id)
        .bind(name)
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

async fn mk_call(pg: &PgPool, owner: Uuid, agent: Uuid, line: Uuid, from: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO calls (id, phone_number_id, provider, provider_call_id, to_e164, from_e164, \
                            owner_user_id, agent_id) \
         VALUES ($1, $2, 'twilio', $3, '+441315550000', $4, $5, $6)",
    )
    .bind(id)
    .bind(line)
    .bind(format!("CA{}", id.simple()))
    .bind(from)
    .bind(owner)
    .bind(agent)
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn mk_chat(pg: &PgPool, owner: Uuid, agent: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO chats (id, owner_user_id, agent_id, title, origin) \
         VALUES ($1, $2, $3, 'A call', 'phone')",
    )
    .bind(id)
    .bind(owner)
    .bind(agent)
    .execute(pg)
    .await
    .unwrap();
    id
}

/// A diary open every day, so a test never has to care which day it runs on.
///
/// Nine to five with no break, half-hour appointments, no lead time and a fortnight ahead.
/// Every weekday is filled in because the suite runs on whatever day it runs on, and a
/// diary that was only open on Tuesdays would make every assertion below depend on the
/// calendar.
async fn mk_diary(c: &Cast, enabled: bool) {
    sqlx::query(
        "INSERT INTO diaries \
           (owner_user_id, timezone, slot_minutes, lead_minutes, horizon_days, enabled) \
         VALUES ($1, $2, 30, 0, 14, $3) \
         ON CONFLICT (owner_user_id) DO UPDATE SET enabled = EXCLUDED.enabled",
    )
    .bind(c.owner)
    .bind(ZONE)
    .bind(enabled)
    .execute(&c.pg)
    .await
    .expect("make a diary");
    for weekday in 0..7i16 {
        sqlx::query(
            "INSERT INTO diary_hours (owner_user_id, weekday, opens_minute, closes_minute) \
             VALUES ($1, $2, 540, 1020) ON CONFLICT DO NOTHING",
        )
        .bind(c.owner)
        .bind(weekday)
        .execute(&c.pg)
        .await
        .expect("open the practice");
    }
}

/// Run one tool call the way a turn runs it: offer the tool, meet the gate, dispatch.
async fn call_tool(
    c: &Cast,
    ctx: &AuthContext,
    name: &str,
    call_ctx: Option<&CallToolCtx>,
    args: serde_json::Value,
) -> String {
    let offered: Vec<String> = phone::ALL
        .iter()
        .filter(|t| tools::host_enabled(t, &c.state.boot.features))
        .filter(|t| !phone::is_phone_tool(t) || call_ctx.is_some())
        .filter(|t| {
            **t != phone::TRANSFER_CALL
                || call_ctx.map(|x| x.transfer_e164.is_some()).unwrap_or(false)
        })
        .filter(|t| {
            **t != phone::SCREEN_CONFLICT
                || call_ctx.map(|x| x.screening_required).unwrap_or(false)
        })
        // The four diary tools are offered only when the account keeps one, switched on.
        .filter(|t| !phone::needs_diary(t) || call_ctx.map(|x| x.diary_enabled).unwrap_or(false))
        .map(|t| t.to_string())
        .collect();
    let authorised = AuthorisedTools::build(&offered, &offered, false, &HashMap::new());
    let decision = tools::authorize_native_call(
        &c.state,
        ctx,
        c.chat,
        &authorised,
        &HashMap::new(),
        name,
        None,
    )
    .await;
    let witness = match decision {
        NativeDecision::Allowed(w) => w,
        NativeDecision::Recoverable(msg) => return msg,
        NativeDecision::Denied(e) => return format!("error: {e}"),
    };
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    tools::dispatch(
        &c.state,
        ctx,
        c.chat,
        Uuid::now_v7(),
        &tx,
        None,
        None,
        None,
        call_ctx,
        &[],
        &HashMap::new(),
        &witness,
        &args,
    )
    .await
    .unwrap_or_else(|e| format!("error: {e}"))
}

/// The codes out of an availability answer, in the order they were offered.
fn tokens(said: &str) -> Vec<String> {
    said.lines()
        .filter_map(|l| l.split("use ").nth(1))
        .map(|t| t.trim_end_matches(')').trim().to_string())
        .collect()
}

async fn live_appointments(c: &Cast) -> Vec<(String, OffsetDateTime)> {
    sqlx::query_as::<_, (String, OffsetDateTime)>(
        "SELECT reference, starts_at FROM appointments \
          WHERE owner_user_id = $1 AND status = 'booked' ORDER BY starts_at",
    )
    .bind(c.owner)
    .fetch_all(&c.pg)
    .await
    .expect("read the diary back")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_is_offered_free_times_and_takes_one() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_booking(&c).await;
    clear_up(&c).await;
    outcome.expect("booking went wrong");
}

async fn check_booking(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);

    // With no diary, the tools are not offered at all: an agent offering times from a
    // diary nobody filled in would be inventing them.
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    if call_ctx.diary_enabled {
        return Err("an account with no diary was told it kept one".into());
    }
    let said = call_tool(c, &ctx, "check_availability", Some(&call_ctx), json!({})).await;
    if !said.starts_with("error:") {
        return Err(format!("times were offered with no diary: {said}"));
    }

    mk_diary(c, true).await;
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    if !call_ctx.diary_enabled {
        return Err("a diary that was switched on did not read as enabled".into());
    }

    let said = call_tool(c, &ctx, "check_availability", Some(&call_ctx), json!({})).await;
    if said.starts_with("error:") {
        return Err(format!("a diary with hours offered nothing: {said}"));
    }
    // It says what today is where the practice is, because the only other clock the agent
    // has reports UTC.
    if !said.contains("It is now") {
        return Err(format!("the answer did not say what time it is there: {said}"));
    }
    // And it tells the agent not to read the codes out, because they are not for saying.
    if !said.contains("never read out the codes") {
        return Err(format!("the answer did not say the codes are not for saying: {said}"));
    }
    let offered = tokens(&said);
    if offered.len() != diary::OFFER_LIMIT {
        return Err(format!("{} times were offered, not {}", offered.len(), diary::OFFER_LIMIT));
    }

    // A time the diary did not offer cannot be booked, however it is asked for.
    for bad in ["not a time at all", "2026-08-04T03:07:00Z", ""] {
        let said = call_tool(
            c,
            &ctx,
            "book_appointment",
            Some(&call_ctx),
            json!({ "slot": bad, "name": "Jane Fraser", "subject": "A survey" }),
        )
        .await;
        if !said.starts_with("error:") {
            return Err(format!("{bad:?} was accepted as a time: {said}"));
        }
    }
    if !live_appointments(c).await.is_empty() {
        return Err("a refused booking still wrote an appointment".into());
    }

    // One of the offered ones is taken.
    let said = call_tool(
        c,
        &ctx,
        "book_appointment",
        Some(&call_ctx),
        json!({ "slot": offered[0], "name": "Jane Fraser", "contact": "07700 900123", "subject": "A survey" }),
    )
    .await;
    if said.starts_with("error:") {
        return Err(format!("a time that was offered could not be booked: {said}"));
    }
    // The caller is given something to quote, and told to read it back.
    if !said.contains("reference is") || !said.contains("letter by letter") {
        return Err(format!("the caller was given nothing to quote: {said}"));
    }
    let live = live_appointments(c).await;
    if live.len() != 1 {
        return Err(format!("{} appointments after one booking", live.len()));
    }

    // And it is no longer offered.
    let again = tokens(&call_tool(c, &ctx, "check_availability", Some(&call_ctx), json!({})).await);
    if again.contains(&offered[0]) {
        return Err("a booked time was offered again".into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_callers_cannot_take_one_time() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_race(&c).await;
    clear_up(&c).await;
    outcome.expect("two callers took one time");
}

/// The failure the unique index exists for, driven concurrently rather than in sequence.
///
/// Checking that a time is free and then taking it is two statements, and two callers are
/// two tasks: in sequence this test would pass against an implementation that has the race,
/// which is the whole reason it is written this way.
async fn check_race(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    mk_diary(c, true).await;
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    let offered = tokens(&call_tool(c, &ctx, "check_availability", Some(&call_ctx), json!({})).await);
    let slot = offered.first().ok_or("nothing was offered")?.clone();

    // A second call, on the same line, from somebody else.
    let other_call = mk_call(&c.pg, c.owner, c.agent, c.line, "+447700900999").await;
    let other_ctx = phone::load_ctx(&c.pg, Some(other_call), Some(c.owner))
        .await
        .ok_or("the second call did not resolve")?;

    let first = call_tool(
        c,
        &ctx,
        "book_appointment",
        Some(&call_ctx),
        json!({ "slot": slot, "name": "Jane Fraser", "subject": "A survey" }),
    );
    let second = call_tool(
        c,
        &ctx,
        "book_appointment",
        Some(&other_ctx),
        json!({ "slot": slot, "name": "Peter Bell", "subject": "Also a survey" }),
    );
    let (a, b) = tokio::join!(first, second);

    let failed = [&a, &b].iter().filter(|s| s.starts_with("error:")).count();
    if failed != 1 {
        return Err(format!("{failed} of two simultaneous bookings failed:\n{a}\n{b}"));
    }
    // The loser is told something it can act on, rather than told an error.
    let loser = if a.starts_with("error:") { &a } else { &b };
    if !loser.contains("just been taken") || !loser.contains("offer another") {
        return Err(format!("the losing caller was given nothing to say: {loser}"));
    }
    let live = live_appointments(c).await;
    if live.len() != 1 {
        return Err(format!("{} appointments exist for one time", live.len()));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_person_who_booked_can_change_it() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_identification(&c).await;
    clear_up(&c).await;
    outcome.expect("the wrong person could change an appointment");
}

async fn check_identification(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    mk_diary(c, true).await;
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    let offered = tokens(&call_tool(c, &ctx, "check_availability", Some(&call_ctx), json!({})).await);
    call_tool(
        c,
        &ctx,
        "book_appointment",
        Some(&call_ctx),
        json!({ "slot": offered[0], "name": "Jane Alice Fraser", "subject": "A survey" }),
    )
    .await;
    let reference = live_appointments(c).await.first().ok_or("nothing was booked")?.0.clone();

    // Somebody else, ringing from another telephone, who knows the reference but not the
    // name. Two things have to agree, so this changes nothing.
    let stranger_call = mk_call(&c.pg, c.owner, c.agent, c.line, "+447700900555").await;
    let stranger = phone::load_ctx(&c.pg, Some(stranger_call), Some(c.owner))
        .await
        .ok_or("the second call did not resolve")?;
    let said = call_tool(
        c,
        &ctx,
        "cancel_appointment",
        Some(&stranger),
        json!({ "reference": reference, "name": "Peter Bell" }),
    )
    .await;
    if !said.starts_with("error:") {
        return Err(format!("a stranger cancelled somebody's appointment: {said}"));
    }
    // And the refusal says nothing about which half was wrong, because that is an oracle.
    if said.contains("name") || said.contains("number") {
        return Err(format!("the refusal said what was wrong: {said}"));
    }
    if live_appointments(c).await.len() != 1 {
        return Err("a refused cancellation still changed the diary".into());
    }

    // A wrong reference from the right telephone is also not enough.
    let said = call_tool(
        c,
        &ctx,
        "cancel_appointment",
        Some(&call_ctx),
        json!({ "reference": "ZZZZZZ", "name": "Jane Alice Fraser" }),
    )
    .await;
    if !said.starts_with("error:") {
        return Err(format!("a wrong reference was accepted: {said}"));
    }

    // The right person: the reference, and the number they are ringing from.
    let said = call_tool(
        c,
        &ctx,
        "move_appointment",
        Some(&call_ctx),
        json!({ "reference": reference, "name": "", "slot": offered[1] }),
    )
    .await;
    if said.starts_with("error:") {
        return Err(format!("the person who booked could not move it: {said}"));
    }
    let live = live_appointments(c).await;
    if live.len() != 1 {
        return Err(format!("{} appointments after a move", live.len()));
    }
    // The old time is free again, and the new one is not.
    let after = tokens(&call_tool(c, &ctx, "check_availability", Some(&call_ctx), json!({})).await);
    if !after.contains(&offered[0]) {
        return Err("moving an appointment did not give its old time back".into());
    }
    if after.contains(&offered[1]) {
        return Err("the time it moved to is still being offered".into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_caller_cannot_keep_guessing() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_attempt_cap(&c).await;
    clear_up(&c).await;
    outcome.expect("the attempt cap did not hold");
}

async fn check_attempt_cap(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    mk_diary(c, true).await;
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    // Six characters is far out of reach at three tries a call, and well within it given
    // unlimited ones, so the cap is the whole of what makes the reference worth anything.
    for i in 0..diary::MAX_IDENTIFY_ATTEMPTS {
        let said = call_tool(
            c,
            &ctx,
            "cancel_appointment",
            Some(&call_ctx),
            json!({ "reference": format!("GUESS{i}"), "name": "Anybody" }),
        )
        .await;
        if !said.starts_with("error:") {
            return Err(format!("a guess succeeded: {said}"));
        }
        if said.contains("too many") {
            return Err(format!("the cap fired on attempt {i}, before it should"));
        }
    }
    let said = call_tool(
        c,
        &ctx,
        "cancel_appointment",
        Some(&call_ctx),
        json!({ "reference": "GUESSX", "name": "Anybody" }),
    )
    .await;
    if !said.contains("too many") {
        return Err(format!("the cap did not fire on the fourth attempt: {said}"));
    }
    let attempts: i16 = sqlx::query_scalar("SELECT appointment_attempts FROM calls WHERE id = $1")
        .bind(c.call)
        .fetch_one(&c.pg)
        .await
        .expect("read the attempts back");
    if attempts < diary::MAX_IDENTIFY_ATTEMPTS {
        return Err(format!("only {attempts} attempts were counted"));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nothing_is_arranged_before_the_caller_is_checked() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_screening_gate(&c).await;
    clear_up(&c).await;
    outcome.expect("the check did not gate the diary");
}

async fn check_screening_gate(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    mk_diary(c, true).await;
    // The practice keeps a list, so callers are checked before anything is arranged for
    // them: offering an appointment to somebody it may not act for is the thing the check
    // exists to prevent.
    sqlx::query(
        "INSERT INTO conflict_names (id, owner_user_id, name, normalised) \
         VALUES ($1, $2, 'Marchetti Quarry Holdings', 'holdings marchetti quarry')",
    )
    .bind(Uuid::now_v7())
    .bind(c.owner)
    .execute(&c.pg)
    .await
    .expect("add a name to the list");

    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    let offered = tokens(&call_tool(c, &ctx, "check_availability", Some(&call_ctx), json!({})).await);
    if offered.is_empty() {
        return Err("times were not offered before the check, which they should be".into());
    }

    // Never checked: nothing is booked.
    let said = call_tool(
        c,
        &ctx,
        "book_appointment",
        Some(&call_ctx),
        json!({ "slot": offered[0], "name": "Jane Fraser", "subject": "A survey" }),
    )
    .await;
    if !said.starts_with("error:") {
        return Err(format!("an unchecked caller was booked in: {said}"));
    }
    if !said.contains("screen_conflict") {
        return Err(format!("the refusal did not say how to proceed: {said}"));
    }

    // Checked and matched: still nothing, and nothing said about why.
    call_tool(
        c,
        &ctx,
        "screen_conflict",
        Some(&call_ctx),
        json!({ "name": "Marchetti Quarry Holdings" }),
    )
    .await;
    let said = call_tool(
        c,
        &ctx,
        "book_appointment",
        Some(&call_ctx),
        json!({ "slot": offered[0], "name": "Marchetti Quarry Holdings", "subject": "A survey" }),
    )
    .await;
    if !said.starts_with("error:") {
        return Err(format!("a matched caller was booked in: {said}"));
    }
    if said.to_lowercase().contains("marchetti") {
        return Err(format!("the refusal named the list entry: {said}"));
    }
    if !live_appointments(c).await.is_empty() {
        return Err("something was booked despite the check".into());
    }

    // Checked and clear: booked.
    call_tool(c, &ctx, "screen_conflict", Some(&call_ctx), json!({ "name": "Jane Fraser" })).await;
    let said = call_tool(
        c,
        &ctx,
        "book_appointment",
        Some(&call_ctx),
        json!({ "slot": offered[0], "name": "Jane Fraser", "subject": "A survey" }),
    )
    .await;
    if said.starts_with("error:") {
        return Err(format!("a checked, clear caller could not be booked: {said}"));
    }
    if live_appointments(c).await.len() != 1 {
        return Err("a clear caller was not booked in".into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn erasing_a_practice_takes_its_diary_with_it() {
    let Some(c) = cast().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let outcome = check_erasure(&c).await;
    clear_up(&c).await;
    outcome.expect("erasure and a diary cannot both be true");
}

async fn check_erasure(c: &Cast) -> Result<(), String> {
    let ctx = ctx_for(c.owner, PlatformRole::User);
    mk_diary(c, true).await;
    sqlx::query("INSERT INTO diary_closures (owner_user_id, closed_on) VALUES ($1, '2026-12-25')")
        .bind(c.owner)
        .execute(&c.pg)
        .await
        .expect("shut for Christmas");
    let call_ctx = phone::load_ctx(&c.pg, Some(c.call), Some(c.owner))
        .await
        .ok_or("the call did not resolve")?;
    let offered = tokens(&call_tool(c, &ctx, "check_availability", Some(&call_ctx), json!({})).await);
    call_tool(
        c,
        &ctx,
        "book_appointment",
        Some(&call_ctx),
        json!({ "slot": offered[0], "name": "Jane Fraser", "subject": "A survey" }),
    )
    .await;

    // The same sequence the erasure routine performs, in the same order. The appointments
    // go before the calls they were made on, and the diary carries its own hours and closed
    // days away with it.
    let mut tx = c.pg.begin().await.unwrap();
    for sql in [
        "DELETE FROM enquiries WHERE owner_user_id = $1",
        "DELETE FROM conflict_names WHERE owner_user_id = $1",
        "DELETE FROM appointments WHERE owner_user_id = $1",
        "DELETE FROM diaries WHERE owner_user_id = $1",
        "DELETE FROM calls WHERE owner_user_id = $1",
        "DELETE FROM phone_numbers WHERE owner_user_id = $1",
        "DELETE FROM chats WHERE owner_user_id = $1",
        "DELETE FROM agents WHERE created_by = $1",
    ] {
        sqlx::query(sql)
            .bind(c.owner)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("erasure failed at {sql:?}: {e}"))?;
    }
    tx.commit().await.map_err(|e| format!("erasure could not be committed: {e}"))?;

    for (what, sql) in [
        ("appointments", "SELECT count(*) FROM appointments WHERE owner_user_id = $1"),
        ("diaries", "SELECT count(*) FROM diaries WHERE owner_user_id = $1"),
        ("opening hours", "SELECT count(*) FROM diary_hours WHERE owner_user_id = $1"),
        ("closed days", "SELECT count(*) FROM diary_closures WHERE owner_user_id = $1"),
    ] {
        let left: i64 =
            sqlx::query_scalar(sql).bind(c.owner).fetch_one(&c.pg).await.expect("count what is left");
        if left != 0 {
            return Err(format!("{left} {what} survived the erasure"));
        }
    }
    Ok(())
}
