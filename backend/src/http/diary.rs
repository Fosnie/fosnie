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

//! A practice's diary: when it is open, when it is shut, and who is coming in.
//!
//! Gated as the practice's own working arrangements rather than as line wiring. The
//! account may read and change its own, and so may an administrator of the platform;
//! whoever may register telephone numbers may not. Opening hours are how a practice runs
//! and the appointments are who its clients are, and neither is a question about which
//! numbers this deployment answers.
//!
//! Every instant crossing this boundary is an instant, in RFC 3339, as everywhere else in
//! the product. The zone belongs to the diary and is sent alongside, so a reader can show
//! the practice's own local time rather than their own.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::telephony::diary;

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

/// May this reader see and change an account's diary?
///
/// The account itself and a platform administrator, and deliberately not the permission
/// that registers telephone lines: see the note at the top of this module.
fn require(ctx: &AuthContext, owner: Uuid) -> Result<()> {
    if ctx.is_admin() || ctx.user_id == Some(owner) {
        Ok(())
    } else {
        // As not found rather than as forbidden: whose account keeps a diary is not this
        // caller's business either.
        Err(AppError::NotFound("no such diary".into()))
    }
}

#[derive(Deserialize)]
pub struct Whose {
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
}

fn whose(ctx: &AuthContext, asked: Option<Uuid>) -> Result<Uuid> {
    let owner = asked.or(ctx.user_id).ok_or_else(|| AppError::NotFound("no such diary".into()))?;
    require(ctx, owner)?;
    Ok(owner)
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Opening {
    /// 0 is Monday.
    pub weekday: i16,
    pub opens_minute: i32,
    pub closes_minute: i32,
}

#[derive(Serialize)]
pub struct DiaryOut {
    pub owner_user_id: Uuid,
    pub timezone: String,
    pub slot_minutes: i32,
    pub lead_minutes: i32,
    pub horizon_days: i32,
    pub enabled: bool,
    pub hours: Vec<Opening>,
    pub closures: Vec<ClosureOut>,
}

#[derive(Serialize)]
pub struct ClosureOut {
    /// A local calendar date, as YYYY-MM-DD.
    pub closed_on: String,
    pub note: Option<String>,
}

/// `GET /api/diary` — the diary as it stands, or nothing when none has been set up.
pub async fn get(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<Whose>,
) -> Result<Json<Option<DiaryOut>>> {
    let owner = whose(&ctx, q.owner_user_id)?;
    let Some(row) = sqlx::query!(
        "SELECT timezone, slot_minutes, lead_minutes, horizon_days, enabled \
           FROM diaries WHERE owner_user_id = $1",
        owner
    )
    .fetch_optional(&state.pg)
    .await?
    else {
        return Ok(Json(None));
    };
    let hours = sqlx::query!(
        "SELECT weekday, opens_minute, closes_minute FROM diary_hours \
          WHERE owner_user_id = $1 ORDER BY weekday, opens_minute",
        owner
    )
    .fetch_all(&state.pg)
    .await?;
    let closures = sqlx::query!(
        "SELECT closed_on, note FROM diary_closures WHERE owner_user_id = $1 ORDER BY closed_on",
        owner
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(Some(DiaryOut {
        owner_user_id: owner,
        timezone: row.timezone,
        slot_minutes: row.slot_minutes,
        lead_minutes: row.lead_minutes,
        horizon_days: row.horizon_days,
        enabled: row.enabled,
        hours: hours
            .into_iter()
            .map(|h| Opening {
                weekday: h.weekday,
                opens_minute: h.opens_minute,
                closes_minute: h.closes_minute,
            })
            .collect(),
        closures: closures
            .into_iter()
            .map(|c| ClosureOut {
                closed_on: c.closed_on.to_string(),
                note: c.note,
            })
            .collect(),
    })))
}

#[derive(Deserialize)]
pub struct SetDiary {
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
    pub timezone: String,
    pub slot_minutes: i32,
    pub lead_minutes: i32,
    pub horizon_days: i32,
    pub enabled: bool,
    pub hours: Vec<Opening>,
}

/// `PUT /api/diary` — set the whole of it, hours included.
///
/// The hours are replaced rather than merged, in one transaction with the settings: a
/// half-applied change would leave a practice open at times nobody chose, and a diary is
/// small enough that sending all of it is simpler than describing a difference.
pub async fn set(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<SetDiary>,
) -> Result<Json<serde_json::Value>> {
    let owner = whose(&ctx, body.owner_user_id)?;
    // Checked against the zone database itself, because there is no sensible fallback: a
    // diary whose zone cannot be resolved would offer nothing at all, and silently using
    // UTC instead would offer times an hour or five out with no sign of trouble.
    if diary::zone(&body.timezone).is_none() {
        return Err(AppError::Validation(format!(
            "{:?} is not a time zone this system knows. Use a name like Europe/London.",
            body.timezone
        )));
    }
    for h in &body.hours {
        if !(0..=6).contains(&h.weekday) {
            return Err(AppError::Validation("a weekday runs from 0 (Monday) to 6".into()));
        }
        if h.opens_minute < 0 || h.closes_minute > 1440 || h.closes_minute <= h.opens_minute {
            return Err(AppError::Validation(
                "an opening period must start before it ends, within one day".into(),
            ));
        }
    }
    // Overlapping periods on one day would offer the same time twice.
    for (i, a) in body.hours.iter().enumerate() {
        for b in body.hours.iter().skip(i + 1) {
            if a.weekday == b.weekday
                && a.opens_minute < b.closes_minute
                && b.opens_minute < a.closes_minute
            {
                return Err(AppError::Validation(
                    "two opening periods on one day overlap".into(),
                ));
            }
        }
    }

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO diaries \
           (owner_user_id, timezone, slot_minutes, lead_minutes, horizon_days, enabled) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (owner_user_id) DO UPDATE SET \
           timezone = EXCLUDED.timezone, slot_minutes = EXCLUDED.slot_minutes, \
           lead_minutes = EXCLUDED.lead_minutes, horizon_days = EXCLUDED.horizon_days, \
           enabled = EXCLUDED.enabled, updated_at = now()",
        owner,
        body.timezone,
        body.slot_minutes,
        body.lead_minutes,
        body.horizon_days,
        body.enabled,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM diary_hours WHERE owner_user_id = $1", owner)
        .execute(&mut *tx)
        .await?;
    for h in &body.hours {
        sqlx::query!(
            "INSERT INTO diary_hours (owner_user_id, weekday, opens_minute, closes_minute) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
            owner,
            h.weekday,
            h.opens_minute,
            h.closes_minute,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let mut ev = AuditEvent::action("telephony.diary.updated", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("diary".into());
    ev.resource_id = Some(owner);
    ev.payload = Some(serde_json::json!({
        "timezone": body.timezone,
        "slot_minutes": body.slot_minutes,
        "enabled": body.enabled,
        "periods": body.hours.len(),
    }));
    let _ = audit::append(&state.pg, &ev).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct NewClosure {
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
    /// A local calendar date, as YYYY-MM-DD.
    pub closed_on: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /api/diary/closures` — a day the practice is shut.
pub async fn add_closure(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<NewClosure>,
) -> Result<Json<serde_json::Value>> {
    let owner = whose(&ctx, body.owner_user_id)?;
    let on = parse_date(&body.closed_on)?;
    sqlx::query!(
        "INSERT INTO diary_closures (owner_user_id, closed_on, note) VALUES ($1, $2, $3) \
         ON CONFLICT (owner_user_id, closed_on) DO UPDATE SET note = EXCLUDED.note",
        owner,
        on,
        body.note.as_deref().map(str::trim).filter(|n| !n.is_empty()),
    )
    .execute(&state.pg)
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `DELETE /api/diary/closures/{date}` — open again after all.
pub async fn remove_closure(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(date): Path<String>,
    Query(q): Query<Whose>,
) -> Result<Json<serde_json::Value>> {
    let owner = whose(&ctx, q.owner_user_id)?;
    let on = parse_date(&date)?;
    sqlx::query!(
        "DELETE FROM diary_closures WHERE owner_user_id = $1 AND closed_on = $2",
        owner,
        on
    )
    .execute(&state.pg)
    .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

fn parse_date(raw: &str) -> Result<time::Date> {
    let mut it = raw.trim().splitn(3, '-');
    let parsed = (|| {
        let y: i32 = it.next()?.parse().ok()?;
        let m: u8 = it.next()?.parse().ok()?;
        let d: u8 = it.next()?.parse().ok()?;
        time::Date::from_calendar_date(y, time::Month::try_from(m).ok()?, d).ok()
    })();
    parsed.ok_or_else(|| AppError::Validation("a date is written as YYYY-MM-DD".into()))
}

#[derive(Deserialize)]
pub struct AppointmentQuery {
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
    /// Only what is still booked, rather than everything including what was cancelled.
    #[serde(default)]
    pub booked_only: Option<bool>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct AppointmentOut {
    pub id: Uuid,
    pub starts_at: String,
    pub ends_at: String,
    pub status: String,
    pub reference: String,
    pub caller_name: String,
    pub caller_e164: String,
    pub contact: Option<String>,
    pub subject: String,
    pub chat_id: Option<Uuid>,
    /// The zone the practice keeps, so a reader can show its local time rather than theirs.
    pub timezone: Option<String>,
}

/// `GET /api/diary/appointments` — who is coming in.
pub async fn appointments(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<AppointmentQuery>,
) -> Result<Json<Vec<AppointmentOut>>> {
    let owner = whose(&ctx, q.owner_user_id)?;
    let limit = q.limit.unwrap_or(200).clamp(1, 500);
    let booked_only = q.booked_only.unwrap_or(true);
    let rows = sqlx::query!(
        r#"SELECT a.id, a.starts_at, a.ends_at, a.status, a.reference, a.caller_name,
                  a.caller_e164, a.contact, a.subject, a.chat_id,
                  (SELECT d.timezone FROM diaries d WHERE d.owner_user_id = a.owner_user_id)
                        AS timezone
             FROM appointments a
            WHERE a.owner_user_id = $1
              AND (NOT $2::bool OR a.status = 'booked')
            ORDER BY a.starts_at
            LIMIT $3"#,
        owner,
        booked_only,
        limit,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| AppointmentOut {
                id: r.id,
                starts_at: rfc3339(r.starts_at),
                ends_at: rfc3339(r.ends_at),
                status: r.status,
                reference: r.reference,
                caller_name: r.caller_name,
                caller_e164: r.caller_e164,
                contact: r.contact,
                subject: r.subject,
                chat_id: r.chat_id,
                timezone: r.timezone,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct NewAppointment {
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
    /// An instant, in RFC 3339.
    pub starts_at: String,
    pub caller_name: String,
    #[serde(default)]
    pub contact: Option<String>,
    pub subject: String,
}

/// `POST /api/diary/appointments` — put somebody in by hand.
///
/// Held to the same opening hours as a caller is, because a diary that a person can write
/// outside its own hours is a diary the telephone would then offer around.
pub async fn book(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<NewAppointment>,
) -> Result<Json<serde_json::Value>> {
    let owner = whose(&ctx, body.owner_user_id)?;
    let d = crate::telephony::booking::load(&state.pg, owner)
        .await
        .ok_or_else(|| AppError::Validation("this account keeps no diary, or it is switched off".into()))?;
    let at = OffsetDateTime::parse(
        body.starts_at.trim(),
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|_| AppError::Validation("a time is written in full, as an instant".into()))?;
    if !diary::is_open_slot(&d, at) {
        return Err(AppError::Validation(
            "that is not one of this diary's own times: check the opening hours, the length of \
             an appointment, and the days the practice is shut"
                .into(),
        ));
    }
    let name = body.caller_name.trim();
    let subject = body.subject.trim();
    if name.is_empty() || subject.is_empty() {
        return Err(AppError::Validation("an appointment needs a name and a subject".into()));
    }
    let id = Uuid::now_v7();
    let done = sqlx::query!(
        "INSERT INTO appointments \
           (id, owner_user_id, starts_at, ends_at, reference, caller_name, contact, subject, \
            created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT DO NOTHING",
        id,
        owner,
        at,
        at + time::Duration::minutes(d.slot_minutes as i64),
        crate::tools::phone::reference(id),
        name,
        body.contact.as_deref().map(str::trim).filter(|c| !c.is_empty()),
        subject,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    if done == 0 {
        return Err(AppError::Conflict("that time is already taken".into()));
    }
    Ok(Json(serde_json::json!({ "id": id })))
}

/// `DELETE /api/diary/appointments/{id}` — cancel one, freeing its time.
pub async fn cancel(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let me = ctx.user_id;
    let admin = ctx.is_admin();
    let gone = sqlx::query!(
        "UPDATE appointments SET status = 'cancelled', cancelled_at = now() \
          WHERE id = $1 AND status = 'booked' AND ($2::bool OR owner_user_id = $3) \
         RETURNING owner_user_id",
        id,
        admin,
        me,
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::NotFound("no such appointment".into()))?;

    let mut ev = AuditEvent::action("telephony.appointment.cancelled", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("appointment".into());
    ev.resource_id = Some(id);
    ev.payload = Some(serde_json::json!({ "owner_user_id": gone.owner_user_id, "by": "interface" }));
    let _ = audit::append(&state.pg, &ev).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}
