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

//! Deep Research report templates: the catalogue the picker reads and CRUD for
//! user-defined templates.
//!
//! A report template controls the *structure* and *writing style* of a Deep
//! Research report: its section skeleton, per-section briefs, an outline mode
//! (fixed structure vs a structure that follows the question) and a block of
//! writing instructions prepended verbatim to the writer. It does NOT touch
//! search depth, budgets or verification.
//!
//! Two sources, not four. The four built-ins live as code constants in the
//! research service, which owns their behaviour and their tuned prompts; this
//! module keeps only the picker *metadata* for them ([`BUILTIN_TEMPLATES`]) and
//! never sends their bodies over the wire. User-defined templates live in the
//! `research_templates` table, personal by default. Duplicating a built-in into
//! an editable copy is the one action that fetches a built-in's full body, from
//! the research service on an explicit click (never on page load).

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::permissions::RESEARCH_TEMPLATES_MANAGE;
use crate::auth::AuthContext;
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;

// ---- Built-in picker metadata ----------------------------------------------

/// Picker metadata for a built-in template. These mirror the four templates the
/// research service defines (`ml/app/research/templates.py`): the picker needs
/// their shape, the service owns their behaviour and their prompts. Labels,
/// section headings and outline modes are pinned against the service by
/// `builtins_mirror_the_research_service` below (and a matching test on the
/// service side) — update BOTH sides when a built-in changes. Descriptions are
/// picker copy and have no counterpart in the service.
pub struct BuiltinTemplate {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub structure: &'static [&'static str],
    pub outline_mode: &'static str,
}

/// The four built-ins, in the research service's own registry order. Only what
/// the picker needs — no writing instructions, no section briefs (those are
/// fetched from the service when a user duplicates a built-in).
pub const BUILTIN_TEMPLATES: &[BuiltinTemplate] = &[
    BuiltinTemplate {
        id: "exploration",
        label: "Exploration brief",
        description:
            "Question-driven brief for working on something new \u{2014} lays out the landscape and options.",
        structure: &[
            "Context & framing",
            "Landscape & options",
            "Key unknowns & risks",
            "Recommendations",
        ],
        outline_mode: "constrained",
    },
    BuiltinTemplate {
        id: "formal",
        label: "Formal report",
        description: "Measured, third-person report; every claim cites its source.",
        structure: &[
            "Executive summary",
            "Background",
            "Findings",
            "Analysis",
            "Conclusions & recommendations",
        ],
        outline_mode: "constrained",
    },
    BuiltinTemplate {
        id: "freeform",
        label: "Free-form",
        description: "No fixed skeleton \u{2014} the structure follows your question.",
        structure: &[],
        outline_mode: "free",
    },
    BuiltinTemplate {
        id: "literature",
        label: "Literature review",
        description:
            "Academic review synthesising themes across the corpus; agreements, conflicts and gaps.",
        structure: &[
            "Executive summary",
            "Introduction & scope",
            "Review method & corpus",
            "Themes in the literature",
            "Consensus, contradictions and gaps",
            "Conclusions & further research",
        ],
        outline_mode: "constrained",
    },
];

/// A built-in by id, or None (the id is a user-defined UUID or unknown).
pub fn builtin_by_id(id: &str) -> Option<&'static BuiltinTemplate> {
    BUILTIN_TEMPLATES.iter().find(|t| t.id == id)
}

/// The heading the pipeline fills with the deterministic corpus analysis under
/// `files`/`hybrid` (matching `corpus_analysis.SECTION_HEADING` in the research
/// service). A template may use it deliberately to place the analysis; it just
/// cannot also be the executive summary, since the pipeline overwrites its body.
const RESERVED_ANALYSIS_HEADING: &str = "Consensus, contradictions and gaps";

// Field length limits (kept in step with the editor hints). `writing_instructions`
// is prepended verbatim to the writer's system prompt, hence the generous cap.
const MAX_LABEL: usize = 60;
const MAX_HEADING: usize = 120;
const MAX_BRIEF: usize = 300;
const MAX_WRITING_INSTRUCTIONS: usize = 4000;
const MAX_SECTIONS: usize = 12;

// ---- Wire / storage shapes --------------------------------------------------

/// One section as authored in the editor and stored in the `skeleton` JSONB. The
/// per-section flags are what the user toggles; the research service derives the
/// tuple-of-expandable-headings / placeholder-sentinel shape it consumes at
/// serialisation, so renaming a heading can never orphan a flag.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SectionInput {
    pub heading: String,
    #[serde(default)]
    pub brief: String,
    /// The outline may expand this heading into several sections.
    #[serde(default)]
    pub expandable: bool,
    /// This section is the executive summary: emitted last and filled by the
    /// coherence pass, not the writer.
    #[serde(default)]
    pub exec_summary: bool,
}

/// The editable content of a template, shared by create and update. `scope` and
/// identity live outside it.
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateContent {
    // Defaulted so this struct can be `#[serde(flatten)]`ed behind an optional
    // `duplicate_of` body without serde demanding it (see `CreateTemplate`). An
    // empty label is caught by `normalise_and_validate`, not by the deserializer.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub skeleton: Vec<SectionInput>,
    #[serde(default)]
    pub writing_instructions: String,
    #[serde(default = "default_outline_mode")]
    pub outline_mode: String,
}

fn default_outline_mode() -> String {
    "constrained".into()
}

/// Normalise then validate the content. In "free" mode the per-section flags do
/// nothing downstream, so they are cleared here — never "saved silently and
/// ignored". Mutates `content` in place; returns an error the frontend shows.
fn normalise_and_validate(content: &mut TemplateContent) -> Result<()> {
    let constrained = match content.outline_mode.as_str() {
        "constrained" => true,
        "free" => false,
        other => {
            return Err(AppError::Validation(format!(
                "unknown outline mode '{other}'"
            )))
        }
    };

    // Free mode: the outline invents structure; the section flags are inert, so
    // clear them rather than persist a flag that will never fire.
    if !constrained {
        for s in &mut content.skeleton {
            s.expandable = false;
            s.exec_summary = false;
        }
    }

    let label = content.label.trim();
    if label.is_empty() {
        return Err(AppError::Validation("a template name is required".into()));
    }
    if label.chars().count() > MAX_LABEL {
        return Err(AppError::Validation(format!(
            "the name must be {MAX_LABEL} characters or fewer"
        )));
    }
    if content.writing_instructions.chars().count() > MAX_WRITING_INSTRUCTIONS {
        return Err(AppError::Validation(format!(
            "the writing style must be {MAX_WRITING_INSTRUCTIONS} characters or fewer"
        )));
    }

    if content.skeleton.len() > MAX_SECTIONS {
        return Err(AppError::Validation(format!(
            "a template can have at most {MAX_SECTIONS} sections"
        )));
    }
    if constrained && content.skeleton.is_empty() {
        return Err(AppError::Validation(
            "a fixed-structure template needs at least one section".into(),
        ));
    }

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut exec_summaries = 0usize;
    for s in &mut content.skeleton {
        // Trim in place so the STORED heading equals the one we validated — the
        // uniqueness invariant would otherwise be weaker in the DB than on the wire.
        s.heading = s.heading.trim().to_string();
        let heading = s.heading.as_str();
        if heading.is_empty() {
            return Err(AppError::Validation("every section needs a heading".into()));
        }
        if heading.chars().count() > MAX_HEADING {
            return Err(AppError::Validation(format!(
                "a section heading must be {MAX_HEADING} characters or fewer"
            )));
        }
        if s.brief.chars().count() > MAX_BRIEF {
            return Err(AppError::Validation(format!(
                "a section brief must be {MAX_BRIEF} characters or fewer"
            )));
        }
        // Case- and whitespace-insensitive, matching the service's heading match.
        if !seen.insert(heading.to_lowercase()) {
            return Err(AppError::Validation(format!(
                "section headings must be unique; '{heading}' is repeated"
            )));
        }
        if s.exec_summary {
            exec_summaries += 1;
            if heading.eq_ignore_ascii_case(RESERVED_ANALYSIS_HEADING) {
                return Err(AppError::Validation(
                    "this heading is filled with the corpus analysis; it cannot also be the executive summary".into(),
                ));
            }
        }
    }
    if exec_summaries > 1 {
        return Err(AppError::Validation(
            "only one section can be the executive summary".into(),
        ));
    }
    Ok(())
}

// ---- Catalogue --------------------------------------------------------------

#[derive(Serialize)]
pub struct BuiltinOut {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub structure: Vec<&'static str>,
    pub outline_mode: &'static str,
}

#[derive(Serialize)]
pub struct CustomSummary {
    pub id: Uuid,
    pub label: String,
    pub description: String,
    /// Section headings only, for the picker preview (briefs/flags load with the
    /// full detail in the editor).
    pub structure: Vec<String>,
    pub outline_mode: String,
    pub scope: String,
    /// May the caller edit/delete this template?
    pub can_manage: bool,
}

#[derive(Serialize)]
pub struct Catalogue {
    pub builtin: Vec<BuiltinOut>,
    pub custom: Vec<CustomSummary>,
}

/// The picker catalogue: the built-ins from the constant (never touches the ML
/// service, so the page loads even when the service is down) plus the caller's
/// visible, non-archived user-defined templates.
pub async fn list_templates(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Catalogue>> {
    let builtin = BUILTIN_TEMPLATES
        .iter()
        .map(|t| BuiltinOut {
            id: t.id,
            label: t.label,
            description: t.description,
            structure: t.structure.to_vec(),
            outline_mode: t.outline_mode,
        })
        .collect();

    let is_admin = ctx.is_admin();
    let me = ctx.user_id;
    let can_manage_global = state
        .rbac
        .has_permission(&state.pg, &ctx, RESEARCH_TEMPLATES_MANAGE)
        .await?;
    let rows = sqlx::query!(
        r#"SELECT id, label, description, skeleton, outline_mode, scope, created_by
           FROM research_templates
           WHERE ($1 OR scope = 'global' OR created_by = $2) AND archived_at IS NULL
           ORDER BY created_at DESC"#,
        is_admin,
        me,
    )
    .fetch_all(&state.pg)
    .await?;
    let custom = rows
        .into_iter()
        .map(|r| CustomSummary {
            structure: headings_of(&r.skeleton),
            can_manage: can_manage_row(&r.scope, r.created_by, is_admin, me, can_manage_global),
            id: r.id,
            label: r.label,
            description: r.description,
            outline_mode: r.outline_mode,
            scope: r.scope,
        })
        .collect();

    Ok(Json(Catalogue { builtin, custom }))
}

/// Extract the section headings from a stored `skeleton` JSONB, tolerating a row
/// written before a schema tweak (missing/blank headings are skipped).
fn headings_of(skeleton: &serde_json::Value) -> Vec<String> {
    skeleton
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.get("heading").and_then(|h| h.as_str()))
                .filter(|h| !h.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// May the caller manage this row? A global template is gated on the
/// deployment-wide permission (its writing instructions run for other users); a
/// personal one is owner-or-admin.
fn can_manage_row(
    scope: &str,
    created_by: Option<Uuid>,
    is_admin: bool,
    me: Option<Uuid>,
    can_manage_global: bool,
) -> bool {
    if scope == "global" {
        can_manage_global
    } else {
        is_admin || (created_by.is_some() && created_by == me)
    }
}

// ---- Detail -----------------------------------------------------------------

#[derive(Serialize, Debug)]
pub struct TemplateDetail {
    pub id: Uuid,
    pub label: String,
    pub description: String,
    pub skeleton: Vec<SectionInput>,
    pub writing_instructions: String,
    pub outline_mode: String,
    pub scope: String,
    pub can_manage: bool,
    /// True when the template has been archived (soft-deleted). The catalogue
    /// hides these, but the picker still resolves one by id when an existing chat
    /// points at it, so this endpoint returns it.
    pub archived: bool,
}

pub async fn get_template(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<TemplateDetail>> {
    let row = sqlx::query!(
        r#"SELECT label, description, skeleton, writing_instructions, outline_mode, scope,
                  created_by, archived_at
           FROM research_templates WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::NotFound("template not found".into()))?;

    // Object-level authz: another user's personal template is invisible. Same
    // "not found" as a missing id so the id is not an existence oracle.
    if !template_visible(&row.scope, row.created_by, &ctx) {
        return Err(AppError::NotFound("template not found".into()));
    }
    let can_manage_global = state
        .rbac
        .has_permission(&state.pg, &ctx, RESEARCH_TEMPLATES_MANAGE)
        .await?;
    Ok(Json(TemplateDetail {
        id,
        skeleton: parse_skeleton(&row.skeleton),
        can_manage: can_manage_row(&row.scope, row.created_by, ctx.is_admin(), ctx.user_id, can_manage_global),
        label: row.label,
        description: row.description,
        writing_instructions: row.writing_instructions,
        outline_mode: row.outline_mode,
        scope: row.scope,
        archived: row.archived_at.is_some(),
    }))
}

fn parse_skeleton(skeleton: &serde_json::Value) -> Vec<SectionInput> {
    serde_json::from_value(skeleton.clone()).unwrap_or_default()
}

/// A caller may VIEW a template if they are an admin, it is global, or they
/// created it. Mirrors the `list_templates` filter (guards `get_template` IDOR).
pub(crate) fn template_visible(scope: &str, created_by: Option<Uuid>, ctx: &AuthContext) -> bool {
    ctx.is_admin() || scope == "global" || created_by == ctx.user_id
}

// ---- Create -----------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateTemplate {
    /// When set — a built-in id or an existing template's UUID — the new template
    /// starts as an editable personal copy of that one (label suffixed " (copy)").
    /// The flattened content fields are then ignored. NOTE: `content` is NOT an
    /// `Option<TemplateContent>`: `#[serde(flatten)]` over `Option` never
    /// deserializes to `None` (serde-rs/serde#1626), so a duplicate-only body
    /// `{"duplicate_of":"..."}` would fail on the missing `label`. All
    /// `TemplateContent` fields default instead, and the empty-label case is
    /// rejected by `normalise_and_validate` on the from-scratch path.
    #[serde(default)]
    pub duplicate_of: Option<String>,
    #[serde(flatten)]
    pub content: TemplateContent,
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "personal".into()
}

#[derive(Serialize, Debug)]
pub struct CreatedId {
    pub id: Uuid,
}

pub async fn create_template(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateTemplate>,
) -> Result<Json<CreatedId>> {
    let me = ctx
        .user_id
        .ok_or_else(|| AppError::Forbidden("a user is required".into()))?;

    // Duplicating forks the source into a personal, editable copy. A UUID names an
    // existing template in the store (its body is already here — no ML needed); a
    // built-in slug is fetched from the research service, which owns its body.
    // Otherwise the flattened content is a from-scratch template.
    let (mut content, scope) = if let Some(src) = body.duplicate_of.as_deref() {
        let content = if let Ok(uuid) = Uuid::parse_str(src) {
            duplicate_custom(&state, &ctx, uuid).await?
        } else {
            duplicate_builtin(&state, src).await?
        };
        (content, "personal".to_string())
    } else {
        (body.content, body.scope)
    };

    if scope != "personal" && scope != "global" {
        return Err(AppError::Validation(format!("unknown scope '{scope}'")));
    }
    // Publishing a template deployment-wide runs its writing instructions for
    // other people; that is the boundary the permission guards.
    if scope == "global" {
        state
            .rbac
            .require_permission(&state.pg, &ctx, RESEARCH_TEMPLATES_MANAGE)
            .await?;
    }
    normalise_and_validate(&mut content)?;

    let id = db::new_id();
    let skeleton = serde_json::to_value(&content.skeleton)
        .map_err(|e| AppError::Other(anyhow::anyhow!("encode skeleton: {e}")))?;
    sqlx::query!(
        r#"INSERT INTO research_templates
             (id, label, description, skeleton, writing_instructions, outline_mode, scope, created_by)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
        id,
        content.label.trim(),
        content.description,
        skeleton,
        content.writing_instructions,
        content.outline_mode,
        scope,
        me,
    )
    .execute(&state.pg)
    .await?;

    audit_template(&state, &ctx, "research.template.created", id).await;
    Ok(Json(CreatedId { id }))
}

/// Fetch a built-in's full definition from the research service and shape it into
/// an editable copy. This is the only path that reaches the service for template
/// content, and only on an explicit Duplicate click, so a service outage fails
/// this action honestly rather than being papered over with a cache.
async fn duplicate_builtin(state: &AppState, src: &str) -> Result<TemplateContent> {
    let builtin = builtin_by_id(src)
        .ok_or_else(|| AppError::Validation(format!("'{src}' is not a built-in template")))?;
    let specs = crate::ml::builtin_research_templates(&state.http, &state.boot.ml.base_url).await?;
    let spec = specs
        .into_iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(src))
        .ok_or_else(|| {
            AppError::Other(anyhow::anyhow!(
                "the research service did not return the '{src}' template"
            ))
        })?;
    let skeleton = spec
        .get("skeleton")
        .map(parse_skeleton)
        .unwrap_or_default();
    Ok(TemplateContent {
        label: format!("{} (copy)", builtin.label),
        // The service holds no description; use the picker copy for the built-in.
        description: builtin.description.to_string(),
        skeleton,
        writing_instructions: spec
            .get("writing_instructions")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        outline_mode: spec
            .get("outline_mode")
            .and_then(|v| v.as_str())
            .unwrap_or("constrained")
            .to_string(),
    })
}

/// Fork an existing user-defined template into a new editable copy. Its body is
/// already in the store, so no research service call is needed. The caller must be
/// able to SEE the source (own personal / any global / admin); a foreign personal
/// template answers "not found" so the id is not an existence oracle.
async fn duplicate_custom(state: &AppState, ctx: &AuthContext, id: Uuid) -> Result<TemplateContent> {
    let row = sqlx::query!(
        r#"SELECT label, description, skeleton, writing_instructions, outline_mode, scope, created_by
           FROM research_templates WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::NotFound("template not found".into()))?;
    if !template_visible(&row.scope, row.created_by, ctx) {
        return Err(AppError::NotFound("template not found".into()));
    }
    Ok(TemplateContent {
        label: format!("{} (copy)", row.label),
        description: row.description,
        skeleton: parse_skeleton(&row.skeleton),
        writing_instructions: row.writing_instructions,
        outline_mode: row.outline_mode,
    })
}

// ---- Update -----------------------------------------------------------------

#[derive(Deserialize)]
pub struct UpdateTemplate {
    #[serde(flatten)]
    pub content: TemplateContent,
    /// Optionally move the template between `personal` and `global`. Moving to
    /// global requires the manage permission.
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn update_template(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateTemplate>,
) -> Result<Json<serde_json::Value>> {
    let row = require_manage(&state, &ctx, id).await?;
    let mut content = body.content;

    // Determine the target scope and gate a move to global.
    let target_scope = body.scope.unwrap_or(row.scope.clone());
    if target_scope != "personal" && target_scope != "global" {
        return Err(AppError::Validation(format!("unknown scope '{target_scope}'")));
    }
    if target_scope == "global" && row.scope != "global" {
        state
            .rbac
            .require_permission(&state.pg, &ctx, RESEARCH_TEMPLATES_MANAGE)
            .await?;
    }
    normalise_and_validate(&mut content)?;

    let skeleton = serde_json::to_value(&content.skeleton)
        .map_err(|e| AppError::Other(anyhow::anyhow!("encode skeleton: {e}")))?;
    sqlx::query!(
        r#"UPDATE research_templates
           SET label = $2, description = $3, skeleton = $4, writing_instructions = $5,
               outline_mode = $6, scope = $7, updated_at = now()
           WHERE id = $1"#,
        id,
        content.label.trim(),
        content.description,
        skeleton,
        content.writing_instructions,
        content.outline_mode,
        target_scope,
    )
    .execute(&state.pg)
    .await?;

    audit_template(&state, &ctx, "research.template.updated", id).await;
    Ok(Json(json!({ "ok": true })))
}

// ---- Archive (soft delete) --------------------------------------------------

pub async fn archive_template(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_manage(&state, &ctx, id).await?;
    // Soft delete: an existing research chat may still point at this template and
    // its Refine must keep working, so the row is kept and merely archived.
    sqlx::query!(
        "UPDATE research_templates SET archived_at = now(), updated_at = now() \
         WHERE id = $1 AND archived_at IS NULL",
        id
    )
    .execute(&state.pg)
    .await?;
    audit_template(&state, &ctx, "research.template.archived", id).await;
    Ok(Json(json!({ "ok": true })))
}

/// The row a manage action operates on.
struct ManageRow {
    scope: String,
}

/// Owner-or-admin for a personal template; the manage permission for a global one
/// (whichever it currently is). Returns the row's manage-relevant fields. A
/// foreign personal template answers "not found" (no existence oracle).
async fn require_manage(state: &AppState, ctx: &AuthContext, id: Uuid) -> Result<ManageRow> {
    let row = sqlx::query!(
        "SELECT scope, created_by FROM research_templates WHERE id = $1",
        id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::NotFound("template not found".into()))?;

    if row.scope == "global" {
        state
            .rbac
            .require_permission(&state.pg, ctx, RESEARCH_TEMPLATES_MANAGE)
            .await?;
    } else {
        // Personal: only the owner or an admin, and a non-owner must not learn it
        // exists.
        if !template_visible(&row.scope, row.created_by, ctx) {
            return Err(AppError::NotFound("template not found".into()));
        }
        let owner = ctx.is_admin() || (row.created_by.is_some() && row.created_by == ctx.user_id);
        if !owner {
            return Err(AppError::Forbidden(
                "only the template's owner or an admin may manage it".into(),
            ));
        }
    }
    Ok(ManageRow { scope: row.scope })
}

async fn audit_template(state: &AppState, ctx: &AuthContext, action: &str, id: Uuid) {
    let mut event = AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("research_template".into());
    event.resource_id = Some(id);
    let _ = audit::append(&state.pg, &event).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(mode: &str, sections: &[(&str, bool, bool)]) -> TemplateContent {
        TemplateContent {
            label: "My template".into(),
            description: String::new(),
            skeleton: sections
                .iter()
                .map(|(h, expandable, exec_summary)| SectionInput {
                    heading: (*h).into(),
                    brief: String::new(),
                    expandable: *expandable,
                    exec_summary: *exec_summary,
                })
                .collect(),
            writing_instructions: String::new(),
            outline_mode: mode.into(),
        }
    }

    #[test]
    fn duplicate_only_body_deserialises() {
        // Regression: the real frontend Duplicate body carries only `duplicate_of`.
        // `#[serde(flatten)]` over `Option<TemplateContent>` never yields None
        // (serde#1626), so this used to fail at the extractor with `missing field
        // 'label'`. With defaulted content fields it must deserialise cleanly.
        let body: CreateTemplate =
            serde_json::from_str(r#"{"duplicate_of":"formal"}"#).expect("duplicate-only body");
        assert_eq!(body.duplicate_of.as_deref(), Some("formal"));
        assert_eq!(body.scope, "personal", "scope defaults");
        assert!(body.content.label.is_empty(), "content is absent, not required");

        // A UUID source (custom fork) deserialises the same way.
        let custom: CreateTemplate =
            serde_json::from_str(r#"{"duplicate_of":"1b9d6bcd-bbfd-4b2d-9b5d-ab8dfbbd4bed"}"#)
                .expect("uuid duplicate body");
        assert!(custom.duplicate_of.is_some());
    }

    #[test]
    fn from_scratch_body_deserialises_flat() {
        // A full create-from-scratch body: content fields flattened at top level.
        let body: CreateTemplate = serde_json::from_str(
            r#"{"label":"Ours","description":"d","skeleton":[{"heading":"H","brief":"b","expandable":true,"exec_summary":false}],"writing_instructions":"w","outline_mode":"constrained","scope":"global"}"#,
        )
        .expect("flat body");
        assert!(body.duplicate_of.is_none());
        assert_eq!(body.content.label, "Ours");
        assert_eq!(body.content.skeleton.len(), 1);
        assert_eq!(body.scope, "global");
    }

    #[test]
    fn headings_are_trimmed_on_write() {
        let mut c = content("constrained", &[("  Padded  ", false, false)]);
        normalise_and_validate(&mut c).expect("valid");
        assert_eq!(c.skeleton[0].heading, "Padded", "stored heading is trimmed");
    }

    #[test]
    fn constrained_needs_a_section() {
        assert!(normalise_and_validate(&mut content("constrained", &[])).is_err());
        assert!(normalise_and_validate(&mut content("constrained", &[("A", false, false)])).is_ok());
    }

    #[test]
    fn free_allows_empty_and_clears_flags() {
        assert!(normalise_and_validate(&mut content("free", &[])).is_ok());
        let mut c = content("free", &[("A", true, true)]);
        normalise_and_validate(&mut c).expect("free is valid");
        assert!(!c.skeleton[0].expandable, "free clears expandable");
        assert!(!c.skeleton[0].exec_summary, "free clears exec_summary");
    }

    #[test]
    fn section_ceiling_is_twelve() {
        let many: Vec<(&str, bool, bool)> = (0..13)
            .map(|i| (SECTION_NAMES[i], false, false))
            .collect();
        assert!(normalise_and_validate(&mut content("constrained", &many)).is_err());
        assert!(normalise_and_validate(&mut content("constrained", &many[..12])).is_ok());
    }
    const SECTION_NAMES: [&str; 13] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m",
    ];

    #[test]
    fn duplicate_headings_rejected_case_insensitively() {
        assert!(normalise_and_validate(&mut content("constrained", &[("Intro", false, false), (" intro ", false, false)])).is_err());
    }

    #[test]
    fn at_most_one_executive_summary() {
        assert!(normalise_and_validate(&mut content("constrained", &[("A", false, true), ("B", false, true)])).is_err());
        assert!(normalise_and_validate(&mut content("constrained", &[("A", false, true), ("B", false, false)])).is_ok());
    }

    #[test]
    fn exec_summary_cannot_be_the_reserved_analysis_heading() {
        assert!(normalise_and_validate(&mut content(
            "constrained",
            &[("Consensus, contradictions and gaps", false, true)]
        ))
        .is_err());
        // Fine without the exec-summary flag (the heading itself is allowed).
        assert!(normalise_and_validate(&mut content(
            "constrained",
            &[("Consensus, contradictions and gaps", false, false)]
        ))
        .is_ok());
    }

    #[test]
    fn length_limits_enforced() {
        let mut long_label = content("constrained", &[("A", false, false)]);
        long_label.label = "x".repeat(MAX_LABEL + 1);
        assert!(normalise_and_validate(&mut long_label).is_err());
    }

    #[test]
    fn builtins_mirror_the_research_service() {
        // These four are mirrored for the picker from the research service's own
        // templates (ml/app/research/templates.py). If a built-in's id, label,
        // outline mode or section headings change there, update this constant too
        // (a matching pin test on the service side guards the other direction).
        assert_eq!(BUILTIN_TEMPLATES.len(), 4);
        let ids: Vec<&str> = BUILTIN_TEMPLATES.iter().map(|t| t.id).collect();
        assert_eq!(ids, ["exploration", "formal", "freeform", "literature"]);
        let lit = builtin_by_id("literature").unwrap();
        assert_eq!(lit.label, "Literature review");
        assert_eq!(lit.outline_mode, "constrained");
        assert_eq!(lit.structure.len(), 6);
        assert_eq!(lit.structure[4], "Consensus, contradictions and gaps");
        assert_eq!(builtin_by_id("freeform").unwrap().outline_mode, "free");
        assert!(builtin_by_id("nope").is_none());
    }
}
