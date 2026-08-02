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

//! Telling somebody outside that a line took something.
//!
//! A message taken at four in the afternoon is no use to anybody if it is only seen when
//! somebody next opens the app. Practices live in a chat service, so this posts a line into
//! one: who rang, what it is about, and a way back into this deployment for the rest.
//!
//! **What leaves is the same as what the internal announcement says, and no more.** Who
//! rang and what about, capped in length, and never a word of what was said. The people in
//! a channel are not necessarily the people entitled to read what a caller dictated, and
//! whoever is can read it where it is kept.
//!
//! **Delivery is durable, and never on the call's own path.** Deciding to notify is a row
//! written and a task queued; posting happens on the worker afterwards. Two reasons rather
//! than posting where it is decided: a caller must never wait on somebody else's chat
//! service, and an outage at that service must not lose the notice that a client rang. The
//! task queue's own backoff and dead-letter do the retrying.
//!
//! **Dormant like every other connector.** Nothing leaves until an administrator switches
//! on outward notifications, and every attempt passes the same egress gate as the rest.

use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, AuditEvent, AuditOutcome};
use crate::error::{AppError, Result};
use crate::integrations::{self, ConnectorKind};
use crate::state::AppState;

/// What happened on a line that somebody outside might want to know about.
///
/// A closed set, mirroring the values a target's event list will accept: a variant added
/// here without the interface that offers it is a notification nobody can ever subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    MessageTaken,
    AppointmentBooked,
    AppointmentMoved,
    AppointmentCancelled,
}

impl Event {
    pub fn as_str(self) -> &'static str {
        match self {
            Event::MessageTaken => "message_taken",
            Event::AppointmentBooked => "appointment_booked",
            Event::AppointmentMoved => "appointment_moved",
            Event::AppointmentCancelled => "appointment_cancelled",
        }
    }

    pub const ALL: [Event; 4] = [
        Event::MessageTaken,
        Event::AppointmentBooked,
        Event::AppointmentMoved,
        Event::AppointmentCancelled,
    ];

    /// How the line opens, so a reader can tell the four apart at a glance.
    fn opener(self) -> &'static str {
        match self {
            Event::MessageTaken => "A message was taken",
            Event::AppointmentBooked => "An appointment was booked",
            Event::AppointmentMoved => "An appointment was moved",
            Event::AppointmentCancelled => "An appointment was cancelled",
        }
    }
}

/// The longest line that will be posted, in characters.
///
/// A notification is a nudge to go and look, not the record itself. Capping it is also what
/// stops a caller dictating a paragraph into somebody's chat channel.
pub const MAX_LINE: usize = 300;

/// The one line that goes out, for every event and every kind of target.
///
/// Pure, so what leaves the deployment can be asserted in a test rather than inspected on a
/// channel afterwards. `from` is the caller's number or an empty string when withheld;
/// `detail` is the subject of a message or the time of an appointment, which is the only
/// free text here and is where the cap bites.
pub fn line(event: Event, from: &str, detail: &str) -> String {
    let who = if from.trim().is_empty() { "a withheld number".to_string() } else { from.trim().to_string() };
    let detail = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = format!("{} from {who}: {detail}", event.opener());
    if out.chars().count() > MAX_LINE {
        out = out.chars().take(MAX_LINE - 1).collect::<String>();
        out.push('…');
    }
    out
}

/// The body posted to a target, in the shape that kind of service expects.
///
/// Slack and Teams both read `text` from a posted object, which is the whole of what an
/// incoming webhook needs. Anything else gets the event named separately, so a deployment
/// routing these into its own system does not have to parse a sentence to sort them.
pub fn body(kind: &str, event: Event, text: &str) -> serde_json::Value {
    match kind {
        "slack" | "teams" => json!({ "text": text }),
        _ => json!({ "event": event.as_str(), "text": text }),
    }
}

/// Check a target's address before it is stored, and again before anything is posted to it.
///
/// The same resolution and address checks an MCP server endpoint gets, which is the point:
/// there is one implementation of "is this a safe outbound address" in this codebase and
/// this is not a second one. The wording is this module's, because a webhook is not an MCP
/// server and an operator pasting one should not be told about servers.
pub fn check_url(raw: &str) -> Result<()> {
    let url = raw.trim();
    if url.is_empty() {
        return Err(AppError::Validation("a notification target needs an address".into()));
    }
    // The same two modes the rest of the platform reaches outward in. A public service is
    // an egress surface and must be reached over https, because the address is itself a
    // credential and the line carries a caller's name. Something on the deployment's own
    // network is a different case: a practice running its own chat service on the other
    // side of the room is not sending anything out, and requiring a certificate there
    // would only push people into pasting a public address instead.
    let public = url.starts_with("https://");
    if !public && !url.starts_with("http://") {
        return Err(AppError::Validation(
            "a notification address is an https address, or an http one on this deployment's \
             own network"
                .into(),
        ));
    }
    crate::mcp::validate::validate_endpoint(url, public).map_err(|_| {
        AppError::Validation(if public {
            "that address cannot be reached from here".to_string()
        } else {
            "a plain http address has to be on this deployment's own network; anything on the \
             public internet needs https, because the address is a credential and the message \
             names a caller"
                .to_string()
        })
    })
}

/// Note that something happened, and queue a line for every target that wants it.
///
/// Never fails the thing it is reporting on: a caller's message is written down whether or
/// not anybody can be told about it, so every failure here is logged and swallowed. Returns
/// how many deliveries were queued, which is what the tests read.
pub async fn fire(state: &AppState, owner: Uuid, event: Event, from: &str, detail: &str) -> u64 {
    let targets = sqlx::query!(
        "SELECT id FROM notify_targets \
          WHERE owner_user_id = $1 AND enabled AND $2 = ANY(events)",
        owner,
        event.as_str(),
    )
    .fetch_all(&state.pg)
    .await;
    let targets = match targets {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "could not read who wanted to be told");
            return 0;
        }
    };
    let text = line(event, from, detail);
    let mut queued = 0u64;
    for t in targets {
        // The rendered line rather than the pieces: what will be posted is decided here,
        // once, so a change to the wording cannot reach a notice that was already agreed.
        let payload = json!({ "target_id": t.id, "event": event.as_str(), "text": text });
        match crate::scheduler::enqueue(&state.pg, crate::scheduler::TaskType::NotifyDeliver, payload)
            .await
        {
            Ok(_) => queued += 1,
            Err(e) => tracing::warn!(error = %e, "could not queue a notification"),
        }
    }
    queued
}

/// Post one queued line to its target.
///
/// Called from the worker. An error here is worth retrying (the queue's own backoff and
/// dead-letter take it from there); a refusal that will never succeed, such as the connector
/// being dormant or the target having been deleted, returns `Ok` so the queue stops.
pub async fn deliver(state: &AppState, payload: &serde_json::Value) -> Result<()> {
    let Some(target_id) = payload.get("target_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok())
    else {
        return Ok(()); // nothing to deliver to; not a fault worth retrying
    };
    let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let event = payload.get("event").and_then(|v| v.as_str()).unwrap_or_default().to_string();

    let Some(row) = sqlx::query!(
        "SELECT owner_user_id, kind, url_enc, enabled FROM notify_targets WHERE id = $1",
        target_id
    )
    .fetch_optional(&state.pg)
    .await?
    else {
        return Ok(()); // deleted while the notice was queued
    };
    if !row.enabled {
        return Ok(()); // switched off while the notice was queued
    }

    // On behalf of the account whose line it is, so the audit trail names somebody real
    // rather than "the system". An account that has gone is an account that notifies
    // nobody: this fails closed rather than posting for a deactivated practice.
    let ctx = match crate::auth::load_context(&state.pg, row.owner_user_id).await {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    // The gate. Dormant means nothing leaves, and the attempt is recorded as blocked.
    integrations::guard_egress(state, &ctx, ConnectorKind::Notify).await?;

    let url = crate::crypto::decrypt_at_rest(&row.url_enc)
        .map_err(|_| AppError::Config("a notification address could not be read".into()))?;
    // Checked again here and not only when it was stored: what a name resolves to is not
    // fixed, and the address is about to be used rather than merely kept.
    check_url(&url)?;

    let ev = Event::ALL.iter().copied().find(|e| e.as_str() == event).unwrap_or(Event::MessageTaken);
    let sent = state
        .http
        .post(&url)
        .json(&body(&row.kind, ev, &text))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await;

    let mut audit_ev = AuditEvent::action("telephony.notified", ctx.role.as_str());
    audit_ev.actor_user_id = ctx.user_id;
    audit_ev.resource_type = Some("notify_target".into());
    audit_ev.resource_id = Some(target_id);
    // Which target and which event, never the line itself: it names a caller, and the
    // audit trail is not where a second copy of that belongs.
    audit_ev.payload = Some(json!({ "kind": row.kind, "event": event }));

    match sent {
        Ok(resp) if resp.status().is_success() => {
            let _ = audit::append(&state.pg, &audit_ev).await;
            Ok(())
        }
        Ok(resp) => {
            let status = resp.status().as_u16();
            audit_ev.outcome = AuditOutcome::Failure;
            audit_ev.outcome_reason = Some(format!("the service answered {status}"));
            let _ = audit::append(&state.pg, &audit_ev).await;
            // A refused address will refuse every time; a server fault may not.
            if (400..500).contains(&status) {
                tracing::warn!(%target_id, status, "a notification was refused by the service");
                Ok(())
            } else {
                Err(AppError::Unavailable(format!("the notification service answered {status}")))
            }
        }
        Err(e) => {
            audit_ev.outcome = AuditOutcome::Failure;
            audit_ev.outcome_reason = Some("the service could not be reached".into());
            let _ = audit::append(&state.pg, &audit_ev).await;
            Err(AppError::Unavailable(format!("the notification service could not be reached: {e}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The event names are stored in a target's list, so they are pinned here rather than
    /// left to whatever an interface happens to send.
    #[test]
    fn the_events_are_a_closed_set_with_stable_names() {
        let names: Vec<&str> = Event::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(
            names,
            vec!["message_taken", "appointment_booked", "appointment_moved", "appointment_cancelled"]
        );
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "two events share a name");
    }

    #[test]
    fn a_line_says_who_rang_and_what_about() {
        let said = line(Event::MessageTaken, "+447700900123", "Wants a call back about the survey");
        assert_eq!(said, "A message was taken from +447700900123: Wants a call back about the survey");
        let withheld = line(Event::AppointmentBooked, "", "Tuesday the 4th of August, 2:10 in the afternoon");
        assert!(withheld.starts_with("An appointment was booked from a withheld number:"), "{withheld}");
    }

    /// The cap is what stops a caller dictating a paragraph into somebody's chat channel.
    #[test]
    fn a_long_detail_is_cut_rather_than_posted_whole() {
        let long = "x".repeat(1_000);
        let said = line(Event::MessageTaken, "+447700900123", &long);
        assert_eq!(said.chars().count(), MAX_LINE);
        assert!(said.ends_with('…'));
    }

    /// Line breaks come from a text box and would be several messages, or a broken one.
    #[test]
    fn what_a_caller_dictated_is_posted_as_one_line() {
        let said = line(Event::MessageTaken, "+447700900123", "one\n\ntwo\tthree");
        assert!(!said.contains('\n'), "{said}");
        assert!(said.ends_with("one two three"), "{said}");
    }

    #[test]
    fn each_kind_of_service_gets_the_shape_it_reads() {
        let slack = body("slack", Event::MessageTaken, "hello");
        assert_eq!(slack["text"], "hello");
        assert!(slack.get("event").is_none(), "a chat service reads text and nothing else");
        let teams = body("teams", Event::MessageTaken, "hello");
        assert_eq!(teams["text"], "hello");
        // Anything else is somebody's own system, which should not have to read a sentence
        // to work out which of the four this is.
        let other = body("webhook", Event::AppointmentCancelled, "hello");
        assert_eq!(other["event"], "appointment_cancelled");
        assert_eq!(other["text"], "hello");
    }

    /// An address is a credential and carries who rang, so a public one over plain http is
    /// refused. Something on the deployment's own network is a different matter and is the
    /// one case plain http is allowed in.
    #[test]
    fn a_public_address_over_plain_http_is_refused() {
        assert!(check_url("http://1.1.1.1/hook").is_err(), "a public http address left the perimeter");
        assert!(check_url("").is_err());
        assert!(check_url("   ").is_err());
        assert!(check_url("ftp://example.test/hook").is_err());
        // Its own network, which is not egress at all.
        assert!(check_url("http://127.0.0.1:9/hook").is_ok());
        assert!(check_url("http://10.0.0.5/hook").is_ok());
        // And the address that gives away the cloud's own credentials is refused in both
        // modes, which is the validator's rule rather than this module's.
        assert!(check_url("http://169.254.169.254/hook").is_err());
    }
}
