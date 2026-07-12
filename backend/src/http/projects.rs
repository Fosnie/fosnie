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

//! Project + Project Knowledge + document-upload REST (behind the Keycloak
//! Bearer layer + `AuthUser`). Upload writes bytes to disk, records the doc,
//! and enqueues an `ingest` task on the durable queue (the scheduler does the
//! extract→chunk→embed→upsert).

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::keycloak::AuthUser;
use crate::auth::rbac::{Permission, ResourceType};
use crate::auth::{AuthContext, PlatformRole};
use crate::db;
use crate::kb;
use crate::error::{AppError, Result};
use crate::scheduler::{self, TaskType};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateProject {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub sector: Option<String>, // "general" | "legal"
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

pub async fn create_project(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateProject>,
) -> Result<Json<CreatedId>> {
    let owner = ctx
        .user_id
        .ok_or_else(|| AppError::Forbidden("a project needs a user owner".into()))?;
    let id = db::new_id();
    let sector = match body.sector.as_deref() {
        Some("legal") => "legal",
        _ => "general",
    };
    sqlx::query!(
        "INSERT INTO projects (id, name, description, owner_user_id, sector) \
         VALUES ($1, $2, $3, $4, ($5::text)::project_sector)",
        id,
        body.name,
        body.description,
        owner,
        sector,
    )
    .execute(&state.pg)
    .await?;

    // Every Project gets its team chat (channels-messaging: the project chat).
    let _ = crate::http::messaging::ensure_project_chat(&state, id, owner).await;
    Ok(Json(CreatedId { id }))
}

/// Archive (soft-delete) a project. Recoverable: sets `archived_at`, so it drops
/// out of every list (which all filter `archived_at IS NULL`) but the data + audit
/// trail are kept. Owner or admin only.
pub async fn delete_project(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let owner: Option<Uuid> = sqlx::query_scalar!(
        "SELECT owner_user_id FROM projects WHERE id = $1 AND archived_at IS NULL",
        project_id
    )
    .fetch_optional(&state.pg)
    .await?;
    let owner = owner.ok_or_else(|| AppError::Validation("project not found".into()))?;
    if ctx.user_id != Some(owner) && !ctx.is_admin() {
        return Err(AppError::Forbidden(
            "only the project owner or an admin may archive a project".into(),
        ));
    }
    sqlx::query!(
        "UPDATE projects SET archived_at = now() WHERE id = $1 AND archived_at IS NULL",
        project_id
    )
    .execute(&state.pg)
    .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize)]
pub struct ProjectSummary {
    pub id: Uuid,
    pub name: String,
    pub sector: String,
    pub description: Option<String>,
}

/// Projects the caller can see: admin → all; else owned ∪ projects with a
/// Read grant (to the user or one of their groups). Mirrors `rbac::can`.
pub async fn list_projects(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<ProjectSummary>>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let out: Vec<ProjectSummary> = if ctx.is_admin() {
        sqlx::query!(
            r#"SELECT id, name, sector::text AS "sector!", description
               FROM projects WHERE archived_at IS NULL ORDER BY created_at DESC"#
        )
        .fetch_all(&state.pg)
        .await?
        .into_iter()
        .map(|r| ProjectSummary { id: r.id, name: r.name, sector: r.sector, description: r.description })
        .collect()
    } else {
        sqlx::query!(
            r#"SELECT id, name, sector::text AS "sector!", description
               FROM projects p WHERE p.archived_at IS NULL AND (
                   p.owner_user_id = $1
                   OR EXISTS (
                       SELECT 1 FROM access_grants g
                       WHERE g.resource_type = 'project' AND g.resource_id = p.id
                         AND g.permission = 'read'
                         AND ( (g.principal_type = 'user'  AND g.principal_id = $1)
                            OR (g.principal_type = 'group' AND g.principal_id IN
                                  (SELECT group_id FROM group_members WHERE user_id = $1)) )
                   )
               ) ORDER BY p.created_at DESC"#,
            uid
        )
        .fetch_all(&state.pg)
        .await?
        .into_iter()
        .map(|r| ProjectSummary { id: r.id, name: r.name, sector: r.sector, description: r.description })
        .collect()
    };
    Ok(Json(out))
}

#[derive(Serialize)]
pub struct CreatedKnowledge {
    pub id: Uuid,
    pub embedding_model_id: String,
    pub embedding_dimension: i32,
}

/// Ensure the Project's default ("Project Knowledge") KB exists, returning its
/// id + embedding facts. A thin shim over `kb::ensure_project_kb` so the existing
/// Project-Knowledge UI keeps working over the standalone-KB model.
pub async fn create_knowledge(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<CreatedKnowledge>> {
    require_project_write(&state, &ctx, project_id).await?;
    let kb_id = kb::ensure_project_kb(&state, &ctx, project_id).await?;
    let row = sqlx::query!(
        "SELECT embedding_model_id, embedding_dimension FROM knowledge_bases WHERE id = $1",
        kb_id
    )
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(CreatedKnowledge {
        id: kb_id,
        embedding_model_id: row.embedding_model_id,
        embedding_dimension: row.embedding_dimension,
    }))
}

#[derive(Deserialize)]
pub struct UploadQuery {
    pub filename: String,
    #[serde(default)]
    pub mime: Option<String>,
}

#[derive(Serialize)]
pub struct UploadedDoc {
    pub doc_id: Uuid,
    pub status: String,
}

pub async fn upload_document(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(project_id): Path<Uuid>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<Json<UploadedDoc>> {
    require_project_write(&state, &ctx, project_id).await?;
    crate::upload::ensure_supported_document(&q.filename)?;

    // The Project's default KB (auto-created on first use over the new model).
    let kb_id = kb::ensure_project_kb(&state, &ctx, project_id).await?;

    let doc_id = db::new_id();
    let safe_name = q.filename.replace(['/', '\\'], "_");
    // Store the RELATIVE suffix (`<doc_id>__<safe_name>`); reads resolve against
    // `documents_dir` (the ML service is handed the resolved absolute path).
    let dir = crate::storage::resolve_dir(&state.boot.storage.documents_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("create documents dir: {e}")))?;
    let bytes_path = format!("{doc_id}__{safe_name}");
    tokio::fs::write(dir.join(&bytes_path), &body)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("write document: {e}")))?;

    sqlx::query!(
        "INSERT INTO kb_documents \
         (id, kb_id, original_filename, mime, bytes_path, ingest_status, created_by) \
         VALUES ($1, $2, $3, $4, $5, 'uploaded', $6)",
        doc_id,
        kb_id,
        q.filename,
        q.mime,
        bytes_path,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;

    scheduler::enqueue(&state.pg, TaskType::Ingest, json!({ "doc_id": doc_id }))
        .await
        .map_err(AppError::from)?;

    Ok(Json(UploadedDoc {
        doc_id,
        status: "uploaded".into(),
    }))
}

async fn require_project_write(
    state: &AppState,
    ctx: &AuthContext,
    project_id: Uuid,
) -> Result<()> {
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

#[derive(Serialize)]
pub struct ProjectKnowledgeEntry {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_name: String,
    pub status: String,
}

/// Directory of Project-Knowledge bases (for binding to an Agent). Power-user+.
pub async fn list_project_knowledge(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<ProjectKnowledgeEntry>>> {
    if !matches!(
        ctx.role,
        PlatformRole::PowerUser | PlatformRole::ClientAdmin | PlatformRole::SuperAdmin
    ) {
        return Err(AppError::Forbidden("only a power user or admin may list knowledge bases".into()));
    }
    // Directory of all Knowledge Bases (project + libraries) an Agent may bind as
    // RAG scope. `project_id`/`project_name` carry the KB's own id+name when it is
    // a standalone Library (no origin project).
    let rows = sqlx::query!(
        r#"SELECT kb.id, COALESCE(kb.origin_project_id, kb.id) AS "project_id!",
                  kb.name AS project_name, kb.status::text AS "status!"
           FROM knowledge_bases kb
           WHERE kb.archived_at IS NULL ORDER BY kb.name"#
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ProjectKnowledgeEntry { id: r.id, project_id: r.project_id, project_name: r.project_name, status: r.status })
            .collect(),
    ))
}

async fn require_project_read(
    state: &AppState,
    ctx: &AuthContext,
    project_id: Uuid,
) -> Result<()> {
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

#[derive(Serialize)]
pub struct KnowledgeSource {
    pub filename: String,
    pub mime: Option<String>,
    pub text: String,
}

/// Extracted text of a Project-Knowledge source document — used to resolve a
/// chat citation (which carries the `knowledge_docs` id) back to its origin so
/// the UI can show the quote in context. RBAC: project read.
pub async fn knowledge_doc_source(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(doc_id): Path<Uuid>,
) -> Result<Json<KnowledgeSource>> {
    let row = sqlx::query!(
        "SELECT original_filename, mime, bytes_path, kb_id FROM kb_documents WHERE id = $1",
        doc_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("knowledge document not found".into()))?;

    // A citation resolves to a KB document the caller retrieved from; gate on KB read.
    kb::require_read(&state.pg, &ctx, row.kb_id).await?;
    let doc_abs = crate::storage::resolve_file(&state.boot.storage.documents_dir, &row.bytes_path);
    let text =
        crate::ml::read_document(&state.http, &state.boot.ml.base_url, &doc_abs.to_string_lossy(), row.mime.as_deref(), None, crate::ml::provider_overrides(&state, ctx.user_id).await)
            .await?;
    Ok(Json(KnowledgeSource { filename: row.original_filename, mime: row.mime, text }))
}

#[derive(Serialize)]
pub struct KnowledgeOut {
    pub id: Uuid,
    pub status: String,
}

#[derive(Serialize)]
pub struct KnowledgeDocOut {
    pub id: Uuid,
    pub filename: String,
    pub mime: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct ProjectDocs {
    pub knowledge: Option<KnowledgeOut>,
    pub documents: Vec<KnowledgeDocOut>,
}

/// A project's knowledge base (if created) + its documents with ingestion status.
pub async fn list_documents(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<ProjectDocs>> {
    require_project_read(&state, &ctx, project_id).await?;
    // The Project's default ("Project Knowledge") KB and its documents.
    let kb = sqlx::query!(
        r#"SELECT id, status::text AS "status!" FROM knowledge_bases
           WHERE origin_project_id = $1 AND visibility = 'project' AND archived_at IS NULL
           ORDER BY created_at LIMIT 1"#,
        project_id
    )
    .fetch_optional(&state.pg)
    .await?;
    let Some(kb) = kb else {
        return Ok(Json(ProjectDocs { knowledge: None, documents: vec![] }));
    };
    let rows = sqlx::query!(
        r#"SELECT id, original_filename, mime, ingest_status::text AS "status!", created_at
           FROM kb_documents WHERE kb_id = $1 ORDER BY created_at DESC"#,
        kb.id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(ProjectDocs {
        knowledge: Some(KnowledgeOut { id: kb.id, status: kb.status }),
        documents: rows
            .into_iter()
            .map(|r| KnowledgeDocOut {
                id: r.id,
                filename: r.original_filename,
                mime: r.mime,
                status: r.status,
                created_at: r.created_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
            })
            .collect(),
    }))
}
