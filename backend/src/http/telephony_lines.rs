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

//! Telephone lines, and what happened on them.
//!
//! Registering a number is the most consequential thing an administrator can do here: it
//! gives an anonymous member of the public a session running as a named person's account,
//! bounded only by the agent the line is bound to. So every change is audited with the
//! number, the account and the agent, and it is gated by a permission of its own rather
//! than by the one that configures speech engines.
//!
//! There is no endpoint here for reading what was said. A call's conversation is an
//! ordinary conversation and is read through the ordinary conversation endpoints, under
//! their own permission. So somebody who may wire a line, but who neither owns the account
//! nor administers the platform, can see that a call happened, from whom, for how long and
//! how it ended, and not a word of it. That is deliberate.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::{permissions, AuthContext};
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::telephony::log::CallEnd;
use crate::telephony::normalise_e164;

/// The most calls one page of the log will return, however many are asked for.
const MAX_PAGE: i64 = 200;
const DEFAULT_PAGE: i64 = 50;

#[derive(Serialize)]
pub struct LineOut {
    pub id: Uuid,
    pub e164: String,
    pub provider: String,
    pub owner_user_id: Uuid,
    pub owner_name: String,
    pub agent_id: Uuid,
    pub agent_name: String,
    /// How many tools the agent may use. The agent is the boundary between a caller and
    /// everything else, so this is the number that says how wide the line is.
    pub agent_tool_count: i64,
    pub label: Option<String>,
    pub greeting: Option<String>,
    /// This line's own notice, when its practice has written one. Null means the standard
    /// wording, which is what `opening` below will show.
    pub notice: Option<String>,
    /// The exact words a caller hears when this line answers, composed the way the line
    /// itself composes them. Sent so the interface shows what will be said rather than
    /// what was typed, and so nobody has to reproduce the joining rules to preview it.
    pub opening: String,
    /// After how many days this line's conversations are deleted. Nought keeps them.
    pub transcript_days: i32,
    /// After how many days the record of a call goes too. Nought keeps it.
    pub log_days: i32,
    /// Whether this line keeps the sound of its calls, and for how long. A line that
    /// records says so in what it speaks to callers, which is why the two travel together.
    pub record_calls: bool,
    pub recording_days: i32,
    pub enabled: bool,
    /// Where this line announces what it took, when it has somewhere.
    pub deliver_group_chat_id: Option<Uuid>,
    /// Where this line puts callers through to. Nothing here means it cannot.
    pub transfer_e164: Option<String>,
    /// How many names this line's account checks callers against.
    ///
    /// A count and not the names. Whether a line screens its callers is a wiring question,
    /// which is this permission's business; who is on the list is the practice's own
    /// confidential holding, which is not.
    pub screening_names: i64,
    /// The length of an appointment in this account's diary, when it keeps one that is
    /// switched on. Null means the line offers no times. A wiring fact, not a confidential
    /// one: who is coming in is read through the diary's own endpoints.
    pub diary_slot_minutes: Option<i32>,
    pub created_epoch: i64,
    pub last_call_epoch: Option<i64>,
}

/// `GET /api/admin/telephony/numbers` — every line this deployment answers.
pub async fn list_lines(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<LineOut>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TELEPHONY_MANAGE).await?;
    let rows = sqlx::query!(
        r#"SELECT p.id, p.e164, p.provider, p.owner_user_id, p.agent_id,
                  p.label, p.greeting, p.notice, p.transcript_days, p.log_days,
                  p.record_calls, p.recording_days,
                  p.enabled, p.deliver_group_chat_id, p.transfer_e164,
                  u.display_name AS owner_name,
                  a.name AS agent_name,
                  (SELECT count(*) FROM agent_tools t WHERE t.agent_id = p.agent_id) AS "agent_tool_count!",
                  (SELECT count(*) FROM conflict_names n
                     WHERE n.owner_user_id = p.owner_user_id) AS "screening_names!",
                  (SELECT d.slot_minutes FROM diaries d
                     WHERE d.owner_user_id = p.owner_user_id AND d.enabled)
                        AS diary_slot_minutes,
                  extract(epoch from p.created_at)::bigint AS "created_epoch!",
                  (SELECT extract(epoch from c.started_at)::bigint FROM calls c
                     WHERE c.phone_number_id = p.id
                     ORDER BY c.started_at DESC LIMIT 1) AS last_call_epoch
           FROM phone_numbers p
           JOIN users u ON u.id = p.owner_user_id
           JOIN agents a ON a.id = p.agent_id
           ORDER BY p.e164"#
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| LineOut {
                id: r.id,
                e164: r.e164,
                provider: r.provider,
                owner_user_id: r.owner_user_id,
                owner_name: r.owner_name,
                agent_id: r.agent_id,
                agent_name: r.agent_name,
                agent_tool_count: r.agent_tool_count,
                label: r.label,
                opening: crate::telephony::notice::opening(
                    r.greeting.as_deref(),
                    r.notice.as_deref(),
                    r.record_calls,
                ),
                record_calls: r.record_calls,
                recording_days: r.recording_days,
                greeting: r.greeting,
                notice: r.notice,
                transcript_days: r.transcript_days,
                log_days: r.log_days,
                enabled: r.enabled,
                deliver_group_chat_id: r.deliver_group_chat_id,
                transfer_e164: r.transfer_e164,
                screening_names: r.screening_names,
                diary_slot_minutes: r.diary_slot_minutes,
                created_epoch: r.created_epoch,
                last_call_epoch: r.last_call_epoch,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct NewLine {
    pub e164: String,
    pub agent_id: Uuid,
    pub owner_user_id: Uuid,
    /// What answers this line: a carrier, or the practice's own telephone system. Omitted
    /// means the carrier, which is what every line was before there was a choice.
    #[serde(default = "carrier")]
    pub provider: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub greeting: Option<String>,
    /// The practice's own notice. Omitted means the standard wording, which is where a
    /// line should start: it has been written to say the things a notice has to say.
    #[serde(default)]
    pub notice: Option<String>,
    /// Both omitted mean nought, which keeps everything. A line that started deleting
    /// what it heard without being asked to would be the wrong default by a long way.
    #[serde(default)]
    pub transcript_days: Option<i32>,
    #[serde(default)]
    pub log_days: Option<i32>,
    /// Whether this line keeps the sound of its calls, and for how long. Omitted means it
    /// does not, which is where every line starts. Switching it on changes what callers are
    /// told, and needs a period: see the check below.
    #[serde(default)]
    pub record_calls: bool,
    #[serde(default)]
    pub recording_days: Option<i32>,
    /// Omitted means off, which is the safe way round: a line that answered the moment it
    /// was created would take calls in the seconds before anybody had checked it.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub deliver_group_chat_id: Option<Uuid>,
    #[serde(default)]
    pub transfer_e164: Option<String>,
}

fn carrier() -> String {
    "twilio".to_string()
}

/// Check that a line is answered by something this deployment knows how to answer with.
///
/// Checked here as well as by the column, because the answer path compares this value
/// exactly: an unknown one is a line that resolves to nothing rather than one that
/// refuses, and the difference matters when somebody is ringing it.
fn check_provider(raw: &str) -> Result<String> {
    match raw.trim() {
        "twilio" => Ok("twilio".into()),
        "audiosocket" => Ok("audiosocket".into()),
        other => Err(AppError::Validation(format!(
            "a line is answered by a carrier or by the practice's own telephone system, not by {other:?}"
        ))),
    }
}

/// Put a transfer destination into the one form every number here is stored in.
///
/// `None` for a line that does not transfer, which is what makes the agent unable to
/// offer it. Nothing checks whose number it is: a deployment can put callers through to
/// anywhere it likes, and the check that matters is that a caller cannot choose it.
fn check_transfer(raw: Option<&str>) -> Result<Option<String>> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => normalise_e164(v).map(Some).ok_or_else(|| {
            AppError::Validation(
                "a line puts callers through to a telephone number in full international form, \
                 such as +441315550000"
                    .into(),
            )
        }),
    }
}

/// Reduce a typed notice to what will be stored, and refuse one nobody would sit through.
///
/// `None` for blank, which puts the line back on the standard wording rather than leaving
/// it with nothing to say: a line with no notice at all is not something this permits,
/// because it is the notice that makes the call lawful to take.
fn check_notice(raw: Option<&str>) -> Result<Option<String>> {
    let Some(text) = raw.map(str::trim).filter(|v| !v.is_empty()) else { return Ok(None) };
    if text.chars().count() > crate::telephony::notice::MAX_NOTICE {
        return Err(AppError::Validation(format!(
            "a notice is read out to every caller before they can speak, so it has to be short \
             enough to listen to: {} characters at most",
            crate::telephony::notice::MAX_NOTICE
        )));
    }
    Ok(Some(text.to_string()))
}

/// The longest a line may be told to keep something, in days. Ten years, which is longer
/// than any professional retention period this is likely to be set to and short enough that
/// a mistyped value is refused rather than stored.
const MAX_RETENTION_DAYS: i32 = 3650;

/// Check a retention period, and say what nought means, because nought is the default and
/// the difference between "delete nothing" and "delete immediately" matters here.
fn check_days(what: &str, raw: Option<i32>) -> Result<Option<i32>> {
    match raw {
        None => Ok(None),
        Some(d) if (0..=MAX_RETENTION_DAYS).contains(&d) => Ok(Some(d)),
        Some(_) => Err(AppError::Validation(format!(
            "{what} is a number of days between 0 and {MAX_RETENTION_DAYS}, where 0 keeps it indefinitely"
        ))),
    }
}

/// Recording and the period it is kept for are one decision, so they are checked as one.
///
/// A line cannot be set to record without saying how long the audio is kept. Every other
/// period here treats nought as "indefinitely", which is the right default for a line of
/// text and the wrong one for somebody's voice. Refused here as well as by the column,
/// because a message that says what to do is better than a constraint violation.
fn check_recording(record: bool, days: i32) -> Result<()> {
    if record && days <= 0 {
        return Err(AppError::Validation(
            "a line that records has to say how long the recordings are kept: audio is the              bulkiest and most sensitive thing a line produces, so there is no keep for ever              option for it"
                .into(),
        ));
    }
    Ok(())
}

/// Check that where a line announces what it took is somewhere its own account can see.
///
/// This is what stops the delivery address becoming a way round the rule that says the
/// person who may wire a line may not read what callers said on it. Without it, whoever
/// holds that permission could point a line at a team chat they are in and be sent the
/// subject of every message it takes. Checked here, and again when a message is actually
/// delivered, because the owner may leave the chat afterwards.
async fn check_delivery(state: &AppState, owner: Uuid, target: Option<Uuid>) -> Result<()> {
    let Some(target) = target else { return Ok(()) };
    if !crate::http::messaging::is_member(state, owner, target).await? {
        return Err(AppError::Validation(
            "messages can only be announced in a team chat the line's own account belongs to".into(),
        ));
    }
    Ok(())
}

/// Check that a line's account and agent are both usable, and say which is not.
async fn check_binding(state: &AppState, owner: Uuid, agent: Uuid) -> Result<()> {
    let owner_ok = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND deactivated_at IS NULL)",
        owner
    )
    .fetch_one(&state.pg)
    .await?
    .unwrap_or(false);
    if !owner_ok {
        return Err(AppError::Validation(
            "that account cannot answer a line: it does not exist, or it has been deactivated".into(),
        ));
    }
    let agent_ok = sqlx::query_scalar!(
        "SELECT EXISTS (SELECT 1 FROM agents WHERE id = $1 AND archived_at IS NULL)",
        agent
    )
    .fetch_one(&state.pg)
    .await?
    .unwrap_or(false);
    if !agent_ok {
        return Err(AppError::Validation(
            "that agent cannot answer a line: it does not exist, or it has been archived".into(),
        ));
    }
    Ok(())
}

async fn audit_line(
    state: &AppState,
    ctx: &AuthContext,
    action: &str,
    id: Uuid,
    payload: serde_json::Value,
) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("phone_number".into());
    ev.resource_id = Some(id);
    ev.payload = Some(payload);
    let _ = audit::append(&state.pg, &ev).await;
}

/// `POST /api/admin/telephony/numbers` — register a line.
pub async fn create_line(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<NewLine>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TELEPHONY_MANAGE).await?;
    let e164 = normalise_e164(&body.e164).ok_or_else(|| {
        AppError::Validation(
            "that is not a telephone number in full international form, such as +441315550000".into(),
        )
    })?;
    check_binding(&state, body.owner_user_id, body.agent_id).await?;
    check_delivery(&state, body.owner_user_id, body.deliver_group_chat_id).await?;
    let transfer = check_transfer(body.transfer_e164.as_deref())?;
    let provider = check_provider(&body.provider)?;
    let notice = check_notice(body.notice.as_deref())?;
    let transcript_days = check_days("how long conversations are kept", body.transcript_days)?.unwrap_or(0);
    let log_days = check_days("how long the record of a call is kept", body.log_days)?.unwrap_or(0);
    let recording_days =
        check_days("how long recordings are kept", body.recording_days)?.unwrap_or(0);
    check_recording(body.record_calls, recording_days)?;

    let taken = sqlx::query_scalar!("SELECT EXISTS (SELECT 1 FROM phone_numbers WHERE e164 = $1)", e164)
        .fetch_one(&state.pg)
        .await?
        .unwrap_or(false);
    if taken {
        return Err(AppError::Conflict("that number is already registered".into()));
    }

    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO phone_numbers \
           (id, e164, owner_user_id, agent_id, label, greeting, enabled, created_by, \
            deliver_group_chat_id, transfer_e164, notice, transcript_days, log_days, \
            provider, record_calls, recording_days) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        id,
        e164,
        body.owner_user_id,
        body.agent_id,
        body.label,
        body.greeting,
        body.enabled,
        ctx.user_id,
        body.deliver_group_chat_id,
        transfer,
        notice,
        transcript_days,
        log_days,
        provider,
        body.record_calls,
        recording_days,
    )
    .execute(&state.pg)
    .await?;

    audit_line(
        &state,
        &ctx,
        "telephony.number.created",
        id,
        serde_json::json!({
            "e164": e164,
            "owner_user_id": body.owner_user_id,
            "agent_id": body.agent_id,
            "enabled": body.enabled,
        }),
    )
    .await;
    Ok(Json(serde_json::json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct EditLine {
    #[serde(default)]
    pub e164: Option<String>,
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub greeting: Option<String>,
    /// Absent leaves the notice alone; explicitly null puts the line back on the standard
    /// wording. The two have to be told apart, or a line could never be given its
    /// wording back once it had its own.
    #[serde(default, deserialize_with = "present_even_when_null")]
    pub notice: Option<Option<String>>,
    #[serde(default)]
    pub transcript_days: Option<i32>,
    #[serde(default)]
    pub log_days: Option<i32>,
    #[serde(default)]
    pub record_calls: Option<bool>,
    #[serde(default)]
    pub recording_days: Option<i32>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Absent leaves the announcement where it is; an explicit null takes it away. The
    /// two have to be told apart, because a change that only switches a line off must not
    /// silently stop it announcing anything as well.
    #[serde(default, deserialize_with = "present_even_when_null")]
    pub deliver_group_chat_id: Option<Option<Uuid>>,
    /// Absent leaves it alone; explicitly null stops this line transferring at all.
    #[serde(default, deserialize_with = "present_even_when_null")]
    pub transfer_e164: Option<Option<String>>,
}

/// Tell "no such field" apart from "this field, set to nothing".
fn present_even_when_null<'de, D, T>(de: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(de).map(Some)
}

/// `PATCH /api/admin/telephony/numbers/{id}` — change a line.
pub async fn update_line(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<EditLine>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TELEPHONY_MANAGE).await?;
    let current = sqlx::query!(
        "SELECT e164, owner_user_id, agent_id, deliver_group_chat_id, transfer_e164, \
                notice, transcript_days, log_days, record_calls, recording_days \
           FROM phone_numbers WHERE id = $1",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::NotFound("no such line".into()))?;

    let e164 = match body.e164.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        Some(raw) => normalise_e164(raw).ok_or_else(|| {
            AppError::Validation(
                "that is not a telephone number in full international form, such as +441315550000".into(),
            )
        })?,
        None => current.e164.clone(),
    };
    let owner = body.owner_user_id.unwrap_or(current.owner_user_id);
    let agent = body.agent_id.unwrap_or(current.agent_id);
    if owner != current.owner_user_id || agent != current.agent_id {
        check_binding(&state, owner, agent).await?;
    }
    let deliver = body.deliver_group_chat_id.unwrap_or(current.deliver_group_chat_id);
    if deliver != current.deliver_group_chat_id {
        check_delivery(&state, owner, deliver).await?;
    }
    let transfer = match body.transfer_e164.as_ref() {
        Some(v) => check_transfer(v.as_deref())?,
        None => current.transfer_e164.clone(),
    };
    let notice = match body.notice.as_ref() {
        Some(v) => check_notice(v.as_deref())?,
        None => current.notice.clone(),
    };
    let transcript_days =
        check_days("how long conversations are kept", body.transcript_days)?.unwrap_or(current.transcript_days);
    let log_days =
        check_days("how long the record of a call is kept", body.log_days)?.unwrap_or(current.log_days);
    let record_calls = body.record_calls.unwrap_or(current.record_calls);
    let recording_days = check_days("how long recordings are kept", body.recording_days)?
        .unwrap_or(current.recording_days);
    check_recording(record_calls, recording_days)?;
    if e164 != current.e164 {
        let taken = sqlx::query_scalar!(
            "SELECT EXISTS (SELECT 1 FROM phone_numbers WHERE e164 = $1 AND id <> $2)",
            e164,
            id
        )
        .fetch_one(&state.pg)
        .await?
        .unwrap_or(false);
        if taken {
            return Err(AppError::Conflict("that number is already registered".into()));
        }
    }

    sqlx::query!(
        "UPDATE phone_numbers SET e164 = $2, owner_user_id = $3, agent_id = $4, \
                label = COALESCE($5, label), greeting = COALESCE($6, greeting), \
                enabled = COALESCE($7, enabled), deliver_group_chat_id = $8, \
                transfer_e164 = $9, notice = $10, transcript_days = $11, log_days = $12, \
                record_calls = $13, recording_days = $14, updated_at = now() \
         WHERE id = $1",
        id,
        e164,
        owner,
        agent,
        body.label,
        body.greeting,
        body.enabled,
        deliver,
        transfer,
        notice,
        transcript_days,
        log_days,
        record_calls,
        recording_days,
    )
    .execute(&state.pg)
    .await?;

    audit_line(
        &state,
        &ctx,
        "telephony.number.updated",
        id,
        serde_json::json!({
            "e164": e164,
            "owner_user_id": owner,
            "agent_id": agent,
            "enabled": body.enabled,
            // Where a line announces what it took decides who is shown the subject of
            // every message, so a change of address is a change worth recording.
            "deliver_group_chat_id": deliver,
            // Who a caller can be handed to is worth a line in the record for the same
            // reason as who the line runs as.
            "transfer_e164": transfer,
            // How long a caller's words are kept, and how long the fact that they rang is,
            // are decisions a practice has to be able to show it made and when.
            "transcript_days": transcript_days,
            "log_days": log_days,
            // Whether a line keeps the sound of its calls is the change most worth being
            // able to point at afterwards, because it changes what every caller is told.
            "record_calls": record_calls,
            "recording_days": recording_days,
        }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `DELETE /api/admin/telephony/numbers/{id}` — release a line.
///
/// Gone rather than marked gone. A released number must stop answering with certainty, and
/// a flag the answer path has to remember to check is exactly the sort of thing that lets a
/// caller reach an account nobody meant them to. Switching a line off is the reversible
/// form; this is the final one. The calls it took survive it.
pub async fn delete_line(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TELEPHONY_MANAGE).await?;
    let gone = sqlx::query!("DELETE FROM phone_numbers WHERE id = $1 RETURNING e164", id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::NotFound("no such line".into()))?;
    audit_line(
        &state,
        &ctx,
        "telephony.number.deleted",
        id,
        serde_json::json!({ "e164": gone.e164 }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /api/admin/telephony/check` — the same readiness list, for whoever wires lines.
///
/// The same findings the deployment's own settings screen shows, because the person who
/// registers a number is usually not the person who configured the carrier, and telling
/// them what is wrong is what stops a line being blamed for a deployment's fault. Nothing
/// here is a secret: whether a credential is stored, never any part of one.
pub async fn check(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<crate::telephony::preflight::Check>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TELEPHONY_MANAGE).await?;
    Ok(Json(crate::telephony::preflight::run(&state).await))
}

#[derive(Deserialize)]
pub struct CallsQuery {
    #[serde(default)]
    pub number_id: Option<Uuid>,
    #[serde(default)]
    pub outcome: Option<String>,
    /// The last call already seen, to read the page after it.
    #[serde(default)]
    pub before: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct CallOut {
    pub id: Uuid,
    pub number_id: Option<Uuid>,
    pub to_e164: String,
    /// Empty when the caller withheld their number.
    pub from_e164: String,
    pub owner_user_id: Uuid,
    pub owner_name: String,
    pub agent_id: Option<Uuid>,
    pub agent_name: Option<String>,
    /// The conversation, when the caller said something. `None` when nobody spoke.
    pub chat_id: Option<Uuid>,
    pub outcome: String,
    /// What checking the caller against the account's own list concluded, if it was done.
    /// The verdict only: never which name it resembled.
    pub conflict_check: Option<String>,
    /// When this caller was told what they were speaking to. Absent on a call that ended
    /// before the notice got out, which is the one thing a compliance reader is looking
    /// for in this log.
    pub notice_epoch: Option<i64>,
    /// When the conversation was deleted, by a retention period or by hand.
    pub transcript_deleted_epoch: Option<i64>,
    /// How long the recording of this call is and how big, when there is one. Null means
    /// there is none, which is every call on a line that does not record.
    pub recording_seconds: Option<i32>,
    pub recording_bytes: Option<i64>,
    /// The line was set to record and no audio came of it. Different from having none.
    pub recording_failed: bool,
    pub started_epoch: i64,
    pub ended_epoch: Option<i64>,
    pub seconds: Option<i64>,
}

/// `GET /api/admin/telephony/calls` — what happened on the lines.
pub async fn list_calls(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<CallsQuery>,
) -> Result<Json<Vec<CallOut>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TELEPHONY_MANAGE).await?;
    // An outcome nobody records would otherwise read as an empty log, which looks like a
    // quiet line rather than a mistyped filter.
    if let Some(wanted) = q.outcome.as_deref() {
        let known = CallEnd::ALL.iter().any(|e| e.as_str() == wanted) || wanted == CallEnd::IN_PROGRESS;
        if !known {
            return Err(AppError::Validation(format!("no call ever ends as {wanted:?}")));
        }
    }
    let limit = q.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let rows = sqlx::query!(
        r#"SELECT c.id, c.phone_number_id, c.to_e164, c.from_e164, c.owner_user_id,
                  c.agent_id, c.chat_id, c.outcome, c.conflict_check,
                  (c.recording_path IS NOT NULL) AS "has_recording!",
                  c.recording_seconds, c.recording_bytes, c.recording_failed,
                  extract(epoch from c.notice_at)::bigint AS notice_epoch,
                  extract(epoch from c.transcript_deleted_at)::bigint AS transcript_deleted_epoch,
                  u.display_name AS owner_name,
                  a.name AS "agent_name?",
                  extract(epoch from c.started_at)::bigint AS "started_epoch!",
                  extract(epoch from c.ended_at)::bigint AS ended_epoch,
                  extract(epoch from (c.ended_at - c.started_at))::bigint AS seconds
           FROM calls c
           JOIN users u ON u.id = c.owner_user_id
           LEFT JOIN agents a ON a.id = c.agent_id
           WHERE ($1::uuid IS NULL OR c.phone_number_id = $1)
             AND ($2::text IS NULL OR c.outcome = $2)
             AND ($3::uuid IS NULL OR (c.started_at, c.id) <
                  (SELECT b.started_at, b.id FROM calls b WHERE b.id = $3))
           ORDER BY c.started_at DESC, c.id DESC
           LIMIT $4"#,
        q.number_id,
        q.outcome,
        q.before,
        limit,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| CallOut {
                id: r.id,
                number_id: r.phone_number_id,
                to_e164: r.to_e164,
                from_e164: r.from_e164,
                owner_user_id: r.owner_user_id,
                owner_name: r.owner_name,
                agent_id: r.agent_id,
                agent_name: r.agent_name,
                chat_id: r.chat_id,
                outcome: r.outcome,
                conflict_check: r.conflict_check,
                recording_seconds: if r.has_recording { r.recording_seconds } else { None },
                recording_bytes: if r.has_recording { r.recording_bytes } else { None },
                recording_failed: r.recording_failed,
                notice_epoch: r.notice_epoch,
                transcript_deleted_epoch: r.transcript_deleted_epoch,
                started_epoch: r.started_epoch,
                ended_epoch: r.ended_epoch,
                seconds: r.seconds,
            })
            .collect(),
    ))
}
