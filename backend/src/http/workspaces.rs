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

//! Connecting a folder on a paired machine, and everything the owner can do
//! about it afterwards.
//!
//! Connecting starts on the machine that has the folder: the client asks the
//! person in front of it which folder and at what level of trust, and only then
//! tells the instance. That order is the point, so registering is refused from
//! anywhere but a device token — a web session cannot conjure a grant over a
//! folder it has not asked anybody about, and could not have shown them the
//! folder it was asking about either.
//!
//! Withdrawing goes the other way and is available everywhere, because taking
//! something back should never be harder than granting it.

use axum::extract::{Path, State};
use axum::Json;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::device::MaybeDevice;
use crate::auth::keycloak::AuthUser;
use crate::auth::{permissions, AuthContext};
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::tools::desktop::{self, Tier};

// Times reach the browser as RFC 3339 strings — a bare `OffsetDateTime`
// serialises to a component array the application's `new Date` cannot read.
fn stamp(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

#[derive(Debug, Serialize)]
pub struct WorkspaceOut {
    pub id: Uuid,
    pub device_id: Uuid,
    pub device_name: String,
    pub path: String,
    pub label: String,
    /// `ro` | `rw` | `rw_nd`.
    pub tier: String,
    pub trusted_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConnectBody {
    pub path: String,
    #[serde(default)]
    pub label: String,
    pub tier: String,
}

#[derive(Debug, Deserialize)]
pub struct BindBody {
    pub workspace_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct PrefixOut {
    pub id: Uuid,
    pub prefix: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct PrefixBody {
    pub prefix: String,
}

fn owner(ctx: &AuthContext) -> Result<Uuid> {
    ctx.user_id.ok_or_else(|| {
        AppError::Forbidden("a user account is required to manage connected folders".into())
    })
}

/// Refuse to add anything new when an administrator has switched the family off
/// instance-wide. Registering a folder, binding one to a chat, or agreeing a
/// command are all no-ops if the tools can never run — so they are refused rather
/// than left to accumulate metadata for a capability that is not there. Taking
/// something back (revoke, unbind) is never gated.
fn require_enabled(state: &AppState) -> Result<()> {
    if state.boot.features.desktop_execution {
        Ok(())
    } else {
        Err(AppError::Forbidden("working in a folder is switched off on this instance".into()))
    }
}

/// `POST /api/me/workspaces` — the machine reports a folder its owner has just
/// connected on it.
///
/// Device-only, and the device is taken from the token rather than the body: a
/// machine can only ever register a folder as itself.
pub async fn connect_workspace(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    MaybeDevice(device): MaybeDevice,
    Json(body): Json<ConnectBody>,
) -> Result<(StatusCode, Json<WorkspaceOut>)> {
    let uid = owner(&ctx)?;
    require_enabled(&state)?;
    let Some(device_id) = device else {
        return Err(AppError::Forbidden(
            "a folder is connected from the desktop client, which is where the folder is".into(),
        ));
    };
    let tier = Tier::parse(body.tier.trim())
        .ok_or_else(|| AppError::Validation("tier must be one of ro, rw, rw_nd".into()))?;
    let path = desktop::normalise_root(&body.path)?;
    let label: String = {
        let l = body.label.trim();
        if l.is_empty() { path.clone() } else { l.chars().take(120).collect() }
    };

    let mut tx = state.pg.begin().await?;
    // Connecting a folder that is already connected returns the grant already
    // held, with the level of trust brought up to what was just agreed. Two rows
    // for one folder would mean withdrawing it withdrew only one of them.
    let row = sqlx::query!(
        "INSERT INTO device_workspaces (id, device_id, user_id, path, label, tier) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (device_id, path) WHERE revoked_at IS NULL \
         DO UPDATE SET tier = EXCLUDED.tier, label = EXCLUDED.label, trusted_at = now() \
         RETURNING id, trusted_at, last_used_at",
        Uuid::now_v7(),
        device_id,
        uid,
        path,
        label,
        tier.as_str(),
    )
    .fetch_one(&mut *tx)
    .await?;

    let mut event = AuditEvent::action("workspace.connected", ctx.role.as_str());
    event.actor_user_id = Some(uid);
    event.resource_type = Some("device_workspace".into());
    event.resource_id = Some(row.id);
    event.payload = Some(json!({ "device_id": device_id, "path": path, "tier": tier.as_str() }));
    audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;

    let device_name = sqlx::query_scalar!("SELECT name FROM devices WHERE id = $1", device_id)
        .fetch_optional(&state.pg)
        .await?
        .unwrap_or_default();

    Ok((
        StatusCode::CREATED,
        Json(WorkspaceOut {
            id: row.id,
            device_id,
            device_name,
            path,
            label,
            tier: tier.as_str().into(),
            trusted_at: stamp(row.trusted_at),
            last_used_at: row.last_used_at.map(stamp),
            revoked_at: None,
        }),
    ))
}

/// `GET /api/me/workspaces` — the folders this account has connected, on every
/// machine it has paired.
pub async fn list_workspaces(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<WorkspaceOut>>> {
    let uid = owner(&ctx)?;
    Ok(Json(load_workspaces(&state, uid).await?))
}

/// `DELETE /api/me/workspaces/{id}` — withdraw a folder. The tools stop being
/// offered on the next turn of any conversation bound to it.
pub async fn revoke_workspace(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    let uid = owner(&ctx)?;
    let mut tx = state.pg.begin().await?;
    let row = sqlx::query!(
        "UPDATE device_workspaces SET revoked_at = now() \
         WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL RETURNING path, device_id",
        id,
        uid,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = row else {
        tx.rollback().await?;
        return Ok(StatusCode::NO_CONTENT);
    };

    let mut event = AuditEvent::action("workspace.revoked", ctx.role.as_str());
    event.actor_user_id = Some(uid);
    event.resource_type = Some("device_workspace".into());
    event.resource_id = Some(id);
    event.payload = Some(json!({ "device_id": row.device_id, "path": row.path }));
    audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/admin/users/{id}/workspaces` — an administrator's read-only view of
/// what a user's machines have been told they may work in.
pub async fn admin_list_workspaces(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(user_id): Path<Uuid>,
) -> Result<Json<Vec<WorkspaceOut>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::USERS_VIEW).await?;
    Ok(Json(load_workspaces(&state, user_id).await?))
}

async fn load_workspaces(state: &AppState, user_id: Uuid) -> Result<Vec<WorkspaceOut>> {
    let rows = sqlx::query!(
        "SELECT w.id, w.device_id, d.name AS device_name, w.path, w.label, w.tier, \
                w.trusted_at, w.last_used_at, w.revoked_at \
         FROM device_workspaces w JOIN devices d ON d.id = w.device_id \
         WHERE w.user_id = $1 ORDER BY w.trusted_at DESC",
        user_id,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| WorkspaceOut {
            id: r.id,
            device_id: r.device_id,
            device_name: r.device_name,
            path: r.path,
            label: r.label,
            tier: r.tier,
            trusted_at: stamp(r.trusted_at),
            last_used_at: r.last_used_at.map(stamp),
            revoked_at: r.revoked_at.map(stamp),
        })
        .collect())
}

/// `GET /api/chats/{id}/workspace` — the folder this conversation is working in,
/// if any.
pub async fn get_chat_workspace(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
) -> Result<Json<Option<WorkspaceOut>>> {
    let uid = owner(&ctx)?;
    own_chat(&state, chat_id, uid).await?;
    let bound = sqlx::query_scalar!(
        "SELECT workspace_id FROM chat_workspace WHERE chat_id = $1",
        chat_id
    )
    .fetch_optional(&state.pg)
    .await?;
    // A folder that has been withdrawn is not the folder this chat works in, even
    // though the binding row survives it: the row records what was chosen, the
    // grant records whether it still stands.
    let live = load_workspaces(&state, uid)
        .await?
        .into_iter()
        .find(|w| Some(w.id) == bound && w.revoked_at.is_none());
    Ok(Json(live))
}

/// `PUT /api/chats/{id}/workspace` — work in this folder for this conversation.
/// One folder at a time: binding a second replaces the first.
pub async fn bind_chat_workspace(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
    Json(body): Json<BindBody>,
) -> Result<StatusCode> {
    let uid = owner(&ctx)?;
    require_enabled(&state)?;
    own_chat(&state, chat_id, uid).await?;
    let ws = sqlx::query!(
        "SELECT id, path, device_id FROM device_workspaces \
         WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
        body.workspace_id,
        uid,
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::NotFound("no such connected folder".into()))?;

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO chat_workspace (chat_id, workspace_id) VALUES ($1, $2) \
         ON CONFLICT (chat_id) DO UPDATE SET workspace_id = EXCLUDED.workspace_id, set_at = now()",
        chat_id,
        ws.id,
    )
    .execute(&mut *tx)
    .await?;

    let mut event = AuditEvent::action("workspace.bound", ctx.role.as_str());
    event.actor_user_id = Some(uid);
    event.resource_type = Some("device_workspace".into());
    event.resource_id = Some(ws.id);
    event.payload = Some(json!({ "chat_id": chat_id, "device_id": ws.device_id, "path": ws.path }));
    audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/chats/{id}/workspace` — stop working in a folder here.
pub async fn unbind_chat_workspace(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
) -> Result<StatusCode> {
    let uid = owner(&ctx)?;
    own_chat(&state, chat_id, uid).await?;
    sqlx::query!("DELETE FROM chat_workspace WHERE chat_id = $1", chat_id)
        .execute(&state.pg)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/workspaces/{id}/command-prefixes` — the commands already agreed to
/// for this folder.
pub async fn list_prefixes(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<PrefixOut>>> {
    let uid = owner(&ctx)?;
    own_workspace(&state, workspace_id, uid).await?;
    let rows = sqlx::query!(
        "SELECT id, prefix, created_at FROM workspace_command_prefixes \
         WHERE workspace_id = $1 ORDER BY created_at",
        workspace_id,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| PrefixOut { id: r.id, prefix: r.prefix, created_at: stamp(r.created_at) })
            .collect(),
    ))
}

/// `POST /api/workspaces/{id}/command-prefixes` — agree to a command by how it
/// starts, so the same run is not asked about twice.
///
/// This is reachable from a paired device, unlike the other few writes that
/// insist on a web session. The button that leads here is on the approval card,
/// and that card is in front of the person at the machine: refusing them there
/// would mean the only way to stop being asked the same question is to go and
/// find a browser. What it grants is narrow and stays where it was granted: one
/// prefix, on one folder, of that same machine, recorded. Deleting can never be
/// agreed to this way — the code that matches a command refuses to consider it.
pub async fn add_prefix(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<PrefixBody>,
) -> Result<(StatusCode, Json<PrefixOut>)> {
    let uid = owner(&ctx)?;
    require_enabled(&state)?;
    let ws = own_workspace(&state, workspace_id, uid).await?;
    let prefix = desktop::normalise_prefix(&body.prefix)?;

    let mut tx = state.pg.begin().await?;
    let row = sqlx::query!(
        "INSERT INTO workspace_command_prefixes (id, workspace_id, prefix, added_by) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (workspace_id, prefix) DO UPDATE SET prefix = EXCLUDED.prefix \
         RETURNING id, created_at",
        Uuid::now_v7(),
        workspace_id,
        prefix,
        uid,
    )
    .fetch_one(&mut *tx)
    .await?;

    let mut event = AuditEvent::action("workspace.command_allowed", ctx.role.as_str());
    event.actor_user_id = Some(uid);
    event.resource_type = Some("device_workspace".into());
    event.resource_id = Some(workspace_id);
    event.payload =
        Some(json!({ "device_id": ws.device_id, "path": ws.path, "prefix": prefix }));
    audit::append_with(&mut tx, &event).await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(PrefixOut { id: row.id, prefix, created_at: stamp(row.created_at) })))
}

/// `DELETE /api/workspaces/{id}/command-prefixes/{prefix_id}` — take an
/// agreement back.
pub async fn remove_prefix(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((workspace_id, prefix_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    let uid = owner(&ctx)?;
    own_workspace(&state, workspace_id, uid).await?;
    sqlx::query!(
        "DELETE FROM workspace_command_prefixes WHERE id = $1 AND workspace_id = $2",
        prefix_id,
        workspace_id,
    )
    .execute(&state.pg)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

struct OwnedWorkspace {
    device_id: Uuid,
    path: String,
}

/// The folder, if it is this account's and still connected. A folder that is not
/// theirs is reported as missing rather than forbidden: whether somebody else has
/// connected a given folder is not theirs to learn.
async fn own_workspace(state: &AppState, id: Uuid, uid: Uuid) -> Result<OwnedWorkspace> {
    sqlx::query!(
        "SELECT device_id, path FROM device_workspaces \
         WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
        id,
        uid,
    )
    .fetch_optional(&state.pg)
    .await?
    .map(|r| OwnedWorkspace { device_id: r.device_id, path: r.path })
    .ok_or_else(|| AppError::NotFound("no such connected folder".into()))
}

async fn own_chat(state: &AppState, chat_id: Uuid, uid: Uuid) -> Result<()> {
    let ok = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM chats WHERE id = $1 AND owner_user_id = $2)",
        chat_id,
        uid,
    )
    .fetch_one(&state.pg)
    .await?
    .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(AppError::NotFound("no such chat".into()))
    }
}
