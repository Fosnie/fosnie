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

//! Team messaging. Direct/group/project
//! chats with members, reliable messages (Postgres truth + per-chat sequence,
//! live fan-out via the WS hub, reconnect replay by `since=<seq>`), shared
//! notes (optimistic concurrency), cross-message search, and internal system
//! messages (the platform posting to its own chat — §B.12).

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::ws::protocol::ServerFrame;

// --- membership / RBAC -------------------------------------------------------

/// Presence-feature gate (Core): team chats + direct messages. When off, every
/// messaging endpoint refuses (defense-in-depth — the SPA also hides the Teams/DM
/// nav and routes). This is a presence capability resolved through the
/// `FeatureResolver` seam (runtime override + per-group restrict), NOT an edition
/// gate — so it uses `features::enabled_for`, not `require_capability`.
async fn require_messaging(state: &AppState, ctx: &AuthContext) -> Result<()> {
    if crate::features::enabled_for(state, ctx, "messaging").await {
        Ok(())
    } else {
        Err(AppError::Forbidden("messaging is not enabled on this deployment".into()))
    }
}

/// Membership role of `ctx` in `chat_id`, or 403 if not a member. Also the
/// choke point for the `messaging` feature gate (every chat-scoped handler goes
/// through here).
async fn require_member(state: &AppState, ctx: &AuthContext, chat_id: Uuid) -> Result<String> {
    require_messaging(state, ctx).await?;
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    sqlx::query_scalar!(
        r#"SELECT role::text AS "role!" FROM group_chat_members WHERE group_chat_id = $1 AND user_id = $2"#,
        chat_id,
        uid
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Forbidden("not a member of this chat".into()))
}

async fn require_admin_of(state: &AppState, ctx: &AuthContext, chat_id: Uuid) -> Result<()> {
    match require_member(state, ctx, chat_id).await?.as_str() {
        "owner" | "admin" => Ok(()),
        _ => Err(AppError::Forbidden("requires chat owner/admin".into())),
    }
}

// --- shared post path (REST send + system messages) --------------------------

/// Persist a message (source of truth) under a per-chat sequence number, then
/// fan it out live to every connected member. `sender` is `None` for system
/// messages. Returns `(message_id, sequence_number, created_at_rfc3339)`.
#[allow(clippy::too_many_arguments)]
async fn post_message(
    state: &AppState,
    chat_id: Uuid,
    sender: Option<Uuid>,
    message_type: &str,
    content: &str,
    attachments: Option<serde_json::Value>,
    shared_resources: Option<serde_json::Value>,
    mentions: Option<serde_json::Value>,
) -> Result<(Uuid, i32, String)> {
    let id = db::new_id();
    let mut tx = state.pg.begin().await?;
    // Serialise sequence assignment per chat (advisory xact lock, like the audit chain).
    sqlx::query!("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", chat_id.to_string())
        .execute(&mut *tx)
        .await?;
    let seq: i32 = sqlx::query_scalar!(
        r#"SELECT COALESCE(MAX(sequence_number), 0) + 1 AS "n!" FROM group_chat_messages WHERE group_chat_id = $1"#,
        chat_id
    )
    .fetch_one(&mut *tx)
    .await?;
    // At-rest encryption: direct messages get an AES-256-GCM ciphertext body when a
    // key is configured; group/project channels too when `encrypt_group_messages` is
    // on. The live fan-out below still uses the in-hand plaintext.
    let kind: String = sqlx::query_scalar!(
        r#"SELECT kind::text AS "kind!" FROM group_chats WHERE id = $1"#,
        chat_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let (stored_content, encrypted) = match state.message_key {
        Some(_key) if should_encrypt_message(true, &kind, state.boot.encrypt_group_messages) => {
            (crate::crypto::encrypt_at_rest(content)?, true)
        }
        _ => (content.to_string(), false),
    };
    // Keep copies for the live fan-out (the bound values are moved into the query).
    let att_for_frame = attachments.clone();
    let shared_for_frame = shared_resources.clone();
    let created_at: OffsetDateTime = sqlx::query_scalar!(
        "INSERT INTO group_chat_messages \
         (id, group_chat_id, sender_user_id, message_type, sequence_number, content, content_encrypted, attachments, shared_resources, mentions) \
         VALUES ($1, $2, $3, ($4::text)::group_msg_type, $5, $6, $7, $8, $9, $10) RETURNING created_at",
        id, chat_id, sender, message_type, seq, stored_content, encrypted, attachments, shared_resources, mentions,
    )
    .fetch_one(&mut *tx)
    .await?;
    // Your own message is never "unread" to you: advance the sender's read marker so
    // the badge stays receiver-only (a human sender; system posts have no marker).
    if let Some(s) = sender {
        sqlx::query!(
            "UPDATE group_chat_members SET last_read_seq = GREATEST(last_read_seq, $3) \
             WHERE group_chat_id = $1 AND user_id = $2",
            chat_id, s, seq,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let ts = created_at.format(&Rfc3339).unwrap_or_default();

    // Audit (skip noisy system posts? keep them — traceability is cheap).
    let mut ev = AuditEvent::action("chat.message.sent", "system");
    ev.actor_user_id = sender;
    ev.resource_type = Some("group_chat".into());
    ev.resource_id = Some(chat_id);
    ev.payload = Some(serde_json::json!({ "message_id": id, "seq": seq, "type": message_type }));
    let _ = audit::append(&state.pg, &ev).await;

    // Live fan-out to connected members (durable history covers the rest).
    let members: Vec<Uuid> =
        sqlx::query_scalar!("SELECT user_id FROM group_chat_members WHERE group_chat_id = $1", chat_id)
            .fetch_all(&state.pg)
            .await?;
    for m in members {
        state.hub.send_to_user(
            m,
            ServerFrame::GroupMessage {
                chat_id,
                id,
                seq,
                sender_user_id: sender,
                message_type: message_type.to_string(),
                content: content.to_string(),
                created_at: ts.clone(),
                attachments: att_for_frame.clone(),
                shared_resources: shared_for_frame.clone(),
            },
        );
    }
    Ok((id, seq, ts))
}

/// Is `user_id` a member of `chat_id`? (Cheap membership probe for callers
/// outside this module — e.g. automation delivery targets.)
pub async fn is_member(state: &AppState, user_id: Uuid, chat_id: Uuid) -> Result<bool> {
    let found: bool = sqlx::query_scalar!(
        r#"SELECT EXISTS(
             SELECT 1 FROM group_chat_members WHERE group_chat_id = $1 AND user_id = $2
           ) AS "e!""#,
        chat_id,
        user_id
    )
    .fetch_one(&state.pg)
    .await?;
    Ok(found)
}

/// Post a platform `system` message into a group chat (no human sender) — the
/// reliable `post_message` path, fanned out live like any other. Used by
/// automation delivery (§B.12). `sender` is `None` for a pure platform post.
pub async fn post_system_message(
    state: &AppState,
    chat_id: Uuid,
    sender: Option<Uuid>,
    content: &str,
    shared_resources: Option<serde_json::Value>,
) -> Result<(Uuid, i32, String)> {
    post_message(state, chat_id, sender, "system", content, None, shared_resources, None).await
}

/// Post a reference to an LLM chat into a group/DM chat: a message whose
/// `shared_resources` carries `{ chat_id }` so the UI can offer "open chat". Used
/// by the user Share action (a `user` message from the sharer) and by automation
/// delivery (a `system` message). The recipient's read access comes from a
/// `chat_shares` row (see http/export.rs `require_chat_read`).
pub async fn post_chat_link(
    state: &AppState,
    target_chat: Uuid,
    sender: Option<Uuid>,
    src_chat_id: Uuid,
    content: &str,
    system: bool,
) -> Result<(Uuid, i32, String)> {
    let shared = serde_json::json!({ "chat_id": src_chat_id });
    let mtype = if system { "system" } else { "user" };
    post_message(state, target_chat, sender, mtype, content, None, Some(shared), None).await
}

/// Find or create the `kind='project'` chat for a project (owner as member). New
/// chats are named after the project (Teams shows the project name, not a generic
/// "Project chat"); pre-existing rows keep their name.
pub async fn ensure_project_chat(state: &AppState, project_id: Uuid, owner: Uuid) -> Result<Uuid> {
    if let Some(id) =
        sqlx::query_scalar!("SELECT id FROM group_chats WHERE project_id = $1 AND kind = 'project'", project_id)
            .fetch_optional(&state.pg)
            .await?
    {
        return Ok(id);
    }
    // Name the chat after the project (fall back to a generic label if the lookup
    // somehow misses).
    let name = sqlx::query_scalar!("SELECT name FROM projects WHERE id = $1", project_id)
        .fetch_optional(&state.pg)
        .await?
        .unwrap_or_else(|| "Project chat".to_string());
    let id = db::new_id();
    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO group_chats (id, kind, name, project_id, created_by) VALUES ($1, 'project', $2, $3, $4) \
         ON CONFLICT DO NOTHING",
        id, name, project_id, owner,
    )
    .execute(&mut *tx)
    .await?;
    // Resolve the actual id (handle the race where another tx created it).
    let chat_id: Uuid = sqlx::query_scalar!(
        "SELECT id FROM group_chats WHERE project_id = $1 AND kind = 'project'", project_id
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO group_chat_members (group_chat_id, user_id, role) VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING",
        chat_id, owner,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(chat_id)
}

/// Reconcile a project chat's membership with the project's people: the chat
/// should contain exactly the project owner ∪ every user holding a `read`
/// access-grant on the project (direct user grants, and members of any granted
/// group). Adds the missing, removes those no longer on the project — never the
/// owner. Idempotent; safe to call after any project grant change.
pub async fn resync_project_chat_members(state: &AppState, project_id: Uuid) -> Result<()> {
    let owner: Option<Uuid> =
        sqlx::query_scalar!("SELECT owner_user_id FROM projects WHERE id = $1", project_id)
            .fetch_optional(&state.pg)
            .await?;
    let Some(owner) = owner else { return Ok(()) };
    let chat_id = ensure_project_chat(state, project_id, owner).await?;

    // The project's people (owner + read-grant holders, group grants expanded).
    let people: Vec<Uuid> = sqlx::query_scalar!(
        r#"SELECT u.id FROM users u WHERE u.id = $1
           OR EXISTS (
               SELECT 1 FROM access_grants g
               WHERE g.resource_type = 'project' AND g.resource_id = $2
                 AND g.permission = 'read'
                 AND ( (g.principal_type = 'user'  AND g.principal_id = u.id)
                    OR (g.principal_type = 'group' AND g.principal_id IN
                          (SELECT group_id FROM group_members WHERE user_id = u.id)) )
           )"#,
        owner,
        project_id
    )
    .fetch_all(&state.pg)
    .await?;

    let mut tx = state.pg.begin().await?;
    // Add anyone missing (members, except the owner who stays 'owner').
    for uid in &people {
        if *uid == owner {
            continue;
        }
        sqlx::query!(
            "INSERT INTO group_chat_members (group_chat_id, user_id, role) VALUES ($1, $2, 'member') \
             ON CONFLICT (group_chat_id, user_id) DO NOTHING",
            chat_id, uid,
        )
        .execute(&mut *tx)
        .await?;
    }
    // Remove members no longer on the project (never the owner).
    sqlx::query!(
        "DELETE FROM group_chat_members \
         WHERE group_chat_id = $1 AND user_id <> $2 AND user_id <> ALL($3)",
        chat_id, owner, &people,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Post a memory-update notice to a project's chat so a power
/// user can moderate. Best-effort — never blocks the memory write.
pub async fn notify_project_memory(state: &AppState, project_id: Uuid, actor: Option<Uuid>, content: &str) {
    let Some(owner) = actor else { return };
    if let Ok(chat_id) = ensure_project_chat(state, project_id, owner).await {
        let _ = post_message(
            state, chat_id, None, "system",
            &format!("Memory added: {content}"),
            None, None, None,
        )
        .await;
    }
}

// --- chats CRUD --------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateChat {
    pub kind: String, // dm | group | project
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub project_id: Option<Uuid>,
    #[serde(default)]
    pub member_user_ids: Vec<Uuid>,
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

pub async fn create_chat(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateChat>,
) -> Result<Json<CreatedId>> {
    require_messaging(&state, &ctx).await?;
    let owner = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    if !matches!(body.kind.as_str(), "dm" | "group" | "project") {
        return Err(AppError::Validation("kind must be dm|group|project".into()));
    }
    // Non-admins may only add members from their team circle (admin bypasses).
    if !ctx.is_admin() {
        for m in &body.member_user_ids {
            if *m != owner {
                crate::auth::rbac::require_circle(&ctx, &state.pg, *m).await?;
            }
        }
    }
    let id = db::new_id();
    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO group_chats (id, kind, name, project_id, created_by) VALUES ($1, ($2::text)::group_chat_kind, $3, $4, $5)",
        id, body.kind, body.name, body.project_id, owner,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO group_chat_members (group_chat_id, user_id, role) VALUES ($1, $2, 'owner')",
        id, owner,
    )
    .execute(&mut *tx)
    .await?;
    for m in &body.member_user_ids {
        if *m != owner {
            sqlx::query!(
                "INSERT INTO group_chat_members (group_chat_id, user_id, role) VALUES ($1, $2, 'member') ON CONFLICT DO NOTHING",
                id, m,
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;

    let mut ev = AuditEvent::action("chat.created", ctx.role.as_str());
    ev.actor_user_id = Some(owner);
    ev.resource_type = Some("group_chat".into());
    ev.resource_id = Some(id);
    let _ = audit::append(&state.pg, &ev).await;
    Ok(Json(CreatedId { id }))
}

#[derive(Serialize)]
pub struct ChatSummary {
    pub id: Uuid,
    pub kind: String,
    pub name: Option<String>,
    pub project_id: Option<Uuid>,
    /// Messages newer than the caller's read watermark (#12 unread indicators).
    pub unread_count: i64,
}

pub async fn list_chats(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<ChatSummary>>> {
    require_messaging(&state, &ctx).await?;
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let rows = sqlx::query!(
        r#"SELECT c.id, c.kind::text AS "kind!", c.project_id,
                  CASE WHEN c.kind = 'dm'
                       THEN (SELECT u.display_name FROM group_chat_members gm2
                             JOIN users u ON u.id = gm2.user_id
                             WHERE gm2.group_chat_id = c.id AND gm2.user_id <> $1 LIMIT 1)
                       ELSE c.name END AS name,
                  (SELECT count(*) FROM group_chat_messages gm
                   WHERE gm.group_chat_id = c.id AND gm.sequence_number > m.last_read_seq
                     AND (gm.sender_user_id IS NULL OR gm.sender_user_id <> $1)) AS "unread_count!"
           FROM group_chats c JOIN group_chat_members m ON m.group_chat_id = c.id
           WHERE m.user_id = $1 ORDER BY c.created_at DESC"#,
        uid
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ChatSummary {
                id: r.id,
                kind: r.kind,
                name: r.name,
                project_id: r.project_id,
                unread_count: r.unread_count,
            })
            .collect(),
    ))
}

/// Start (or reuse) the 1:1 DM between the caller and `other`. Idempotent — a `dm`
/// chat containing exactly those two users is returned if it exists, else created.
pub async fn start_dm(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(other): Path<Uuid>,
) -> Result<Json<CreatedId>> {
    require_messaging(&state, &ctx).await?;
    let me = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    if me == other {
        return Err(AppError::Validation("cannot start a DM with yourself".into()));
    }
    // Non-admins may only DM within their team circle (admin bypasses).
    crate::auth::rbac::require_circle(&ctx, &state.pg, other).await?;
    let existing: Option<Uuid> = sqlx::query_scalar!(
        r#"SELECT gc.id FROM group_chats gc
           WHERE gc.kind = 'dm'
             AND (SELECT count(*) FROM group_chat_members m WHERE m.group_chat_id = gc.id) = 2
             AND EXISTS (SELECT 1 FROM group_chat_members m WHERE m.group_chat_id = gc.id AND m.user_id = $1)
             AND EXISTS (SELECT 1 FROM group_chat_members m WHERE m.group_chat_id = gc.id AND m.user_id = $2)
           LIMIT 1"#,
        me,
        other
    )
    .fetch_optional(&state.pg)
    .await?;
    if let Some(id) = existing {
        return Ok(Json(CreatedId { id }));
    }
    let id = db::new_id();
    let mut tx = state.pg.begin().await?;
    sqlx::query!("INSERT INTO group_chats (id, kind, created_by) VALUES ($1, 'dm', $2)", id, me)
        .execute(&mut *tx)
        .await?;
    sqlx::query!(
        "INSERT INTO group_chat_members (group_chat_id, user_id, role) VALUES ($1, $2, 'owner')",
        id,
        me
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO group_chat_members (group_chat_id, user_id, role) VALUES ($1, $2, 'member')",
        id,
        other
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(CreatedId { id }))
}

#[derive(Serialize)]
pub struct MemberOut {
    pub user_id: Uuid,
    pub role: String,
}

#[derive(Serialize)]
pub struct ChatDetail {
    pub id: Uuid,
    pub kind: String,
    pub name: Option<String>,
    pub project_id: Option<Uuid>,
    pub members: Vec<MemberOut>,
}

pub async fn get_chat(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
) -> Result<Json<ChatDetail>> {
    require_member(&state, &ctx, chat_id).await?;
    let uid = ctx.user_id;
    // For a DM, show the other participant's name instead of the (null) chat name.
    let c = sqlx::query!(
        r#"SELECT kind::text AS "kind!", project_id,
                  CASE WHEN kind = 'dm'
                       THEN (SELECT u.display_name FROM group_chat_members gm2
                             JOIN users u ON u.id = gm2.user_id
                             WHERE gm2.group_chat_id = $1 AND gm2.user_id <> $2 LIMIT 1)
                       ELSE name END AS name
           FROM group_chats WHERE id = $1"#,
        chat_id,
        uid
    )
    .fetch_one(&state.pg)
    .await?;
    let members = sqlx::query!(
        r#"SELECT user_id, role::text AS "role!" FROM group_chat_members WHERE group_chat_id = $1"#,
        chat_id
    )
    .fetch_all(&state.pg)
    .await?
    .into_iter()
    .map(|r| MemberOut { user_id: r.user_id, role: r.role })
    .collect();
    Ok(Json(ChatDetail { id: chat_id, kind: c.kind, name: c.name, project_id: c.project_id, members }))
}

// --- members -----------------------------------------------------------------

#[derive(Deserialize)]
pub struct AddMember {
    pub user_id: Uuid,
    #[serde(default)]
    pub role: Option<String>,
}

pub async fn add_member(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
    Json(body): Json<AddMember>,
) -> Result<Json<serde_json::Value>> {
    require_admin_of(&state, &ctx, chat_id).await?;
    // A non-platform-admin chat owner/admin may only add circle members.
    crate::auth::rbac::require_circle(&ctx, &state.pg, body.user_id).await?;
    let role = match body.role.as_deref() {
        Some("admin") => "admin",
        Some("owner") => "owner",
        _ => "member",
    };
    // Upsert the membership and emit `chat.member_added` atomically (transactional
    // outbox, §12.1). `xmax = 0` distinguishes a genuine insert from a role change,
    // so re-adding an existing member (role update) does not fire the event.
    let mut tx = state.pg.begin().await?;
    let row = sqlx::query!(
        r#"INSERT INTO group_chat_members (group_chat_id, user_id, role) VALUES ($1, $2, ($3::text)::group_member_role)
           ON CONFLICT (group_chat_id, user_id) DO UPDATE SET role = EXCLUDED.role
           RETURNING (xmax = 0) AS "inserted!""#,
        chat_id, body.user_id, role,
    )
    .fetch_one(&mut *tx)
    .await?;
    if row.inserted {
        let evd = crate::events::NewEvent::new(
            crate::events::CHAT_MEMBER_ADDED,
            crate::events::ActorType::Human,
        )
        .actor(ctx.user_id)
        .resource("group_chat", chat_id)
        .payload(serde_json::json!({ "group_chat_id": chat_id.to_string(), "user_id": body.user_id.to_string(), "role": role }));
        crate::events::emit_with(&mut tx, &evd).await?;
    }
    tx.commit().await?;
    let mut ev = AuditEvent::action("chat.member.added", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("group_chat".into());
    ev.resource_id = Some(chat_id);
    ev.payload = Some(serde_json::json!({ "user_id": body.user_id, "role": role }));
    let _ = audit::append(&state.pg, &ev).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn remove_member(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((chat_id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    require_admin_of(&state, &ctx, chat_id).await?;
    sqlx::query!("DELETE FROM group_chat_members WHERE group_chat_id = $1 AND user_id = $2", chat_id, user_id)
        .execute(&state.pg)
        .await?;
    let mut ev = AuditEvent::action("chat.member.removed", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("group_chat".into());
    ev.resource_id = Some(chat_id);
    ev.payload = Some(serde_json::json!({ "user_id": user_id }));
    let _ = audit::append(&state.pg, &ev).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- messages ----------------------------------------------------------------

#[derive(Deserialize)]
pub struct SendMessage {
    pub content: String,
    #[serde(default)]
    pub attachments: Option<serde_json::Value>,
    #[serde(default)]
    pub shared_resources: Option<serde_json::Value>,
    #[serde(default)]
    pub mentions: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct SentMessage {
    pub id: Uuid,
    pub seq: i32,
    pub created_at: String,
}

pub async fn send_message(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
    Json(body): Json<SendMessage>,
) -> Result<Json<SentMessage>> {
    require_member(&state, &ctx, chat_id).await?;
    // A message must have text or at least one attachment (an image-only post is fine).
    let has_attachment = body
        .attachments
        .as_ref()
        .and_then(|a| a.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if body.content.trim().is_empty() && !has_attachment {
        return Err(AppError::Validation("message must have text or an attachment".into()));
    }
    let (id, seq, created_at) = post_message(
        &state, chat_id, ctx.user_id, "user", &body.content,
        body.attachments, body.shared_resources, body.mentions,
    )
    .await?;
    Ok(Json(SentMessage { id, seq, created_at }))
}

// --- reactions ---------------------------------------------------------------

#[derive(Deserialize)]
pub struct ReactBody {
    pub emoji: String,
}

#[derive(Serialize)]
pub struct ReactResult {
    pub added: bool,
}

/// Toggle the caller's reaction (emoji) on a message. Member-gated; idempotent
/// per (message, user, emoji). Fans out a `group.reaction` frame to members.
pub async fn toggle_reaction(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((chat_id, message_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ReactBody>,
) -> Result<Json<ReactResult>> {
    require_member(&state, &ctx, chat_id).await?;
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let emoji = body.emoji.trim().to_string();
    if emoji.is_empty() || emoji.len() > 32 {
        return Err(AppError::Validation("invalid emoji".into()));
    }
    let belongs: bool = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM group_chat_messages WHERE id = $1 AND group_chat_id = $2) AS "e!""#,
        message_id,
        chat_id
    )
    .fetch_one(&state.pg)
    .await?;
    if !belongs {
        return Err(AppError::Validation("message not in this chat".into()));
    }
    // Toggle: remove if present, else add.
    let removed = sqlx::query!(
        "DELETE FROM message_reactions WHERE message_id = $1 AND user_id = $2 AND emoji = $3",
        message_id,
        uid,
        emoji
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    let added = if removed == 0 {
        sqlx::query!(
            "INSERT INTO message_reactions (message_id, user_id, emoji) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            message_id,
            uid,
            emoji
        )
        .execute(&state.pg)
        .await?;
        true
    } else {
        false
    };
    let members: Vec<Uuid> =
        sqlx::query_scalar!("SELECT user_id FROM group_chat_members WHERE group_chat_id = $1", chat_id)
            .fetch_all(&state.pg)
            .await?;
    for m in members {
        state.hub.send_to_user(
            m,
            ServerFrame::GroupReaction { chat_id, message_id, emoji: emoji.clone(), user_id: uid, added },
        );
    }
    Ok(Json(ReactResult { added }))
}

/// WS-side send: same reliable post path as the REST handler, gated by
/// membership. Called from the WebSocket reader for `ClientFrame::GroupSend`.
pub async fn send_via_ws(
    state: &AppState,
    ctx: &AuthContext,
    chat_id: Uuid,
    content: &str,
    mentions: Option<serde_json::Value>,
) -> Result<()> {
    require_member(state, ctx, chat_id).await?;
    if content.trim().is_empty() {
        return Err(AppError::Validation("content must not be empty".into()));
    }
    post_message(state, chat_id, ctx.user_id, "user", content, None, None, mentions).await?;
    Ok(())
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub since: i32, // sequence_number exclusive; 0 = from start
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct ReactionAgg {
    pub emoji: String,
    pub count: i64,
    pub mine: bool,
}

#[derive(Serialize)]
pub struct MessageOut {
    pub id: Uuid,
    pub seq: i32,
    pub sender_user_id: Option<Uuid>,
    pub message_type: String,
    pub content: String,
    pub created_at: String,
    pub mentions: Option<serde_json::Value>,
    pub shared_resources: Option<serde_json::Value>,
    pub reactions: Vec<ReactionAgg>,
}

pub async fn list_messages(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<MessageOut>>> {
    require_member(&state, &ctx, chat_id).await?;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let rows = sqlx::query!(
        r#"SELECT id, sequence_number, sender_user_id, message_type::text AS "message_type!",
                  content, content_encrypted, created_at, mentions, shared_resources
           FROM group_chat_messages
           WHERE group_chat_id = $1 AND sequence_number > $2
           ORDER BY sequence_number ASC LIMIT $3"#,
        chat_id, q.since, limit
    )
    .fetch_all(&state.pg)
    .await?;

    // Reactions for this page, grouped (emoji, count, did the caller react).
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let me = ctx.user_id;
    let react_rows = sqlx::query!(
        r#"SELECT message_id, emoji, count(*) AS "n!", bool_or(user_id = $2) AS "mine!"
           FROM message_reactions WHERE message_id = ANY($1)
           GROUP BY message_id, emoji ORDER BY min(created_at)"#,
        &ids,
        me
    )
    .fetch_all(&state.pg)
    .await?;
    let mut by_msg: std::collections::HashMap<Uuid, Vec<ReactionAgg>> = std::collections::HashMap::new();
    for r in react_rows {
        by_msg.entry(r.message_id).or_default().push(ReactionAgg { emoji: r.emoji, count: r.n, mine: r.mine });
    }

    // Opening a chat marks it read up to its latest message (#12). Best-effort.
    if let Some(uid) = ctx.user_id {
        let _ = sqlx::query!(
            "UPDATE group_chat_members SET last_read_seq = GREATEST(last_read_seq, \
               COALESCE((SELECT max(sequence_number) FROM group_chat_messages WHERE group_chat_id = $1), 0)) \
             WHERE group_chat_id = $1 AND user_id = $2",
            chat_id, uid
        )
        .execute(&state.pg)
        .await;
    }

    Ok(Json(
        rows.into_iter()
            .map(|r| {
                let content = if r.content_encrypted {
                    match state.message_key {
                        Some(_key) => crate::crypto::decrypt_at_rest(&r.content).unwrap_or_else(|_| "[unable to decrypt]".into()),
                        None => "[encrypted]".into(),
                    }
                } else {
                    r.content
                };
                MessageOut {
                    id: r.id,
                    seq: r.sequence_number,
                    sender_user_id: r.sender_user_id,
                    message_type: r.message_type,
                    content,
                    created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
                    mentions: r.mentions,
                    shared_resources: r.shared_resources,
                    reactions: by_msg.remove(&r.id).unwrap_or_default(),
                }
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize)]
pub struct SearchHit {
    pub chat_id: Uuid,
    pub id: Uuid,
    pub seq: i32,
    pub content: String,
}

/// Cross-message search over the caller's chats (ILIKE; tsvector is Pass-2).
pub async fn search_messages(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<SearchHit>>> {
    require_messaging(&state, &ctx).await?;
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    if q.q.trim().is_empty() {
        return Ok(Json(Vec::new()));
    }

    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    // Full-text over the maintained tsvector (stemming + ranking).
    // `websearch_to_tsquery` parses user input safely (quotes/operators), so no
    // manual escaping is needed.
    // Encrypted bodies are excluded: their `content_tsv` indexes ciphertext, so they
    // neither match nor leak through search. DMs are always encrypted; group/project
    // messages are encrypted too when `encrypt_group_messages` is on. The
    // `content_encrypted` guard covers both (the `kind <> 'dm'` keeps DMs out cheaply
    // even when no key is set, so DM bodies are never searched regardless).
    let rows = sqlx::query!(
        r#"SELECT m.group_chat_id, m.id, m.sequence_number, m.content
           FROM group_chat_messages m
           JOIN group_chat_members mem ON mem.group_chat_id = m.group_chat_id AND mem.user_id = $1
           JOIN group_chats gc ON gc.id = m.group_chat_id
           WHERE gc.kind <> 'dm'
             AND NOT m.content_encrypted
             AND m.content_tsv @@ websearch_to_tsquery('english', $2)
           ORDER BY ts_rank(m.content_tsv, websearch_to_tsquery('english', $2)) DESC
           LIMIT $3"#,
        uid, q.q, limit
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| SearchHit { chat_id: r.group_chat_id, id: r.id, seq: r.sequence_number, content: r.content })
            .collect(),
    ))
}

/// Whether an outgoing message body is encrypted at rest: DMs always (when a key is
/// configured); group/project channels only when `encrypt_group_messages` is on.
/// Encryption is mutually exclusive with full-text search — `search_messages`
/// excludes encrypted rows.
fn should_encrypt_message(has_key: bool, kind: &str, encrypt_group: bool) -> bool {
    has_key && (kind == "dm" || encrypt_group)
}

#[cfg(test)]
mod encryption_tests {
    use super::should_encrypt_message;

    #[test]
    fn dm_encrypts_with_key_regardless_of_toggle() {
        assert!(should_encrypt_message(true, "dm", false));
        assert!(should_encrypt_message(true, "dm", true));
    }

    #[test]
    fn group_project_encrypt_only_with_toggle() {
        for kind in ["group", "project"] {
            assert!(!should_encrypt_message(true, kind, false), "{kind} stays plaintext, toggle off");
            assert!(should_encrypt_message(true, kind, true), "{kind} encrypts, toggle on");
        }
    }

    #[test]
    fn nothing_encrypts_without_a_key() {
        for kind in ["dm", "group", "project"] {
            assert!(!should_encrypt_message(false, kind, true), "{kind} needs a key");
        }
    }
}

// --- shared notes ------------------------------------------------------------

#[derive(Serialize)]
pub struct NoteOut {
    pub id: Uuid,
    pub content: String,
    pub version: i32,
}

pub async fn list_notes(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
) -> Result<Json<Vec<NoteOut>>> {
    require_member(&state, &ctx, chat_id).await?;
    let rows = sqlx::query!(
        "SELECT id, content, version FROM group_chat_notes WHERE group_chat_id = $1 ORDER BY created_at",
        chat_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(rows.into_iter().map(|r| NoteOut { id: r.id, content: r.content, version: r.version }).collect()))
}

#[derive(Deserialize)]
pub struct CreateNote {
    pub content: String,
}

pub async fn create_note(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
    Json(body): Json<CreateNote>,
) -> Result<Json<NoteOut>> {
    require_member(&state, &ctx, chat_id).await?;
    let id = db::new_id();
    sqlx::query!(
        "INSERT INTO group_chat_notes (id, group_chat_id, content, updated_by) VALUES ($1, $2, $3, $4)",
        id, chat_id, body.content, ctx.user_id,
    )
    .execute(&state.pg)
    .await?;
    Ok(Json(NoteOut { id, content: body.content, version: 1 }))
}

#[derive(Deserialize)]
pub struct UpdateNote {
    pub content: String,
    pub version: i32, // optimistic concurrency token
}

pub async fn update_note(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((chat_id, note_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateNote>,
) -> Result<Json<NoteOut>> {
    require_member(&state, &ctx, chat_id).await?;
    let row = sqlx::query!(
        "UPDATE group_chat_notes SET content = $3, version = version + 1, updated_by = $4, updated_at = now() \
         WHERE id = $1 AND group_chat_id = $2 AND version = $5 RETURNING version",
        note_id, chat_id, body.content, ctx.user_id, body.version,
    )
    .fetch_optional(&state.pg)
    .await?;
    match row {
        Some(r) => Ok(Json(NoteOut { id: note_id, content: body.content, version: r.version })),
        None => Err(AppError::Conflict("note version conflict — reload and retry".into())),
    }
}

pub async fn delete_note(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((chat_id, note_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    require_member(&state, &ctx, chat_id).await?;
    sqlx::query!("DELETE FROM group_chat_notes WHERE id = $1 AND group_chat_id = $2", note_id, chat_id)
        .execute(&state.pg)
        .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
