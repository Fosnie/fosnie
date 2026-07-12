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

//! Persisted attachments for group/DM chat messages. Upload stores the bytes on
//! disk + a `message_attachments` row; the message itself keeps a compact
//! `[{ id, filename, mime }]` list in its `attachments` JSONB. Download is gated
//! by membership of a group chat whose message references the attachment (or being
//! the uploader / an admin). Unlike per-turn `chat_attachments` (Redis, one-shot),
//! these are durable so a chat's images/files keep rendering.

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::keycloak::AuthUser;
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;

const MAX_BYTES: usize = 15 * 1024 * 1024; // 15 MB

#[derive(Deserialize)]
pub struct UploadQuery {
    pub filename: String,
    #[serde(default)]
    pub mime: Option<String>,
}

#[derive(Serialize)]
pub struct Uploaded {
    pub id: Uuid,
    pub filename: String,
    pub mime: String,
}

/// Resolve the attachments dir to an absolute path (config value may be relative).
fn attach_dir(state: &AppState) -> std::path::PathBuf {
    crate::storage::resolve_dir(&state.boot.storage.message_attachments_dir)
}

pub async fn upload(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<Json<Uploaded>> {
    if body.is_empty() {
        return Err(AppError::Validation("empty attachment".into()));
    }
    if body.len() > MAX_BYTES {
        return Err(AppError::Validation("attachment too large (max 15 MB)".into()));
    }
    crate::upload::ensure_supported_document(&q.filename)?;
    let id = db::new_id();
    let mime = q.mime.clone().unwrap_or_else(|| "application/octet-stream".into());
    let safe = q.filename.replace(['/', '\\'], "_");
    let dir = attach_dir(&state);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("create dir: {e}")))?;
    // Store the RELATIVE suffix under `message_attachments_dir`.
    let rel = format!("{id}__{safe}");
    tokio::fs::write(dir.join(&rel), &body)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("write attachment: {e}")))?;
    sqlx::query!(
        "INSERT INTO message_attachments (id, uploaded_by, filename, mime, byte_size, disk_path) \
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
    Ok(Json(Uploaded { id, filename: q.filename, mime }))
}

pub async fn download(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Response> {
    let row = sqlx::query!(
        "SELECT uploaded_by, filename, mime, disk_path FROM message_attachments WHERE id = $1",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("attachment not found".into()))?;

    // Access: admin, the uploader, or a member of a group chat whose message
    // references this attachment id.
    let allowed = ctx.is_admin()
        || (row.uploaded_by.is_some() && row.uploaded_by == ctx.user_id)
        || match ctx.user_id {
            Some(uid) => sqlx::query_scalar!(
                r#"SELECT EXISTS(
                     SELECT 1 FROM group_chat_messages gm
                     JOIN group_chat_members m ON m.group_chat_id = gm.group_chat_id
                     WHERE m.user_id = $1 AND gm.attachments @> $2
                   ) AS "e!""#,
                uid,
                serde_json::json!([{ "id": id }])
            )
            .fetch_one(&state.pg)
            .await?,
            None => false,
        };
    if !allowed {
        return Err(AppError::Forbidden("not permitted to access this attachment".into()));
    }

    let abs = crate::storage::resolve_file(&state.boot.storage.message_attachments_dir, &row.disk_path);
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
