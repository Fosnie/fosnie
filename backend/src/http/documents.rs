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

//! Workspace-document REST. Upload
//! creates v1; every edit/accept/reject appends a retained version. Tracked
//! changes are proposed by the `edit_document` tool (see `crate::tools`) and
//! accepted/rejected here. RBAC: project **write** to upload/delete/resolve,
//! project **read** to view/download. All access is audited.

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::rbac::{Permission, ResourceType};
use crate::auth::AuthContext;
use crate::documents;
use crate::error::{AppError, Result};
use crate::ext::DocAccess;
use crate::state::AppState;

const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

// --- RBAC helpers ------------------------------------------------------------

async fn require_project_write(state: &AppState, ctx: &AuthContext, project_id: Uuid) -> Result<()> {
    let owner: Option<Uuid> = sqlx::query_scalar!(
        "SELECT owner_user_id FROM projects WHERE id = $1 AND archived_at IS NULL",
        project_id
    )
    .fetch_optional(&state.pg)
    .await?;
    let owner = owner.ok_or_else(|| AppError::Validation("project not found".into()))?;
    if ctx.user_id == Some(owner) || ctx.is_admin() {
        return Ok(());
    }
    state.rbac.require(&state.pg, ctx, ResourceType::Project, project_id, Permission::Write).await
}

async fn require_project_read(state: &AppState, ctx: &AuthContext, project_id: Uuid) -> Result<()> {
    let owner: Option<Uuid> = sqlx::query_scalar!(
        "SELECT owner_user_id FROM projects WHERE id = $1 AND archived_at IS NULL",
        project_id
    )
    .fetch_optional(&state.pg)
    .await?;
    let owner = owner.ok_or_else(|| AppError::Validation("project not found".into()))?;
    if ctx.user_id == Some(owner) || ctx.is_admin() {
        return Ok(());
    }
    state.rbac.require(&state.pg, ctx, ResourceType::Project, project_id, Permission::Read).await
}

/// A source-ACL denial (Enterprise ACL-inheritance seam, [`DocAccess::Denied`])
/// surfaces as **404** — a document hidden by an enforced source ACL must not have
/// its existence leaked by a 403. Core's default seam always returns `Full`, so
/// this is a no-op there (behaviour byte-identical). Checked *after* the project
/// gate in both document guards below.
async fn ensure_document_access(
    state: &AppState,
    ctx: &AuthContext,
    project_id: Uuid,
    doc_id: Uuid,
) -> Result<()> {
    if state.rbac.document_access(&state.pg, ctx, project_id, doc_id).await? == DocAccess::Denied {
        return Err(AppError::NotFound("document not found".into()));
    }
    Ok(())
}

/// Edit access to a document = write access to its project (no special role), plus
/// the source-ACL entitlement (Enterprise) — a denied source ACL hides the document.
async fn require_document_edit(state: &AppState, ctx: &AuthContext, doc_id: Uuid) -> Result<Uuid> {
    let project_id = documents::project_of(&state.pg, doc_id).await?;
    require_project_write(state, ctx, project_id).await?;
    ensure_document_access(state, ctx, project_id, doc_id).await?;
    Ok(project_id)
}

async fn require_document_read(state: &AppState, ctx: &AuthContext, doc_id: Uuid) -> Result<Uuid> {
    let project_id = documents::project_of(&state.pg, doc_id).await?;
    require_project_read(state, ctx, project_id).await?;
    ensure_document_access(state, ctx, project_id, doc_id).await?;
    Ok(project_id)
}

// --- Upload / list / get -----------------------------------------------------

#[derive(Deserialize)]
pub struct UploadQuery {
    pub filename: String,
    #[serde(default)]
    pub mime: Option<String>,
}

#[derive(Serialize)]
pub struct UploadedDoc {
    pub document_id: Uuid,
    pub version_id: Uuid,
}

pub async fn upload_workspace_document(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path(project_id): Path<Uuid>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<Json<UploadedDoc>> {
    require_project_write(&state, &ctx, project_id).await?;
    crate::cache::rate_limit_guard(&state.redis, &format!("upload:{}", ctx.user_id.unwrap_or_default()), 20, 60).await?;
    crate::upload::ensure_supported_document(&q.filename)?;
    let (document_id, version_id) =
        documents::create_document(&state, &ctx, project_id, &q.filename, q.mime.as_deref(), &body)
            .await?;
    Ok(Json(UploadedDoc { document_id, version_id }))
}

#[derive(Serialize)]
pub struct DocSummary {
    pub id: Uuid,
    pub original_filename: String,
    pub mime: Option<String>,
    pub current_version_id: Option<Uuid>,
}

pub async fn list_workspace_documents(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<DocSummary>>> {
    require_project_read(&state, &ctx, project_id).await?;
    let rows = sqlx::query!(
        "SELECT id, original_filename, mime, current_version_id FROM documents \
         WHERE project_id = $1 AND deleted_at IS NULL ORDER BY created_at DESC",
        project_id
    )
    .fetch_all(&state.pg)
    .await?;
    // Source-ACL filter (Enterprise): drop any document the caller may not read
    // under an enforced connector ACL. Core's default returns every id (no-op).
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let allowed = state.rbac.filter_documents(&state.pg, &ctx, project_id, &ids).await?;
    Ok(Json(
        rows.into_iter()
            .filter(|r| allowed.contains(&r.id))
            .map(|r| DocSummary {
                id: r.id,
                original_filename: r.original_filename,
                mime: r.mime,
                current_version_id: r.current_version_id,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct VersionOut {
    pub id: Uuid,
    pub version_number: i32,
    pub source: String,
    pub byte_size: Option<i64>,
    pub has_pdf: bool,
}

#[derive(Serialize)]
pub struct DocDetail {
    pub id: Uuid,
    pub original_filename: String,
    pub mime: Option<String>,
    pub current_version_id: Option<Uuid>,
    pub versions: Vec<VersionOut>,
    pub pending_edits: i64,
}

pub async fn get_document(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path(doc_id): Path<Uuid>,
) -> Result<Json<DocDetail>> {
    require_document_read(&state, &ctx, doc_id).await?;
    let doc = sqlx::query!(
        "SELECT original_filename, mime, current_version_id FROM documents \
         WHERE id = $1 AND deleted_at IS NULL",
        doc_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("document not found".into()))?;
    let versions = sqlx::query!(
        r#"SELECT id, version_number, source::text AS "source!", byte_size, pdf_path
           FROM document_versions WHERE document_id = $1 ORDER BY version_number"#,
        doc_id
    )
    .fetch_all(&state.pg)
    .await?;
    let pending: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM document_edits WHERE document_id = $1 AND status = 'pending'"#,
        doc_id
    )
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(DocDetail {
        id: doc_id,
        original_filename: doc.original_filename,
        mime: doc.mime,
        current_version_id: doc.current_version_id,
        versions: versions
            .into_iter()
            .map(|v| VersionOut {
                id: v.id,
                version_number: v.version_number,
                source: v.source,
                byte_size: v.byte_size,
                has_pdf: v.pdf_path.is_some(),
            })
            .collect(),
        pending_edits: pending,
    }))
}

// --- Version bytes / text / pdf ---------------------------------------------

async fn version_bytes_path(pool: &sqlx::PgPool, workspace_dir: &str, doc_id: Uuid, ver_id: Uuid) -> Result<String> {
    let stored = sqlx::query_scalar!(
        "SELECT bytes_path FROM document_versions WHERE id = $1 AND document_id = $2",
        ver_id, doc_id
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Validation("version not found".into()))?;
    // Resolve the stored relative suffix to an absolute path;
    // a legacy absolute row passes through unchanged until backfill.
    Ok(crate::storage::resolve_file(workspace_dir, &stored).to_string_lossy().to_string())
}

pub async fn download_version(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path((doc_id, ver_id)): Path<(Uuid, Uuid)>,
) -> Result<Response> {
    require_document_read(&state, &ctx, doc_id).await?;
    let path = version_bytes_path(&state.pg, &state.boot.storage.workspace_dir, doc_id, ver_id).await?;
    let safe = crate::upload::ensure_within_storage(&state.boot.storage.workspace_dir, &path)?;
    let bytes = tokio::fs::read(&safe)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read version bytes: {e}")))?;
    let filename: String =
        sqlx::query_scalar!("SELECT original_filename FROM documents WHERE id = $1", doc_id)
            .fetch_one(&state.pg)
            .await?;
    audit_doc(&state, &ctx, "document.accessed", doc_id, Some(serde_json::json!({ "version_id": ver_id }))).await;
    Ok(bytes_response(bytes, DOCX_MIME, &filename))
}

pub async fn version_text(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path((doc_id, ver_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    require_document_read(&state, &ctx, doc_id).await?;
    let path = version_bytes_path(&state.pg, &state.boot.storage.workspace_dir, doc_id, ver_id).await?;
    crate::upload::ensure_within_storage(&state.boot.storage.workspace_dir, &path)?;
    let text = crate::ml::read_document(&state.http, &state.boot.ml.base_url, &path, Some(DOCX_MIME), None, crate::ml::provider_overrides(&state, ctx.user_id).await).await?;
    audit_doc(&state, &ctx, "document.accessed", doc_id, Some(serde_json::json!({ "version_id": ver_id, "as": "text" }))).await;
    Ok(Json(serde_json::json!({ "text": text })))
}

pub async fn render_version_pdf(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path((doc_id, ver_id)): Path<(Uuid, Uuid)>,
) -> Result<Response> {
    require_document_read(&state, &ctx, doc_id).await?;
    let docx_path = version_bytes_path(&state.pg, &state.boot.storage.workspace_dir, doc_id, ver_id).await?;
    let safe_docx = crate::upload::ensure_within_storage(&state.boot.storage.workspace_dir, &docx_path)?;

    // Already a PDF upload → serve the stored bytes directly (no LibreOffice
    // rendition needed; that path 503s on hosts without LibreOffice).
    if docx_path.to_lowercase().ends_with(".pdf") {
        let bytes = tokio::fs::read(&safe_docx)
            .await
            .map_err(|e| AppError::Other(anyhow::anyhow!("read pdf: {e}")))?;
        audit_doc(&state, &ctx, "document.accessed", doc_id, Some(serde_json::json!({ "version_id": ver_id, "as": "pdf" }))).await;
        return Ok(bytes_response(bytes, "application/pdf", &format!("{ver_id}.pdf")));
    }

    // Serve a cached rendition if present, else render once and cache pdf_path.
    let cached: Option<String> =
        sqlx::query_scalar!("SELECT pdf_path FROM document_versions WHERE id = $1", ver_id)
            .fetch_one(&state.pg)
            .await?;
    let pdf_path = match cached {
        Some(p) if tokio::fs::try_exists(&p).await.unwrap_or(false) => p,
        _ => {
            let out_dir = std::path::Path::new(&docx_path)
                .parent()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".into());
            let p = crate::ml::render_pdf(&state.http, &state.boot.ml.base_url, &docx_path, &out_dir).await?;
            sqlx::query!("UPDATE document_versions SET pdf_path = $1 WHERE id = $2", p, ver_id)
                .execute(&state.pg)
                .await?;
            p
        }
    };
    let safe_pdf = crate::upload::ensure_within_storage(&state.boot.storage.workspace_dir, &pdf_path)?;
    let bytes = tokio::fs::read(&safe_pdf)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read pdf: {e}")))?;
    audit_doc(&state, &ctx, "document.accessed", doc_id, Some(serde_json::json!({ "version_id": ver_id, "as": "pdf" }))).await;
    Ok(bytes_response(bytes, "application/pdf", &format!("{ver_id}.pdf")))
}

pub async fn delete_document(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path(doc_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let project_id = require_document_edit(&state, &ctx, doc_id).await?;
    // A legal hold (on the document or its project) beats deletion — refuse with
    // 409 and audit the blocked attempt.
    if state.retention.is_held(&state, project_id, doc_id).await? {
        audit_doc(&state, &ctx, "document.delete.blocked", doc_id,
            Some(serde_json::json!({ "reason": "legal_hold" }))).await;
        return Err(AppError::Conflict("document is under legal hold".into()));
    }
    // Soft-delete and emit the `document.deleted` domain event atomically
    // (transactional outbox).
    let mut tx = state.pg.begin().await?;
    sqlx::query!("UPDATE documents SET deleted_at = now() WHERE id = $1 AND deleted_at IS NULL", doc_id)
        .execute(&mut *tx)
        .await?;
    let ev = crate::events::NewEvent::new(
        crate::events::DOCUMENT_DELETED,
        crate::events::ActorType::Human,
    )
    .actor(ctx.user_id)
    .resource("document", doc_id)
    .project(Some(project_id));
    crate::events::emit_with(&mut tx, &ev).await?;
    tx.commit().await?;
    audit_doc(&state, &ctx, "document.deleted", doc_id, None).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Tracked changes: list + accept/reject ----------------------------------

#[derive(Deserialize)]
pub struct EditsQuery {
    /// `pending` (default), `accepted`, `rejected`, or `all`.
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Serialize)]
pub struct EditOut {
    pub id: Uuid,
    pub w_id: String,
    pub author: String,
    pub find_text: Option<String>,
    pub replace_text: Option<String>,
    pub status: String,
}

pub async fn list_edits(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path(doc_id): Path<Uuid>,
    Query(q): Query<EditsQuery>,
) -> Result<Json<Vec<EditOut>>> {
    require_document_read(&state, &ctx, doc_id).await?;
    let status = q.status.as_deref().unwrap_or("pending");
    let rows = sqlx::query!(
        r#"SELECT id, w_id, author::text AS "author!", find_text, replace_text, status::text AS "status!"
           FROM document_edits
           WHERE document_id = $1 AND ($2 = 'all' OR status::text = $2)
           ORDER BY created_at"#,
        doc_id, status
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| EditOut {
                id: r.id,
                w_id: r.w_id,
                author: r.author,
                find_text: r.find_text,
                replace_text: r.replace_text,
                status: r.status,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct ResolvedOut {
    pub version_id: Uuid,
    pub version_number: i32,
}

async fn resolve_one(state: &AppState, ctx: &AuthContext, doc_id: Uuid, w_id: &str, action: &str) -> Result<Json<ResolvedOut>> {
    require_document_edit(state, ctx, doc_id).await?;
    // The change must be pending on this document.
    let exists: Option<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM document_edits WHERE document_id = $1 AND w_id = $2 AND status = 'pending'",
        doc_id, w_id
    )
    .fetch_optional(&state.pg)
    .await?;
    if exists.is_none() {
        return Err(AppError::Validation("no pending change with that id".into()));
    }

    let cur = documents::current_version(&state.pg, &state.boot.storage.workspace_dir, doc_id).await?;
    let base = cur.version_id; // optimistic concurrency anchor
    let out = resolved_tmp_path(&cur.bytes_path);
    crate::ml::resolve_tracked_change(&state.http, &state.boot.ml.base_url, &cur.bytes_path, &out, w_id, action).await?;
    let bytes = read_and_cleanup(&out).await?;

    let source = if action == "accept" { "user_accept" } else { "user_reject" };
    // CAS on the version pointer: a concurrent resolve that advanced the
    // document first loses here (409), so two accepts cannot fork the version.
    let (version_id, version_number) =
        documents::add_version_cas(state, ctx, doc_id, source, &bytes, ctx.user_id, base).await?;

    let new_status = if action == "accept" { "accepted" } else { "rejected" };
    sqlx::query!(
        "UPDATE document_edits SET status = ($3::text)::edit_status, resolved_by = $4, resolved_at = now() \
         WHERE document_id = $1 AND w_id = $2 AND status = 'pending'",
        doc_id, w_id, new_status, ctx.user_id
    )
    .execute(&state.pg)
    .await?;

    let act = if action == "accept" { "document.edit.accepted" } else { "document.edit.rejected" };
    audit_doc(state, ctx, act, doc_id, Some(serde_json::json!({ "w_id": w_id, "version_id": version_id }))).await;
    Ok(Json(ResolvedOut { version_id, version_number }))
}

pub async fn accept_edit(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path((doc_id, w_id)): Path<(Uuid, String)>,
) -> Result<Json<ResolvedOut>> {
    resolve_one(&state, &ctx, doc_id, &w_id, "accept").await
}

pub async fn reject_edit(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path((doc_id, w_id)): Path<(Uuid, String)>,
) -> Result<Json<ResolvedOut>> {
    resolve_one(&state, &ctx, doc_id, &w_id, "reject").await
}

#[derive(Deserialize)]
pub struct ResolveAllQuery {
    /// Optional `w:author` filter (e.g. the branded assistant actor).
    #[serde(default)]
    pub author: Option<String>,
}

async fn resolve_all(state: &AppState, ctx: &AuthContext, doc_id: Uuid, action: &str, author: Option<&str>) -> Result<Json<ResolvedOut>> {
    require_document_edit(state, ctx, doc_id).await?;
    let cur = documents::current_version(&state.pg, &state.boot.storage.workspace_dir, doc_id).await?;
    let base = cur.version_id; // optimistic concurrency anchor
    let out = resolved_tmp_path(&cur.bytes_path);
    let resolved = crate::ml::resolve_all_tracked_changes(
        &state.http, &state.boot.ml.base_url, &cur.bytes_path, &out, action, author,
    )
    .await?;
    let bytes = read_and_cleanup(&out).await?;

    let source = if action == "accept" { "user_accept" } else { "user_reject" };
    let (version_id, version_number) =
        documents::add_version_cas(state, ctx, doc_id, source, &bytes, ctx.user_id, base).await?;

    let new_status = if action == "accept" { "accepted" } else { "rejected" };
    sqlx::query!(
        "UPDATE document_edits SET status = ($3::text)::edit_status, resolved_by = $4, resolved_at = now() \
         WHERE document_id = $1 AND w_id = ANY($2) AND status = 'pending'",
        doc_id, &resolved, new_status, ctx.user_id
    )
    .execute(&state.pg)
    .await?;

    let act = if action == "accept" { "document.edit.accepted" } else { "document.edit.rejected" };
    audit_doc(state, ctx, act, doc_id, Some(serde_json::json!({ "resolved": resolved, "version_id": version_id }))).await;
    Ok(Json(ResolvedOut { version_id, version_number }))
}

pub async fn accept_all_edits(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path(doc_id): Path<Uuid>,
    Query(q): Query<ResolveAllQuery>,
) -> Result<Json<ResolvedOut>> {
    resolve_all(&state, &ctx, doc_id, "accept", q.author.as_deref()).await
}

pub async fn reject_all_edits(
    State(state): State<AppState>,
    crate::auth::keycloak::AuthUser(ctx): crate::auth::keycloak::AuthUser,
    Path(doc_id): Path<Uuid>,
    Query(q): Query<ResolveAllQuery>,
) -> Result<Json<ResolvedOut>> {
    resolve_all(&state, &ctx, doc_id, "reject", q.author.as_deref()).await
}

// --- helpers -----------------------------------------------------------------

fn resolved_tmp_path(src: &str) -> String {
    let ext = std::path::Path::new(src).extension().and_then(|e| e.to_str()).unwrap_or("docx");
    std::env::temp_dir()
        .join(format!("pai_resolve_{}.{ext}", Uuid::now_v7()))
        .to_string_lossy()
        .to_string()
}

async fn read_and_cleanup(path: &str) -> Result<Vec<u8>> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read resolved docx: {e}")))?;
    let _ = tokio::fs::remove_file(path).await;
    Ok(bytes)
}

fn bytes_response(bytes: Vec<u8>, content_type: &str, filename: &str) -> Response {
    let disposition = format!("attachment; filename=\"{}\"", filename.replace('"', ""));
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        Body::from(bytes),
    )
        .into_response()
}

async fn audit_doc(state: &AppState, ctx: &AuthContext, action: &str, doc_id: Uuid, payload: Option<serde_json::Value>) {
    let mut event = AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("document".into());
    event.resource_id = Some(doc_id);
    event.payload = payload;
    let _ = audit::append(&state.pg, &event).await;
}
