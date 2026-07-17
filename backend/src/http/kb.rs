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

//! Knowledge Base ("Library") REST. Standalone KBs with their own owner + ReBAC
//! grants, attachable to Projects/chats. Every mutating endpoint writes a
//! hash-chain audit event; widening an audience (grant / promote / attach) is a
//! **disclosure event** logged with before/after. `knowledge_base_id` is stamped
//! backend-side at ingest — never taken from client input (anti-spoof).

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::keycloak::AuthUser;
use crate::auth::rbac::{self, Permission};
use crate::auth::AuthContext;
use crate::db;
use crate::error::{AppError, Result};
use crate::kb::{self, KbPermission};
use crate::state::AppState;

// --- Create / list / detail --------------------------------------------------

#[derive(Deserialize)]
pub struct CreateKb {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 'personal' | 'team' | 'shared'. ('project' is reserved for default KBs.)
    #[serde(default)]
    pub visibility: Option<String>,
    /// Parent–child chunking — recommended for statutes/contracts.
    /// Defaults to false when omitted.
    #[serde(default)]
    pub parent_child: Option<bool>,
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

/// Create a Library. Owner gets a `manage` grant. Audited.
pub async fn create_kb(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateKb>,
) -> Result<Json<CreatedId>> {
    let owner = ctx
        .user_id
        .ok_or_else(|| AppError::Forbidden("a library needs a user owner".into()))?;
    let visibility = match body.visibility.as_deref() {
        Some("team") => "team",
        Some("shared") => "shared",
        _ => "personal",
    };
    if body.name.trim().is_empty() {
        return Err(AppError::Validation("a library needs a name".into()));
    }

    let info = crate::ml::embed_info(&state.http, &state.boot.ml.base_url, crate::ml::provider_overrides(&state, ctx.user_id).await).await?;
    // Seed the embedding-index provenance on the first KB build (idempotent), so
    // retrieval/ingest bind to the model that actually produced the live vectors
    // and a later model change is detected as a migration.
    if let Ok(ep) = state.providers.resolve(&state.pg, "embed", ctx.user_id).await {
        let ep = ep.unwrap_or(crate::ext::ResolvedProvider { base_url: None, model: None, api_key: None, enabled: true, reasoning_mode: None });
        let _ = crate::embedding_index::seed_if_absent(&state.pg, state.message_key, &info.model, ep.base_url.as_deref(), ep.api_key.as_deref(), info.dimension).await;
    }
    let id = db::new_id();
    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO knowledge_bases \
           (id, name, description, owner_id, visibility, restricted, \
            embedding_model_id, embedding_dimension, status, parent_child) \
         VALUES ($1, $2, $3, $4, ($5::text)::kb_visibility, false, $6, $7, 'empty', $8)",
        id,
        body.name.trim(),
        body.description,
        owner,
        visibility,
        info.model,
        info.dimension,
        body.parent_child.unwrap_or(false),
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO kb_access_grants (id, kb_id, principal_type, principal_id, permission, granted_by) \
         VALUES ($1, $2, 'user', $3, 'manage', $3)",
        db::new_id(),
        id,
        owner,
    )
    .execute(&mut *tx)
    .await?;
    let mut ev = crate::audit::AuditEvent::action("kb.created", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("knowledge_base".into());
    ev.resource_id = Some(id);
    ev.payload = Some(json!({ "visibility": visibility, "name": body.name.trim() }));
    crate::audit::append_with(&mut tx, &ev).await?;
    tx.commit().await?;

    Ok(Json(CreatedId { id }))
}

/// Libraries visible to the caller (Personal / Shared / Team — project KBs excluded).
pub async fn list_kb(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<kb::KbSummary>>> {
    Ok(Json(kb::list_visible(&state.pg, &ctx).await?))
}

#[derive(Serialize)]
pub struct KbDocOut {
    pub id: Uuid,
    pub filename: String,
    pub mime: Option<String>,
    pub status: String,
    pub created_at: String,
    /// How the document entered the KB: `upload` | `connector_import`
    /// (drives the source badge in the library UI).
    pub source: String,
}

#[derive(Serialize)]
pub struct KbDetail {
    #[serde(flatten)]
    pub summary: kb::KbSummary,
    pub documents: Vec<KbDocOut>,
}

/// KB detail: header + documents with ingest status. Read access required.
pub async fn get_kb(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> Result<Json<KbDetail>> {
    kb::require_read(&state.pg, &ctx, kb_id).await?;
    let summary = kb::summary(&state.pg, &ctx, kb_id).await?;
    let documents = load_docs(&state, kb_id).await?;
    Ok(Json(KbDetail { summary, documents }))
}

async fn load_docs(state: &AppState, kb_id: Uuid) -> Result<Vec<KbDocOut>> {
    let rows = sqlx::query!(
        r#"SELECT id, original_filename, mime, ingest_status::text AS "status!", created_at, source
           FROM kb_documents WHERE kb_id = $1 ORDER BY created_at DESC"#,
        kb_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| KbDocOut {
            id: r.id,
            filename: r.original_filename,
            mime: r.mime,
            status: r.status,
            created_at: r
                .created_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            source: r.source,
        })
        .collect())
}

#[derive(Deserialize)]
pub struct PatchKb {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub restricted: Option<bool>,
}

/// Rename / re-describe / re-tag / restrict. Manage required. Audited.
pub async fn patch_kb(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(body): Json<PatchKb>,
) -> Result<Json<serde_json::Value>> {
    kb::require_manage(&state.pg, &ctx, kb_id).await?;
    if let Some(name) = &body.name {
        if name.trim().is_empty() {
            return Err(AppError::Validation("name cannot be empty".into()));
        }
        sqlx::query!("UPDATE knowledge_bases SET name = $2 WHERE id = $1", kb_id, name.trim())
            .execute(&state.pg)
            .await?;
    }
    if let Some(desc) = &body.description {
        sqlx::query!("UPDATE knowledge_bases SET description = $2 WHERE id = $1", kb_id, desc)
            .execute(&state.pg)
            .await?;
    }
    if let Some(vis) = &body.visibility {
        let v = match vis.as_str() {
            "personal" | "team" | "shared" | "project" => vis.as_str(),
            _ => return Err(AppError::Validation("invalid visibility".into())),
        };
        sqlx::query!(
            "UPDATE knowledge_bases SET visibility = ($2::text)::kb_visibility WHERE id = $1",
            kb_id,
            v
        )
        .execute(&state.pg)
        .await?;
    }
    if let Some(r) = body.restricted {
        sqlx::query!("UPDATE knowledge_bases SET restricted = $2 WHERE id = $1", kb_id, r)
            .execute(&state.pg)
            .await?;
    }
    kb::audit_kb(&state.pg, &ctx, "kb.updated", kb_id, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

/// Archive (soft-delete) a KB. Manage required. Cascade (chunks/links) is handled
/// by FK cascade on hard delete; here we archive so it drops from every list.
pub async fn delete_kb(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    kb::require_manage(&state.pg, &ctx, kb_id).await?;
    sqlx::query!(
        "UPDATE knowledge_bases SET archived_at = now() WHERE id = $1 AND archived_at IS NULL",
        kb_id
    )
    .execute(&state.pg)
    .await?;
    kb::audit_kb(&state.pg, &ctx, "kb.archived", kb_id, json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

// --- Documents ---------------------------------------------------------------

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

/// Upload a document into a KB → async ingest (extract→chunk→embed→upsert). The
/// backend resolves `kb_id` from the path and stamps it on every chunk; a client
/// can never spoof which KB a chunk lands in. Manage required.
pub async fn upload_kb_document(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(kb_id): Path<Uuid>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> Result<Json<UploadedDoc>> {
    kb::require_manage(&state.pg, &ctx, kb_id).await?;
    let doc_id =
        kb::ingest_bytes(&state, &ctx, kb_id, &q.filename, q.mime.as_deref(), &body[..], "upload")
            .await?;
    Ok(Json(UploadedDoc { doc_id, status: "uploaded".into() }))
}

/// Remove a document: drop its row and purge its chunks from Qdrant. Manage required.
pub async fn delete_kb_document(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((kb_id, doc_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    kb::require_manage(&state.pg, &ctx, kb_id).await?;
    let existed = sqlx::query!(
        "DELETE FROM kb_documents WHERE id = $1 AND kb_id = $2",
        doc_id,
        kb_id
    )
    .execute(&state.pg)
    .await?
    .rows_affected();
    if existed == 0 {
        return Err(AppError::Validation("document not found in this library".into()));
    }
    // Best-effort chunk purge (Postgres row is the record of truth).
    let _ = crate::ml::delete_doc(&state.http, &state.boot.ml.base_url, &kb_id.to_string(), &doc_id.to_string()).await;
    kb::audit_kb(&state.pg, &ctx, "kb.document.removed", kb_id, json!({ "doc_id": doc_id })).await;
    Ok(Json(json!({ "ok": true })))
}

// --- Grants (share dialog) ---------------------------------------------------

#[derive(Serialize)]
pub struct GrantOut {
    pub id: Uuid,
    pub principal_type: String,
    pub principal_id: Uuid,
    pub permission: String,
    pub name: Option<String>,
}

/// List a KB's grants (the grantee roster). Manage required.
pub async fn list_grants(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(kb_id): Path<Uuid>,
) -> Result<Json<Vec<GrantOut>>> {
    kb::require_manage(&state.pg, &ctx, kb_id).await?;
    let rows = sqlx::query!(
        r#"SELECT g.id, g.principal_type::text AS "principal_type!", g.principal_id,
                  g.permission::text AS "permission!",
                  COALESCE(u.display_name, grp.name) AS name
           FROM kb_access_grants g
           LEFT JOIN users u   ON g.principal_type = 'user'  AND u.id = g.principal_id
           LEFT JOIN groups grp ON g.principal_type = 'group' AND grp.id = g.principal_id
           WHERE g.kb_id = $1
           ORDER BY g.created_at"#,
        kb_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| GrantOut {
                id: r.id,
                principal_type: r.principal_type,
                principal_id: r.principal_id,
                permission: r.permission,
                name: r.name,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct PutGrant {
    pub principal_type: String, // 'user' | 'group'
    pub principal_id: Uuid,
    pub permission: String, // 'read' | 'manage'
}

/// Add or update a grant (share). Manage required. Widening the audience is a
/// **disclosure event** — audited with the principal and before/after permission.
pub async fn put_grant(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(body): Json<PutGrant>,
) -> Result<Json<serde_json::Value>> {
    kb::require_manage(&state.pg, &ctx, kb_id).await?;
    let ptype = match body.principal_type.as_str() {
        "user" | "group" => body.principal_type.as_str(),
        _ => return Err(AppError::Validation("principal_type must be 'user' or 'group'".into())),
    };
    let perm = KbPermission::parse(&body.permission)?;

    let before: Option<String> = sqlx::query_scalar!(
        r#"SELECT permission::text AS "p!" FROM kb_access_grants
           WHERE kb_id = $1 AND principal_type = ($2::text)::principal_type AND principal_id = $3"#,
        kb_id,
        ptype,
        body.principal_id,
    )
    .fetch_optional(&state.pg)
    .await?;

    sqlx::query!(
        "INSERT INTO kb_access_grants (id, kb_id, principal_type, principal_id, permission, granted_by) \
         VALUES ($1, $2, ($3::text)::principal_type, $4, ($5::text)::kb_permission, $6) \
         ON CONFLICT (kb_id, principal_type, principal_id) \
         DO UPDATE SET permission = EXCLUDED.permission, granted_by = EXCLUDED.granted_by",
        db::new_id(),
        kb_id,
        ptype,
        body.principal_id,
        perm.as_str(),
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;

    kb::audit_kb(
        &state.pg,
        &ctx,
        "kb.grant.changed",
        kb_id,
        json!({
            "op": "grant", "disclosure": true,
            "principal_type": ptype, "principal_id": body.principal_id,
            "before": before, "after": perm.as_str(),
        }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// Revoke a grant by id. Manage required. Audited. Takes effect on the next query.
pub async fn delete_grant(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((kb_id, grant_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    kb::require_manage(&state.pg, &ctx, kb_id).await?;
    let row = sqlx::query!(
        r#"DELETE FROM kb_access_grants WHERE id = $1 AND kb_id = $2
           RETURNING principal_type::text AS "principal_type!", principal_id, permission::text AS "permission!""#,
        grant_id,
        kb_id,
    )
    .fetch_optional(&state.pg)
    .await?;
    let Some(row) = row else {
        return Err(AppError::Validation("grant not found".into()));
    };
    kb::audit_kb(
        &state.pg,
        &ctx,
        "kb.grant.changed",
        kb_id,
        json!({ "op": "revoke", "principal_type": row.principal_type, "principal_id": row.principal_id, "before": row.permission }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

// --- Project / chat links ----------------------------------------------------

#[derive(Deserialize)]
pub struct LinkKb {
    pub kb_id: Uuid,
}

/// Attach a KB to a Project. Requires project-write (power-user/owner) AND kb-read.
/// Refused if the KB is `restricted` and this is not its origin Project (hard
/// ethical wall). The cross-matter exposure surface — audited.
pub async fn attach_project(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(project_id): Path<Uuid>,
    Json(body): Json<LinkKb>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_project(&state.pg, &ctx, project_id, Permission::Write).await?;
    kb::require_read(&state.pg, &ctx, body.kb_id).await?;
    let brief = kb::brief(&state.pg, body.kb_id).await?;
    if brief.restricted && brief.origin_project_id != Some(project_id) {
        kb::audit_kb(
            &state.pg,
            &ctx,
            "kb.attach.refused",
            body.kb_id,
            json!({ "project_id": project_id, "reason": "restricted" }),
        )
        .await;
        return Err(AppError::Forbidden(
            "this library is restricted to its origin project".into(),
        ));
    }
    sqlx::query!(
        "INSERT INTO project_kb_links (project_id, kb_id, attached_by) VALUES ($1, $2, $3) \
         ON CONFLICT DO NOTHING",
        project_id,
        body.kb_id,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;
    kb::audit_kb(
        &state.pg,
        &ctx,
        "kb.attached",
        body.kb_id,
        json!({ "scope": "project", "project_id": project_id, "disclosure": true }),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

/// Detach a KB from a Project. Project-write required. Instant; audited.
pub async fn detach_project(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((project_id, kb_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    state.rbac.require_project(&state.pg, &ctx, project_id, Permission::Write).await?;
    sqlx::query!(
        "DELETE FROM project_kb_links WHERE project_id = $1 AND kb_id = $2",
        project_id,
        kb_id
    )
    .execute(&state.pg)
    .await?;
    kb::audit_kb(&state.pg, &ctx, "kb.detached", kb_id, json!({ "scope": "project", "project_id": project_id })).await;
    Ok(Json(json!({ "ok": true })))
}

/// Libraries attached to a Project (default project KB + attached Libraries),
/// with each one's read-visibility for the caller. Project-read required.
#[derive(Serialize)]
pub struct AttachedLib {
    pub id: Uuid,
    pub name: String,
    pub visibility: String,
    pub restricted: bool,
    pub is_default: bool,
}

pub async fn list_project_links(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<AttachedLib>>> {
    // project read
    let owner: Option<Uuid> = sqlx::query_scalar!(
        "SELECT owner_user_id FROM projects WHERE id = $1 AND archived_at IS NULL",
        project_id
    )
    .fetch_optional(&state.pg)
    .await?;
    let owner = owner.ok_or_else(|| AppError::Validation("project not found".into()))?;
    if ctx.user_id != Some(owner) && !ctx.is_admin() {
        state.rbac.require(&state.pg, &ctx, rbac::ResourceType::Project, project_id, Permission::Read).await?;
    }
    let rows = sqlx::query!(
        r#"SELECT kb.id, kb.name, kb.visibility::text AS "visibility!", kb.restricted,
                  (kb.visibility = 'project' AND kb.origin_project_id = $1) AS "is_default!"
           FROM project_kb_links pl JOIN knowledge_bases kb ON kb.id = pl.kb_id
           WHERE pl.project_id = $1 AND kb.archived_at IS NULL
           ORDER BY (kb.visibility = 'project' AND kb.origin_project_id = $1) DESC, kb.name"#,
        project_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| AttachedLib {
                id: r.id,
                name: r.name,
                visibility: r.visibility,
                restricted: r.restricted,
                is_default: r.is_default,
            })
            .collect(),
    ))
}

/// Ad-hoc attach a KB to a personal chat. Chat owner (or admin) AND kb-read.
pub async fn attach_chat(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
    Json(body): Json<LinkKb>,
) -> Result<Json<serde_json::Value>> {
    require_chat_owner(&state, &ctx, chat_id).await?;
    kb::require_read(&state.pg, &ctx, body.kb_id).await?;
    let brief = kb::brief(&state.pg, body.kb_id).await?;
    if brief.restricted {
        return Err(AppError::Forbidden("a restricted library cannot be attached to an ad-hoc chat".into()));
    }
    sqlx::query!(
        "INSERT INTO chat_kb_links (chat_id, kb_id, attached_by) VALUES ($1, $2, $3) \
         ON CONFLICT DO NOTHING",
        chat_id,
        body.kb_id,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;
    kb::audit_kb(&state.pg, &ctx, "kb.attached", body.kb_id, json!({ "scope": "chat", "chat_id": chat_id })).await;
    Ok(Json(json!({ "ok": true })))
}

/// Detach a KB from a chat. Chat owner (or admin) required.
pub async fn detach_chat(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((chat_id, kb_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    require_chat_owner(&state, &ctx, chat_id).await?;
    sqlx::query!("DELETE FROM chat_kb_links WHERE chat_id = $1 AND kb_id = $2", chat_id, kb_id)
        .execute(&state.pg)
        .await?;
    kb::audit_kb(&state.pg, &ctx, "kb.detached", kb_id, json!({ "scope": "chat", "chat_id": chat_id })).await;
    Ok(Json(json!({ "ok": true })))
}

/// Libraries attached to a chat. Chat owner (or admin) required.
pub async fn list_chat_links(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(chat_id): Path<Uuid>,
) -> Result<Json<Vec<AttachedLib>>> {
    require_chat_owner(&state, &ctx, chat_id).await?;
    let rows = sqlx::query!(
        r#"SELECT kb.id, kb.name, kb.visibility::text AS "visibility!", kb.restricted
           FROM chat_kb_links cl JOIN knowledge_bases kb ON kb.id = cl.kb_id
           WHERE cl.chat_id = $1 AND kb.archived_at IS NULL
           ORDER BY kb.name"#,
        chat_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| AttachedLib {
                id: r.id,
                name: r.name,
                visibility: r.visibility,
                restricted: r.restricted,
                is_default: false,
            })
            .collect(),
    ))
}

async fn require_chat_owner(state: &AppState, ctx: &AuthContext, chat_id: Uuid) -> Result<()> {
    let owner: Option<Uuid> = sqlx::query_scalar!(
        "SELECT owner_user_id FROM chats WHERE id = $1 AND archived_at IS NULL",
        chat_id
    )
    .fetch_optional(&state.pg)
    .await?;
    let owner = owner.ok_or_else(|| AppError::Validation("chat not found".into()))?;
    if ctx.user_id == Some(owner) || ctx.is_admin() {
        Ok(())
    } else {
        Err(AppError::Forbidden("only the chat owner may change its libraries".into()))
    }
}

// --- Promote -----------------------------------------------------------------

#[derive(Deserialize)]
pub struct PromoteKb {
    /// Target visibility: 'team' | 'shared' | 'personal'.
    pub visibility: String,
    /// Optional grants to add as part of the disclosure.
    #[serde(default)]
    pub grants: Vec<PutGrant>,
}

/// Promote a Project KB → Library: widen `visibility` and add the broader grants.
/// The origin `project_kb_links` row stays (still attached there). Manage on the
/// KB required. A **disclosure event** — audited with before/after audience.
pub async fn promote_kb(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(kb_id): Path<Uuid>,
    Json(body): Json<PromoteKb>,
) -> Result<Json<serde_json::Value>> {
    kb::require_manage(&state.pg, &ctx, kb_id).await?;
    let target = match body.visibility.as_str() {
        "team" | "shared" | "personal" => body.visibility.as_str(),
        _ => return Err(AppError::Validation("promote target must be team|shared|personal".into())),
    };
    let before = sqlx::query_scalar!(
        r#"SELECT visibility::text AS "v!" FROM knowledge_bases WHERE id = $1 AND archived_at IS NULL"#,
        kb_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("knowledge base not found".into()))?;

    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "UPDATE knowledge_bases SET visibility = ($2::text)::kb_visibility, restricted = false WHERE id = $1",
        kb_id,
        target
    )
    .execute(&mut *tx)
    .await?;
    let mut added: Vec<serde_json::Value> = Vec::new();
    for g in &body.grants {
        let ptype = match g.principal_type.as_str() {
            "user" | "group" => g.principal_type.as_str(),
            _ => return Err(AppError::Validation("principal_type must be 'user' or 'group'".into())),
        };
        let perm = KbPermission::parse(&g.permission)?;
        sqlx::query!(
            "INSERT INTO kb_access_grants (id, kb_id, principal_type, principal_id, permission, granted_by) \
             VALUES ($1, $2, ($3::text)::principal_type, $4, ($5::text)::kb_permission, $6) \
             ON CONFLICT (kb_id, principal_type, principal_id) \
             DO UPDATE SET permission = EXCLUDED.permission",
            db::new_id(),
            kb_id,
            ptype,
            g.principal_id,
            perm.as_str(),
            ctx.user_id,
        )
        .execute(&mut *tx)
        .await?;
        added.push(json!({ "principal_type": ptype, "principal_id": g.principal_id, "permission": perm.as_str() }));
    }
    let mut ev = crate::audit::AuditEvent::action("kb.promoted", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("knowledge_base".into());
    ev.resource_id = Some(kb_id);
    ev.payload = Some(json!({ "disclosure": true, "before": before, "after": target, "grants_added": added }));
    crate::audit::append_with(&mut tx, &ev).await?;
    tx.commit().await?;

    Ok(Json(json!({ "ok": true })))
}
