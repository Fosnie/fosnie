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

//! Prompt templates. A Prompt is a Markdown template with
//! `{{placeholder}}` slots, invoked from the composer with `/`. Content lives in
//! Postgres (`prompts.content`). Rendering substitutes values and returns plain
//! text the client sends as a normal `chat.send` — so there is no turn-driver
//! change. Personal prompts: any user; project/global: power-user.

use std::collections::BTreeSet;

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::rbac::{self, Permission};
use crate::auth::{AuthContext, PlatformRole};
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Extract the distinct `{{placeholder}}` names from a template, in first-seen
/// order. Whitespace inside the braces is trimmed; malformed/unclosed braces are
/// ignored. Pure — unit-tested.
pub fn placeholders(template: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        match after.find("}}") {
            Some(close) => {
                let name = after[..close].trim();
                if !name.is_empty() && seen.insert(name.to_string()) {
                    out.push(name.to_string());
                }
                rest = &after[close + 2..];
            }
            None => break, // unclosed — stop scanning
        }
    }
    out
}

/// Substitute `{{name}}` occurrences with the supplied values. Unknown
/// placeholders are left intact (so a partial render is visible, not silently
/// blanked). Pure — unit-tested.
pub fn render(template: &str, values: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        match after.find("}}") {
            Some(close) => {
                out.push_str(&rest[..open]); // literal text before the slot
                let raw = &after[..close];
                let name = raw.trim();
                match values.get(name).and_then(value_as_str) {
                    Some(v) => out.push_str(&v),
                    None => {
                        // unknown placeholder — leave the `{{...}}` intact
                        out.push_str("{{");
                        out.push_str(raw);
                        out.push_str("}}");
                    }
                }
                rest = &after[close + 2..];
            }
            None => break, // unclosed — emit the remainder verbatim below
        }
    }
    out.push_str(rest);
    out
}

fn value_as_str(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

#[derive(Deserialize)]
pub struct CreatePrompt {
    pub name: String,
    pub content: String,
    #[serde(default = "default_scope")]
    pub scope: String, // personal | project | global
    #[serde(default)]
    pub project_id: Option<Uuid>,
    /// Optional default Agent the client pre-selects when invoking this prompt.
    #[serde(default)]
    pub agent_id: Option<Uuid>,
    /// Friendly field metadata for the `{{key}}` slots — label, input type, help,
    /// options. The author builds these visually; the template stays `{{key}}`.
    #[serde(default)]
    pub variables: Vec<PromptVar>,
}

/// One template field's presentation metadata. `kind` ∈ short | long | date |
/// select. Storage is `{{key}}` in the content; this annotates it for the UI.
#[derive(Serialize, Deserialize, Clone)]
pub struct PromptVar {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
}

fn default_scope() -> String {
    "personal".into()
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

pub async fn create_prompt(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreatePrompt>,
) -> Result<Json<CreatedId>> {
    // Personal scope is open to any user; project/global needs power-user/admin.
    if body.scope != "personal"
        && !matches!(
            ctx.role,
            PlatformRole::PowerUser | PlatformRole::ClientAdmin | PlatformRole::SuperAdmin
        )
    {
        return Err(AppError::Forbidden(
            "only a power user or admin may create project/global prompts".into(),
        ));
    }
    // A named default Agent must exist (a bad id is a 400, not an FK 500).
    if let Some(aid) = body.agent_id {
        let exists = sqlx::query_scalar!("SELECT 1 AS x FROM agents WHERE id = $1", aid)
            .fetch_optional(&state.pg)
            .await?
            .is_some();
        if !exists {
            return Err(AppError::Validation("agent_id does not exist".into()));
        }
    }
    let id = db::new_id();
    let variables = serde_json::to_value(&body.variables).unwrap_or(serde_json::Value::Null);
    sqlx::query!(
        "INSERT INTO prompts (id, name, content, scope, project_id, created_by, agent_id, variables) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        id,
        body.name,
        body.content,
        body.scope,
        body.project_id,
        ctx.user_id,
        body.agent_id,
        variables,
    )
    .execute(&state.pg)
    .await?;

    audit_prompt(&state, &ctx, "prompt.created", id).await;
    Ok(Json(CreatedId { id }))
}

#[derive(Serialize)]
pub struct PromptSummary {
    pub id: Uuid,
    pub name: String,
    pub scope: String,
    pub project_id: Option<Uuid>,
    pub agent_id: Option<Uuid>,
}

/// Visible prompts: the caller's own personal prompts + all project/global ones.
pub async fn list_prompts(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<PromptSummary>>> {
    let rows = sqlx::query!(
        "SELECT id, name, scope, project_id, agent_id, created_by FROM prompts \
         WHERE scope <> 'personal' OR created_by = $1 \
         ORDER BY created_at DESC",
        ctx.user_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| PromptSummary {
                id: r.id,
                name: r.name,
                scope: r.scope,
                project_id: r.project_id,
                agent_id: r.agent_id,
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct PromptDetail {
    pub id: Uuid,
    pub name: String,
    pub content: String,
    pub placeholders: Vec<String>,
    pub agent_id: Option<Uuid>,
    /// Field metadata (label/type/help/options) for the `{{key}}` slots; `[]` for a
    /// legacy prompt without it (the UI then derives labels from the keys).
    pub variables: Vec<PromptVar>,
}

/// A prompt's stored fields after a scope-visibility check has passed.
pub struct AuthorizedPrompt {
    name: String,
    content: String,
    agent_id: Option<Uuid>,
    variables: Vec<PromptVar>,
}

/// Load a prompt and enforce scope visibility: `global` → any user; `personal`
/// → its creator (or admin); `project` → read on the project. Without this a
/// known/guessed id would expose another user's personal prompt text (IDOR).
/// Content comes from the DB; a legacy file-backed row (NULL content) is read
/// best-effort from disk and degrades to empty — never a 500, so the UI can't hang.
/// Takes a bare pool (no Redis) so the security guard tests exercise it directly.
pub async fn load_authorized(pg: &sqlx::PgPool, ctx: &AuthContext, id: Uuid) -> Result<AuthorizedPrompt> {
    let row = sqlx::query!(
        "SELECT name, content, content_path, agent_id, scope, project_id, created_by, variables FROM prompts WHERE id = $1",
        id
    )
    .fetch_optional(pg)
    .await?
    .ok_or_else(|| AppError::Validation("prompt not found".into()))?;

    let allowed = match row.scope.as_str() {
        "global" => true,
        "personal" => row.created_by == ctx.user_id || ctx.is_admin(),
        "project" => match row.project_id {
            Some(pid) => rbac::project_can(pg, ctx, pid, Permission::Read).await?,
            None => ctx.is_admin(),
        },
        _ => ctx.is_admin(),
    };
    if !allowed {
        return Err(AppError::Forbidden("not permitted to view this prompt".into()));
    }

    let content = match (row.content, row.content_path) {
        (Some(c), _) => c,
        // Legacy file-backed prompt: best-effort read, empty on failure.
        (None, Some(path)) => tokio::fs::read_to_string(&path).await.unwrap_or_default(),
        (None, None) => String::new(),
    };
    let variables = row.variables.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    Ok(AuthorizedPrompt { name: row.name, content, agent_id: row.agent_id, variables })
}

pub async fn get_prompt(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<PromptDetail>> {
    let row = load_authorized(&state.pg, &ctx, id).await?;
    let ph = placeholders(&row.content);
    Ok(Json(PromptDetail { id, name: row.name, content: row.content, placeholders: ph, agent_id: row.agent_id, variables: row.variables }))
}

#[derive(Deserialize)]
pub struct RenderRequest {
    #[serde(default)]
    pub values: serde_json::Map<String, serde_json::Value>,
}

#[derive(Serialize)]
pub struct RenderedPrompt {
    pub content: String,
}

pub async fn render_prompt(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<RenderRequest>,
) -> Result<Json<RenderedPrompt>> {
    let row = load_authorized(&state.pg, &ctx, id).await?;
    let content = render(&row.content, &body.values);
    audit_prompt(&state, &ctx, "prompt.invoked", id).await;
    Ok(Json(RenderedPrompt { content }))
}

async fn audit_prompt(state: &AppState, ctx: &AuthContext, action: &str, prompt_id: Uuid) {
    let mut event = AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("prompt".into());
    event.resource_id = Some(prompt_id);
    let _ = audit::append(&state.pg, &event).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_distinct_in_order() {
        let t = "Dear {{ name }}, your {{item}} re {{name}} and {{ topic }}.";
        assert_eq!(placeholders(t), vec!["name", "item", "topic"]);
    }

    #[test]
    fn placeholders_ignores_unclosed() {
        assert!(placeholders("hi {{ name").is_empty());
    }

    #[test]
    fn render_preserves_non_ascii_literals() {
        let mut m = serde_json::Map::new();
        m.insert("name".into(), serde_json::json!("Renée"));
        assert_eq!(render("Café — {{name}} — naïve", &m), "Café — Renée — naïve");
    }

    #[test]
    fn render_substitutes_known_keeps_unknown() {
        let mut m = serde_json::Map::new();
        m.insert("name".into(), serde_json::json!("Alice"));
        let out = render("Hi {{name}}, see {{missing}}.", &m);
        assert_eq!(out, "Hi Alice, see {{missing}}.");
    }

    #[test]
    fn render_handles_non_string_values() {
        let mut m = serde_json::Map::new();
        m.insert("n".into(), serde_json::json!(42));
        assert_eq!(render("count={{n}}", &m), "count=42");
    }
}
