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

//! Skill CRUD + Agent attachment. A Skill is an instruction module
//! (open Agent-Skills standard): a `<id>/SKILL.md` folder on disk with YAML
//! frontmatter (name + description) and a Markdown body. The DB row is a pointer;
//! the slot-[2] metadata (name + description) is what rides the prompt — the full
//! SKILL.md is load-on-demand (deferred). Create/attach are power-user/admin.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::AuthContext;
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct CreateSkill {
    pub name: String,
    pub description: String,
    /// Markdown instruction body (after the frontmatter).
    #[serde(default)]
    pub body: String,
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "personal".into()
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

/// Owner-or-admin guard for managing (edit/delete) a skill. Personal-only: a non-admin
/// may manage only the skills they created; seeded/default skills (`is_default`, or
/// `created_by IS NULL`) and other people's are read-only. Admins manage anything.
async fn require_manage_skill(state: &AppState, ctx: &AuthContext, id: Uuid) -> Result<()> {
    if ctx.is_admin() {
        return Ok(());
    }
    let me = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let row = sqlx::query!("SELECT created_by, is_default FROM skills WHERE id = $1", id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("skill not found".into()))?;
    if !row.is_default && row.created_by == Some(me) {
        Ok(())
    } else {
        Err(AppError::Forbidden("only the skill's owner or an admin may manage it".into()))
    }
}

/// Admin, or the owner of the agent being modified — guards skill attach/detach
/// (changing an agent's skill set is an agent-management action).
async fn require_manage_agent_skills(state: &AppState, ctx: &AuthContext, agent_id: Uuid) -> Result<()> {
    if ctx.is_admin() {
        return Ok(());
    }
    let me = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let created_by = sqlx::query_scalar!(
        "SELECT created_by FROM agents WHERE id = $1 AND archived_at IS NULL",
        agent_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("agent not found".into()))?;
    if created_by == Some(me) {
        Ok(())
    } else {
        Err(AppError::Forbidden("only the agent's owner or an admin may change its skills".into()))
    }
}

/// Visibility (personal-only model): a caller may VIEW a skill if they are an admin,
/// it is a seeded/default/global skill (`is_default`, or `created_by IS NULL`), or
/// they created it. This mirrors the `list_skills` filter exactly — without it,
/// `get_skill` would return any user's private SKILL.md body by id (IDOR), and
/// `attach_skill` would bind a foreign private skill into an agent's prompt.
fn skill_visible(is_default: bool, created_by: Option<Uuid>, ctx: &AuthContext) -> bool {
    ctx.is_admin() || is_default || created_by.is_none() || created_by == ctx.user_id
}

/// Resolve the skills base dir to an absolute path (the dir may be relative in config).
fn skills_root(state: &AppState) -> PathBuf {
    crate::storage::resolve_dir(&state.boot.storage.skills_dir)
}

/// Resolve a stored (relative `<id>`) skill folder to its absolute path. Legacy
/// absolute rows pass through unchanged until the boot backfill/seed normalises them.
fn skill_dir(state: &AppState, stored: &str) -> String {
    crate::storage::resolve_file(&state.boot.storage.skills_dir, stored)
        .to_string_lossy()
        .to_string()
}

pub async fn create_skill(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateSkill>,
) -> Result<Json<CreatedId>> {
    // Personal-only: any authenticated user may create a skill — it becomes THEIRS.
    let me = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let id = db::new_id();

    let dir = skills_root(&state).join(id.to_string());
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("create skill dir: {e}")))?;
    // SKILL.md: YAML frontmatter + Markdown body (Agent-Skills standard shape).
    let md = format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}\n",
        body.name, body.description, body.body
    );
    let skill_md = dir.join("SKILL.md");
    tokio::fs::write(&skill_md, md)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("write SKILL.md: {e}")))?;
    // Store the RELATIVE folder (`<id>`) under `skills_dir`; resolved on read.
    let disk_path = id.to_string();

    sqlx::query!(
        "INSERT INTO skills (id, name, description, disk_path, scope, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        body.name,
        body.description,
        disk_path,
        body.scope,
        me,
    )
    .execute(&state.pg)
    .await?;

    audit_skill(&state, &ctx, "skill.created", id).await;
    Ok(Json(CreatedId { id }))
}

#[derive(Serialize)]
pub struct SkillSummary {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub scope: String,
    /// May the caller edit/delete this skill? Owner (created_by) or admin only;
    /// seeded/default skills are read-only for non-admins.
    pub can_manage: bool,
    /// True if this is a built-in default skill (applied to every agent).
    pub is_default: bool,
    /// Whether the skill is active. A disabled skill never enters the model's slot
    /// [2] / `read_skill`; toggled by `can_manage` callers (admin for defaults).
    pub enabled: bool,
}

pub async fn list_skills(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<SkillSummary>>> {
    let is_admin = ctx.is_admin();
    let me = ctx.user_id;
    // Visibility (personal-only): a non-admin sees default/seeded/global skills + their
    // own; others' personal skills are hidden. Admin sees all.
    let rows = sqlx::query!(
        r#"SELECT id, name, description, scope, created_by, is_default, enabled
           FROM skills
           WHERE $1 OR is_default OR created_by IS NULL OR created_by = $2
           ORDER BY created_at DESC"#,
        is_admin,
        me,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| SkillSummary {
                id: r.id,
                name: r.name,
                description: r.description,
                scope: r.scope,
                can_manage: is_admin || (!r.is_default && r.created_by.is_some() && r.created_by == me),
                is_default: r.is_default,
                enabled: r.enabled,
            })
            .collect(),
    ))
}

/// Strip the YAML frontmatter from a SKILL.md, returning the Markdown body.
/// Normalises CRLF→LF first so a Windows-authored file (whose closing delimiter is
/// `\r\n---`) still parses and does not strip to an empty body.
pub(crate) fn strip_frontmatter(md: &str) -> String {
    let normalised = md.replace("\r\n", "\n");
    let s = normalised.trim_start();
    if let Some(rest) = s.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            return rest[end + 4..].trim_start().to_string();
        }
    }
    normalised.trim().to_string()
}

pub(crate) async fn read_skill_body(disk_path: &str) -> Result<String> {
    let path = std::path::Path::new(disk_path).join("SKILL.md");
    let md = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("read SKILL.md: {e}")))?;
    Ok(strip_frontmatter(&md))
}

#[derive(Serialize)]
pub struct SkillDetail {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub body: String,
    pub scope: String,
    /// May the caller edit/delete this skill? (owner or admin; seeded/default = false)
    pub can_manage: bool,
    pub is_default: bool,
    pub enabled: bool,
}

pub async fn get_skill(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SkillDetail>> {
    let row = sqlx::query!(
        "SELECT name, description, disk_path, scope, created_by, is_default, enabled FROM skills WHERE id = $1",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("skill not found".into()))?;
    // Object-level authz: a non-admin must not read another user's personal skill
    // body. Return the same "not found" as a missing id so the id is not an
    // existence oracle.
    if !skill_visible(row.is_default, row.created_by, &ctx) {
        return Err(AppError::Validation("skill not found".into()));
    }
    let body = read_skill_body(&skill_dir(&state, &row.disk_path)).await.unwrap_or_default();
    let can_manage =
        ctx.is_admin() || (!row.is_default && row.created_by.is_some() && row.created_by == ctx.user_id);
    Ok(Json(SkillDetail {
        id,
        name: row.name,
        description: row.description,
        body,
        scope: row.scope,
        can_manage,
        is_default: row.is_default,
        enabled: row.enabled,
    }))
}

#[derive(Deserialize)]
pub struct SetEnabled {
    pub enabled: bool,
}

/// Enable/disable a skill. A disabled skill stays
/// in the DB + admin list but never enters the model's slot [2] / `read_skill`.
/// Same guard as edit/delete — admin for default/seeded skills, owner for their own.
pub async fn set_skill_enabled(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<SetEnabled>,
) -> Result<Json<serde_json::Value>> {
    require_manage_skill(&state, &ctx, id).await?;
    sqlx::query!("UPDATE skills SET enabled = $2 WHERE id = $1", id, req.enabled)
        .execute(&state.pg)
        .await?;
    audit_skill(&state, &ctx, if req.enabled { "skill.enabled" } else { "skill.disabled" }, id).await;
    Ok(Json(serde_json::json!({ "ok": true, "enabled": req.enabled })))
}

#[derive(Deserialize)]
pub struct UpdateSkill {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

pub async fn update_skill(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateSkill>,
) -> Result<Json<serde_json::Value>> {
    require_manage_skill(&state, &ctx, id).await?;
    let row = sqlx::query!("SELECT name, description, disk_path FROM skills WHERE id = $1", id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("skill not found".into()))?;
    let name = req.name.unwrap_or(row.name);
    let description = req.description.unwrap_or(row.description);
    let dir = skill_dir(&state, &row.disk_path);
    let body = match req.body {
        Some(b) => b,
        None => read_skill_body(&dir).await.unwrap_or_default(),
    };
    // Rewrite the SKILL.md (frontmatter + body) and the DB pointer fields.
    let md = format!("---\nname: {}\ndescription: {}\n---\n\n{}\n", name, description, body);
    tokio::fs::write(std::path::Path::new(&dir).join("SKILL.md"), md)
        .await
        .map_err(|e| AppError::Other(anyhow::anyhow!("write SKILL.md: {e}")))?;
    sqlx::query!("UPDATE skills SET name = $2, description = $3 WHERE id = $1", id, name, description)
        .execute(&state.pg)
        .await?;
    audit_skill(&state, &ctx, "skill.updated", id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn delete_skill(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_manage_skill(&state, &ctx, id).await?;
    let disk_path: Option<String> =
        sqlx::query_scalar!("SELECT disk_path FROM skills WHERE id = $1", id)
            .fetch_optional(&state.pg)
            .await?;
    let mut tx = state.pg.begin().await?;
    sqlx::query!("DELETE FROM agent_skills WHERE skill_id = $1", id).execute(&mut *tx).await?;
    sqlx::query!("DELETE FROM skills WHERE id = $1", id).execute(&mut *tx).await?;
    tx.commit().await?;
    if let Some(p) = disk_path {
        let _ = tokio::fs::remove_dir_all(skill_dir(&state, &p)).await; // best-effort
    }
    audit_skill(&state, &ctx, "skill.deleted", id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct TestSkill {
    pub input: String,
}

#[derive(Serialize)]
pub struct SkillTestOut {
    pub output: String,
}

/// Dry-run a Skill: feed its instructions as the system message and the caller's
/// sample input as the user message through the ML chat-step, returning the text.
pub async fn test_skill(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<TestSkill>,
) -> Result<Json<SkillTestOut>> {
    require_manage_skill(&state, &ctx, id).await?;
    let disk_path: String = sqlx::query_scalar!("SELECT disk_path FROM skills WHERE id = $1", id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("skill not found".into()))?;
    let body = read_skill_body(&skill_dir(&state, &disk_path)).await.unwrap_or_default();
    let messages = vec![
        serde_json::json!({ "role": "system", "content": body }),
        serde_json::json!({ "role": "user", "content": req.input }),
    ];
    let sampling = crate::ml::Sampling { max_tokens: Some(512), ..Default::default() };
    let step = crate::ml::chat_step(&state.http, &state.boot.ml.base_url, &messages, None, &sampling, crate::ml::provider_overrides(&state, ctx.user_id).await).await?;
    Ok(Json(SkillTestOut { output: step.content }))
}

/// Attach a Skill to an Agent (idempotent).
pub async fn attach_skill(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((agent_id, skill_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    require_manage_agent_skills(&state, &ctx, agent_id).await?;
    // You may only attach a skill you can SEE — otherwise a foreign private skill's
    // body would be smuggled into your agent's system prompt and exfiltrated at run
    // time. Same "not found" as a missing id (no existence oracle).
    let skill = sqlx::query!("SELECT created_by, is_default FROM skills WHERE id = $1", skill_id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("skill not found".into()))?;
    if !skill_visible(skill.is_default, skill.created_by, &ctx) {
        return Err(AppError::Validation("skill not found".into()));
    }
    sqlx::query!(
        "INSERT INTO agent_skills (agent_id, skill_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        agent_id,
        skill_id
    )
    .execute(&state.pg)
    .await?;
    audit_skill(&state, &ctx, "skill.attached", skill_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Detach a Skill from an Agent.
pub async fn detach_skill(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((agent_id, skill_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    require_manage_agent_skills(&state, &ctx, agent_id).await?;
    sqlx::query!(
        "DELETE FROM agent_skills WHERE agent_id = $1 AND skill_id = $2",
        agent_id,
        skill_id
    )
    .execute(&state.pg)
    .await?;
    audit_skill(&state, &ctx, "skill.detached", skill_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn audit_skill(state: &AppState, ctx: &AuthContext, action: &str, skill_id: Uuid) {
    let mut event = AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("skill".into());
    event.resource_id = Some(skill_id);
    let _ = audit::append(&state.pg, &event).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::PlatformRole;

    fn ctx(role: PlatformRole, user_id: Option<Uuid>) -> AuthContext {
        AuthContext { user_id, email: None, display_name: None, role, break_glass: role == PlatformRole::SuperAdmin, mfa_enroll_only: false }
    }

    #[test]
    fn other_users_personal_skill_is_hidden() {
        let alice = Uuid::from_u128(1);
        let bob = Uuid::from_u128(2);
        let bob_ctx = ctx(PlatformRole::User, Some(bob));
        // Alice's personal skill is invisible to Bob (the IDOR this guards).
        assert!(!skill_visible(false, Some(alice), &bob_ctx));
        // Bob sees his own; everyone sees seeded (created_by NULL) and default skills.
        assert!(skill_visible(false, Some(bob), &bob_ctx));
        assert!(skill_visible(false, None, &bob_ctx));
        assert!(skill_visible(true, Some(alice), &bob_ctx));
    }

    #[test]
    fn admins_and_breakglass_see_every_skill() {
        let alice = Uuid::from_u128(1);
        assert!(skill_visible(false, Some(alice), &ctx(PlatformRole::ClientAdmin, Some(Uuid::from_u128(9)))));
        assert!(skill_visible(false, Some(alice), &ctx(PlatformRole::SuperAdmin, None)));
    }

    #[test]
    fn strip_frontmatter_lf_returns_body() {
        let md = "---\nname: X\ndescription: Y\n---\n\nHello body\n";
        assert_eq!(strip_frontmatter(md).trim(), "Hello body");
    }

    #[test]
    fn strip_frontmatter_crlf_returns_nonempty_body() {
        // A Windows-authored SKILL.md must not strip to an empty body (the delimiter
        // is `\r\n---`). Regression for the empty-Instructions bug.
        let md = "---\r\nname: X\r\ndescription: Y\r\n---\r\n\r\nBODY\r\n";
        let body = strip_frontmatter(md);
        assert!(!body.trim().is_empty(), "CRLF body must not strip to empty");
        assert_eq!(body.trim(), "BODY");
    }

    #[test]
    fn strip_frontmatter_no_frontmatter_returns_trimmed() {
        assert_eq!(strip_frontmatter("  just body  "), "just body");
    }
}
