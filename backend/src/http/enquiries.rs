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

//! Reading what callers wanted, and marking it dealt with.
//!
//! Two ways in, for two different people. Your own list needs no permission at all,
//! because these are records addressed to your account and somebody whose telephone line
//! it is need not be an administrator of anything. The other is the delegated view, and
//! it is deliberately poorer: whoever may wire a line may see that the line took a
//! message, and not what the message says.
//!
//! That is the same refusal as the missing endpoint for reading a call's transcript, and
//! it is made in the projection rather than by leaving a field out of a response type,
//! so the words never leave the database in the first place.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::keycloak::AuthUser;
use crate::auth::{permissions, AuthContext};
use crate::error::{AppError, Result};
use crate::state::AppState;

const MAX_PAGE: i64 = 200;
const DEFAULT_PAGE: i64 = 50;

/// May this reader see what the caller actually said?
///
/// The person whose line took it, and a platform administrator, and nobody else. A
/// delegated telephony administrator is deliberately not on this list: that permission is
/// to wire a line, not to read what people say down it. Anything moved out of the
/// withheld set below needs this same argument made about it again.
pub fn may_read_body(ctx: &AuthContext, owner_user_id: Uuid) -> bool {
    ctx.is_admin() || ctx.user_id == Some(owner_user_id)
}

#[derive(Deserialize)]
pub struct EnquiryQuery {
    /// Only the ones nobody has dealt with yet.
    #[serde(default)]
    pub open: Option<bool>,
    /// Which line took it (delegated view only).
    #[serde(default)]
    pub number_id: Option<Uuid>,
    /// The last one already seen, to read the page after it.
    #[serde(default)]
    pub before: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct EnquiryOut {
    pub id: Uuid,
    pub kind: String,
    pub urgency: String,
    pub handled: bool,
    pub call_id: Option<Uuid>,
    pub chat_id: Option<Uuid>,
    pub number_id: Option<Uuid>,
    /// The number that was rung. Which line took a call is not what was said on it.
    pub to_e164: String,
    pub owner_user_id: Uuid,
    pub created_epoch: i64,
    pub handled_epoch: Option<i64>,
    // Everything below is withheld from a reader who may see that this exists and not
    // what it says. Null means withheld, and is rendered as such: it never means empty.
    pub subject: Option<String>,
    pub body: Option<String>,
    pub caller_e164: Option<String>,
    pub caller_name: Option<String>,
    pub contact: Option<String>,
    pub for_whom: Option<String>,
    pub details: Option<serde_json::Value>,
}

/// One page, under one rule, whichever door it was asked for through.
///
/// `mine_only` narrows to the reader's own; `full` is the body class. They are separate
/// because the delegated view widens who is listed without widening what is shown, and
/// folding them into one flag would make that pair impossible to express.
async fn page(
    state: &AppState,
    ctx: &AuthContext,
    q: &EnquiryQuery,
    mine_only: bool,
) -> Result<Vec<EnquiryOut>> {
    let me = ctx.user_id;
    let full = ctx.is_admin();
    let limit = q.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let rows = sqlx::query!(
        r#"SELECT e.id, e.kind, e.urgency, e.call_id, e.chat_id, e.phone_number_id,
                  e.owner_user_id,
                  (e.handled_at IS NOT NULL) AS "handled!",
                  COALESCE(c.to_e164, '') AS "to_e164!",
                  extract(epoch from e.created_at)::bigint AS "created_epoch!",
                  extract(epoch from e.handled_at)::bigint AS handled_epoch,
                  -- The body class. One predicate, per row, so a reader who owns one of
                  -- several lines sees their own words and not anybody else's in the
                  -- same page, and the withheld ones are never read out of the table.
                  CASE WHEN $1::bool OR e.owner_user_id = $2 THEN e.subject     END AS subject,
                  CASE WHEN $1::bool OR e.owner_user_id = $2 THEN e.body        END AS body,
                  CASE WHEN $1::bool OR e.owner_user_id = $2 THEN e.caller_e164 END AS caller_e164,
                  CASE WHEN $1::bool OR e.owner_user_id = $2 THEN e.caller_name END AS caller_name,
                  CASE WHEN $1::bool OR e.owner_user_id = $2 THEN e.contact     END AS contact,
                  CASE WHEN $1::bool OR e.owner_user_id = $2 THEN e.for_whom    END AS for_whom,
                  CASE WHEN $1::bool OR e.owner_user_id = $2 THEN e.details     END AS details
             FROM enquiries e
             LEFT JOIN calls c ON c.id = e.call_id
            WHERE (NOT $3::bool OR e.owner_user_id = $2)
              AND ($4::bool IS NOT TRUE OR e.handled_at IS NULL)
              AND ($5::uuid IS NULL OR c.phone_number_id = $5)
              AND ($6::uuid IS NULL OR (e.created_at, e.id) <
                   (SELECT b.created_at, b.id FROM enquiries b WHERE b.id = $6))
            ORDER BY e.created_at DESC, e.id DESC
            LIMIT $7"#,
        full,
        me,
        mine_only,
        q.open,
        q.number_id,
        q.before,
        limit,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| EnquiryOut {
            id: r.id,
            kind: r.kind,
            urgency: r.urgency,
            handled: r.handled,
            call_id: r.call_id,
            chat_id: r.chat_id,
            number_id: r.phone_number_id,
            to_e164: r.to_e164,
            owner_user_id: r.owner_user_id,
            created_epoch: r.created_epoch,
            handled_epoch: r.handled_epoch,
            subject: r.subject,
            body: r.body,
            caller_e164: r.caller_e164,
            caller_name: r.caller_name,
            contact: r.contact,
            for_whom: r.for_whom,
            details: r.details,
        })
        .collect())
}

/// `GET /api/enquiries` — what callers wanted from you.
///
/// No permission: your own messages, in the way your own remembered facts are your own.
pub async fn list_mine(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<EnquiryQuery>,
) -> Result<Json<Vec<EnquiryOut>>> {
    Ok(Json(page(&state, &ctx, &q, true).await?))
}

/// `GET /api/admin/telephony/enquiries` — what the deployment's lines have taken.
pub async fn list_all(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<EnquiryQuery>,
) -> Result<Json<Vec<EnquiryOut>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::TELEPHONY_MANAGE).await?;
    Ok(Json(page(&state, &ctx, &q, false).await?))
}

#[derive(Deserialize)]
pub struct Handled {
    pub handled: bool,
}

/// `PATCH /api/enquiries/{id}` — dealt with, or not after all.
///
/// The owner or a platform administrator. Marking somebody else's message dealt with is
/// not a wiring decision, so the permission that wires lines does not reach it: the
/// update simply matches no row, and an identifier belonging to another account is
/// answered exactly as one that never existed.
pub async fn set_handled(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<Handled>,
) -> Result<Json<serde_json::Value>> {
    let me = ctx.user_id;
    let admin = ctx.is_admin();
    let done = sqlx::query!(
        "UPDATE enquiries \
            SET handled_at = CASE WHEN $3 THEN now() ELSE NULL END, \
                handled_by = CASE WHEN $3 THEN $2 ELSE NULL END \
          WHERE id = $1 AND ($4::bool OR owner_user_id = $2)",
        id,
        me,
        body.handled,
        admin,
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    if done == 0 {
        return Err(AppError::NotFound("no such message".into()));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}
