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

//! Throwing away what a line has finished needing.
//!
//! Two periods, set per line and both dormant at nought, because the words a caller said
//! and the fact that they rang are different things to a practice and are usually kept for
//! different lengths of time. A deployment that sets neither keeps everything, which is what
//! every deployment did before this existed: nothing starts deleting on its own.
//!
//! **What this does not touch, deliberately.** Messages and enquiries a caller left,
//! appointments, and the screening list are the practice's own records rather than a
//! by-product of a call, and a retention period on a telephone line has no business
//! deleting an appointment somebody is expecting to be kept. Each of those keeps a reference
//! to the call it came from, and that reference clears itself when the call goes.

use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::state::AppState;

/// What one sweep threw away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Swept {
    /// Conversations deleted from calls whose record was kept.
    pub transcripts: u64,
    /// Whole call records deleted, conversation and all.
    pub calls: u64,
    /// Recordings deleted from the disk, from calls whose record was kept.
    pub recordings: u64,
    /// Recordings on the disk with no call to belong to, taken away. Erasing an account
    /// deletes its calls in the database and cannot reach the files, so something has to.
    pub orphans: u64,
    /// Conversations that would not delete and were left alone. Reported rather than
    /// hidden: a period that is set and quietly not being honoured is worse than one that
    /// is not set at all.
    pub refused: u64,
}

/// Delete the conversations, and then the calls, that their own line has finished with.
///
/// Runs daily and is a no-op on any deployment that has set no period. Both stages walk
/// row by row rather than issuing one bulk delete, because a conversation can refuse to go
/// (something else may still point at it) and one refusal must not stop the rest.
pub async fn sweep(state: &AppState) -> Result<Swept, crate::error::AppError> {
    let mut swept = Swept::default();

    // Stage one: the words, from calls whose record is kept. `make_interval` rather than a
    // cutoff computed here, because the period belongs to the line and every line may have
    // a different one, so there is no single cutoff to compute.
    let aged = sqlx::query!(
        r#"SELECT c.id, c.chat_id AS "chat_id!"
           FROM calls c
           JOIN phone_numbers p ON p.id = c.phone_number_id
           WHERE p.transcript_days > 0
             AND c.chat_id IS NOT NULL
             AND c.ended_at IS NOT NULL
             AND c.ended_at < now() - make_interval(days => p.transcript_days)
           ORDER BY c.ended_at
           LIMIT 5000"#
    )
    .fetch_all(&state.pg)
    .await?;
    for row in aged {
        if delete_chat(state, row.chat_id).await {
            // Cleared and dated together: a log entry with no conversation should read as
            // one that was tidied away rather than one that never had anything to say.
            let done = sqlx::query!(
                "UPDATE calls SET chat_id = NULL, transcript_deleted_at = now() WHERE id = $1",
                row.id,
            )
            .execute(&state.pg)
            .await;
            match done {
                Ok(_) => swept.transcripts += 1,
                Err(e) => {
                    tracing::warn!(error = %e, call = %row.id, "deleted a conversation but could not mark the call");
                    swept.refused += 1;
                }
            }
        } else {
            swept.refused += 1;
        }
    }

    // Stage two: the record itself, which takes any conversation still attached with it.
    let expired = sqlx::query!(
        "SELECT c.id, c.chat_id
         FROM calls c
         JOIN phone_numbers p ON p.id = c.phone_number_id
         WHERE p.log_days > 0
           AND c.ended_at IS NOT NULL
           AND c.ended_at < now() - make_interval(days => p.log_days)
         ORDER BY c.ended_at
         LIMIT 5000"
    )
    .fetch_all(&state.pg)
    .await?;
    for row in expired {
        // The call row first. Anything a caller left behind refers to it and lets go of
        // that reference by itself; the conversation does not, so it is taken afterwards
        // and only if the call it belonged to has actually gone.
        let dropped = sqlx::query!("DELETE FROM calls WHERE id = $1", row.id)
            .execute(&state.pg)
            .await;
        match dropped {
            Ok(r) if r.rows_affected() > 0 => {
                swept.calls += 1;
                if let Some(chat_id) = row.chat_id {
                    if !delete_chat(state, chat_id).await {
                        swept.refused += 1;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, call = %row.id, "could not delete an aged call");
                swept.refused += 1;
            }
        }
    }

    // Stage three: the sound, from calls whose record is kept. Its own period rather than
    // the conversation's, because audio is the bulkiest and most sensitive thing a line
    // produces and is usually kept for far less time than the words.
    let old_audio = sqlx::query!(
        r#"SELECT c.id, c.recording_path AS "recording_path!"
           FROM calls c
           JOIN phone_numbers p ON p.id = c.phone_number_id
           WHERE p.recording_days > 0
             AND c.recording_path IS NOT NULL
             AND c.ended_at IS NOT NULL
             AND c.ended_at < now() - make_interval(days => p.recording_days)
           ORDER BY c.ended_at
           LIMIT 5000"#
    )
    .fetch_all(&state.pg)
    .await?;
    for row in old_audio {
        // The file first, then the row: a row cleared before the file would leave audio on
        // the disk that nothing points at and nothing would go looking for.
        remove_file(state, &row.recording_path).await;
        let cleared = sqlx::query!(
            "UPDATE calls SET recording_path = NULL, recording_bytes = NULL, \
                    recording_seconds = NULL \
             WHERE id = $1",
            row.id,
        )
        .execute(&state.pg)
        .await;
        match cleared {
            Ok(_) => swept.recordings += 1,
            Err(e) => {
                tracing::warn!(error = %e, call = %row.id, "deleted a recording but could not clear the call");
                swept.refused += 1;
            }
        }
    }

    // Stage four: recordings with nothing to belong to. Erasing an account deletes its
    // calls in the database, and a file is not a row: without this, the words would go and
    // the voice would stay, which is the opposite of what was asked for.
    swept.orphans += sweep_orphans(state).await;

    if swept != Swept::default() {
        let mut ev = AuditEvent::action("telephony.retention.swept", "system");
        ev.resource_type = Some("telephony".into());
        // The counts, not the calls. What was deleted was deleted; naming each row here
        // would keep in the trail exactly what the sweep exists to remove.
        ev.payload = Some(serde_json::json!({
            "transcripts": swept.transcripts,
            "calls": swept.calls,
            "recordings": swept.recordings,
            "orphans": swept.orphans,
            "refused": swept.refused,
        }));
        let _ = audit::append(&state.pg, &ev).await;
        tracing::info!(
            transcripts = swept.transcripts,
            calls = swept.calls,
            recordings = swept.recordings,
            orphans = swept.orphans,
            refused = swept.refused,
            "swept aged calls"
        );
    }
    Ok(swept)
}

/// Take one recording off the disk. A file already gone is the state that was wanted.
async fn remove_file(state: &AppState, relative: &str) {
    let abs = crate::storage::resolve_file(&state.boot.storage.recordings_dir, relative);
    if let Err(e) = tokio::fs::remove_file(&abs).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(error = %e, path = %abs.display(), "could not delete a call recording");
        }
    }
}

/// Remove recordings that belong to no call.
///
/// A recording is named after its call, so what is on the disk and what is in the database
/// can be compared by name alone. Anything here is the residue of a call row that has gone:
/// an erased account, a swept log, a deployment restored from a backup taken at a different
/// moment. Left alone it would be a voice recording of a member of the public that nothing
/// in the product can see, retain or delete, which is the worst state of the three.
///
/// Deliberately conservative in one way: a file whose name is not a call identifier is left
/// alone, because it is not this sweep's to judge.
async fn sweep_orphans(state: &AppState) -> u64 {
    let dir = crate::storage::resolve_dir(&state.boot.storage.recordings_dir);
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        // No directory means no recordings, which is every deployment that records nothing.
        Err(_) => return 0,
    };
    let mut taken = 0u64;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".wav") else { continue };
        let Ok(call_id) = uuid::Uuid::parse_str(stem) else { continue };
        let still_there = sqlx::query_scalar!(
            "SELECT EXISTS (SELECT 1 FROM calls WHERE id = $1 AND recording_path IS NOT NULL)",
            call_id
        )
        .fetch_one(&state.pg)
        .await
        .ok()
        .flatten()
        .unwrap_or(true); // unreadable means leave it alone
        if !still_there {
            if tokio::fs::remove_file(entry.path()).await.is_ok() {
                taken += 1;
            }
        }
    }
    taken
}

/// Delete one conversation, reporting whether it went.
///
/// A conversation can be pointed at by rows that do not give way when it is deleted, so
/// this can legitimately fail on one call while succeeding on the rest. Failing the whole
/// sweep for it would mean one stuck conversation stops a practice's retention from being
/// honoured at all, so the refusal is counted and the sweep carries on.
pub async fn delete_chat(state: &AppState, chat_id: Uuid) -> bool {
    match sqlx::query!("DELETE FROM chats WHERE id = $1", chat_id).execute(&state.pg).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(error = %e, %chat_id, "a conversation would not delete; leaving it alone");
            false
        }
    }
}
