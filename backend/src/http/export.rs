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

//! Export. Three role-gated types: (1) user chat export
//! (MD/JSON/PDF), (2) compliance/audit evidence package (hash-chain + a
//! verification result), (3) administrative project-DB export (Postgres-scoped
//! JSON). Qdrant vectors are NEVER exported (embedding-inversion risk). Every
//! export is written to the audit hash-chain.
//!
//! Two paths share one set of *builders*: the synchronous endpoints (small
//! exports, bytes returned inline) and an **async job** (`POST /api/exports` →
//! durable task → `run_export` builds the file to disk → download link) for
//! large exports.
//!
//! The compliance/audit evidence endpoints (live audit query, evidence packs,
//! checkpoints, erasure) live in the sibling [`super::audit_export`] module
//! (part of Fosnie Enterprise); the async `audit` export kind still reuses its
//! [`super::audit_export::build_audit_export`] builder.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::rbac::{Permission, ResourceType};
use crate::auth::AuthContext;
use crate::db;
use crate::error::{AppError, Result};
use crate::scheduler::{self, TaskType};
use crate::state::AppState;

pub async fn audit_export(state: &AppState, ctx: &AuthContext, phase: &str, kind: &str, id: Option<Uuid>) {
    let mut ev = AuditEvent::action(format!("export.{phase}"), ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("export".into());
    ev.resource_id = id;
    ev.payload = Some(json!({ "kind": kind }));
    let _ = audit::append(&state.pg, &ev).await;
}

pub fn require_admin(ctx: &AuthContext) -> Result<()> {
    if ctx.is_admin() || ctx.break_glass {
        Ok(())
    } else {
        Err(AppError::Forbidden("admin only".into()))
    }
}

// --- builders (shared by the sync endpoints and the async worker) ------------

/// A built export: the bytes, their MIME type, and a download filename.
pub struct ExportFile {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub filename: String,
}

/// Builds the file for one **non-Core** export kind (the async-job builder). Registered
/// at boot via [`crate::state::AppStateBuilder::with_export_kinds`]; the Core registry
/// is empty, so e.g. the Enterprise `audit` evidence export is supplied by
/// `fosnie-enterprise`. Mirrors the scheduler [`JobRegistry`](crate::scheduler::JobRegistry).
#[async_trait::async_trait]
pub trait ExportKindHandler: Send + Sync {
    async fn build(&self, state: &AppState, target_id: Option<Uuid>) -> Result<ExportFile>;
}

/// Registry of non-Core export kinds (kind → builder). Empty in Core; a private
/// `fosnie-enterprise` crate registers `audit` → its `build_audit_export`.
#[derive(Default, Clone)]
pub struct ExportRegistry {
    handlers: std::collections::HashMap<String, std::sync::Arc<dyn ExportKindHandler>>,
}

impl ExportRegistry {
    /// Register a builder for an export `kind`.
    pub fn register(&mut self, kind: &str, handler: std::sync::Arc<dyn ExportKindHandler>) {
        self.handlers.insert(kind.to_string(), handler);
    }
    /// The builder registered for `kind`, if any.
    pub fn get(&self, kind: &str) -> Option<std::sync::Arc<dyn ExportKindHandler>> {
        self.handlers.get(kind).cloned()
    }
}

pub fn to_vec(v: &serde_json::Value) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(v).map_err(|e| AppError::Other(anyhow::anyhow!("serialise export: {e}")))
}

/// Build a chat transcript export in `md` | `json` | `pdf`.
pub async fn build_chat_export(state: &AppState, chat_id: Uuid, format: &str) -> Result<ExportFile> {
    let title: String = sqlx::query_scalar!("SELECT title FROM chats WHERE id = $1", chat_id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("chat not found".into()))?;
    let messages = sqlx::query!(
        r#"SELECT role::text AS "role!", content, sequence_number
           FROM messages WHERE chat_id = $1 AND role IN ('user','assistant')
           ORDER BY sequence_number"#,
        chat_id
    )
    .fetch_all(&state.pg)
    .await?;

    let mut transcript = format!("# {title}\n\n");
    for m in &messages {
        transcript.push_str(&format!("## {}\n\n{}\n\n", m.role, m.content));
    }

    match format {
        "json" => {
            let msgs: Vec<_> = messages
                .iter()
                .map(|m| json!({ "role": m.role, "content": m.content, "seq": m.sequence_number }))
                .collect();
            let bytes = to_vec(&json!({ "chat_id": chat_id, "title": title, "messages": msgs }))?;
            Ok(ExportFile { bytes, mime: "application/json".into(), filename: format!("{}.json", safe(&title)) })
        }
        "md" => Ok(ExportFile {
            bytes: transcript.into_bytes(),
            mime: "text/markdown".into(),
            filename: format!("{}.md", safe(&title)),
        }),
        "pdf" => {
            let out_path = std::env::temp_dir()
                .join(format!("pai_chat_{chat_id}.pdf"))
                .to_string_lossy()
                .to_string();
            let (path, mime) = crate::ml::generate_artefact(
                &state.http, &state.boot.ml.base_url, "pdf", &title, &transcript, &out_path,
            )
            .await?;
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("read export pdf: {e}")))?;
            let _ = tokio::fs::remove_file(&path).await;
            Ok(ExportFile { bytes, mime, filename: format!("{}.pdf", safe(&title)) })
        }
        other => Err(AppError::Validation(format!("unknown export format: {other}"))),
    }
}

/// Build the administrative project-DB export (Postgres-scoped JSON; no vectors).
/// `viewer` is the admin the export runs *as* — documents hidden from them by an
/// enforced connector ACL are omitted (ТЗ #4 D5: with `acl.admin_override=false` a
/// strict-walls firm's admin does not receive walled documents even in an export).
/// Core's default seam allows all, so a Core-only export is byte-identical.
pub async fn build_project_db_export(
    state: &AppState,
    viewer: &AuthContext,
    project_id: Uuid,
) -> Result<ExportFile> {
    let project = sqlx::query!(
        r#"SELECT id, name, description, sector::text AS "sector!", created_at FROM projects WHERE id = $1"#,
        project_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("project not found".into()))?;

    let chats = sqlx::query!(
        "SELECT id, title, owner_user_id, created_at FROM chats WHERE project_id = $1",
        project_id
    )
    .fetch_all(&state.pg)
    .await?;
    let chat_ids: Vec<Uuid> = chats.iter().map(|c| c.id).collect();

    let messages = sqlx::query!(
        r#"SELECT id, chat_id, role::text AS "role!", content, sequence_number
           FROM messages WHERE chat_id = ANY($1) ORDER BY chat_id, sequence_number"#,
        &chat_ids
    )
    .fetch_all(&state.pg)
    .await?;
    let memory = sqlx::query!(
        "SELECT id, content, pinned FROM memory_facts WHERE owner_project_id = $1",
        project_id
    )
    .fetch_all(&state.pg)
    .await?;
    let all_documents = sqlx::query!(
        "SELECT id, original_filename, mime, current_version_id FROM documents WHERE project_id = $1 AND deleted_at IS NULL",
        project_id
    )
    .fetch_all(&state.pg)
    .await?;
    // Source-ACL filter (Enterprise): exclude documents the viewer may not read
    // under an enforced connector ACL. Core allows all — no rows dropped.
    let doc_ids: Vec<Uuid> = all_documents.iter().map(|d| d.id).collect();
    let allowed = state.rbac.filter_documents(&state.pg, viewer, project_id, &doc_ids).await?;
    let documents: Vec<_> = all_documents.into_iter().filter(|d| allowed.contains(&d.id)).collect();
    let excluded_documents = doc_ids.len().saturating_sub(documents.len());
    let reviews = sqlx::query!(
        "SELECT id, name, status FROM tabular_reviews WHERE project_id = $1",
        project_id
    )
    .fetch_all(&state.pg)
    .await?;

    let dump = json!({
        "project": { "id": project.id, "name": project.name, "description": project.description,
                     "sector": project.sector, "created_at": project.created_at.to_string() },
        "chats": chats.iter().map(|c| json!({ "id": c.id, "title": c.title, "owner_user_id": c.owner_user_id, "created_at": c.created_at.to_string() })).collect::<Vec<_>>(),
        "messages": messages.iter().map(|m| json!({ "id": m.id, "chat_id": m.chat_id, "role": m.role, "content": m.content, "seq": m.sequence_number })).collect::<Vec<_>>(),
        "memory_facts": memory.iter().map(|m| json!({ "id": m.id, "content": m.content, "pinned": m.pinned })).collect::<Vec<_>>(),
        "documents": documents.iter().map(|d| json!({ "id": d.id, "filename": d.original_filename, "mime": d.mime, "current_version_id": d.current_version_id })).collect::<Vec<_>>(),
        "documents_excluded_by_source_acl": excluded_documents,
        "tabular_reviews": reviews.iter().map(|r| json!({ "id": r.id, "name": r.name, "status": r.status })).collect::<Vec<_>>(),
        "note": "Qdrant vectors are intentionally NOT exported (embedding-inversion risk — §B.19)."
    });
    let bytes = to_vec(&dump)?;
    Ok(ExportFile { bytes, mime: "application/json".into(), filename: format!("{}.json", safe(&project.name)) })
}

// --- 1. User chat export (synchronous) ---------------------------------------

#[derive(Deserialize)]
pub struct ChatExportQuery {
    #[serde(default = "default_format")]
    pub format: String, // md | json | pdf
}

fn default_format() -> String {
    "md".into()
}

pub async fn require_chat_read(state: &AppState, ctx: &AuthContext, chat_id: Uuid) -> Result<(Option<Uuid>, String)> {
    let row = sqlx::query!(
        "SELECT owner_user_id, project_id, title FROM chats WHERE id = $1",
        chat_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("chat not found".into()))?;
    if ctx.user_id == Some(row.owner_user_id) || ctx.is_admin() {
        return Ok((row.project_id, row.title));
    }
    // Project members can read a project-grounded chat.
    if let Some(pid) = row.project_id {
        if state.rbac.can(&state.pg, ctx, ResourceType::Project, pid, Permission::Read).await? {
            return Ok((row.project_id, row.title));
        }
    }
    // A chat shared into a group/DM is readable by that chat's members (the
    // "open the shared chat" link). DMs are group_chats too, so this covers both.
    if let Some(uid) = ctx.user_id {
        let shared: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS(
                 SELECT 1 FROM chat_shares cs
                 JOIN group_chat_members m ON m.group_chat_id = cs.group_chat_id
                 WHERE cs.chat_id = $1 AND m.user_id = $2
               ) AS "e!""#,
            chat_id,
            uid
        )
        .fetch_one(&state.pg)
        .await?;
        if shared {
            return Ok((row.project_id, row.title));
        }
    }
    Err(AppError::Forbidden("not permitted to read this chat".into()))
}

pub async fn export_chat(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
    Query(q): Query<ChatExportQuery>,
) -> Result<Response> {
    require_chat_read(&state, &ctx, chat_id).await?;
    audit_export(&state, &ctx, "requested", "chat", Some(chat_id)).await;
    let f = build_chat_export(&state, chat_id, &q.format).await?;
    audit_export(&state, &ctx, "completed", "chat", Some(chat_id)).await;
    Ok(text_response(f.bytes, &f.mime, &f.filename))
}

// --- 3. Administrative project-DB export (synchronous, admin) ----------------

pub async fn export_project_db(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Response> {
    require_admin(&ctx)?;
    audit_export(&state, &ctx, "requested", "project_db", Some(project_id)).await;
    let f = build_project_db_export(&state, &ctx, project_id).await?;
    audit_export(&state, &ctx, "completed", "project_db", Some(project_id)).await;
    Ok(text_response(f.bytes, &f.mime, &f.filename))
}

// --- async export jobs (large exports) ---------------------------------------

#[derive(Deserialize)]
pub struct CreateExport {
    pub kind: String, // chat | project_db | audit
    #[serde(default)]
    pub target_id: Option<Uuid>,
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Serialize)]
pub struct ExportRow {
    pub id: Uuid,
    pub kind: String,
    pub status: String,
    pub format: String,
    pub target_id: Option<Uuid>,
    pub filename: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// Most exports a single user may have in flight at once (each can be a heavy
/// full-DB / audit-chain traversal).
const MAX_INFLIGHT_EXPORTS: i64 = 3;

/// Queue an export to run in the background; returns the job id to poll. RBAC is
/// enforced here exactly as for the synchronous endpoints.
pub async fn create_export(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateExport>,
) -> Result<Json<serde_json::Value>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    if !crate::cache::rate_limit_ok(&state.redis, &format!("export:{uid}"), 10, 60).await {
        return Err(AppError::TooManyRequests("export rate limit; try again shortly".into()));
    }
    // Bound concurrent work per user (an export can be an expensive traversal).
    let inflight = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM exports WHERE requested_by = $1 AND completed_at IS NULL"#,
        uid
    )
    .fetch_one(&state.pg)
    .await?;
    if inflight >= MAX_INFLIGHT_EXPORTS {
        return Err(AppError::Validation(format!(
            "too many exports in progress ({inflight}); wait for one to finish"
        )));
    }
    let format = body.format.clone().unwrap_or_else(|| "json".into());
    match body.kind.as_str() {
        "chat" => {
            let tid = body.target_id.ok_or_else(|| AppError::Validation("target_id (chat) required".into()))?;
            require_chat_read(&state, &ctx, tid).await?;
            if !matches!(format.as_str(), "md" | "json" | "pdf") {
                return Err(AppError::Validation("format must be md|json|pdf".into()));
            }
        }
        "project_db" => {
            require_admin(&ctx)?;
            body.target_id.ok_or_else(|| AppError::Validation("target_id (project) required".into()))?;
        }
        "audit" => {
            require_admin(&ctx)?;
            crate::http::require_capability(&state, &ctx, "compliance_audit", "compliance/audit").await?;
        }
        other => return Err(AppError::Validation(format!("unknown export kind: {other}"))),
    }

    let id = db::new_id();
    sqlx::query!(
        "INSERT INTO exports (id, requested_by, kind, target_id, format) \
         VALUES ($1, $2, ($3::text)::export_kind, $4, $5)",
        id, uid, body.kind, body.target_id, format,
    )
    .execute(&state.pg)
    .await?;
    scheduler::enqueue(&state.pg, TaskType::Export, json!({ "export_id": id }))
        .await
        .map_err(AppError::from)?;
    audit_export(&state, &ctx, "requested", &body.kind, body.target_id).await;
    Ok(Json(json!({ "id": id, "status": "queued" })))
}

pub async fn list_exports(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<ExportRow>>> {
    let uid = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let rows = sqlx::query!(
        r#"SELECT id, kind::text AS "kind!", status::text AS "status!", format, target_id,
                  filename, error, created_at, completed_at
           FROM exports WHERE requested_by = $1 ORDER BY created_at DESC LIMIT 100"#,
        uid
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(rows.into_iter().map(|r| ExportRow {
        id: r.id, kind: r.kind, status: r.status, format: r.format, target_id: r.target_id,
        filename: r.filename, error: r.error,
        created_at: r.created_at.to_string(), completed_at: r.completed_at.map(|t| t.to_string()),
    }).collect()))
}

/// Owner of the export, or an admin.
async fn require_export_access(state: &AppState, ctx: &AuthContext, id: Uuid) -> Result<Uuid> {
    let requester: Option<Uuid> =
        sqlx::query_scalar!("SELECT requested_by FROM exports WHERE id = $1", id)
            .fetch_optional(&state.pg)
            .await?;
    let requester = requester.ok_or_else(|| AppError::Validation("export not found".into()))?;
    if ctx.user_id == Some(requester) || ctx.is_admin() {
        Ok(requester)
    } else {
        Err(AppError::Forbidden("not your export".into()))
    }
}

pub async fn get_export(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ExportRow>> {
    require_export_access(&state, &ctx, id).await?;
    let r = sqlx::query!(
        r#"SELECT id, kind::text AS "kind!", status::text AS "status!", format, target_id,
                  filename, error, created_at, completed_at
           FROM exports WHERE id = $1"#,
        id
    )
    .fetch_one(&state.pg)
    .await?;
    Ok(Json(ExportRow {
        id: r.id, kind: r.kind, status: r.status, format: r.format, target_id: r.target_id,
        filename: r.filename, error: r.error,
        created_at: r.created_at.to_string(), completed_at: r.completed_at.map(|t| t.to_string()),
    }))
}

pub async fn download_export(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Response> {
    require_export_access(&state, &ctx, id).await?;
    let r = sqlx::query!(
        r#"SELECT status::text AS "status!", disk_path, mime, filename FROM exports WHERE id = $1"#,
        id
    )
    .fetch_one(&state.pg)
    .await?;
    if r.status != "ready" {
        return Err(AppError::Conflict(format!("export is '{}', not ready", r.status)));
    }
    let path = r.disk_path.ok_or_else(|| AppError::Other(anyhow::anyhow!("ready export has no path")))?;
    let abs = crate::storage::resolve_file(&state.boot.storage.exports_dir, &path);
    let safe = crate::upload::ensure_within_storage(&state.boot.storage.exports_dir, &abs.to_string_lossy())?;
    let bytes = tokio::fs::read(&safe)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read export file: {e}")))?;
    let mime = r.mime.unwrap_or_else(|| "application/octet-stream".into());
    let filename = r.filename.unwrap_or_else(|| format!("{id}.bin"));
    Ok(text_response(bytes, &mime, &filename))
}

/// Background worker: build the queued export to disk and mark it ready (or
/// failed). Called by the scheduler's `Export` arm.
pub async fn run_export(state: &AppState, export_id: Uuid) -> Result<()> {
    let row = sqlx::query!(
        r#"SELECT requested_by, kind::text AS "kind!", target_id, format FROM exports WHERE id = $1"#,
        export_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("export not found".into()))?;

    sqlx::query!(
        "UPDATE exports SET status = 'running' WHERE id = $1", export_id
    )
    .execute(&state.pg)
    .await?;

    let built = match row.kind.as_str() {
        "chat" => {
            let tid = row.target_id.ok_or_else(|| AppError::Validation("chat export missing target".into()))?;
            build_chat_export(state, tid, &row.format).await
        }
        "project_db" => {
            let tid = row.target_id.ok_or_else(|| AppError::Validation("project export missing target".into()))?;
            // Run the export as the admin who requested it, so an enforced source
            // ACL (D5) filters documents from their evidence copy too.
            let viewer = crate::auth::load_context(&state.pg, row.requested_by).await?;
            build_project_db_export(state, &viewer, tid).await
        }
        // Non-Core export kinds (e.g. the Enterprise `audit` evidence export) come
        // from the export-kind registry seam; the Core registry is empty, so an
        // `audit` job only succeeds in the combined edition. create_export already
        // gated it on the `compliance_audit` capability (false in Core).
        other => match state.export_kinds.get(other) {
            Some(h) => h.build(state, row.target_id).await,
            None => Err(AppError::Validation(format!("unknown export kind: {other}"))),
        },
    };

    match built {
        Ok(f) => {
            let ext = ext_of(&f.filename);
            let root = exports_root(state);
            tokio::fs::create_dir_all(&root)
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("create exports dir: {e}")))?;
            // Store the RELATIVE suffix (`<export_id>.<ext>`) under `exports_dir`.
            let rel = format!("{export_id}.{ext}");
            tokio::fs::write(root.join(&rel), &f.bytes)
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("write export: {e}")))?;
            sqlx::query!(
                "UPDATE exports SET status = 'ready', disk_path = $2, mime = $3, filename = $4, \
                 completed_at = now() WHERE id = $1",
                export_id, rel, f.mime, f.filename,
            )
            .execute(&state.pg)
            .await?;
            audit_export_sys(state, "completed", &row.kind, export_id, row.requested_by).await;
            Ok(())
        }
        Err(e) => {
            let _ = sqlx::query!(
                "UPDATE exports SET status = 'failed', error = $2, completed_at = now() WHERE id = $1",
                export_id, e.to_string(),
            )
            .execute(&state.pg)
            .await;
            audit_export_sys(state, "failed", &row.kind, export_id, row.requested_by).await;
            Err(e)
        }
    }
}

async fn audit_export_sys(state: &AppState, phase: &str, kind: &str, id: Uuid, actor: Uuid) {
    let mut ev = AuditEvent::action(format!("export.{phase}"), "system");
    ev.actor_user_id = Some(actor);
    ev.resource_type = Some("export".into());
    ev.resource_id = Some(id);
    ev.payload = Some(json!({ "kind": kind }));
    let _ = audit::append(&state.pg, &ev).await;
}

// --- helpers -----------------------------------------------------------------

fn safe(name: &str) -> String {
    name.replace(['"', '/', '\\', ' '], "_")
}

fn ext_of(filename: &str) -> &str {
    filename.rsplit_once('.').map(|(_, e)| e).unwrap_or("bin")
}

fn exports_root(state: &AppState) -> std::path::PathBuf {
    crate::storage::resolve_dir(&state.boot.storage.exports_dir)
}

pub fn text_response(bytes: Vec<u8>, content_type: &str, filename: &str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\"")),
        ],
        Body::from(bytes),
    )
        .into_response()
}
