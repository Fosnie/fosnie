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

//! Memory facts. **Explicit-only**: facts are written solely when
//! the user asks — either via the `remember_fact` tool (LLM-mediated) or this
//! REST surface (deterministic + the moderation path). No background scraping,
//! no auto-extraction. Slot [4] of the compose order reads these back
//! (pinned-first, capped) — see `chat::load_memory`.
//!
//! Scope is `user` (about the person; owner = caller) or `project` (shared with
//! a project's members; owner = the project). The DB CHECK enforces exactly one
//! owner column per scope.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::rbac::{self, Permission};
use crate::auth::AuthContext;
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Insert a memory fact (the shared write path for both the tool and REST).
/// `scope` is `"user"` or `"project"`. Returns the new fact id. Audits
/// `memory.fact.created`.
pub async fn insert_fact(
    state: &AppState,
    ctx: &AuthContext,
    scope: &str,
    content: &str,
    project_id: Option<Uuid>,
    source_ref: Option<Uuid>,
) -> Result<Uuid> {
    let (owner_user_id, owner_project_id) = match scope {
        "user" => {
            let uid = ctx
                .user_id
                .ok_or_else(|| AppError::Forbidden("a user-scoped fact needs a user".into()))?;
            (Some(uid), None)
        }
        "project" => {
            let pid = project_id.ok_or_else(|| {
                AppError::Validation("a project-scoped fact needs a project chat".into())
            })?;
            // Writing into a project's shared memory requires write on that project.
            state.rbac.require_project(&state.pg, ctx, pid, Permission::Write).await?;
            (None, Some(pid))
        }
        other => return Err(AppError::Validation(format!("unknown memory scope: {other}"))),
    };

    let id = db::new_id();
    sqlx::query!(
        "INSERT INTO memory_facts \
         (id, scope, owner_user_id, owner_project_id, content, source_ref, created_by) \
         VALUES ($1, ($2::text)::mem_scope, $3, $4, $5, $6, $7)",
        id,
        scope,
        owner_user_id,
        owner_project_id,
        content,
        source_ref,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;

    audit_memory(state, ctx, "memory.fact.created", id).await;

    // Index for relevance recall (best-effort; Postgres remains source of truth).
    if let Some(key) = scope_key(scope, owner_user_id, owner_project_id) {
        let providers = crate::ml::provider_overrides(state, ctx.user_id).await;
        let _ = crate::ml::memory_upsert(&state.http, &state.boot.ml.base_url, &key, &id.to_string(), content, providers).await;
    }
    // Project-scoped facts post a system notice to the project chat so a power
    // user can moderate the addition. Best-effort.
    if let Some(pid) = owner_project_id {
        crate::http::messaging::notify_project_memory(state, pid, ctx.user_id, content).await;
    }
    Ok(id)
}

/// Qdrant collection scope key for a fact: `user_<uid>` or `proj_<pid>`.
fn scope_key(scope: &str, owner_user_id: Option<Uuid>, owner_project_id: Option<Uuid>) -> Option<String> {
    match scope {
        "user" => owner_user_id.map(|u| format!("user_{u}")),
        "project" => owner_project_id.map(|p| format!("proj_{p}")),
        _ => None,
    }
}

/// Slot-[4] recall: pinned facts always, plus either all non-pinned (small
/// memory) or the Qdrant-ranked top matches for `query` (large memory).
/// Used by the chat turn and the `/recall` test seam.
pub async fn recall(
    state: &AppState,
    ctx: &AuthContext,
    project_id: Option<Uuid>,
    query: &str,
) -> Result<Vec<String>> {
    const SMALL: i64 = 20;
    const CAP: i64 = 30;
    let uid = ctx.user_id;

    let mut pinned: Vec<String> = sqlx::query_scalar!(
        r#"SELECT content FROM memory_facts
           WHERE pinned AND ((scope = 'user' AND owner_user_id = $1)
                          OR (scope = 'project' AND owner_project_id = $2))
           ORDER BY updated_at DESC"#,
        uid,
        project_id
    )
    .fetch_all(&state.pg)
    .await?;

    let n: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM memory_facts
           WHERE NOT pinned AND ((scope = 'user' AND owner_user_id = $1)
                              OR (scope = 'project' AND owner_project_id = $2))"#,
        uid,
        project_id
    )
    .fetch_one(&state.pg)
    .await?;

    let nonpinned: Vec<String> = if n <= SMALL || query.trim().is_empty() {
        sqlx::query_scalar!(
            r#"SELECT content FROM memory_facts
               WHERE NOT pinned AND ((scope = 'user' AND owner_user_id = $1)
                                  OR (scope = 'project' AND owner_project_id = $2))
               ORDER BY updated_at DESC LIMIT $3"#,
            uid,
            project_id,
            CAP
        )
        .fetch_all(&state.pg)
        .await?
    } else {
        // Large memory → relevance-rank via Qdrant across both scopes.
        let providers = crate::ml::provider_overrides(state, ctx.user_id).await;
        let mut ids: Vec<Uuid> = Vec::new();
        if let Some(u) = uid {
            if let Ok(v) = crate::ml::memory_search(&state.http, &state.boot.ml.base_url, &format!("user_{u}"), query, 15, providers.clone()).await {
                ids.extend(v.iter().filter_map(|s| Uuid::parse_str(s).ok()));
            }
        }
        if let Some(p) = project_id {
            if let Ok(v) = crate::ml::memory_search(&state.http, &state.boot.ml.base_url, &format!("proj_{p}"), query, 15, providers.clone()).await {
                ids.extend(v.iter().filter_map(|s| Uuid::parse_str(s).ok()));
            }
        }
        if ids.is_empty() {
            // Search unavailable → fall back to recency (never silently empty).
            sqlx::query_scalar!(
                r#"SELECT content FROM memory_facts
                   WHERE NOT pinned AND ((scope = 'user' AND owner_user_id = $1)
                                      OR (scope = 'project' AND owner_project_id = $2))
                   ORDER BY updated_at DESC LIMIT $3"#,
                uid,
                project_id,
                CAP
            )
            .fetch_all(&state.pg)
            .await?
        } else {
            sqlx::query_scalar!(
                "SELECT content FROM memory_facts WHERE id = ANY($1) AND NOT pinned",
                &ids
            )
            .fetch_all(&state.pg)
            .await?
        }
    };

    pinned.extend(nonpinned);
    pinned.truncate(CAP as usize);
    Ok(pinned)
}

#[derive(Deserialize)]
pub struct RecallQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub project_id: Option<Uuid>,
}

/// Test/inspection seam: what slot [4] would inject for `q`.
pub async fn recall_facts(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<RecallQuery>,
) -> Result<Json<Vec<String>>> {
    // Same project gate as list_facts — recall would otherwise echo a project's
    // facts to a non-member. (The internal `recall` used by the chat turn is
    // already authorised by the chat's own project membership.)
    if let Some(pid) = q.project_id {
        state.rbac.require_project(&state.pg, &ctx, pid, Permission::Read).await?;
    }
    Ok(Json(recall(&state, &ctx, q.project_id, &q.q).await?))
}

#[derive(Deserialize)]
pub struct CreateFact {
    pub content: String,
    #[serde(default = "default_scope")]
    pub scope: String,
    #[serde(default)]
    pub project_id: Option<Uuid>,
}

fn default_scope() -> String {
    "user".into()
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

pub async fn create_fact(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateFact>,
) -> Result<Json<CreatedId>> {
    let content = body.content.trim();
    if content.is_empty() {
        return Err(AppError::Validation("content must not be empty".into()));
    }
    let id = insert_fact(&state, &ctx, &body.scope, content, body.project_id, None).await?;
    Ok(Json(CreatedId { id }))
}

#[derive(Deserialize)]
pub struct ListQuery {
    /// When set, list that project's facts; otherwise the caller's own user facts.
    #[serde(default)]
    pub project_id: Option<Uuid>,
}

#[derive(Serialize)]
pub struct FactOut {
    pub id: Uuid,
    pub scope: String,
    pub content: String,
    pub pinned: bool,
    pub user_edited: bool,
}

pub async fn list_facts(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<FactOut>>> {
    // Reading a project's shared memory requires read on that project; user-scoped
    // facts (project_id = None) are always the caller's own.
    if let Some(pid) = q.project_id {
        state.rbac.require_project(&state.pg, &ctx, pid, Permission::Read).await?;
    }
    let rows = sqlx::query!(
        r#"SELECT id, scope::text AS "scope!", content, pinned, user_edited
           FROM memory_facts
           WHERE (scope = 'user' AND owner_user_id = $1)
              OR (scope = 'project' AND owner_project_id = $2)
           ORDER BY pinned DESC, updated_at DESC"#,
        ctx.user_id,
        q.project_id,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| FactOut {
                id: r.id,
                scope: r.scope,
                content: r.content,
                pinned: r.pinned,
                user_edited: r.user_edited,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct UpdateFact {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub pinned: Option<bool>,
}

pub async fn update_fact(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateFact>,
) -> Result<Json<serde_json::Value>> {
    fetch_owned(&state.pg, &ctx, id).await?;
    // COALESCE keeps the existing value when a field is omitted; editing content
    // flags user_edited (manual moderation).
    sqlx::query!(
        "UPDATE memory_facts \
         SET content = COALESCE($2, content), \
             pinned = COALESCE($3, pinned), \
             user_edited = user_edited OR ($2 IS NOT NULL), \
             updated_at = now() \
         WHERE id = $1",
        id,
        body.content,
        body.pinned,
    )
    .execute(&state.pg)
    .await?;
    audit_memory(&state, &ctx, "memory.fact.edited", id).await;

    // Re-index the (possibly edited) content.
    if let Ok(row) = sqlx::query!(
        r#"SELECT scope::text AS "scope!", owner_user_id, owner_project_id, content FROM memory_facts WHERE id = $1"#,
        id
    )
    .fetch_one(&state.pg)
    .await
    {
        if let Some(key) = scope_key(&row.scope, row.owner_user_id, row.owner_project_id) {
            let providers = crate::ml::provider_overrides(&state, ctx.user_id).await;
            let _ = crate::ml::memory_upsert(&state.http, &state.boot.ml.base_url, &key, &id.to_string(), &row.content, providers).await;
        }
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn delete_fact(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    fetch_owned(&state.pg, &ctx, id).await?;
    // Capture scope key before deletion so we can de-index.
    let key = sqlx::query!(
        r#"SELECT scope::text AS "scope!", owner_user_id, owner_project_id FROM memory_facts WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .and_then(|r| scope_key(&r.scope, r.owner_user_id, r.owner_project_id));

    sqlx::query!("DELETE FROM memory_facts WHERE id = $1", id)
        .execute(&state.pg)
        .await?;
    audit_memory(&state, &ctx, "memory.fact.deleted", id).await;

    if let Some(key) = key {
        let _ = crate::ml::memory_delete(&state.http, &state.boot.ml.base_url, &key, &id.to_string()).await;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Enforce moderation RBAC: a user fact may be edited/deleted by its owner (or
/// an admin); a project fact by anyone with **write** on that project (admins
/// included via the rbac admin override). Takes a bare pool (no Redis) so it is
/// directly exercised by the security guard tests.
pub async fn fetch_owned(pg: &sqlx::PgPool, ctx: &AuthContext, id: Uuid) -> Result<()> {
    let row = sqlx::query!(
        r#"SELECT scope::text AS "scope!", owner_user_id, owner_project_id FROM memory_facts WHERE id = $1"#,
        id
    )
    .fetch_optional(pg)
    .await?
    .ok_or_else(|| AppError::Validation("memory fact not found".into()))?;

    match row.scope.as_str() {
        "user" => {
            if row.owner_user_id == ctx.user_id || ctx.is_admin() {
                Ok(())
            } else {
                Err(AppError::Forbidden("not permitted to moderate this fact".into()))
            }
        }
        _ => {
            let pid = row
                .owner_project_id
                .ok_or_else(|| AppError::Validation("project fact missing project".into()))?;
            rbac::require_project(pg, ctx, pid, Permission::Write).await
        }
    }
}

async fn audit_memory(state: &AppState, ctx: &AuthContext, action: &str, fact_id: Uuid) {
    let mut event = AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("memory_fact".into());
    event.resource_id = Some(fact_id);
    let _ = audit::append(&state.pg, &event).await;
}
