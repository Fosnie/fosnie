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

//! What the telephone lines do with what they are told, stated in one place.
//!
//! An assessment of how a telephone line handles personal information needs facts that are
//! spread across several tables: what each line says to a caller, what it can do with what
//! it hears, how long each part is kept, and what leaves the deployment. Written down by
//! hand, that assessment is out of date the first time somebody changes a setting. So it is
//! assembled from the settings themselves, every time it is read.
//!
//! Gated as the practice's own business, like the screening list and the diary: the account
//! whose lines they are, and an administrator of the platform. Deliberately not the
//! permission that registers numbers, and refused as not found rather than as forbidden,
//! because whether an account answers a telephone at all is not a stranger's business.
//!
//! Deleting a conversation lives here too, rather than beside the call log, for the same
//! reason: the log is wiring, and this is the practice deciding what it keeps.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::telephony::notice;

/// The account itself, or an administrator of the platform.
fn require(ctx: &AuthContext, owner_user_id: Uuid) -> Result<()> {
    if ctx.is_admin() || ctx.user_id == Some(owner_user_id) {
        Ok(())
    } else {
        Err(AppError::NotFound("no such record".into()))
    }
}

#[derive(Deserialize)]
pub struct WhoseRecord {
    /// The account this is about. Omitted means the reader's own.
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
}

/// One line, as the assessment describes it.
#[derive(Serialize)]
pub struct LineRecord {
    pub id: Uuid,
    pub e164: String,
    pub label: Option<String>,
    pub enabled: bool,
    /// The exact words a caller hears before anything they say is acted on.
    pub spoken_to_callers: String,
    /// Whether this line is using the standard notice or wording of its own.
    pub notice_is_standard: bool,
    /// Whether a caller can ask to be put through to a person, and where to.
    pub transfers_to: Option<String>,
    /// Whether what a caller leaves is announced in a team conversation.
    pub announces_to_team: bool,
    /// After how many days conversations on this line are deleted. Nought keeps them.
    pub transcript_days: i32,
    /// After how many days the record of a call on this line is deleted too.
    pub log_days: i32,
    /// Whether the sound of a call is kept on this line, and for how long. The one fact in
    /// this record that a reader is most likely to have assumed the answer to.
    pub records_calls: bool,
    pub recording_days: i32,
    /// How many calls this line has taken, and how many of those got the notice out. The
    /// two being equal is the whole claim this record makes about consent.
    pub calls: i64,
    pub calls_with_notice: i64,
}

/// What an account holds because it answers a telephone.
#[derive(Serialize)]
pub struct HoldingRecord {
    /// What the category is, in the words a person would use for it.
    pub held: &'static str,
    /// What is in it.
    pub contents: &'static str,
    /// How long it is kept, in the words that are true of this deployment.
    pub kept: String,
    /// How many there are now.
    pub rows: i64,
}

#[derive(Serialize)]
pub struct ComplianceRecord {
    pub owner_user_id: Uuid,
    pub lines: Vec<LineRecord>,
    pub holdings: Vec<HoldingRecord>,
    /// The two facts about this platform that nothing in a settings page implies and every
    /// assessment asks for first.
    pub no_audio_is_kept: bool,
    pub leaves_the_deployment: Vec<&'static str>,
    /// Whether callers are checked against a list the practice keeps, and how many names
    /// are on it. A count, never the names.
    pub screening_names: i64,
    /// Whether an appointment can be arranged by telephone.
    pub diary_enabled: bool,
    /// When this was read, so a printed copy says what it was true of.
    pub as_at_epoch: i64,
}

/// `GET /api/telephony/compliance` — what these lines do with what they are told.
pub async fn record(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<WhoseRecord>,
) -> Result<Json<ComplianceRecord>> {
    let owner = q
        .owner_user_id
        .or(ctx.user_id)
        .ok_or_else(|| AppError::NotFound("no such record".into()))?;
    require(&ctx, owner)?;

    let lines = sqlx::query!(
        r#"SELECT p.id, p.e164, p.label, p.enabled, p.greeting, p.notice,
                  p.transfer_e164, p.deliver_group_chat_id,
                  p.transcript_days, p.log_days, p.record_calls, p.recording_days,
                  (SELECT count(*) FROM calls c WHERE c.phone_number_id = p.id) AS "calls!",
                  (SELECT count(*) FROM calls c
                     WHERE c.phone_number_id = p.id AND c.notice_at IS NOT NULL) AS "with_notice!"
             FROM phone_numbers p
            WHERE p.owner_user_id = $1
            ORDER BY p.e164"#,
        owner
    )
    .fetch_all(&state.pg)
    .await?;

    let lines: Vec<LineRecord> = lines
        .into_iter()
        .map(|r| LineRecord {
            id: r.id,
            e164: r.e164,
            label: r.label,
            enabled: r.enabled,
            spoken_to_callers: notice::opening(
                r.greeting.as_deref(),
                r.notice.as_deref(),
                r.record_calls,
            ),
            records_calls: r.record_calls,
            recording_days: r.recording_days,
            notice_is_standard: r.notice.as_deref().map(str::trim).unwrap_or_default().is_empty(),
            transfers_to: r.transfer_e164,
            announces_to_team: r.deliver_group_chat_id.is_some(),
            transcript_days: r.transcript_days,
            log_days: r.log_days,
            calls: r.calls,
            calls_with_notice: r.with_notice,
        })
        .collect();

    // The periods actually in force, taken from the lines rather than stated in general:
    // one account can hold two lines that keep things for different lengths of time, and an
    // assessment that quoted only one of them would be wrong about the other.
    let transcript_days: Vec<i32> = lines.iter().map(|l| l.transcript_days).collect();
    let log_days: Vec<i32> = lines.iter().map(|l| l.log_days).collect();
    // Whether any line records at all, and for how long those that do keep the sound. This
    // is the fact a reader is most likely to have assumed the answer to, so it is computed
    // from the lines rather than stated.
    let recording_lines: Vec<&LineRecord> = lines.iter().filter(|l| l.records_calls).collect();
    let records_anything = !recording_lines.is_empty();
    let recording_days: Vec<i32> = recording_lines.iter().map(|l| l.recording_days).collect();

    let counts = sqlx::query!(
        r#"SELECT
             (SELECT count(*) FROM calls        WHERE owner_user_id = $1) AS "calls!",
             (SELECT count(*) FROM calls        WHERE owner_user_id = $1
                                                 AND chat_id IS NOT NULL) AS "transcripts!",
             (SELECT count(*) FROM enquiries    WHERE owner_user_id = $1) AS "enquiries!",
             (SELECT count(*) FROM appointments WHERE owner_user_id = $1
                                                 AND status = 'booked')   AS "appointments!",
             (SELECT count(*) FROM conflict_names WHERE owner_user_id = $1) AS "names!",
             (SELECT count(*) FROM calls        WHERE owner_user_id = $1
                                                 AND recording_path IS NOT NULL) AS "recordings!",
             (SELECT count(*) FROM diaries      WHERE owner_user_id = $1
                                                 AND enabled)             AS "diaries!""#,
        owner
    )
    .fetch_one(&state.pg)
    .await?;

    let holdings = vec![
        HoldingRecord {
            held: "Call records",
            contents: "The number that rang, the number it rang, when, for how long, how the \
                       call ended, and whether the caller was told what they were speaking to.",
            kept: period(&log_days),
            rows: counts.calls,
        },
        HoldingRecord {
            held: "Conversations",
            contents: "What the caller said, as text, and what the line said back. No audio.",
            kept: period(&transcript_days),
            rows: counts.transcripts,
        },
        HoldingRecord {
            held: "Recordings",
            contents: "The sound of the call, both sides, on the lines that record and only                        those. Callers on a recording line are told so before they say anything.",
            kept: if records_anything {
                period(&recording_days)
            } else {
                "Nothing is recorded: no line keeps audio.".to_string()
            },
            rows: counts.recordings,
        },
        HoldingRecord {
            held: "Messages and enquiries",
            contents: "What a caller asked to be passed on: their name, how to reach them, and \
                       what it is about.",
            kept: "Until deleted by hand. A retention period set on a line does not remove \
                   these: they are the practice's own record of somebody asking for something."
                .to_string(),
            rows: counts.enquiries,
        },
        HoldingRecord {
            held: "Appointments",
            contents: "Who is coming in and when, the reference they were given, and how to \
                       reach them.",
            kept: "Until cancelled or deleted by hand.".to_string(),
            rows: counts.appointments,
        },
        HoldingRecord {
            held: "Screening list",
            contents: "The names this account checks callers against before a caller is put \
                       through or given an appointment.",
            kept: "Until deleted by hand. Entered by the practice, not collected from callers."
                .to_string(),
            rows: counts.names,
        },
    ];

    Ok(Json(ComplianceRecord {
        owner_user_id: owner,
        lines,
        holdings,
        // Computed rather than declared, because it stopped being always true the day a
        // line could record. On a deployment where no line records it means exactly what it
        // used to: speech is recognised as it arrives and the samples are discarded, and
        // there is no recording anywhere to disclose, produce or lose.
        no_audio_is_kept: !records_anything,
        leaves_the_deployment: vec![
            "The telephone call itself, which is carried by the telephone network.",
            "Nothing else. Recognition, synthesis and the assistant all run against the \
             services this deployment is configured with.",
        ],
        screening_names: counts.names,
        diary_enabled: counts.diaries > 0,
        as_at_epoch: time::OffsetDateTime::now_utc().unix_timestamp(),
    }))
}

/// How long something is kept, given what the lines actually say.
///
/// Plain words rather than a number, because nought does not mean "no time at all" here and
/// a table of days with a nought in it reads as exactly the wrong thing.
fn period(days: &[i32]) -> String {
    let set: Vec<i32> = {
        let mut d: Vec<i32> = days.iter().copied().collect();
        d.sort_unstable();
        d.dedup();
        d
    };
    match set.as_slice() {
        [] => "No line is registered.".to_string(),
        [0] => "Indefinitely: no retention period is set.".to_string(),
        [d] => format!("{d} days after the call ends."),
        many => {
            let spelt: Vec<String> = many
                .iter()
                .map(|d| if *d == 0 { "indefinitely".to_string() } else { format!("{d} days") })
                .collect();
            format!("Per line: {}.", spelt.join(", "))
        }
    }
}

/// `DELETE /api/telephony/calls/{id}/transcript` — throw away what was said on one call.
///
/// For the moment somebody asks for their information to be removed, which will not wait
/// for the nightly sweep. The record of the call survives: that the call happened, from what
/// number and for how long is the practice's own record and is not what was asked about.
pub async fn delete_transcript(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let call = sqlx::query!(
        "SELECT owner_user_id, chat_id, recording_path FROM calls WHERE id = $1",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::NotFound("no such call".into()))?;
    // Whose call it is decides who may do this, and a reader who may not is told the same
    // thing they would be told about a call that does not exist.
    if !(ctx.is_admin() || ctx.user_id == Some(call.owner_user_id)) {
        return Err(AppError::NotFound("no such call".into()));
    }

    // A call with nothing to delete is not an error: somebody asking for a conversation to
    // be removed and being told it already has been is the answer they wanted.
    if let Some(chat_id) = call.chat_id {
        if !crate::telephony::retention::delete_chat(&state, chat_id).await {
            return Err(AppError::Conflict(
                "that conversation could not be deleted because something else still refers to it"
                    .into(),
            ));
        }
    }
    // And the sound of it, where there is any. Somebody asking for what they said to be
    // removed does not mean the text only, and leaving the audio behind would be the one
    // way of honouring that request that honours none of it.
    let had_recording = call.recording_path.is_some();
    if let Some(path) = call.recording_path.as_deref() {
        remove_recording(&state, path).await;
    }
    sqlx::query!(
        "UPDATE calls SET chat_id = NULL, recording_path = NULL, recording_bytes = NULL, \
                recording_seconds = NULL, \
                transcript_deleted_at = COALESCE(transcript_deleted_at, now()) \
         WHERE id = $1",
        id
    )
    .execute(&state.pg)
    .await?;

    let mut ev = AuditEvent::action("telephony.transcript.deleted", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("call".into());
    ev.resource_id = Some(id);
    // That it went, not what was in it.
    ev.payload = Some(serde_json::json!({
        "had_conversation": call.chat_id.is_some(),
        "had_recording": had_recording,
    }));
    let _ = audit::append(&state.pg, &ev).await;

    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Take a recording off the disk, wherever this installation keeps them.
///
/// Best effort by design: a file that has already gone is the state that was wanted, and one
/// that will not go is logged rather than allowed to stop the row being cleared. The nightly
/// sweep collects anything left behind.
async fn remove_recording(state: &AppState, relative: &str) {
    let abs = crate::storage::resolve_file(&state.boot.storage.recordings_dir, relative);
    if let Err(e) = tokio::fs::remove_file(&abs).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, path = %abs.display(), "could not delete a call recording");
        }
    }
}

/// `GET /api/telephony/calls/{id}/recording` — listen to what was said.
///
/// Gated exactly as the transcript is, and **audited every time**: listening to a recording
/// of a member of the public is an act rather than a page view, and the trail should say who
/// did it and when. Served as ordinary samples rather than as it is stored, because a
/// companded file is not something every player will open.
pub async fn play_recording(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response> {
    use axum::response::IntoResponse;
    let call = sqlx::query!(
        "SELECT owner_user_id, recording_path, recording_failed FROM calls WHERE id = $1",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::NotFound("no such call".into()))?;
    if !(ctx.is_admin() || ctx.user_id == Some(call.owner_user_id)) {
        return Err(AppError::NotFound("no such call".into()));
    }
    let Some(relative) = call.recording_path else {
        return Err(AppError::NotFound(if call.recording_failed {
            "that call was to be recorded and the recording did not survive".into()
        } else {
            "that call has no recording".into()
        }));
    };
    let abs = crate::storage::resolve_file(&state.boot.storage.recordings_dir, &relative);
    // The row says there is one and the disk says otherwise. Reported as absent rather than
    // as a fault, because absent is what it is from the reader's side.
    let stored = tokio::fs::read(&abs)
        .await
        .map_err(|_| AppError::NotFound("that recording is no longer on this deployment".into()))?;
    let wav = crate::voice::telephony::record::to_pcm_wav(&stored)
        .ok_or_else(|| AppError::Conflict("that recording cannot be read".into()))?;

    let mut ev = AuditEvent::action("telephony.recording.played", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("call".into());
    ev.resource_id = Some(id);
    ev.payload = Some(serde_json::json!({ "bytes": wav.len() }));
    let _ = audit::append(&state.pg, &ev).await;

    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "audio/wav"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        wav,
    )
        .into_response())
}

/// `DELETE /api/telephony/calls/{id}/recording` — remove the sound and keep the rest.
///
/// Separate from deleting the conversation, because the two are asked for separately: a
/// practice that wants the words kept and the voice gone is being careful rather than
/// inconsistent.
pub async fn delete_recording(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let call = sqlx::query!("SELECT owner_user_id, recording_path FROM calls WHERE id = $1", id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::NotFound("no such call".into()))?;
    if !(ctx.is_admin() || ctx.user_id == Some(call.owner_user_id)) {
        return Err(AppError::NotFound("no such call".into()));
    }
    let had = call.recording_path.is_some();
    if let Some(path) = call.recording_path.as_deref() {
        remove_recording(&state, path).await;
    }
    sqlx::query!(
        "UPDATE calls SET recording_path = NULL, recording_bytes = NULL, \
                recording_seconds = NULL \
         WHERE id = $1",
        id
    )
    .execute(&state.pg)
    .await?;

    let mut ev = AuditEvent::action("telephony.recording.deleted", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("call".into());
    ev.resource_id = Some(id);
    ev.payload = Some(serde_json::json!({ "had_recording": had }));
    let _ = audit::append(&state.pg, &ev).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nought is a length of time nobody would read correctly as a number, so it is words.
    #[test]
    fn a_period_is_stated_in_words_a_reader_can_act_on() {
        assert_eq!(period(&[]), "No line is registered.");
        assert_eq!(period(&[0]), "Indefinitely: no retention period is set.");
        assert_eq!(period(&[0, 0]), "Indefinitely: no retention period is set.");
        assert_eq!(period(&[90]), "90 days after the call ends.");
        assert_eq!(period(&[90, 90]), "90 days after the call ends.");
        // Two lines that disagree: both are stated, because quoting one would be wrong
        // about the other.
        assert_eq!(period(&[0, 90]), "Per line: indefinitely, 90 days.");
        assert_eq!(period(&[365, 90]), "Per line: 90 days, 365 days.");
    }
}
