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

//! Knowledge Bases ("Libraries") — the authorisation authority for modular,
//! shareable RAG knowledge.
//!
//! A KB is a first-class entity with its own owner + ReBAC grants
//! (`kb_access_grants`), decoupled from any single Project and **attached**
//! explicitly to Projects (`project_kb_links`) and ad-hoc chats
//! (`chat_kb_links`). Retrieval authorisation is resolved **fresh at query time**
//! from Postgres into an allow-list of KB ids — the **intersection invariant**:
//!
//! ```text
//! retrieval_allowlist(user, chat) = attached_kbs(chat) ∩ { kb : can_read_kb(user, kb) }
//! ```
//!
//! never just one side (the first leaks a restricted attached KB to an
//! ungranted member; the second bleeds another matter's KB into this chat). The
//! allow-list is computed server-side from the authenticated principal; the
//! client never sends KB ids to retrieve from.

use serde::Serialize;
use sqlx::PgPool;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::AuthContext;
use crate::error::{AppError, Result};

/// A grant level on a KB. `manage` implies `read` plus edit/grant/promote rights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "kb_permission", rename_all = "snake_case")]
pub enum KbPermission {
    Read,
    Manage,
}

impl KbPermission {
    pub fn as_str(self) -> &'static str {
        match self {
            KbPermission::Read => "read",
            KbPermission::Manage => "manage",
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "read" => Ok(KbPermission::Read),
            "manage" => Ok(KbPermission::Manage),
            _ => Err(AppError::Validation("permission must be 'read' or 'manage'".into())),
        }
    }
}

/// Summary row of a KB (the Library card / detail header).
#[derive(Debug, Clone, Serialize)]
pub struct KbSummary {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: Uuid,
    pub visibility: String,
    pub origin_project_id: Option<Uuid>,
    pub restricted: bool,
    pub embedding_model_id: String,
    pub embedding_dimension: i32,
    pub status: String,
    pub created_at: String,
    /// True when the caller owns this KB (UI affordances; never an access decision).
    pub mine: bool,
    /// True when the caller may manage (owner / manage-grant / admin).
    pub can_manage: bool,
}

/// Minimal KB facts used by guards (attach checks, promote, ingest dimension).
pub struct KbBrief {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub visibility: String,
    pub origin_project_id: Option<Uuid>,
    pub restricted: bool,
    pub embedding_dimension: i32,
}

/// Load the guard-relevant facts for a KB (404-as-validation if missing/archived).
pub async fn brief(pool: &PgPool, kb_id: Uuid) -> Result<KbBrief> {
    let row = sqlx::query!(
        r#"SELECT id, owner_id, visibility::text AS "visibility!", origin_project_id,
                  restricted, embedding_dimension
           FROM knowledge_bases WHERE id = $1 AND archived_at IS NULL"#,
        kb_id
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Validation("knowledge base not found".into()))?;
    Ok(KbBrief {
        id: row.id,
        owner_id: row.owner_id,
        visibility: row.visibility,
        origin_project_id: row.origin_project_id,
        restricted: row.restricted,
        embedding_dimension: row.embedding_dimension,
    })
}

/// `can_read_kb(user, kb)` — owner OR a read/manage grant (direct or via a group);
/// admin levels override. Mirrors `auth::rbac::can`'s group-membership shape.
pub async fn can_read(pool: &PgPool, ctx: &AuthContext, kb_id: Uuid) -> Result<bool> {
    if ctx.is_admin() {
        return Ok(true);
    }
    let Some(uid) = ctx.user_id else { return Ok(false) };
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM knowledge_bases kb
             WHERE kb.id = $2 AND kb.archived_at IS NULL AND (
                 kb.owner_id = $1
                 OR EXISTS (
                     SELECT 1 FROM kb_access_grants g WHERE g.kb_id = kb.id AND (
                         (g.principal_type = 'user'  AND g.principal_id = $1)
                      OR (g.principal_type = 'group' AND g.principal_id IN
                            (SELECT group_id FROM group_members WHERE user_id = $1))
                     )
                 )
             )
           ) AS "ok!""#,
        uid,
        kb_id,
    )
    .fetch_one(pool)
    .await?)
}

/// `can_manage_kb(user, kb)` — owner OR a `manage` grant; admin levels override.
pub async fn can_manage(pool: &PgPool, ctx: &AuthContext, kb_id: Uuid) -> Result<bool> {
    if ctx.is_admin() {
        return Ok(true);
    }
    let Some(uid) = ctx.user_id else { return Ok(false) };
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM knowledge_bases kb
             WHERE kb.id = $2 AND kb.archived_at IS NULL AND (
                 kb.owner_id = $1
                 OR EXISTS (
                     SELECT 1 FROM kb_access_grants g
                     WHERE g.kb_id = kb.id AND g.permission = 'manage' AND (
                         (g.principal_type = 'user'  AND g.principal_id = $1)
                      OR (g.principal_type = 'group' AND g.principal_id IN
                            (SELECT group_id FROM group_members WHERE user_id = $1))
                     )
                 )
             )
           ) AS "ok!""#,
        uid,
        kb_id,
    )
    .fetch_one(pool)
    .await?)
}

/// `can_read` as a guard. 403 when denied.
pub async fn require_read(pool: &PgPool, ctx: &AuthContext, kb_id: Uuid) -> Result<()> {
    if can_read(pool, ctx, kb_id).await? {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!("no read access to knowledge base {kb_id}")))
    }
}

/// `can_manage` as a guard. 403 when denied.
pub async fn require_manage(pool: &PgPool, ctx: &AuthContext, kb_id: Uuid) -> Result<()> {
    if can_manage(pool, ctx, kb_id).await? {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!("no manage access to knowledge base {kb_id}")))
    }
}

// --- KB ingest entry ---------------------------------------------------------

/// The single entry point for putting a document into a KB — whether from a
/// manual upload (`source = "upload"`) or a connector import
/// (`source = "connector_import"`). Runs the shared format gate, writes the bytes
/// under the documents dir, inserts the `kb_documents` row (stamping `source`),
/// enqueues async ingest, and audits `kb.document.uploaded`. The KB id is stamped
/// backend-side and never taken from client input (anti-spoof). The caller
/// owns the manage/authorisation check.
pub async fn ingest_bytes(
    state: &crate::state::AppState,
    ctx: &AuthContext,
    kb_id: Uuid,
    filename: &str,
    mime: Option<&str>,
    bytes: &[u8],
    source: &str,
) -> Result<Uuid> {
    crate::upload::ensure_supported_document(filename)?;
    let doc_id = crate::db::new_id();
    let bytes_path = write_kb_bytes(state, doc_id, filename, bytes).await?;
    sqlx::query!(
        "INSERT INTO kb_documents \
           (id, kb_id, original_filename, mime, bytes_path, ingest_status, source, created_by) \
         VALUES ($1, $2, $3, $4, $5, 'uploaded', $6, $7)",
        doc_id,
        kb_id,
        filename,
        mime,
        bytes_path,
        source,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;
    crate::scheduler::enqueue(
        &state.pg,
        crate::scheduler::TaskType::Ingest,
        serde_json::json!({ "doc_id": doc_id }),
    )
    .await
    .map_err(AppError::from)?;
    audit_kb(
        &state.pg,
        ctx,
        "kb.document.uploaded",
        kb_id,
        serde_json::json!({ "doc_id": doc_id, "source": source }),
    )
    .await;
    Ok(doc_id)
}

/// Overwrite the stored bytes of an existing KB document and re-enqueue ingest
/// (D2 — a connector source update refreshes the same read-corpus row rather than
/// versioning it). `ingest.py` deletes the old chunks before re-indexing, so this
/// is idempotent. No new row and no upload-audit — the provenance already exists.
pub async fn replace_bytes(
    state: &crate::state::AppState,
    kb_document_id: Uuid,
    bytes: &[u8],
) -> Result<()> {
    let row = sqlx::query!(
        "SELECT bytes_path FROM kb_documents WHERE id = $1",
        kb_document_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("kb document not found".into()))?;
    let path = crate::storage::resolve_file(&state.boot.storage.documents_dir, &row.bytes_path);
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("rewrite kb document: {e}")))?;
    // Reset to 'uploaded' so the library UI shows re-indexing in progress.
    sqlx::query!(
        "UPDATE kb_documents SET ingest_status = 'uploaded' WHERE id = $1",
        kb_document_id
    )
    .execute(&state.pg)
    .await?;
    crate::scheduler::enqueue(
        &state.pg,
        crate::scheduler::TaskType::Ingest,
        serde_json::json!({ "doc_id": kb_document_id }),
    )
    .await
    .map_err(AppError::from)?;
    Ok(())
}

/// Write bytes under the documents dir and return the RELATIVE suffix
/// (`<doc_id>__<safe_name>`) to store; reads resolve it against `documents_dir`.
async fn write_kb_bytes(
    state: &crate::state::AppState,
    doc_id: Uuid,
    filename: &str,
    body: &[u8],
) -> Result<String> {
    let safe_name = filename.replace(['/', '\\'], "_");
    let dir = crate::storage::resolve_dir(&state.boot.storage.documents_dir);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("create documents dir: {e}")))?;
    let rel = format!("{doc_id}__{safe_name}");
    tokio::fs::write(dir.join(&rel), body)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("write document: {e}")))?;
    Ok(rel)
}

/// The retrieval allow-list (the ethical wall): KBs attached to this
/// chat's context (agent-bound ∪ project-linked ∪ chat-linked) **intersected**
/// with the KBs this user may personally read. Resolved live from Postgres in a
/// single query, so grant/attach/detach take effect on the very next call.
/// Only `ready` KBs are returned. An empty result ⇒ caller must skip retrieval
/// (fail-closed — never "search everything").
pub async fn retrieval_allowlist(
    pool: &PgPool,
    ctx: &AuthContext,
    chat_id: Uuid,
    project_id: Option<Uuid>,
    agent_id: Option<Uuid>,
) -> Result<Vec<Uuid>> {
    let Some(uid) = ctx.user_id else { return Ok(Vec::new()) };
    let ids = sqlx::query_scalar!(
        r#"
        WITH attached AS (
            SELECT project_knowledge_id AS kb_id FROM agent_project_knowledge WHERE agent_id = $4
            UNION
            SELECT kb_id FROM project_kb_links WHERE project_id = $3
            UNION
            SELECT kb_id FROM chat_kb_links    WHERE chat_id    = $2
        )
        SELECT kb.id AS "id!"
        FROM knowledge_bases kb
        WHERE kb.id IN (SELECT kb_id FROM attached)
          AND kb.archived_at IS NULL
          AND kb.status = 'ready'
          AND (
              $5
           OR kb.owner_id = $1
           OR EXISTS (
                SELECT 1 FROM kb_access_grants g WHERE g.kb_id = kb.id AND (
                    (g.principal_type = 'user'  AND g.principal_id = $1)
                 OR (g.principal_type = 'group' AND g.principal_id IN
                       (SELECT group_id FROM group_members WHERE user_id = $1))
                )
              )
          )
        "#,
        uid,
        chat_id,
        project_id,
        agent_id,
        ctx.is_admin(),
    )
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// Libraries visible to the caller for the top-level Library list: readable KBs
/// that are not un-promoted project KBs (`visibility <> 'project'`). Project KBs
/// live inside their Project; a promoted one reappears here.
pub async fn list_visible(pool: &PgPool, ctx: &AuthContext) -> Result<Vec<KbSummary>> {
    let uid = ctx.user_id;
    let is_admin = ctx.is_admin();
    let rows = sqlx::query!(
        r#"SELECT kb.id, kb.name, kb.description, kb.owner_id,
                  kb.visibility::text AS "visibility!", kb.origin_project_id, kb.restricted,
                  kb.embedding_model_id, kb.embedding_dimension, kb.status::text AS "status!",
                  kb.created_at,
                  EXISTS (
                      SELECT 1 FROM kb_access_grants g
                      WHERE g.kb_id = kb.id AND g.permission = 'manage' AND (
                          (g.principal_type = 'user'  AND g.principal_id = $1)
                       OR (g.principal_type = 'group' AND g.principal_id IN
                             (SELECT group_id FROM group_members WHERE user_id = $1))
                      )
                  ) AS "has_manage!"
           FROM knowledge_bases kb
           WHERE kb.archived_at IS NULL
             AND kb.visibility <> 'project'
             AND (
                 $2
              OR kb.owner_id = $1
              OR EXISTS (
                   SELECT 1 FROM kb_access_grants g WHERE g.kb_id = kb.id AND (
                       (g.principal_type = 'user'  AND g.principal_id = $1)
                    OR (g.principal_type = 'group' AND g.principal_id IN
                          (SELECT group_id FROM group_members WHERE user_id = $1))
                   )
                 )
             )
           ORDER BY kb.created_at DESC"#,
        uid,
        is_admin,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| KbSummary {
            id: r.id,
            name: r.name,
            description: r.description,
            owner_id: r.owner_id,
            visibility: r.visibility,
            origin_project_id: r.origin_project_id,
            restricted: r.restricted,
            embedding_model_id: r.embedding_model_id,
            embedding_dimension: r.embedding_dimension,
            status: r.status,
            created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
            mine: uid == Some(r.owner_id),
            can_manage: is_admin || uid == Some(r.owner_id) || r.has_manage,
        })
        .collect())
}

/// One library in the Deep Research scope (Phase 2): a readable KB with its
/// document count, for the scope picker + the census inventory.
#[derive(Debug, Clone, Serialize)]
pub struct DrScopeKb {
    pub id: Uuid,
    pub name: String,
    /// "project" (a Project's Project-Knowledge) or "library" (a first-class KB).
    pub kind: String,
    pub origin_project_id: Option<Uuid>,
    pub doc_count: i64,
}

/// The full Deep Research scope: every `ready`, non-archived KB the caller may
/// read — first-class Libraries (owner / `kb_access_grants`) AND Project
/// Knowledge whose owning Project the caller can access. This is the universe a
/// DR run may sweep; the request's `kb_ids` (if any) intersect it, fail-closed.
/// Unlike `list_visible` it INCLUDES project-visibility KBs (a DR has no chat /
/// project / agent context to attach through).
pub async fn dr_scope(pool: &PgPool, ctx: &AuthContext) -> Result<Vec<DrScopeKb>> {
    let Some(uid) = ctx.user_id else { return Ok(Vec::new()) };
    let is_admin = ctx.is_admin();
    let rows = sqlx::query!(
        r#"
        SELECT kb.id AS "id!", kb.name AS "name!",
               kb.visibility::text AS "visibility!", kb.origin_project_id,
               (SELECT count(*) FROM kb_documents d
                 WHERE d.kb_id = kb.id AND d.ingest_status = 'ready')::int8 AS "doc_count!"
        FROM knowledge_bases kb
        WHERE kb.archived_at IS NULL AND kb.status = 'ready'
          AND (
            (kb.visibility <> 'project' AND (
                $2 OR kb.owner_id = $1
             OR EXISTS (
                  SELECT 1 FROM kb_access_grants g WHERE g.kb_id = kb.id AND (
                      (g.principal_type = 'user'  AND g.principal_id = $1)
                   OR (g.principal_type = 'group' AND g.principal_id IN
                         (SELECT group_id FROM group_members WHERE user_id = $1))
                  )
                )
            ))
         OR (kb.visibility = 'project' AND (
                $2
             OR EXISTS (
                  SELECT 1 FROM project_kb_links l
                    JOIN projects p ON p.id = l.project_id AND p.archived_at IS NULL
                  WHERE l.kb_id = kb.id AND (
                      p.owner_user_id = $1
                   OR EXISTS (
                        SELECT 1 FROM access_grants g
                        WHERE g.resource_type = 'project' AND g.resource_id = p.id AND (
                            (g.principal_type = 'user'  AND g.principal_id = $1)
                         OR (g.principal_type = 'group' AND g.principal_id IN
                               (SELECT group_id FROM group_members WHERE user_id = $1))
                        )
                      )
                  )
                )
            ))
          )
        ORDER BY kb.name
        "#,
        uid,
        is_admin,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DrScopeKb {
            id: r.id,
            name: r.name,
            kind: if r.visibility == "project" { "project".into() } else { "library".into() },
            origin_project_id: r.origin_project_id,
            doc_count: r.doc_count,
        })
        .collect())
}

/// Intersect a requested set of KB ids with the caller's DR scope (fail-closed).
/// An empty request ⇒ the whole scope. Order follows `scope`.
pub fn intersect_scope(scope: &[DrScopeKb], requested: &[Uuid]) -> Vec<DrScopeKb> {
    if requested.is_empty() {
        return scope.to_vec();
    }
    let want: std::collections::HashSet<Uuid> = requested.iter().copied().collect();
    scope.iter().filter(|k| want.contains(&k.id)).cloned().collect()
}

/// Fetch a single KB summary (caller must already be allowed to read it).
pub async fn summary(pool: &PgPool, ctx: &AuthContext, kb_id: Uuid) -> Result<KbSummary> {
    let uid = ctx.user_id;
    let r = sqlx::query!(
        r#"SELECT id, name, description, owner_id, visibility::text AS "visibility!",
                  origin_project_id, restricted, embedding_model_id, embedding_dimension,
                  status::text AS "status!", created_at
           FROM knowledge_bases WHERE id = $1 AND archived_at IS NULL"#,
        kb_id
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Validation("knowledge base not found".into()))?;
    let can_manage = can_manage(pool, ctx, kb_id).await?;
    Ok(KbSummary {
        id: r.id,
        name: r.name,
        description: r.description,
        owner_id: r.owner_id,
        visibility: r.visibility,
        origin_project_id: r.origin_project_id,
        restricted: r.restricted,
        embedding_model_id: r.embedding_model_id,
        embedding_dimension: r.embedding_dimension,
        status: r.status,
        created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
        mine: uid == Some(r.owner_id),
        can_manage,
    })
}

/// The default ("Project Knowledge") KB for a Project: a `visibility='project'`
/// KB that originated in this Project, self-linked. Auto-created on first use so
/// the existing Project-Knowledge UI keeps working over the new model. Returns
/// the KB id. The caller must already hold project-write.
pub async fn ensure_project_kb(
    state: &crate::state::AppState,
    ctx: &AuthContext,
    project_id: Uuid,
) -> Result<Uuid> {
    if let Some(id) = sqlx::query_scalar!(
        "SELECT id FROM knowledge_bases \
         WHERE origin_project_id = $1 AND visibility = 'project' AND archived_at IS NULL \
         ORDER BY created_at LIMIT 1",
        project_id
    )
    .fetch_optional(&state.pg)
    .await?
    {
        return Ok(id);
    }

    let proj = sqlx::query!(
        "SELECT name, owner_user_id FROM projects WHERE id = $1 AND archived_at IS NULL",
        project_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("project not found".into()))?;

    let info = crate::ml::embed_info(&state.http, &state.boot.ml.base_url, crate::ml::provider_overrides(state, ctx.user_id).await).await?;
    // Seed embedding-index provenance on first KB build (idempotent).
    if let Ok(ep) = state.providers.resolve(&state.pg, "embed", ctx.user_id).await {
        let ep = ep.unwrap_or(crate::ext::ResolvedProvider { base_url: None, model: None, api_key: None, enabled: true, reasoning_mode: None });
        let _ = crate::embedding_index::seed_if_absent(&state.pg, state.message_key, &info.model, ep.base_url.as_deref(), ep.api_key.as_deref(), info.dimension).await;
    }
    let id = crate::db::new_id();
    let mut tx = state.pg.begin().await?;
    sqlx::query!(
        "INSERT INTO knowledge_bases \
           (id, name, description, owner_id, visibility, origin_project_id, restricted, \
            embedding_model_id, embedding_dimension, status) \
         VALUES ($1, $2, NULL, $3, 'project', $4, false, $5, $6, 'empty')",
        id,
        proj.name,
        proj.owner_user_id,
        project_id,
        info.model,
        info.dimension,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO kb_access_grants (id, kb_id, principal_type, principal_id, permission, granted_by) \
         VALUES ($1, $2, 'user', $3, 'manage', $3)",
        crate::db::new_id(),
        id,
        proj.owner_user_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO project_kb_links (project_id, kb_id, attached_by) VALUES ($1, $2, $3) \
         ON CONFLICT DO NOTHING",
        project_id,
        id,
        ctx.user_id,
    )
    .execute(&mut *tx)
    .await?;

    let mut ev = AuditEvent::action("kb.created", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("knowledge_base".into());
    ev.resource_id = Some(id);
    ev.payload = Some(serde_json::json!({ "visibility": "project", "origin_project_id": project_id }));
    audit::append_with(&mut tx, &ev).await?;

    tx.commit().await?;
    Ok(id)
}

/// Append a KB lifecycle audit event (best-effort; the hash-chain is the record).
pub async fn audit_kb(
    pool: &PgPool,
    ctx: &AuthContext,
    action: &str,
    kb_id: Uuid,
    payload: serde_json::Value,
) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("knowledge_base".into());
    ev.resource_id = Some(kb_id);
    ev.payload = Some(payload);
    let _ = audit::append(pool, &ev).await;
}

#[cfg(test)]
mod dr_scope_tests {
    use super::*;

    fn kb(name: &str) -> DrScopeKb {
        DrScopeKb {
            id: Uuid::now_v7(),
            name: name.into(),
            kind: "library".into(),
            origin_project_id: None,
            doc_count: 1,
        }
    }

    #[test]
    fn empty_request_returns_whole_scope() {
        let scope = vec![kb("a"), kb("b")];
        let got = intersect_scope(&scope, &[]);
        assert_eq!(got.len(), 2, "no narrowing ⇒ the full readable scope");
    }

    #[test]
    fn request_intersects_fail_closed() {
        let scope = vec![kb("a"), kb("b")];
        let wanted = scope[0].id;
        let unreadable = Uuid::now_v7(); // not in scope — must never leak through
        let got = intersect_scope(&scope, &[wanted, unreadable]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, wanted, "only readable+requested KBs survive");
    }

    #[test]
    fn disjoint_request_yields_nothing() {
        let scope = vec![kb("a")];
        assert!(intersect_scope(&scope, &[Uuid::now_v7()]).is_empty());
    }
}
