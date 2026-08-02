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

//! The record of what happened on a line.
//!
//! Only answered calls are here. A refused call was never picked up, so it was never a
//! call: those are in the audit trail with the reason, which answers a different question
//! ("why is my line not answering?") from the one this answers ("what happened on my
//! line?").
//!
//! A row is opened when the carrier's media socket is accepted, because that is the
//! moment the call is answered and starts costing the caller money, and closed exactly
//! once however the call ends. Closing is idempotent, which is what lets a socket, a
//! carrier's end-of-call notice and a sweep at startup all reach for the same row without
//! racing each other.

use uuid::Uuid;

use crate::error::Result;

/// How a call ended.
///
/// A closed set, mirroring the values the row will accept: a variant added here without
/// the migration that permits it fails a test rather than a write, at the end of somebody's
/// telephone call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEnd {
    /// The caller or the carrier hung up, or the socket closed cleanly.
    Completed,
    /// The carrier told us separately that the call was over.
    CarrierEnded,
    /// The audio stopped arriving, or arrived broken, without anybody saying goodbye.
    Dropped,
    /// The socket opened but the call never really started: it closed first, or offered
    /// audio this line cannot carry, or named a different call.
    NoMedia,
    /// The deployment was already carrying as many calls as it will.
    LineFull,
    /// The caller was put through to somebody else, and the network took the call on.
    Transferred,
    /// The caller could not be told what they were speaking to, so they were not listened
    /// to. Its own outcome rather than one of the failures above, because an operator
    /// looking at a line that answers and hangs up needs to know it is the synthesiser
    /// and not the network.
    NoticeFailed,
}

impl CallEnd {
    pub fn as_str(self) -> &'static str {
        match self {
            CallEnd::Completed => "completed",
            CallEnd::CarrierEnded => "carrier_ended",
            CallEnd::Dropped => "dropped",
            CallEnd::NoMedia => "no_media",
            CallEnd::LineFull => "line_full",
            CallEnd::Transferred => "transferred",
            CallEnd::NoticeFailed => "notice_failed",
        }
    }

    /// Every value a finished call can hold, plus the one an unfinished call holds.
    /// Pinned against the migration by a test.
    pub const ALL: [CallEnd; 7] = [
        CallEnd::Completed,
        CallEnd::CarrierEnded,
        CallEnd::Dropped,
        CallEnd::NoMedia,
        CallEnd::LineFull,
        CallEnd::Transferred,
        CallEnd::NoticeFailed,
    ];
    pub const IN_PROGRESS: &'static str = "in_progress";
}

/// Open a row for a call now being answered.
///
/// Idempotent on the carrier's own identifier, so a retried notice yields the row that
/// already exists rather than a second one. The conflict clause updates nothing that
/// matters; it is there because a clause that did nothing at all would return no row, and
/// the caller needs the identifier either way.
pub async fn open(
    pg: &sqlx::PgPool,
    details: &super::CallDetails,
    provider: &str,
) -> Result<Uuid> {
    let row = sqlx::query!(
        r#"INSERT INTO calls
             (id, phone_number_id, provider, provider_call_id, to_e164, from_e164,
              owner_user_id, agent_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           ON CONFLICT (provider, provider_call_id)
             DO UPDATE SET started_at = calls.started_at
           RETURNING id"#,
        Uuid::now_v7(),
        details.phone_number_id,
        provider,
        details.call_sid,
        details.to,
        details.from,
        details.owner_user_id,
        details.agent_id,
    )
    .fetch_one(pg)
    .await?;
    Ok(row.id)
}

/// Record that this caller was told what they were speaking to, and in what words.
///
/// The words rather than a flag. A line's notice can be edited any time, and the question
/// a complaint asks is what was said on that call, so what was said is written down beside
/// the call it was said on. Failing to record it does not fail the call: the caller has
/// already heard it, and hanging up on somebody who has been properly informed would be a
/// worse answer to a database that is briefly unavailable.
pub async fn record_notice(pg: &sqlx::PgPool, id: Uuid, spoken: &str) {
    let done = sqlx::query!(
        "UPDATE calls SET notice_at = now(), notice_text = $2 WHERE id = $1 AND notice_at IS NULL",
        id,
        spoken,
    )
    .execute(pg)
    .await;
    if let Err(e) = done {
        tracing::warn!(error = %e, %id, "could not record what the caller was told");
    }
}

/// Note what became of a call's recording.
///
/// Written after the file has been finished and every handle dropped, so what is stored is
/// what is on the disk rather than what was hoped for. A recording that failed is recorded
/// as having failed rather than as absent: "there is none" and "it did not work" are
/// different answers to somebody asking to hear one.
pub async fn record_recording(
    pg: &sqlx::PgPool,
    id: Uuid,
    done: &crate::voice::telephony::record::Finished,
) {
    let written = if done.failed {
        sqlx::query!(
            "UPDATE calls SET recording_failed = true WHERE id = $1",
            id,
        )
        .execute(pg)
        .await
    } else {
        sqlx::query!(
            "UPDATE calls SET recording_path = $2, recording_bytes = $3, recording_seconds = $4              WHERE id = $1",
            id,
            done.path,
            done.bytes as i64,
            done.seconds as i32,
        )
        .execute(pg)
        .await
    };
    if let Err(e) = written {
        tracing::warn!(error = %e, %id, "could not record what became of a call's recording");
    }
}

/// Close a call, once.
///
/// The guard on the end time is what makes this safe to call from more than one place:
/// whichever gets there first records how the call ended, and the others change nothing.
pub async fn close(pg: &sqlx::PgPool, id: Uuid, end: CallEnd, chat_id: Option<Uuid>) {
    let done = sqlx::query!(
        "UPDATE calls SET outcome = $2, ended_at = now(), chat_id = COALESCE($3, chat_id) \
         WHERE id = $1 AND ended_at IS NULL",
        id,
        end.as_str(),
        chat_id,
    )
    .execute(pg)
    .await;
    if let Err(e) = done {
        // Losing the record is not a reason to fail the teardown, which still has a socket
        // to close and a slot to give back. The sweep at the next start finds it.
        tracing::warn!(error = %e, %id, "could not record how a call ended");
    }
}

/// Close a call by the carrier's own identifier, for when nothing in this process is
/// carrying it any more. Returns whether a row was actually still open.
pub async fn close_by_provider_id(
    pg: &sqlx::PgPool,
    provider: &str,
    provider_call_id: &str,
    end: CallEnd,
) -> bool {
    sqlx::query!(
        "UPDATE calls SET outcome = $3, ended_at = now() \
         WHERE provider = $1 AND provider_call_id = $2 AND ended_at IS NULL",
        provider,
        provider_call_id,
        end.as_str(),
    )
    .execute(pg)
    .await
    .map(|r| r.rows_affected() > 0)
    .unwrap_or(false)
}

/// How long a call may have been open before a start is entitled to declare it over.
///
/// Only here so that a second instance behind the same carrier cannot close a call the
/// first one is still carrying. Comfortably longer than any call this deployment would
/// take, given how few it carries at once. Written into the statement below as a literal
/// rather than composed into it: a query built by formatting is a query nothing checks.
pub const STALE_AFTER_MINUTES: i64 = 15;

/// Close the calls a stopped process left open.
///
/// An open row means "this process is carrying that call", and what carries it is held in
/// memory, so after a start nothing can be. Left alone they would sit as though live for
/// ever, and a log where finished calls look unfinished is worse than no log.
pub async fn reconcile_open_calls(pg: &sqlx::PgPool) {
    let swept = sqlx::query!(
        "UPDATE calls SET outcome = 'dropped', ended_at = now() \
         WHERE ended_at IS NULL AND started_at < now() - interval '15 minutes'"
    )
    .execute(pg)
    .await;
    match swept {
        Ok(r) if r.rows_affected() > 0 => {
            tracing::info!(calls = r.rows_affected(), "closed calls left open by an earlier run")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "could not close calls left open by an earlier run"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strings a finished call is recorded with. They are the values the row permits,
    /// so a variant renamed here without the migration that allows it would fail at the
    /// end of a real call rather than in a test.
    #[test]
    fn every_ending_is_a_value_the_row_accepts() {
        let written: Vec<&str> = CallEnd::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(
            written,
            vec![
                "completed",
                "carrier_ended",
                "dropped",
                "no_media",
                "line_full",
                "transferred",
                "notice_failed"
            ]
        );
        assert_eq!(CallEnd::IN_PROGRESS, "in_progress");
        let mut sorted = written.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), written.len(), "two endings share a value");
        assert!(!written.contains(&CallEnd::IN_PROGRESS), "an ending cannot mean unfinished");
    }

    /// The sweep's age bound is documented next to a statement that spells it out, so the
    /// two are pinned together: changing one without the other would either close live
    /// calls or leave dead ones open.
    #[test]
    fn the_sweep_waits_longer_than_any_call_would_last() {
        assert_eq!(STALE_AFTER_MINUTES, 15);
    }
}
