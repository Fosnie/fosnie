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

//! Writing down what a caller wanted, and saying so.
//!
//! One way in, so the ceiling and the audit cannot be walked around by a second caller
//! arriving later. What is durable is the row; announcing it is best effort, because a
//! team chat being unavailable is not a reason to tell somebody on a telephone that
//! their message was not taken.
//!
//! Note what is not here: nothing removes these records after a period. They hold the
//! name and number of somebody with no account, who cannot ask what is held about them,
//! and a deployment keeping them indefinitely is making that choice by default rather
//! than deliberately. The call log has the same gap. Both want one retention setting and
//! one sweep, and that is the next thing this area needs.

use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::AuthContext;
use crate::state::AppState;
use crate::telephony::conflict;
use crate::tools::phone::{CallToolCtx, Enquiry, PER_CALL_LIMIT};

/// Write the record, then announce it.
///
/// Returns the identifier of what was written. The insert is awaited because it is the
/// promise being made to the caller; the announcement is not, because the caller is
/// listening to silence for as long as this takes.
pub async fn record(
    state: &AppState,
    ctx: &AuthContext,
    call: &CallToolCtx,
    chat_id: Uuid,
    e: &Enquiry,
) -> Result<Uuid, String> {
    // Counted over the two kinds a caller can ask for again and again. A handover is
    // written once, as the call leaves, and refusing to put somebody through because
    // they had already left five messages would be the wrong way round.
    let counted = e.kind != "handover";
    let taken: i64 = sqlx::query_scalar!(
        "SELECT count(*) AS \"n!\" FROM enquiries WHERE call_id = $1 AND kind <> 'handover'",
        call.call_id
    )
    .fetch_one(&state.pg)
    .await
    .map_err(|err| {
        tracing::warn!(error = %err, call_id = %call.call_id, "could not count this call's records");
        "error: the message could not be written down. Apologise, and ask the caller to ring back."
            .to_string()
    })?;
    if counted && taken >= PER_CALL_LIMIT {
        return Err(format!(
            "error: {PER_CALL_LIMIT} records have already been taken on this call, which is as many \
             as this line accepts. Tell the caller everything is written down, and finish the call \
             politely."
        ));
    }

    let id = Uuid::now_v7();
    // The line is read from the call row inside the insert rather than carried in from
    // the start of the turn. A line can be released while a call is still up, and a value
    // read minutes ago would then name a row that no longer exists: the same statement
    // that reads it holds it, so the reference is either live or already null.
    sqlx::query!(
        "INSERT INTO enquiries \
           (id, kind, call_id, chat_id, phone_number_id, owner_user_id, agent_id, \
            caller_e164, caller_name, contact, for_whom, subject, body, urgency, details) \
         SELECT $1, $2, c.id, $3, c.phone_number_id, c.owner_user_id, c.agent_id, \
                c.from_e164, $4, $5, $6, $7, $8, $9, $10 \
           FROM calls c WHERE c.id = $11",
        id,
        e.kind,
        chat_id,
        e.caller_name,
        e.contact,
        e.for_whom,
        e.subject,
        e.body,
        e.urgency,
        e.details,
        call.call_id,
    )
    .execute(&state.pg)
    .await
    .map_err(|err| {
        tracing::warn!(error = %err, call_id = %call.call_id, "could not write down a caller's message");
        "error: the message could not be written down. Apologise, and ask the caller to ring back."
            .to_string()
    })?;

    let mut ev = AuditEvent::action("telephony.enquiry.recorded", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("enquiry".into());
    ev.resource_id = Some(id);
    ev.payload = Some(json!({
        "kind": e.kind,
        "urgency": e.urgency,
        "call_id": call.call_id,
        "phone_number_id": call.phone_number_id,
        "from_e164": call.from_e164,
    }));
    let _ = audit::append(&state.pg, &ev).await;

    announce(state, call, chat_id, e.kind, &e.subject, e.urgency, id);
    // And outside the deployment, where a practice actually watches for things. Only for a
    // message: a caller being put through is already being dealt with by a person, so
    // announcing it in a channel afterwards tells nobody anything they need.
    if e.kind == "message" || e.kind == "lead" {
        let subject = e.subject.clone();
        let from = call.from_e164.clone();
        let owner = call.owner_user_id;
        let state = state.clone();
        // On its own task: a caller must never wait on somebody else's chat service, and
        // this call is still in progress.
        tokio::spawn(async move {
            super::notify::fire(&state, owner, super::notify::Event::MessageTaken, &from, &subject).await;
        });
    }
    Ok(id)
}

/// Check the caller against the practice's own list, and remember the answer.
///
/// The verdict goes on the call so that the refusal to transfer can be enforced later
/// without trusting anybody's memory of this moment, and so that whoever picks up a
/// message can see that the check was made. What is returned is a decision and an
/// instruction: never which name matched, and never anything the agent could repeat to
/// the caller as evidence that a check happened at all.
pub async fn screen(
    state: &AppState,
    ctx: &AuthContext,
    call: &CallToolCtx,
    name: &str,
    organisation: &str,
) -> String {
    // Only the reduced forms are read. The names themselves are never fetched into this
    // process, so there is nothing here for a caller to talk out of it even in principle.
    let stored = sqlx::query_scalar!(
        "SELECT normalised FROM conflict_names WHERE owner_user_id = $1",
        call.owner_user_id,
    )
    .fetch_all(&state.pg)
    .await;
    let verdict = match stored {
        Ok(list) => conflict::screen(name, organisation, &list),
        Err(e) => {
            // A check that could not be made is not a check that passed.
            tracing::warn!(error = %e, call_id = %call.call_id, "could not read the list to check against");
            conflict::Verdict::Unknown
        }
    };

    if let Err(e) = sqlx::query!(
        "UPDATE calls SET conflict_check = $2 WHERE id = $1 AND ended_at IS NULL",
        call.call_id,
        verdict.as_str(),
    )
    .execute(&state.pg)
    .await
    {
        // The answer stands for what the agent says next, but an unrecorded clear verdict
        // must not open the gate: without the row nothing later can know it was checked.
        tracing::warn!(error = %e, call_id = %call.call_id, "could not record the result of a check");
    }

    let mut ev = AuditEvent::action("telephony.conflict.screened", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("telephony".into());
    ev.resource_id = Some(call.call_id);
    if verdict != conflict::Verdict::Clear {
        ev.outcome = crate::audit::AuditOutcome::Failure;
        ev.outcome_reason = Some(verdict.as_str().to_string());
    }
    // The name the caller gave, because it is their own claim about themselves and the
    // record has to say who was checked. Never which stored name matched: that would put
    // somebody else's name in the record of this caller's telephone call.
    ev.payload = Some(json!({
        "verdict": verdict.as_str(),
        "name_given": name,
        "organisation_given": organisation,
        "from_e164": call.from_e164,
    }));
    let _ = audit::append(&state.pg, &ev).await;
    metrics::counter!("telephony_screenings_total", "verdict" => verdict.as_str().to_string())
        .increment(1);

    conflict::advice(verdict).to_string()
}

/// Hand the caller to the person this line puts callers through to.
///
/// Two steps, in this order and for one reason each. The number to ring is written to
/// the call's own row first, because what happens next arrives as a fresh request from
/// the network that knows only which call it is about, and may well arrive after the
/// process carrying that call has stopped. Only then is the end asked for, and asking is
/// all it is: the caller is still listening to the sentence that explains it, and the
/// transport waits until they have heard it.
pub async fn transfer(state: &AppState, call: &CallToolCtx) -> String {
    let Some(to) = call.transfer_e164.clone() else {
        // Not reachable by an ordinary route: a line with nowhere to transfer to never
        // offers the ability. Kept because the alternative to saying so is a caller told
        // they are being put through and then left holding a line that goes nowhere.
        return "error: this line has nowhere to put callers through to. Offer to take a \
                message instead."
            .into();
    };

    // The check, enforced here rather than asked for in the instructions.
    //
    // Read from the call row rather than remembered from earlier in the turn, and read
    // now rather than trusted from when the turn started. A practice that keeps a list has
    // decided that callers are checked before they reach a person, and a model that
    // forgets to check, or checks and ignores the answer, must not be able to defeat that.
    // Never having been checked counts as not clear, which is what makes forgetting safe.
    if call.screening_required {
        let recorded = sqlx::query_scalar!(
            "SELECT conflict_check FROM calls WHERE id = $1",
            call.call_id
        )
        .fetch_optional(&state.pg)
        .await
        .ok()
        .flatten()
        .flatten();
        let verdict = recorded.as_deref().and_then(conflict::Verdict::from_str);
        if !conflict::Verdict::lets_a_call_through(verdict) {
            return match verdict {
                // Checked, and the answer was not clear. Say nothing about why: the caller
                // must not learn that a check was made or what it found.
                Some(conflict::Verdict::Possible) => {
                    "error: this caller cannot be put through. Take a message instead, and say \
                     only that somebody will be in touch. Do not tell the caller why."
                        .into()
                }
                _ => "error: this caller has not been checked yet, so they cannot be put \
                      through. Ask for their full name and use screen_conflict first; if they \
                      will not give it, take a message instead."
                    .to_string(),
            };
        }
    }
    if let Err(e) = sqlx::query!(
        "UPDATE calls SET transfer_to = $2 WHERE id = $1 AND ended_at IS NULL",
        call.call_id,
        to,
    )
    .execute(&state.pg)
    .await
    {
        tracing::warn!(error = %e, call_id = %call.call_id, "could not record where to put the caller through");
        return "error: the caller could not be put through. Apologise, and offer to take a \
                message instead."
            .into();
    }
    if !state.telephony.ask_to_end(&call.provider_call_id, super::log::CallEnd::Transferred) {
        // The call is not being carried here any more, so nothing is left to end it. The
        // row now says a transfer was wanted and the network will be told so if it asks,
        // which it will not if the call is already over.
        tracing::warn!(call_id = %call.call_id, "asked to transfer a call this process is not carrying");
    }
    format!(
        "Putting the caller through to {}. Tell them you are connecting them now, in one short \
         sentence, and then stop talking: the call moves as soon as they have heard it. Do not \
         promise that somebody will pick up, and do not say anything else afterwards.",
        call.owner_name,
    )
}

/// Finish the call, once the caller has heard the goodbye.
pub async fn end_call(state: &AppState, call: &CallToolCtx) -> String {
    if !state.telephony.ask_to_end(&call.provider_call_id, super::log::CallEnd::Completed) {
        tracing::warn!(call_id = %call.call_id, "asked to end a call this process is not carrying");
    }
    "Say goodbye in one short sentence and then stop talking. The call ends as soon as the \
     caller has heard it, so do not ask them anything further."
        .into()
}

/// Tell the line's own team chat that something was taken, without saying what.
///
/// Spawned, because the caller is on the line and none of this is worth their silence.
/// A failure is a warning and nothing else: the record is the durable thing and it is
/// already written by the time this runs.
fn announce(
    state: &AppState,
    call: &CallToolCtx,
    chat_id: Uuid,
    kind: &str,
    subject: &str,
    urgency: &str,
    id: Uuid,
) {
    let Some(target) = call.deliver_group_chat_id else {
        return;
    };
    let state = state.clone();
    let owner = call.owner_user_id;
    let from = if call.from_e164.is_empty() { "a withheld number".to_string() } else { call.from_e164.clone() };
    let subject = subject.to_string();
    let noun = match kind {
        "message" => "message",
        "handover" => "call being put through",
        _ => "enquiry",
    };
    let mark = if urgency == "urgent" { "urgent " } else { "" };
    tokio::spawn(async move {
        // Checked again here and not only when it was chosen. Somebody may have left the
        // chat since, and a target that was theirs to pick is not a target that stays
        // theirs for ever.
        match crate::http::messaging::is_member(&state, owner, target).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                tracing::warn!(error = %e, "could not check the delivery target's membership");
                return;
            }
        }
        // A share, so the members of that chat can open the conversation the message was
        // taken during. Without it the link would be there and refuse them.
        if let Err(e) = sqlx::query!(
            "INSERT INTO chat_shares (chat_id, group_chat_id, shared_by) VALUES ($1, $2, $3) \
             ON CONFLICT (chat_id, group_chat_id) DO NOTHING",
            chat_id,
            target,
            owner,
        )
        .execute(&state.pg)
        .await
        {
            tracing::warn!(error = %e, "could not share the call with the delivery target");
            return;
        }
        // The subject and who rang, and not a word of what was said. The people in a team
        // chat are not necessarily the people entitled to read what a caller dictated,
        // and whoever is can read it where it is kept.
        let content = format!("☎ A {mark}{noun} from {from}: “{subject}”");
        if let Err(e) =
            crate::http::messaging::post_chat_link(&state, target, None, chat_id, &content, true).await
        {
            tracing::warn!(error = %e, "could not announce a caller's message");
            return;
        }
        // A hint to whatever the owner already has open, and to nobody else. Postgres is
        // the record; a frame that never arrives costs a refresh, not a message.
        tracing::debug!(%id, "announced a caller's message");
        state.hub.send_to_user(
            owner,
            crate::ws::protocol::ServerFrame::Invalidate { keys: vec![vec!["enquiries".into()]] },
        );
    });
}
