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

//! Chat listing + message history (read-only) for the SPA app shell. Chats are
//! created/driven over the WebSocket (chat-turn); this module just exposes the
//! caller's chat list and a chat's persisted messages so the sidebar can list
//! conversations and reopen one with its history.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::state::AppState;

#[derive(Serialize)]
pub struct ChatSummary {
    pub id: Uuid,
    pub title: String,
    pub project_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
    pub created_at: String,
    /// Workspace mode (`general` | `legal` | `research`). Research runs are
    /// listed only in the Research mode (see `list_chats` filtering).
    pub mode: String,
    /// The saved Deep Research request params (research mode only), so a run can
    /// be re-opened prefilled ('Refine'). NULL for non-research chats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub research_params: Option<serde_json::Value>,
    /// Which client started this conversation (`web` | `desktop`). Present so an
    /// interface can mark where a conversation came from; `api` never appears
    /// here because that traffic is excluded from the list.
    pub origin: String,
}

#[derive(Deserialize, Default)]
pub struct ListChatsParams {
    /// Optional exact-mode filter. ABSENT ⇒ research chats are EXCLUDED —
    /// existing clients (and the sector-based General/Legal scoping) keep
    /// seeing exactly what they saw before Deep Research existed.
    pub mode: Option<String>,
}

/// The caller's chats (most recent first). Scoped to the owner.
pub async fn list_chats(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    axum::extract::Query(params): axum::extract::Query<ListChatsParams>,
) -> Result<Json<Vec<ChatSummary>>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let mode = match params.mode.as_deref() {
        None => None,
        Some(m @ ("general" | "legal" | "research")) => Some(m.to_string()),
        Some(other) => {
            return Err(AppError::Validation(format!("unknown chat mode '{other}'")));
        }
    };
    // Conversations driven by an external application are excluded rather than
    // "only web included": a conversation from any other first-class client
    // belongs in this list, marked by its origin. Only machine traffic is kept
    // out, and it stays reachable by direct URL for debugging.
    let rows = sqlx::query!(
        "SELECT id, title, project_id, agent_id, created_at, mode, research_params, origin \
         FROM chats WHERE owner_user_id = $1 AND archived_at IS NULL AND origin <> 'api' \
           AND (($2::text IS NULL AND mode <> 'research') OR mode = $2) \
         ORDER BY created_at DESC",
        uid,
        mode as Option<String>,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ChatSummary {
                id: r.id,
                title: r.title,
                project_id: r.project_id,
                agent_id: r.agent_id,
                created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
                mode: r.mode,
                research_params: r.research_params,
                origin: r.origin,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct MessageOut {
    pub id: Uuid,
    pub role: String,
    pub content: String,
    pub sequence_number: i32,
    pub created_at: String,
    /// The agent activity of an assistant turn (track_steps plan + tools used),
    /// for the inline activity timeline. NULL for plain turns.
    pub activity: Option<serde_json::Value>,
    /// The live groundedness verdict of a RAG answer (score + flagged spans), for
    /// the inline groundedness block. NULL when not verified.
    pub groundedness: Option<serde_json::Value>,
    /// True while an assistant turn is still being written (neither completed nor
    /// interrupted). The SPA renders it as pending and polls until it settles, so a
    /// reload / return mid-turn resumes the answer.
    pub streaming: bool,
    /// The human sign-off on this assistant turn, if any (approved |
    /// changes_requested | rejected) — drives the review badge. NULL = unreviewed.
    pub review_decision: Option<String>,
    /// Files the user attached to this (user) message — rendered as thumbnails/chips
    /// under the bubble and in the docs rail. Empty for assistant turns / no attach.
    pub attachments: Vec<AttachmentOut>,
    /// Document (RAG) + web citations anchored to this assistant turn — the "Sources"
    /// list. Returned on load so it survives a reload (otherwise only the live
    /// `chat.citations` frame delivered them). Empty for non-RAG/non-web turns.
    pub citations: Vec<crate::ws::protocol::CitationOut>,
}

#[derive(Serialize)]
pub struct AttachmentOut {
    pub id: Uuid,
    pub filename: String,
    pub mime: String,
    pub byte_size: i64,
}

/// A chat's user/assistant message history (ordered). RBAC: owner, admin, or a
/// project-read grant on the chat's project (same gate as export).
pub async fn list_messages(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
) -> Result<Json<Vec<MessageOut>>> {
    crate::http::export::require_chat_read(&state, &ctx, chat_id).await?;
    // Bounded read: take the newest MSG_CAP messages (DESC + LIMIT) then restore
    // chronological order, so an enormous chat can't exhaust server memory. Older
    // history remains available via export. SPA-side paging can refine this later.
    const MSG_CAP: i64 = 1000;
    let mut rows = sqlx::query!(
        r#"SELECT m.id, m.role::text AS "role!", m.content, m.sequence_number, m.created_at,
                  m.activity, m.groundedness,
                  (m.completed_at IS NULL AND m.interrupted_at IS NULL AND m.role = 'assistant') AS "streaming!",
                  mr.decision::text AS review_decision
           FROM messages m
           LEFT JOIN message_reviews mr ON mr.message_id = m.id
           WHERE m.chat_id = $1 AND m.role IN ('user','assistant')
           ORDER BY m.sequence_number DESC LIMIT $2"#,
        chat_id,
        MSG_CAP
    )
    .fetch_all(&state.pg)
    .await?;
    rows.reverse();

    // Durable attachments for this chat, grouped per message (one query).
    let mut by_msg: std::collections::HashMap<Uuid, Vec<AttachmentOut>> = std::collections::HashMap::new();
    let atts = sqlx::query!(
        "SELECT id, message_id, filename, mime, byte_size FROM chat_attachments \
         WHERE chat_id = $1 AND message_id IS NOT NULL ORDER BY created_at",
        chat_id
    )
    .fetch_all(&state.pg)
    .await?;
    for a in atts {
        if let Some(mid) = a.message_id {
            by_msg.entry(mid).or_default().push(AttachmentOut {
                id: a.id,
                filename: a.filename,
                mime: a.mime,
                byte_size: a.byte_size,
            });
        }
    }

    // Persisted citations for this chat, grouped per message (RAG then web) — so the
    // Sources list survives a reload (it was previously live-frame only).
    let mut by_msg_cit = crate::chat::load_citations(&state.pg, chat_id).await;

    Ok(Json(
        rows.into_iter()
            .map(|r| MessageOut {
                attachments: by_msg.remove(&r.id).unwrap_or_default(),
                citations: by_msg_cit.remove(&r.id).unwrap_or_default(),
                id: r.id,
                role: r.role,
                content: r.content,
                sequence_number: r.sequence_number,
                created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
                activity: r.activity,
                groundedness: r.groundedness,
                streaming: r.streaming,
                review_decision: r.review_decision,
            })
            .collect(),
    ))
}

// --- Rename / delete ---------------------------------------------------------

/// Owner (or admin) may rename/delete their chat.
async fn require_chat_write(state: &AppState, ctx: &AuthContext, chat_id: Uuid) -> Result<()> {
    let owner: Option<Uuid> =
        sqlx::query_scalar!("SELECT owner_user_id FROM chats WHERE id = $1 AND archived_at IS NULL", chat_id)
            .fetch_optional(&state.pg)
            .await?;
    let owner = owner.ok_or_else(|| AppError::Validation("chat not found".into()))?;
    if ctx.user_id == Some(owner) || ctx.is_admin() {
        Ok(())
    } else {
        Err(AppError::Forbidden("not permitted to modify this chat".into()))
    }
}

async fn audit_chat(state: &AppState, ctx: &AuthContext, action: &str, chat_id: Uuid) {
    let mut event = AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("chat".into());
    event.resource_id = Some(chat_id);
    let _ = audit::append(&state.pg, &event).await;
}

#[derive(Deserialize)]
pub struct RenameChat {
    pub title: String,
}

pub async fn rename_chat(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
    Json(body): Json<RenameChat>,
) -> Result<Json<serde_json::Value>> {
    require_chat_write(&state, &ctx, chat_id).await?;
    let title = body.title.trim();
    if title.is_empty() {
        return Err(AppError::Validation("title cannot be empty".into()));
    }
    sqlx::query!("UPDATE chats SET title = $2 WHERE id = $1 AND archived_at IS NULL", chat_id, title)
        .execute(&state.pg)
        .await?;
    audit_chat(&state, &ctx, "chat.renamed", chat_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Soft delete — archive the chat (drops out of the list; history retained).
pub async fn delete_chat(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_chat_write(&state, &ctx, chat_id).await?;
    sqlx::query!("UPDATE chats SET archived_at = now() WHERE id = $1 AND archived_at IS NULL", chat_id)
        .execute(&state.pg)
        .await?;
    audit_chat(&state, &ctx, "chat.deleted", chat_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Share ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ShareChat {
    pub group_chat_id: Uuid,
}

/// Share this chat into a group/DM chat: record a `chat_share` (so the target's
/// members may open it — see `export::require_chat_read`) and post a link message
/// into that chat. You may only share a chat you can read, into a chat you belong
/// to.
pub async fn share_chat(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
    Json(body): Json<ShareChat>,
) -> Result<Json<serde_json::Value>> {
    let (_pid, title) = crate::http::export::require_chat_read(&state, &ctx, chat_id).await?;
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    if !crate::http::messaging::is_member(&state, uid, body.group_chat_id).await? {
        return Err(AppError::Forbidden("not a member of the target chat".into()));
    }
    sqlx::query!(
        "INSERT INTO chat_shares (chat_id, group_chat_id, shared_by) VALUES ($1, $2, $3) \
         ON CONFLICT (chat_id, group_chat_id) DO NOTHING",
        chat_id,
        body.group_chat_id,
        uid,
    )
    .execute(&state.pg)
    .await?;
    let content = format!("Shared a chat: {title}");
    crate::http::messaging::post_chat_link(&state, body.group_chat_id, Some(uid), chat_id, &content, false)
        .await?;
    audit_chat(&state, &ctx, "chat.shared", chat_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize)]
pub struct ChatShareOut {
    pub chat_id: Uuid,
    pub chat_title: String,
    pub group_chat_id: Uuid,
    pub group_chat_name: String,
    pub group_chat_kind: String,
    pub shared_at: String,
}

/// List the chats the caller has shared, so they can review and revoke them
/// (governance — "see/revoke all my shares"). Group names resolve friendly:
/// a DM shows the other member's display name.
pub async fn list_chat_shares(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<ChatShareOut>>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let rows = sqlx::query!(
        r#"SELECT cs.chat_id, c.title AS chat_title, cs.group_chat_id,
                  gc.kind::text AS "kind!",
                  COALESCE(
                    CASE WHEN gc.kind = 'dm' THEN
                      (SELECT u.display_name FROM group_chat_members m
                       JOIN users u ON u.id = m.user_id
                       WHERE m.group_chat_id = gc.id AND m.user_id <> $1 LIMIT 1)
                    ELSE gc.name END,
                    'Chat') AS "group_chat_name!",
                  cs.shared_at
           FROM chat_shares cs
           JOIN chats c ON c.id = cs.chat_id
           JOIN group_chats gc ON gc.id = cs.group_chat_id
           WHERE cs.shared_by = $1
           ORDER BY cs.shared_at DESC"#,
        uid
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ChatShareOut {
                chat_id: r.chat_id,
                chat_title: r.chat_title,
                group_chat_id: r.group_chat_id,
                group_chat_kind: r.kind,
                group_chat_name: r.group_chat_name,
                shared_at: r.shared_at.format(&Rfc3339).unwrap_or_default(),
            })
            .collect(),
    ))
}

/// Revoke a share the caller created — deletes the `chat_shares` row, which cuts
/// access for the target chat's members (see `export::require_chat_read`). Only
/// the original sharer may revoke. The posted link message is left in place
/// (history); opening it afterwards 403s. Audited.
pub async fn revoke_chat_share(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((chat_id, group_chat_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let deleted = sqlx::query!(
        "DELETE FROM chat_shares WHERE chat_id = $1 AND group_chat_id = $2 AND shared_by = $3",
        chat_id,
        group_chat_id,
        uid,
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(AppError::Forbidden("no such share to revoke".into()));
    }
    audit_chat(&state, &ctx, "chat.share.revoked", chat_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}
