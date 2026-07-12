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

//! Legal-workspace documents — version-pinned, first-class, retained.
//! DISTINCT from the unversioned RAG
//! `knowledge_docs`: every edit/accept/reject appends a new retained version
//! with a `source` provenance, so a citation against an old version stays valid.
//!
//! This module is the shared substrate: the REST surface (`http::documents`) and
//! the `edit_document` tool both go through [`create_document`] / [`add_version`].
//! File bytes live on disk (`{workspace_dir}/{doc_id}/{version_id}{ext}`); the DB
//! holds the version chain + provenance.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::AuthContext;
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Absolute root for workspace document storage (the configured dir may be relative).
pub fn workspace_root(state: &AppState) -> Result<PathBuf> {
    Ok(crate::storage::resolve_dir(&state.boot.storage.workspace_dir))
}

/// Lower-cased file extension including the dot (e.g. `.docx`), or empty.
pub fn ext_of(filename: &str) -> String {
    Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_lowercase()))
        .unwrap_or_default()
}

/// On-disk path for one version's bytes.
pub fn version_path(root: &Path, doc_id: Uuid, ver_id: Uuid, ext: &str) -> PathBuf {
    root.join(doc_id.to_string()).join(format!("{ver_id}{ext}"))
}

async fn next_version_number(pool: &sqlx::PgPool, doc_id: Uuid) -> Result<i32> {
    let n: i32 = sqlx::query_scalar!(
        r#"SELECT COALESCE(MAX(version_number), 0) + 1 AS "n!" FROM document_versions WHERE document_id = $1"#,
        doc_id
    )
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Write one version's bytes to the absolute `version_path` and return the
/// **category-relative** suffix (`<doc_id>/<ver_id><ext>`) to store in the DB, so
/// `document_versions.bytes_path` is install-location independent.
/// `category_dir` is the configured `workspace_dir` (used only to strip the prefix).
async fn write_bytes(root: &Path, category_dir: &str, doc_id: Uuid, ver_id: Uuid, ext: &str, bytes: &[u8]) -> Result<String> {
    let path = version_path(root, doc_id, ver_id, ext);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("create workspace dir: {e}")))?;
    }
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("write version bytes: {e}")))?;
    Ok(crate::storage::relativise(category_dir, &path))
}

/// Create a new workspace document with its v1 (`source = 'user_upload'`).
/// Returns `(document_id, version_id)`.
pub async fn create_document(
    state: &AppState,
    ctx: &AuthContext,
    project_id: Uuid,
    filename: &str,
    mime: Option<&str>,
    bytes: &[u8],
) -> Result<(Uuid, Uuid)> {
    create_document_with(state, ctx, project_id, filename, mime, bytes, "user_upload", None).await
}

/// Create a workspace document with an explicit v1 provenance `source` and an
/// optional `parent_document_id` (document lineage — e.g. an email attachment
/// pointing at its email-markdown parent). [`create_document`] is the
/// `("user_upload", None)` special case; connector import passes
/// `"connector_import"` and, for attachments, the parent email document id.
/// Returns `(document_id, version_id)`.
#[allow(clippy::too_many_arguments)]
pub async fn create_document_with(
    state: &AppState,
    ctx: &AuthContext,
    project_id: Uuid,
    filename: &str,
    mime: Option<&str>,
    bytes: &[u8],
    source: &str,
    parent_document_id: Option<Uuid>,
) -> Result<(Uuid, Uuid)> {
    let doc_id = db::new_id();
    let ver_id = db::new_id();
    let root = workspace_root(state)?;
    let ext = ext_of(filename);
    let bytes_path = write_bytes(&root, &state.boot.storage.workspace_dir, doc_id, ver_id, &ext, bytes).await?;

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO documents (id, project_id, original_filename, mime, created_by, parent_document_id) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        doc_id, project_id, filename, mime, ctx.user_id, parent_document_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO document_versions \
         (id, document_id, version_number, source, bytes_path, byte_size, created_by) \
         VALUES ($1, $2, 1, ($3::text)::doc_source, $4, $5, $6)",
        ver_id, doc_id, source, bytes_path, bytes.len() as i64, ctx.user_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("UPDATE documents SET current_version_id = $1 WHERE id = $2", ver_id, doc_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    audit_doc(state, ctx, "document.uploaded", doc_id, None).await;
    audit_doc(state, ctx, "document.version.created", doc_id, Some(serde_json::json!({
        "version_id": ver_id, "version_number": 1, "source": source
    }))).await;
    Ok((doc_id, ver_id))
}

/// Append a new version to an existing document and make it current. Returns
/// `(version_id, version_number)`. The shared extension point for the
/// `edit_document` tool and accept/reject.
pub async fn add_version(
    state: &AppState,
    ctx: &AuthContext,
    doc_id: Uuid,
    source: &str,
    bytes: &[u8],
    created_by: Option<Uuid>,
) -> Result<(Uuid, i32)> {
    let filename: String =
        sqlx::query_scalar!("SELECT original_filename FROM documents WHERE id = $1", doc_id)
            .fetch_optional(&state.pg)
            .await?
            .ok_or_else(|| AppError::Validation("document not found".into()))?;
    let ext = ext_of(&filename);
    let ver_id = db::new_id();
    let n = next_version_number(&state.pg, doc_id).await?;
    let root = workspace_root(state)?;
    let bytes_path = write_bytes(&root, &state.boot.storage.workspace_dir, doc_id, ver_id, &ext, bytes).await?;

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO document_versions \
         (id, document_id, version_number, source, bytes_path, byte_size, created_by) \
         VALUES ($1, $2, $3, ($4::text)::doc_source, $5, $6, $7)",
        ver_id, doc_id, n, source, bytes_path, bytes.len() as i64, created_by,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("UPDATE documents SET current_version_id = $1 WHERE id = $2", ver_id, doc_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    audit_doc(state, ctx, "document.version.created", doc_id, Some(serde_json::json!({
        "version_id": ver_id, "version_number": n, "source": source
    }))).await;
    Ok((ver_id, n))
}

/// Like [`add_version`] but optimistically guarded: the new version is only made
/// current if `documents.current_version_id` still equals `expected_current`.
/// If another writer advanced the pointer first (concurrent accept/reject), the
/// transaction rolls back — no orphan version row — and a `Conflict` is returned
/// so the caller can reload and retry. This closes the tracked-changes
/// accept/reject race (two resolves against the same base version). A bytes file
/// may be left on disk on conflict (rare); it is unreferenced and harmless.
pub async fn add_version_cas(
    state: &AppState,
    ctx: &AuthContext,
    doc_id: Uuid,
    source: &str,
    bytes: &[u8],
    created_by: Option<Uuid>,
    expected_current: Uuid,
) -> Result<(Uuid, i32)> {
    let filename: String =
        sqlx::query_scalar!("SELECT original_filename FROM documents WHERE id = $1", doc_id)
            .fetch_optional(&state.pg)
            .await?
            .ok_or_else(|| AppError::Validation("document not found".into()))?;
    let ext = ext_of(&filename);
    let ver_id = db::new_id();
    let n = next_version_number(&state.pg, doc_id).await?;
    let root = workspace_root(state)?;
    let bytes_path = write_bytes(&root, &state.boot.storage.workspace_dir, doc_id, ver_id, &ext, bytes).await?;

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO document_versions \
         (id, document_id, version_number, source, bytes_path, byte_size, created_by) \
         VALUES ($1, $2, $3, ($4::text)::doc_source, $5, $6, $7)",
        ver_id, doc_id, n, source, bytes_path, bytes.len() as i64, created_by,
    )
    .execute(&mut *tx)
    .await?;
    let res = sqlx::query!(
        "UPDATE documents SET current_version_id = $1 \
         WHERE id = $2 AND current_version_id = $3",
        ver_id, doc_id, expected_current,
    )
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        tx.rollback().await?;
        return Err(AppError::Conflict("document changed; reload and retry".into()));
    }
    tx.commit().await?;

    audit_doc(state, ctx, "document.version.created", doc_id, Some(serde_json::json!({
        "version_id": ver_id, "version_number": n, "source": source
    }))).await;
    Ok((ver_id, n))
}

/// The current version's identity + bytes path.
pub struct CurrentVersion {
    pub version_id: Uuid,
    pub version_number: i32,
    pub bytes_path: String,
    pub mime: Option<String>,
}

pub async fn current_version(pool: &sqlx::PgPool, workspace_dir: &str, doc_id: Uuid) -> Result<CurrentVersion> {
    let row = sqlx::query!(
        r#"SELECT dv.id, dv.version_number, dv.bytes_path, d.mime
           FROM documents d JOIN document_versions dv ON dv.id = d.current_version_id
           WHERE d.id = $1 AND d.deleted_at IS NULL"#,
        doc_id
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Validation("document not found".into()))?;
    Ok(CurrentVersion {
        version_id: row.id,
        version_number: row.version_number,
        // Resolve the stored relative suffix to an absolute path;
        // legacy absolute rows pass through unchanged until backfill.
        bytes_path: crate::storage::resolve_file(workspace_dir, &row.bytes_path)
            .to_string_lossy()
            .to_string(),
        mime: row.mime,
    })
}

/// The project a document belongs to (for RBAC scoping). Errors if absent/deleted.
pub async fn project_of(pool: &sqlx::PgPool, doc_id: Uuid) -> Result<Uuid> {
    sqlx::query_scalar!(
        "SELECT project_id FROM documents WHERE id = $1 AND deleted_at IS NULL",
        doc_id
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Validation("document not found".into()))
}

async fn audit_doc(
    state: &AppState,
    ctx: &AuthContext,
    action: &str,
    doc_id: Uuid,
    payload: Option<serde_json::Value>,
) {
    let mut event = AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("document".into());
    event.resource_id = Some(doc_id);
    event.payload = payload;
    let _ = audit::append(&state.pg, &event).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_of_handles_common_cases() {
        assert_eq!(ext_of("Contract.DOCX"), ".docx");
        assert_eq!(ext_of("report.pdf"), ".pdf");
        assert_eq!(ext_of("no_extension"), "");
        assert_eq!(ext_of("archive.tar.gz"), ".gz");
    }

    #[test]
    fn version_path_is_doc_then_version() {
        let root = Path::new("/data/workspace");
        let doc = Uuid::nil();
        let ver = Uuid::nil();
        let p = version_path(root, doc, ver, ".docx");
        assert!(p.ends_with(format!("{doc}/{ver}.docx")), "got {}", p.display());
    }
}
