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

//! The list of names a telephone line checks callers against.
//!
//! Gated as the practice's own confidential holding, not as line wiring. The account the
//! list belongs to may read and write it, and so may an administrator of the platform, who
//! can read everything anyway. The permission that registers telephone numbers cannot:
//! that permission exists to decide which numbers this deployment answers and whose
//! account they run as, and this is a list of the practice's clients and the parties they
//! are dealing with. It is the same refusal made about what callers say on a call, for the
//! same reason.
//!
//! Names are entered in bulk, because a real list arrives pasted out of a practice system
//! rather than typed one at a time.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::telephony::conflict;

/// The most names one request may add. A practice's list is thousands of names, and a
/// paste of more than this is a mistake worth refusing rather than a list worth loading.
const MAX_PER_REQUEST: usize = 2_000;

/// May this reader see the names on an account's list?
///
/// The account itself, and a platform administrator. Deliberately not the permission that
/// registers telephone lines: see the note at the top of this module.
fn may_read(ctx: &AuthContext, owner_user_id: Uuid) -> bool {
    ctx.is_admin() || ctx.user_id == Some(owner_user_id)
}

fn require(ctx: &AuthContext, owner_user_id: Uuid) -> Result<()> {
    if may_read(ctx, owner_user_id) {
        Ok(())
    } else {
        // Refused as not found rather than as forbidden: whether an account keeps a list,
        // and whose account it is, is not this caller's business either.
        Err(AppError::NotFound("no such list".into()))
    }
}

#[derive(Serialize)]
pub struct ConflictName {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    pub note: Option<String>,
    pub created_epoch: i64,
}

#[derive(Deserialize)]
pub struct WhoseList {
    /// The account whose list this is. Omitted means the caller's own.
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
}

/// `GET /api/conflict-names` — the list, as a person reads it.
pub async fn list(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    axum::extract::Query(q): axum::extract::Query<WhoseList>,
) -> Result<Json<Vec<ConflictName>>> {
    let owner = q.owner_user_id.or(ctx.user_id).ok_or_else(|| AppError::NotFound("no such list".into()))?;
    require(&ctx, owner)?;
    let rows = sqlx::query!(
        r#"SELECT id, owner_user_id, name, note,
                  extract(epoch from created_at)::bigint AS "created_epoch!"
             FROM conflict_names WHERE owner_user_id = $1 ORDER BY name"#,
        owner
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ConflictName {
                id: r.id,
                owner_user_id: r.owner_user_id,
                name: r.name,
                note: r.note,
                created_epoch: r.created_epoch,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct AddNames {
    #[serde(default)]
    pub owner_user_id: Option<Uuid>,
    /// One name per line, as pasted.
    pub names: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /api/conflict-names` — add names, skipping the ones already there.
pub async fn add(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<AddNames>,
) -> Result<Json<serde_json::Value>> {
    let owner = body.owner_user_id.or(ctx.user_id).ok_or_else(|| AppError::NotFound("no such list".into()))?;
    require(&ctx, owner)?;
    let note = body.note.as_deref().map(str::trim).filter(|n| !n.is_empty());

    // Reduced here, on the way in, and stored reduced. A check compares reduced forms, so
    // doing it once at this point is what makes the check an index lookup rather than a
    // scan, and what makes a change to the reduction rules a deliberate rewrite.
    let mut wanted: Vec<(String, String)> = Vec::new();
    for line in body.names.lines() {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }
        let normalised = conflict::normalise(name);
        if normalised.is_empty() {
            continue;
        }
        wanted.push((name.chars().take(200).collect(), normalised));
    }
    if wanted.len() > MAX_PER_REQUEST {
        return Err(AppError::Validation(format!(
            "that is {} names at once; add at most {MAX_PER_REQUEST}",
            wanted.len()
        )));
    }

    let mut added = 0usize;
    for (name, normalised) in &wanted {
        let done = sqlx::query!(
            "INSERT INTO conflict_names (id, owner_user_id, name, normalised, note, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
            Uuid::now_v7(),
            owner,
            name,
            normalised,
            note,
            ctx.user_id,
        )
        .execute(&state.pg)
        .await?
        .rows_affected();
        added += done as usize;
    }

    // Audited by count, never by name. Which names a practice holds is the substance of
    // the list, and an audit trail is not the place to copy it to.
    let mut ev = AuditEvent::action("telephony.conflict_list.added", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("conflict_list".into());
    ev.resource_id = Some(owner);
    ev.payload = Some(serde_json::json!({ "added": added, "offered": wanted.len() }));
    let _ = audit::append(&state.pg, &ev).await;

    Ok(Json(serde_json::json!({
        "added": added,
        "already_there": wanted.len() - added,
    })))
}

/// `DELETE /api/conflict-names/{id}` — take one name off the list.
pub async fn remove(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let me = ctx.user_id;
    let admin = ctx.is_admin();
    let gone = sqlx::query!(
        "DELETE FROM conflict_names WHERE id = $1 AND ($2::bool OR owner_user_id = $3) \
         RETURNING owner_user_id",
        id,
        admin,
        me,
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::NotFound("no such name".into()))?;

    let mut ev = AuditEvent::action("telephony.conflict_list.removed", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("conflict_list".into());
    ev.resource_id = Some(gone.owner_user_id);
    let _ = audit::append(&state.pg, &ev).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}
