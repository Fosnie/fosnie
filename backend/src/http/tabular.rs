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

//! Tabular review REST. Create a review (N
//! workspace documents × M extraction columns), run it (bounded-concurrency
//! cell generation on the background scheduler), read the matrix, export to
//! Excel, and open a review-scoped chat. RBAC: project **write** to create/run,
//! project **read** to view/export.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::rbac::{Permission, ResourceType};
use crate::auth::AuthContext;
use crate::db;
use crate::error::{AppError, Result};
use crate::scheduler::{self, TaskType};
use crate::state::AppState;

/// Upper bound on a review's (documents × columns) cell count — each cell is one
/// background LLM call, so this caps the work a single review can schedule.
const MAX_REVIEW_CELLS: usize = 2000;

// --- RBAC helpers (same pattern as http/documents.rs) ------------------------

async fn project_access(state: &AppState, ctx: &AuthContext, project_id: Uuid, perm: Permission) -> Result<()> {
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
    state.rbac.require(&state.pg, ctx, ResourceType::Project, project_id, perm).await
}

async fn review_project(state: &AppState, review_id: Uuid) -> Result<Uuid> {
    sqlx::query_scalar!("SELECT project_id FROM tabular_reviews WHERE id = $1", review_id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("review not found".into()))
}

// --- Create ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ColumnSpec {
    pub key: String,
    pub name: String,
    #[serde(default = "default_format")]
    pub format: String,
    pub prompt: String,
    /// `stuff` (default) | `per_document_rag` | `map_section`.
    #[serde(default = "default_mechanism")]
    pub mechanism: String,
}

fn default_format() -> String {
    "text".into()
}

fn default_mechanism() -> String {
    "stuff".into()
}

#[derive(Deserialize)]
pub struct CreateReview {
    pub project_id: Uuid,
    pub name: String,
    pub document_ids: Vec<Uuid>,
    pub columns: Vec<ColumnSpec>,
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

pub async fn create_review(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateReview>,
) -> Result<Json<CreatedId>> {
    project_access(&state, &ctx, body.project_id, Permission::Write).await?;
    if let Some(uid) = ctx.user_id {
        if !crate::cache::rate_limit_ok(&state.redis, &format!("review:{uid}"), 20, 60).await {
            return Err(AppError::TooManyRequests("review rate limit; try again shortly".into()));
        }
    }
    if body.columns.is_empty() || body.document_ids.is_empty() {
        return Err(AppError::Validation("a review needs at least one document and column".into()));
    }
    // Bound the work: each (document, column) is one LLM cell. Reject oversized
    // matrices so a single review can't enqueue a runaway number of ML calls.
    let cells = body.document_ids.len().saturating_mul(body.columns.len());
    if cells > MAX_REVIEW_CELLS {
        return Err(AppError::Validation(format!(
            "review too large: {cells} cells exceeds the {MAX_REVIEW_CELLS}-cell limit"
        )));
    }
    // Documents must belong to this project.
    for doc_id in &body.document_ids {
        if crate::documents::project_of(&state.pg, *doc_id).await? != body.project_id {
            return Err(AppError::Validation("a document is not in this project".into()));
        }
    }
    // Source-ACL filter (Enterprise): a document hidden from the caller by an
    // enforced connector ACL cannot be pulled into a review (would leak content).
    // Core's default allows all, so this is a no-op there.
    let allowed = state
        .rbac
        .filter_documents(&state.pg, &ctx, body.project_id, &body.document_ids)
        .await?;
    if body.document_ids.iter().any(|d| !allowed.contains(d)) {
        return Err(AppError::NotFound("a document is not available".into()));
    }

    let id = db::new_id();
    let columns_config = serde_json::json!(body
        .columns
        .iter()
        .map(|c| serde_json::json!({
            "key": c.key, "name": c.name, "format": c.format,
            "prompt": c.prompt, "mechanism": c.mechanism
        }))
        .collect::<Vec<_>>());

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO tabular_reviews (id, project_id, name, columns_config, created_by) \
         VALUES ($1, $2, $3, $4, $5)",
        id, body.project_id, body.name, columns_config, ctx.user_id,
    )
    .execute(&mut *tx)
    .await?;
    // Two UNNEST inserts instead of (1 + docs + docs×cols) round-trips
    // (re-audit §9.3). review_documents: doc ids + positions; cells: the full
    // doc×col matrix built up front.
    let positions: Vec<i32> = (0..body.document_ids.len() as i32).collect();
    sqlx::query!(
        "INSERT INTO tabular_review_documents (review_id, document_id, position) \
         SELECT $1, document_id, position \
         FROM UNNEST($2::uuid[], $3::int4[]) AS t(document_id, position)",
        id,
        &body.document_ids,
        &positions,
    )
    .execute(&mut *tx)
    .await?;

    let mut cell_ids: Vec<Uuid> = Vec::new();
    let mut cell_docs: Vec<Uuid> = Vec::new();
    let mut cell_keys: Vec<String> = Vec::new();
    for doc_id in &body.document_ids {
        for col in &body.columns {
            cell_ids.push(db::new_id());
            cell_docs.push(*doc_id);
            cell_keys.push(col.key.clone());
        }
    }
    sqlx::query!(
        "INSERT INTO tabular_cells (id, review_id, document_id, column_key) \
         SELECT id, $1, document_id, column_key \
         FROM UNNEST($2::uuid[], $3::uuid[], $4::text[]) AS t(id, document_id, column_key)",
        id,
        &cell_ids,
        &cell_docs,
        &cell_keys,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    audit_review(&state, &ctx, "review.created", id, None).await;
    Ok(Json(CreatedId { id }))
}

// --- List + get --------------------------------------------------------------

#[derive(Serialize)]
pub struct ReviewSummary {
    pub id: Uuid,
    pub name: String,
    pub status: String,
}

pub async fn list_reviews(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<ReviewSummary>>> {
    project_access(&state, &ctx, project_id, Permission::Read).await?;
    let rows = sqlx::query!(
        "SELECT id, name, status FROM tabular_reviews WHERE project_id = $1 ORDER BY created_at DESC",
        project_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| ReviewSummary { id: r.id, name: r.name, status: r.status })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct CellOut {
    pub document_id: Uuid,
    pub column_key: String,
    pub status: String,
    pub value: Option<serde_json::Value>,
    pub reasoning: Option<String>,
    pub citations: Option<serde_json::Value>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct DocOut {
    pub id: Uuid,
    pub filename: String,
}

#[derive(Serialize)]
pub struct ReviewDetail {
    pub id: Uuid,
    pub name: String,
    pub status: String,
    pub columns: serde_json::Value,
    pub documents: Vec<DocOut>,
    pub cells: Vec<CellOut>,
}

pub async fn get_review(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(review_id): Path<Uuid>,
) -> Result<Json<ReviewDetail>> {
    let project_id = review_project(&state, review_id).await?;
    project_access(&state, &ctx, project_id, Permission::Read).await?;

    let review = sqlx::query!(
        "SELECT name, status, columns_config FROM tabular_reviews WHERE id = $1",
        review_id
    )
    .fetch_one(&state.pg)
    .await?;
    let doc_rows = sqlx::query!(
        "SELECT d.id, d.original_filename FROM tabular_review_documents trd \
         JOIN documents d ON d.id = trd.document_id \
         WHERE trd.review_id = $1 ORDER BY trd.position",
        review_id
    )
    .fetch_all(&state.pg)
    .await?;
    // Source-ACL filter (Enterprise): omit any review document the caller may not
    // read under an enforced connector ACL, and its cells. Core allows all (no-op).
    let doc_ids: Vec<Uuid> = doc_rows.iter().map(|r| r.id).collect();
    let allowed = state.rbac.filter_documents(&state.pg, &ctx, project_id, &doc_ids).await?;
    let documents = doc_rows
        .into_iter()
        .filter(|r| allowed.contains(&r.id))
        .map(|r| DocOut { id: r.id, filename: r.original_filename })
        .collect();
    let cells = sqlx::query!(
        r#"SELECT document_id, column_key, status::text AS "status!", value, reasoning, citations, error
           FROM tabular_cells WHERE review_id = $1"#,
        review_id
    )
    .fetch_all(&state.pg)
    .await?
    .into_iter()
    .filter(|r| allowed.contains(&r.document_id))
    .map(|r| CellOut {
        document_id: r.document_id,
        column_key: r.column_key,
        status: r.status,
        value: r.value,
        reasoning: r.reasoning,
        citations: r.citations,
        error: r.error,
    })
    .collect();

    Ok(Json(ReviewDetail {
        id: review_id,
        name: review.name,
        status: review.status,
        columns: review.columns_config,
        documents,
        cells,
    }))
}

// --- Run ---------------------------------------------------------------------

pub async fn run_review(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(review_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let project_id = review_project(&state, review_id).await?;
    project_access(&state, &ctx, project_id, Permission::Write).await?;
    sqlx::query!("UPDATE tabular_reviews SET status = 'running' WHERE id = $1", review_id)
        .execute(&state.pg)
        .await?;
    scheduler::enqueue(&state.pg, TaskType::TabularGenerate, serde_json::json!({ "review_id": review_id }))
        .await
        .map_err(AppError::from)?;
    audit_review(&state, &ctx, "review.run", review_id, None).await;
    Ok(Json(serde_json::json!({ "status": "running" })))
}

/// Cell-level interrupt: stop a running generation between cells.
pub async fn cancel_review(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(review_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let project_id = review_project(&state, review_id).await?;
    project_access(&state, &ctx, project_id, Permission::Write).await?;
    state.cancellations.request(review_id);
    sqlx::query!("UPDATE tabular_reviews SET status = 'cancelled' WHERE id = $1", review_id)
        .execute(&state.pg)
        .await?;
    audit_review(&state, &ctx, "review.cancelled", review_id, None).await;
    Ok(Json(serde_json::json!({ "status": "cancelled" })))
}

/// Re-run a single cell (idempotent regeneration of one (document, column)).
pub async fn rerun_cell(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((review_id, document_id, column_key)): Path<(Uuid, Uuid, String)>,
) -> Result<Json<serde_json::Value>> {
    let project_id = review_project(&state, review_id).await?;
    project_access(&state, &ctx, project_id, Permission::Write).await?;
    let n = sqlx::query!(
        "UPDATE tabular_cells SET status = 'pending', value = NULL, reasoning = NULL, \
         citations = NULL, error = NULL, updated_at = now() \
         WHERE review_id = $1 AND document_id = $2 AND column_key = $3",
        review_id, document_id, column_key
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::Validation("no such cell".into()));
    }
    sqlx::query!("UPDATE tabular_reviews SET status = 'running' WHERE id = $1", review_id)
        .execute(&state.pg)
        .await?;
    scheduler::enqueue(
        &state.pg,
        TaskType::TabularGenerate,
        serde_json::json!({ "review_id": review_id, "only": [{ "document_id": document_id, "column_key": column_key }] }),
    )
    .await
    .map_err(AppError::from)?;
    Ok(Json(serde_json::json!({ "status": "running" })))
}

/// Re-run every cell currently in `error`.
pub async fn rerun_errors(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(review_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let project_id = review_project(&state, review_id).await?;
    project_access(&state, &ctx, project_id, Permission::Write).await?;
    let errs = sqlx::query!(
        "SELECT document_id, column_key FROM tabular_cells WHERE review_id = $1 AND status = 'error'",
        review_id
    )
    .fetch_all(&state.pg)
    .await?;
    if errs.is_empty() {
        return Ok(Json(serde_json::json!({ "status": "noop", "reran": 0 })));
    }
    let only: Vec<serde_json::Value> = errs
        .iter()
        .map(|r| serde_json::json!({ "document_id": r.document_id, "column_key": r.column_key }))
        .collect();
    sqlx::query!(
        "UPDATE tabular_cells SET status = 'pending', value = NULL, reasoning = NULL, \
         citations = NULL, error = NULL, updated_at = now() WHERE review_id = $1 AND status = 'error'",
        review_id
    )
    .execute(&state.pg)
    .await?;
    sqlx::query!("UPDATE tabular_reviews SET status = 'running' WHERE id = $1", review_id)
        .execute(&state.pg)
        .await?;
    let reran = only.len();
    scheduler::enqueue(
        &state.pg,
        TaskType::TabularGenerate,
        serde_json::json!({ "review_id": review_id, "only": only }),
    )
    .await
    .map_err(AppError::from)?;
    Ok(Json(serde_json::json!({ "status": "running", "reran": reran })))
}

// --- Export ------------------------------------------------------------------

pub async fn export_review(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(review_id): Path<Uuid>,
) -> Result<Response> {
    let project_id = review_project(&state, review_id).await?;
    project_access(&state, &ctx, project_id, Permission::Read).await?;

    let review = sqlx::query!(
        "SELECT name, columns_config FROM tabular_reviews WHERE id = $1",
        review_id
    )
    .fetch_one(&state.pg)
    .await?;
    // Columns for the exporter: [{key, name}].
    let columns: serde_json::Value = serde_json::json!(review
        .columns_config
        .as_array()
        .map(|a| a
            .iter()
            .map(|c| serde_json::json!({
                "key": c.get("key").and_then(|v| v.as_str()).unwrap_or(""),
                "name": c.get("name").and_then(|v| v.as_str()).unwrap_or("")
            }))
            .collect::<Vec<_>>())
        .unwrap_or_default());

    // Rows: one per document, cells keyed by column.
    let docs = sqlx::query!(
        "SELECT d.id, d.original_filename FROM tabular_review_documents trd \
         JOIN documents d ON d.id = trd.document_id WHERE trd.review_id = $1 ORDER BY trd.position",
        review_id
    )
    .fetch_all(&state.pg)
    .await?;
    let cells = sqlx::query!(
        "SELECT document_id, column_key, value FROM tabular_cells WHERE review_id = $1",
        review_id
    )
    .fetch_all(&state.pg)
    .await?;
    let mut rows = Vec::with_capacity(docs.len());
    for d in &docs {
        let mut cell_map = serde_json::Map::new();
        for c in cells.iter().filter(|c| c.document_id == d.id) {
            cell_map.insert(c.column_key.clone(), c.value.clone().unwrap_or(serde_json::Value::Null));
        }
        rows.push(serde_json::json!({ "document": d.original_filename, "cells": cell_map }));
    }

    let out_path = std::env::temp_dir()
        .join(format!("pai_review_{review_id}.xlsx"))
        .to_string_lossy()
        .to_string();
    crate::ml::export_review(
        &state.http,
        &state.boot.ml.base_url,
        &review.name,
        &columns,
        &serde_json::json!(rows),
        &out_path,
    )
    .await?;
    let bytes = tokio::fs::read(&out_path)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read xlsx: {e}")))?;
    let _ = tokio::fs::remove_file(&out_path).await;

    audit_review(&state, &ctx, "review.exported", review_id, None).await;

    let disposition = format!("attachment; filename=\"{}.xlsx\"", review.name.replace(['"', '/', '\\'], "_"));
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        Body::from(bytes),
    )
        .into_response())
}

// --- Review-scoped chat ------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct CreateReviewChat {
    /// The Agent the chat runs under (should enable `read_table_cells`).
    #[serde(default)]
    pub agent_id: Option<Uuid>,
}

#[derive(Serialize)]
pub struct CreatedChat {
    pub chat_id: Uuid,
}

pub async fn create_review_chat(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(review_id): Path<Uuid>,
    body: Option<Json<CreateReviewChat>>,
) -> Result<Json<CreatedChat>> {
    let agent_id = body.and_then(|Json(b)| b.agent_id);
    let project_id = review_project(&state, review_id).await?;
    project_access(&state, &ctx, project_id, Permission::Read).await?;
    let owner = ctx
        .user_id
        .ok_or_else(|| AppError::Forbidden("a chat needs a user owner".into()))?;
    let chat_id = db::new_id();
    sqlx::query!(
        "INSERT INTO chats (id, owner_user_id, project_id, agent_id, tabular_review_id, title) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        chat_id, owner, project_id, agent_id, review_id, "Tabular review chat",
    )
    .execute(&state.pg)
    .await?;
    Ok(Json(CreatedChat { chat_id }))
}

async fn audit_review(state: &AppState, ctx: &AuthContext, action: &str, review_id: Uuid, payload: Option<serde_json::Value>) {
    let mut event = AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("tabular_review".into());
    event.resource_id = Some(review_id);
    event.payload = payload;
    let _ = audit::append(&state.pg, &event).await;
}
