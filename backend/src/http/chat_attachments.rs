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

//! Per-turn chat attachments. Upload a file → extract its text (ML) → stage the
//! text in Redis under a one-shot key with a short TTL. The chat turn consumes
//! the staged text for that turn only (see `chat::run_turn`); nothing is indexed.
//! On a code-interpreter host the raw bytes are staged too (under a size cap) so the
//! sandbox can read the original file; otherwise only the text is kept.

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use deadpool_redis::redis;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::chat::Attachment;
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;

const TTL_SECS: i64 = 3600;
const MAX_TEXT_CHARS: usize = 100_000;
/// Upper bound on the raw bytes staged for the code interpreter (per attachment).
/// Above this we keep text only — the sandbox just won't get that file.
const MAX_INPUT_BYTES: usize = 10 * 1024 * 1024;
/// Upper bound on an image staged for inline vision. Matches the Anthropic
/// per-image ceiling (~5 MB); over this the image is kept as OCR text only.
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Deserialize)]
pub struct UploadQuery {
    pub filename: String,
    #[serde(default)]
    pub mime: Option<String>,
}

#[derive(Serialize)]
pub struct UploadedAttachment {
    pub id: Uuid,
    pub filename: String,
    pub chars: usize,
}

#[derive(Serialize, Deserialize)]
struct Staged {
    owner_user_id: Option<Uuid>,
    filename: String,
    text: String,
    /// base64 of the original bytes — staged for the code interpreter (sandbox file)
    /// and/or for inline image vision, under the relevant size cap. `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes_b64: Option<String>,
    /// MIME type as reported by the client (e.g. `image/jpeg`). Drives the inline
    /// vision path at turn time. `None` when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mime: Option<String>,
}

pub async fn upload_attachment(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<Json<UploadedAttachment>> {
    crate::upload::ensure_supported_document(&q.filename)?;
    crate::cache::rate_limit_guard(&state.redis, &format!("upload:{}", ctx.user_id.unwrap_or_default()), 20, 60).await?;
    // Write the bytes durably (storage.chat_attachments_dir) — the file is both the
    // ML extractor's input AND the backing store served back to render the attachment
    // under the message / in the docs rail. A DB row points at it; the chat/message
    // link is backfilled when the turn persists. Orphans (never sent) are pruned.
    let id = db::new_id();
    let safe_name = q.filename.replace(['/', '\\'], "_");
    let dir = crate::storage::resolve_dir(&state.boot.storage.chat_attachments_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("create dir: {e}")))?;
    // Store the RELATIVE suffix; the ML read below gets the resolved absolute path.
    let rel = format!("{id}__{safe_name}");
    let path = dir.join(&rel);
    tokio::fs::write(&path, &body)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("write attachment: {e}")))?;
    let path_str = path.to_string_lossy().to_string();

    // Whole-document read at upload time — no task is known yet, so this is a
    // plain stuff read (the per-turn injection caps it to MAX_TEXT_CHARS below).
    let text_res =
        crate::ml::read_document(&state.http, &state.boot.ml.base_url, &path_str, q.mime.as_deref(), None, crate::ml::provider_overrides(&state, ctx.user_id).await).await;
    let mut text = match text_res {
        Ok(t) => t,
        Err(e) => {
            // Extraction failed → the durable file is useless without metadata; drop it.
            let _ = tokio::fs::remove_file(&path).await;
            return Err(e);
        }
    };
    if text.chars().count() > MAX_TEXT_CHARS {
        text = text.chars().take(MAX_TEXT_CHARS).collect();
    }
    let chars = text.chars().count();

    // Keep the raw bytes when either consumer can use them:
    //  - the code interpreter, where it can actually run (host capability), under its cap;
    //  - inline image vision, for any `image/*` within the (smaller) per-image cap.
    let is_image = q.mime.as_deref().is_some_and(|m| m.starts_with("image/"));
    let keep_for_ci = state.boot.features.code_interpreter && body.len() <= MAX_INPUT_BYTES;
    let keep_for_vision = is_image && body.len() <= MAX_IMAGE_BYTES;
    let bytes_b64 = (keep_for_ci || keep_for_vision).then(|| B64.encode(body.as_ref()));
    let staged = Staged {
        owner_user_id: ctx.user_id,
        filename: q.filename.clone(),
        text,
        bytes_b64,
        mime: q.mime.clone(),
    };
    let payload = serde_json::to_string(&staged).map_err(|e| AppError::Other(anyhow::anyhow!("serialize: {e}")))?;
    let mut conn = state.redis.get().await.map_err(|e| AppError::Other(anyhow::anyhow!("redis: {e}")))?;
    let _: () = redis::cmd("SETEX")
        .arg(format!("chatattach:{id}"))
        .arg(TTL_SECS)
        .arg(payload)
        .query_async(&mut conn)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("redis SETEX: {e}")))?;

    // Durable pointer row (chat_id/message_id NULL until the turn links it).
    let mime = q.mime.clone().unwrap_or_else(|| "application/octet-stream".into());
    sqlx::query!(
        "INSERT INTO chat_attachments (id, owner_user_id, filename, mime, byte_size, disk_path) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        ctx.user_id,
        q.filename,
        mime,
        body.len() as i64,
        rel,
    )
    .execute(&state.pg)
    .await?;

    Ok(Json(UploadedAttachment { id, filename: q.filename, chars }))
}

/// Consume staged attachments for a turn: fetch by id, verify owner, delete the
/// key (one-shot), return their (filename, text). Unknown / foreign / expired ids
/// are silently skipped.
pub async fn take_attachments(state: &AppState, ctx: &AuthContext, ids: &[Uuid]) -> Vec<Attachment> {
    if ids.is_empty() {
        return Vec::new();
    }
    let mut conn = match state.redis.get().await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for id in ids {
        let key = format!("chatattach:{id}");
        let raw: Option<String> = redis::cmd("GET").arg(&key).query_async(&mut conn).await.unwrap_or(None);
        let _: i64 = redis::cmd("DEL").arg(&key).query_async(&mut conn).await.unwrap_or(0);
        if let Some(raw) = raw {
            if let Ok(s) = serde_json::from_str::<Staged>(&raw) {
                if s.owner_user_id == ctx.user_id {
                    let bytes = s.bytes_b64.as_deref().and_then(|b| B64.decode(b).ok());
                    out.push(Attachment { id: *id, filename: s.filename, text: s.text, bytes, mime: s.mime });
                }
            }
        }
    }
    out
}

/// Serve a durable chat attachment's bytes for inline display / download. Access:
/// admin, the uploader, or anyone with chat-read on the attachment's chat. An
/// orphan (never sent → chat_id NULL) is owner-only.
pub async fn download(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Response> {
    let row = sqlx::query!(
        "SELECT chat_id, owner_user_id, filename, mime, disk_path FROM chat_attachments WHERE id = $1",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("attachment not found".into()))?;

    let is_owner = row.owner_user_id.is_some() && row.owner_user_id == ctx.user_id;
    let allowed = ctx.is_admin()
        || is_owner
        || match row.chat_id {
            Some(cid) => crate::http::export::require_chat_read(&state, &ctx, cid).await.is_ok(),
            None => false,
        };
    if !allowed {
        return Err(AppError::Forbidden("not permitted to access this attachment".into()));
    }

    let abs = crate::storage::resolve_file(&state.boot.storage.chat_attachments_dir, &row.disk_path);
    let bytes = tokio::fs::read(&abs)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read attachment: {e}")))?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, row.mime),
            (header::CONTENT_DISPOSITION, format!("inline; filename=\"{}\"", row.filename)),
        ],
        Body::from(bytes),
    )
        .into_response())
}

/// Prune orphan attachments (uploaded but never sent → `message_id` still NULL)
/// older than `older_than`. Deletes the disk file then the row. Returns the count
/// removed. Called from the periodic scheduler task.
pub async fn prune_orphans(state: &AppState, older_than: time::Duration) -> Result<u64> {
    let cutoff = time::OffsetDateTime::now_utc() - older_than;
    let rows = sqlx::query!(
        "SELECT id, disk_path FROM chat_attachments WHERE message_id IS NULL AND created_at < $1",
        cutoff
    )
    .fetch_all(&state.pg)
    .await?;
    let mut removed = 0u64;
    for r in rows {
        let abs = crate::storage::resolve_file(&state.boot.storage.chat_attachments_dir, &r.disk_path);
        let _ = tokio::fs::remove_file(&abs).await;
        let _ = sqlx::query!("DELETE FROM chat_attachments WHERE id = $1", r.id)
            .execute(&state.pg)
            .await;
        removed += 1;
    }
    Ok(removed)
}
