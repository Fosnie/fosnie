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

//! Arranging a time with somebody on the telephone.
//!
//! The rules about when a practice is open are in `diary`, and none of them are repeated
//! here: this is the part that reads the diary out of the database, writes an appointment
//! into it, and decides who is allowed to change one.
//!
//! Three things are enforced here rather than asked for in an instruction, because a model
//! talking to a stranger is not where any of them belong.
//!
//! **The time is never the model's to choose.** Availability hands out tokens, and booking
//! re-derives the instant from the opening hours before it writes anything, so a time
//! outside them cannot become an appointment however it is asked for.
//!
//! **Two callers cannot take one slot.** The insert leans on a unique index rather than on
//! having looked first: checking and then writing is two statements and two callers are two
//! tasks, so the loser is told the time has gone and offers another.
//!
//! **Somebody ringing back has to be identified by two independent things**, with a hard
//! cap on how many times one call may try, and a refusal that says nothing about which half
//! was wrong.

use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditEvent, AuditOutcome};
use crate::auth::AuthContext;
use crate::state::AppState;
use crate::telephony::{conflict, diary, notify};
use crate::tools::phone::CallToolCtx;

/// Load a practice's diary, or nothing when it keeps none or has it switched off.
pub async fn load(pg: &sqlx::PgPool, owner: Uuid) -> Option<diary::Diary> {
    let row = sqlx::query!(
        "SELECT timezone, slot_minutes, lead_minutes, horizon_days \
           FROM diaries WHERE owner_user_id = $1 AND enabled",
        owner
    )
    .fetch_optional(pg)
    .await
    .ok()
    .flatten()?;
    let hours = sqlx::query!(
        "SELECT weekday, opens_minute, closes_minute FROM diary_hours \
          WHERE owner_user_id = $1 ORDER BY weekday, opens_minute",
        owner
    )
    .fetch_all(pg)
    .await
    .ok()?;
    let closures = sqlx::query_scalar!(
        "SELECT closed_on FROM diary_closures WHERE owner_user_id = $1",
        owner
    )
    .fetch_all(pg)
    .await
    .ok()?;
    Some(diary::Diary {
        timezone: row.timezone,
        slot_minutes: row.slot_minutes,
        lead_minutes: row.lead_minutes,
        horizon_days: row.horizon_days,
        hours: hours
            .into_iter()
            .map(|h| diary::Opening {
                weekday: h.weekday as u8,
                opens_minute: h.opens_minute,
                closes_minute: h.closes_minute,
            })
            .collect(),
        closures,
    })
}

/// The instants already taken, from now on.
async fn taken(pg: &sqlx::PgPool, owner: Uuid, from: OffsetDateTime) -> Vec<OffsetDateTime> {
    sqlx::query_scalar!(
        "SELECT starts_at FROM appointments \
          WHERE owner_user_id = $1 AND status = 'booked' AND starts_at >= $2",
        owner,
        from,
    )
    .fetch_all(pg)
    .await
    .unwrap_or_default()
}

/// What the practice can offer, said the way a receptionist would say it.
///
/// The answer opens with the practice's own date and time. That is not decoration: the
/// only clock the agent has otherwise reports UTC, so without this it cannot work out what
/// "tomorrow" means to the person it is speaking to.
pub async fn availability(
    state: &AppState,
    call: &CallToolCtx,
    from_date: Option<&str>,
    part: Option<&str>,
) -> String {
    let Some(d) = load(&state.pg, call.owner_user_id).await else {
        return "error: this line keeps no diary, so there are no times to offer. Offer to \
                take a message instead."
            .into();
    };
    let Some(tz) = diary::zone(&d.timezone) else {
        return "error: the diary's time zone is not one this system knows, so no times can \
                be offered. Offer to take a message instead."
            .into();
    };
    let now = OffsetDateTime::now_utc();
    let booked = taken(&state.pg, call.owner_user_id, now).await;
    let part = diary::PartOfDay::parse(part);

    // A day the caller asked for, read in the practice's own calendar. Anything that is not
    // a date is ignored rather than refused: the caller said something vague and the answer
    // is simply the next available times.
    let wanted = from_date.and_then(parse_local_date);
    let all = diary::open_slots(&d, now, &booked, 5_000);
    let offered: Vec<diary::Slot> = all
        .into_iter()
        .filter(|s| part.covers(tz, s.starts_at))
        .filter(|s| match wanted {
            Some(day) => diary::local_date(tz, s.starts_at).map(|d| d >= day).unwrap_or(false),
            None => true,
        })
        .take(diary::OFFER_LIMIT)
        .collect();

    if offered.is_empty() {
        return "Nothing is free in that time. Say so, offer to look at another day, or offer \
                to take a message."
            .into();
    }
    let today = diary::spoken_at(tz, now);
    let lines: Vec<String> = offered
        .iter()
        .map(|s| format!("- {} (to book this one, use {})", s.spoken, token(s.starts_at)))
        .collect();
    format!(
        "It is now {today} where the practice is. Free times, soonest first:\n{}\n\nOffer two or \
         three of these aloud, not the whole list, and never read out the codes: they are for \
         book_appointment only.",
        lines.join("\n")
    )
}

/// The token that names a slot: the instant itself, in the one form that round trips.
fn token(at: OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

fn parse_token(raw: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(raw.trim(), &time::format_description::well_known::Rfc3339).ok()
}

/// A bare local date, as the model would offer one.
fn parse_local_date(raw: &str) -> Option<time::Date> {
    let mut it = raw.trim().splitn(3, '-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u8 = it.next()?.parse().ok()?;
    let d: u8 = it.next()?.parse().ok()?;
    time::Date::from_calendar_date(y, time::Month::try_from(m).ok()?, d).ok()
}

/// Is this call allowed to arrange anything at all?
///
/// The same check that governs putting a caller through governs booking them in, and for
/// the same reason: offering an appointment to somebody the practice may not act for is
/// the thing the check exists to prevent. Never having been checked counts as not clear.
async fn screened_ok(state: &AppState, call: &CallToolCtx) -> bool {
    if !call.screening_required {
        return true;
    }
    let recorded: Option<String> =
        sqlx::query_scalar!("SELECT conflict_check FROM calls WHERE id = $1", call.call_id)
            .fetch_optional(&state.pg)
            .await
            .ok()
            .flatten()
            .flatten();
    conflict::Verdict::lets_a_call_through(recorded.as_deref().and_then(conflict::Verdict::from_str))
}

const NOT_SCREENED: &str = "error: this caller has not been checked yet, so nothing can be \
     arranged for them. Ask for their full name and use screen_conflict first; if the check says \
     otherwise, take a message instead and do not say why.";

/// Book a caller in.
pub async fn book(
    state: &AppState,
    ctx: &AuthContext,
    call: &CallToolCtx,
    chat_id: Uuid,
    slot: &str,
    name: &str,
    contact: Option<&str>,
    subject: &str,
) -> String {
    let Some(d) = load(&state.pg, call.owner_user_id).await else {
        return "error: this line keeps no diary, so nothing can be booked. Offer to take a \
                message instead."
            .into();
    };
    if !screened_ok(state, call).await {
        return NOT_SCREENED.into();
    }
    let name = name.trim();
    let subject = subject.trim();
    if name.is_empty() || subject.is_empty() {
        return "error: an appointment needs the caller's full name and a short line saying \
                what it is about."
            .into();
    }
    let Some(at) = parse_token(slot) else {
        return "error: that is not one of the times offered. Use check_availability and book \
                one of the codes it gives."
            .into();
    };
    // Derived again from the opening hours, not trusted from the token. This is what stops
    // an appointment being written for a time the practice is not open, whether the token
    // is stale, invented, or a real time on a day that has since been closed.
    if !diary::is_open_slot(&d, at) {
        return "error: that time is not one the practice can offer any more. Use \
                check_availability again and offer one of those."
            .into();
    }
    let ends_at = at + time::Duration::minutes(d.slot_minutes as i64);
    let id = Uuid::now_v7();
    let reference = crate::tools::phone::reference(id);

    // Leaning on the unique index rather than on having looked first: two callers are two
    // tasks, and "is it free" followed by "take it" is a race whichever order it is in.
    let done = sqlx::query!(
        "INSERT INTO appointments \
           (id, owner_user_id, starts_at, ends_at, reference, caller_name, caller_e164, \
            contact, subject, call_id, chat_id) \
         SELECT $1, c.owner_user_id, $2, $3, $4, $5, c.from_e164, $6, $7, c.id, $8 \
           FROM calls c WHERE c.id = $9 \
         ON CONFLICT DO NOTHING",
        id,
        at,
        ends_at,
        reference,
        name,
        contact,
        subject,
        chat_id,
        call.call_id,
    )
    .execute(&state.pg)
    .await;
    match done {
        Ok(r) if r.rows_affected() == 1 => {}
        Ok(_) => {
            // Somebody else took it between being offered and being booked, which is the
            // ordinary case on a busy morning rather than a fault.
            return "error: that time has just been taken. Say so, and offer another from \
                    check_availability."
                .into();
        }
        Err(e) => {
            tracing::warn!(error = %e, call_id = %call.call_id, "could not write an appointment");
            return "error: the appointment could not be made. Apologise, and offer to take a \
                    message instead."
                .into();
        }
    }

    audit_appointment(state, ctx, "telephony.appointment.booked", id, call, Some(at), true).await;
    metrics::counter!("telephony_appointments_total", "action" => "booked").increment(1);
    announce(state, call, chat_id, &d, at, "booked");
    tell_outside(state, call, &d, at, notify::Event::AppointmentBooked);

    let tz = diary::zone(&d.timezone);
    let spoken = tz.map(|z| diary::spoken_at(z, at)).unwrap_or_else(|| token(at));
    format!(
        "Booked for {spoken}. Their reference is {reference}: read it back to them, letter by \
         letter and digit by digit, and tell them to quote it if they need to change or cancel. \
         Confirm the day and time once, then stop."
    )
}

/// An appointment this caller is allowed to act on, or nothing.
///
/// Two independent things have to agree, and the refusal never says which failed: "that
/// reference exists but the name is wrong" tells somebody guessing that they are halfway
/// there. Attempts are counted on the call, so a caller cannot keep trying.
async fn identify(
    state: &AppState,
    ctx: &AuthContext,
    call: &CallToolCtx,
    reference: &str,
    name: &str,
) -> Result<(Uuid, OffsetDateTime, String), String> {
    let attempts: i16 = sqlx::query_scalar!(
        "UPDATE calls SET appointment_attempts = appointment_attempts + 1 \
          WHERE id = $1 RETURNING appointment_attempts",
        call.call_id
    )
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten()
    .unwrap_or(diary::MAX_IDENTIFY_ATTEMPTS + 1);
    if attempts > diary::MAX_IDENTIFY_ATTEMPTS {
        audit_identify(state, ctx, call, reference, "too_many_attempts").await;
        return Err("error: that appointment cannot be found, and there have been too many \
                    attempts on this call. Offer to take a message instead."
            .into());
    }

    let reference = reference.trim().to_uppercase();
    let found = sqlx::query!(
        "SELECT id, starts_at, caller_name, caller_e164 FROM appointments \
          WHERE owner_user_id = $1 AND reference = $2 AND status = 'booked'",
        call.owner_user_id,
        reference,
    )
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten();

    // One refusal for every way this can fail: no such reference, the wrong person, or an
    // appointment made in person that has no reference to quote at all.
    let refusal = "error: that appointment cannot be found. Ask them to check the reference \
                   they were given, or offer to take a message. Do not say what was wrong.";
    let Some(row) = found else {
        audit_identify(state, ctx, call, &reference, "no_such_reference").await;
        return Err(refusal.into());
    };
    if !diary::caller_matches(&row.caller_name, &row.caller_e164, name, &call.from_e164) {
        audit_identify(state, ctx, call, &reference, "caller_did_not_match").await;
        return Err(refusal.into());
    }
    audit_identify(state, ctx, call, &reference, "matched").await;
    Ok((row.id, row.starts_at, row.caller_name))
}

/// Move an appointment to another time.
pub async fn move_to(
    state: &AppState,
    ctx: &AuthContext,
    call: &CallToolCtx,
    chat_id: Uuid,
    reference: &str,
    name: &str,
    slot: &str,
) -> String {
    let Some(d) = load(&state.pg, call.owner_user_id).await else {
        return "error: this line keeps no diary, so nothing can be moved.".into();
    };
    if !screened_ok(state, call).await {
        return NOT_SCREENED.into();
    }
    let (id, _was, recorded_name) = match identify(state, ctx, call, reference, name).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let Some(at) = parse_token(slot) else {
        return "error: that is not one of the times offered. Use check_availability first."
            .into();
    };
    if !diary::is_open_slot(&d, at) {
        return "error: that time is not one the practice can offer. Use check_availability \
                again and offer one of those."
            .into();
    }
    let ends_at = at + time::Duration::minutes(d.slot_minutes as i64);
    let new_id = Uuid::now_v7();

    // One transaction, and the old appointment is released inside it **before** the new one
    // is written. Both orders are tempting and only this one works: an appointment keeps its
    // reference when it moves, and a reference is unique among live appointments, so writing
    // the new row first would collide with the very row it is replacing. Releasing first is
    // safe because losing the new time rolls the whole thing back, leaving the caller with
    // the appointment they started with.
    let mut tx = match state.pg.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::warn!(error = %e, "could not begin to move an appointment");
            return "error: the appointment could not be moved. Offer to take a message.".into();
        }
    };
    if let Err(e) = sqlx::query!(
        "UPDATE appointments SET status = 'cancelled', cancelled_at = now() WHERE id = $1",
        id
    )
    .execute(&mut *tx)
    .await
    {
        tracing::warn!(error = %e, "could not release the old appointment");
        return "error: the appointment could not be moved. Offer to take a message.".into();
    }
    let inserted = sqlx::query!(
        "INSERT INTO appointments \
           (id, owner_user_id, starts_at, ends_at, reference, caller_name, caller_e164, \
            contact, subject, call_id, chat_id) \
         SELECT $1, a.owner_user_id, $2, $3, a.reference, a.caller_name, a.caller_e164, \
                a.contact, a.subject, $4, $5 \
           FROM appointments a WHERE a.id = $6 \
         ON CONFLICT DO NOTHING",
        new_id,
        at,
        ends_at,
        call.call_id,
        chat_id,
        id,
    )
    .execute(&mut *tx)
    .await;
    match inserted {
        Ok(r) if r.rows_affected() == 1 => {}
        Ok(_) => {
            // Dropped without committing, so releasing the old time is undone with it and
            // the caller still has the appointment they rang about.
            return "error: that time has just been taken, so nothing has changed. Offer \
                    another from check_availability."
                .into();
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not write the moved appointment");
            return "error: the appointment could not be moved. Offer to take a message.".into();
        }
    }
    if let Err(e) = tx.commit().await {
        tracing::warn!(error = %e, "could not commit a moved appointment");
        return "error: the appointment could not be moved. Offer to take a message.".into();
    }

    audit_appointment(state, ctx, "telephony.appointment.moved", new_id, call, Some(at), true).await;
    metrics::counter!("telephony_appointments_total", "action" => "moved").increment(1);
    announce(state, call, chat_id, &d, at, "moved");
    tell_outside(state, call, &d, at, notify::Event::AppointmentMoved);
    let spoken = diary::zone(&d.timezone).map(|z| diary::spoken_at(z, at)).unwrap_or_else(|| token(at));
    let _ = recorded_name;
    format!(
        "Moved to {spoken}. The reference is unchanged. Confirm the new day and time once, then \
         stop."
    )
}

/// Cancel an appointment.
pub async fn cancel(
    state: &AppState,
    ctx: &AuthContext,
    call: &CallToolCtx,
    chat_id: Uuid,
    reference: &str,
    name: &str,
) -> String {
    if load(&state.pg, call.owner_user_id).await.is_none() {
        return "error: this line keeps no diary, so nothing can be cancelled.".into();
    }
    let (id, was, _) = match identify(state, ctx, call, reference, name).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    if let Err(e) = sqlx::query!(
        "UPDATE appointments SET status = 'cancelled', cancelled_at = now() \
          WHERE id = $1 AND status = 'booked'",
        id
    )
    .execute(&state.pg)
    .await
    {
        tracing::warn!(error = %e, "could not cancel an appointment");
        return "error: the appointment could not be cancelled. Offer to take a message.".into();
    }
    audit_appointment(state, ctx, "telephony.appointment.cancelled", id, call, Some(was), true)
        .await;
    metrics::counter!("telephony_appointments_total", "action" => "cancelled").increment(1);
    if let Some(d) = load(&state.pg, call.owner_user_id).await {
        announce(state, call, chat_id, &d, was, "cancelled");
        tell_outside(state, call, &d, was, notify::Event::AppointmentCancelled);
    }
    "Cancelled. Say it is cancelled, ask whether they would like another time, and if not \
     finish the call politely."
        .into()
}

async fn audit_appointment(
    state: &AppState,
    ctx: &AuthContext,
    action: &str,
    id: Uuid,
    call: &CallToolCtx,
    at: Option<OffsetDateTime>,
    ok: bool,
) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("appointment".into());
    ev.resource_id = Some(id);
    if !ok {
        ev.outcome = AuditOutcome::Failure;
    }
    ev.payload = Some(json!({
        "call_id": call.call_id,
        "from_e164": call.from_e164,
        "starts_at": at.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok()),
    }));
    let _ = audit::append(&state.pg, &ev).await;
}

/// Note an attempt to name an appointment, and whether it was allowed.
///
/// The reference offered and the outcome, because that is the record of who tried what.
/// Never the name recorded against the appointment: an attempt by the wrong person must not
/// write the right person's name into the record of this caller's telephone call.
async fn audit_identify(
    state: &AppState,
    ctx: &AuthContext,
    call: &CallToolCtx,
    reference: &str,
    outcome: &str,
) {
    let mut ev = AuditEvent::action("telephony.appointment.identify", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("appointment".into());
    ev.resource_id = Some(call.call_id);
    if outcome != "matched" {
        ev.outcome = AuditOutcome::Failure;
        ev.outcome_reason = Some(outcome.to_string());
    }
    ev.payload = Some(json!({
        "reference_offered": reference,
        "outcome": outcome,
        "from_e164": call.from_e164,
    }));
    let _ = audit::append(&state.pg, &ev).await;
    metrics::counter!("telephony_appointment_identify_total", "outcome" => outcome.to_string())
        .increment(1);
}

/// Tell the practice's own team chat that something was arranged.
///
/// The same shape as announcing a message: the day and time and what it is about, spawned
/// so the caller is not left listening to silence, and skipped silently when the line has
/// Tell whoever outside this deployment asked to be told.
///
/// Separate from the announcement above, and not conditional on it: a practice may watch a
/// chat service without keeping a team conversation here, and the appointment is theirs to
/// hear about either way. The time is spoken in the practice's own zone, like everywhere
/// else it appears, so nobody reads a time and means another one.
fn tell_outside(
    state: &AppState,
    call: &CallToolCtx,
    d: &diary::Diary,
    at: OffsetDateTime,
    event: notify::Event,
) {
    let spoken =
        diary::zone(&d.timezone).map(|z| diary::spoken_at(z, at)).unwrap_or_else(|| token(at));
    let state = state.clone();
    let owner = call.owner_user_id;
    let from = call.from_e164.clone();
    // On its own task: the caller is still on the line, and what happens to a notification
    // is no business of theirs.
    tokio::spawn(async move {
        notify::fire(&state, owner, event, &from, &spoken).await;
    });
}

/// nowhere to announce into.
fn announce(
    state: &AppState,
    call: &CallToolCtx,
    chat_id: Uuid,
    d: &diary::Diary,
    at: OffsetDateTime,
    what: &str,
) {
    let Some(target) = call.deliver_group_chat_id else { return };
    let spoken =
        diary::zone(&d.timezone).map(|z| diary::spoken_at(z, at)).unwrap_or_else(|| token(at));
    let state = state.clone();
    let owner = call.owner_user_id;
    let from = if call.from_e164.is_empty() { "a withheld number".to_string() } else { call.from_e164.clone() };
    let what = what.to_string();
    tokio::spawn(async move {
        match crate::http::messaging::is_member(&state, owner, target).await {
            Ok(true) => {}
            _ => return,
        }
        if sqlx::query!(
            "INSERT INTO chat_shares (chat_id, group_chat_id, shared_by) VALUES ($1, $2, $3) \
             ON CONFLICT (chat_id, group_chat_id) DO NOTHING",
            chat_id,
            target,
            owner,
        )
        .execute(&state.pg)
        .await
        .is_err()
        {
            return;
        }
        let content = format!("📅 An appointment was {what} by {from}: {spoken}");
        let _ = crate::http::messaging::post_chat_link(&state, target, None, chat_id, &content, true)
            .await;
        state.hub.send_to_user(
            owner,
            crate::ws::protocol::ServerFrame::Invalidate {
                keys: vec![vec!["diary-appointments".into()]],
            },
        );
    });
}
