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

//! Deep Research REST entry-points (the
//! lightweight plan gate): `prepare` is side-effect-free (egress gate for
//! web/hybrid + validation + scope resolution + ambiguity triage → scope
//! summary, estimate and any clarifying chips), `start` creates the
//! `mode='research'` chat, mints the durable killable agent-run and enqueues
//! the `deep_research` task. The run itself is `research::run_research`.
//!
//! Corpus modes (`files`/`hybrid`, Phase 2) resolve the readable library scope
//! here so the home screen can show document counts and narrow it; the run
//! re-resolves the inventory at execution time (fail-closed). A `files`-only
//! run performs ZERO egress and is NOT gated on the web-search connector.

use std::time::Duration;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::error::{AppError, Result};
use crate::integrations::{self, ConnectorKind};
use crate::research::RESEARCH_WALL_CLOCK_SECS;
use crate::state::AppState;

/// Short timeout for the interactive triage call — it must never make the user
/// wait on the plan gate; on timeout we simply show no chips.
const TRIAGE_TIMEOUT_SECS: u64 = 10;

/// Display-only census/sampling threshold for the scope line (the authoritative
/// cap lives in ML config; this just labels the estimate).
const DISPLAY_CENSUS_CAP: i64 = 500;

#[derive(Deserialize)]
pub struct ResearchRequest {
    pub question: String,
    /// "web" | "files" | "hybrid".
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_template")]
    pub template: String,
    /// Narrowed corpus scope (a subset of the user's readable libraries). Empty
    /// ⇒ the whole readable scope. Intersected server-side, fail-closed.
    #[serde(default)]
    pub kb_ids: Vec<Uuid>,
    /// Triage-chip answers that steer scope voice (non-scope clarifications).
    #[serde(default)]
    pub refinements: Vec<String>,
    /// Set by the re-prepare after the user answers/skips the chips.
    #[serde(default)]
    pub skip_triage: bool,
}

fn default_source() -> String {
    "web".into()
}
fn default_template() -> String {
    "exploration".into()
}

fn validate(req: &ResearchRequest) -> Result<()> {
    if req.question.trim().is_empty() {
        return Err(AppError::Validation("a research question is required".into()));
    }
    if !matches!(req.source.as_str(), "web" | "files" | "hybrid") {
        return Err(AppError::Validation(format!("unknown research source '{}'", req.source)));
    }
    if !matches!(req.template.as_str(), "exploration" | "formal" | "freeform" | "literature") {
        return Err(AppError::Validation(format!("unknown template '{}'", req.template)));
    }
    Ok(())
}

fn needs_egress(source: &str) -> bool {
    matches!(source, "web" | "hybrid")
}

fn clamp(v: i64, lo: i64, hi: i64) -> i64 {
    v.max(lo).min(hi)
}

/// Coarse minute estimate (no ML call). Files scale with document count; hybrid
/// adds the web budget on top; both capped at 30. Deep Research has one (deep)
/// mode, so the web band is the deep band.
fn estimate(source: &str, doc_count: i64) -> (u32, u32) {
    let web = (15i64, 25i64);
    let f_lo = clamp(3 + doc_count / 25, 5, 20);
    let files = (f_lo, (f_lo + 10).min(30));
    let (lo, hi) = match source {
        "web" => web,
        "files" => files,
        _ => ((web.0 + files.0).min(30), (web.1 + files.1).min(30)),
    };
    (lo as u32, hi as u32)
}

fn ellipsis(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
    format!("{}…", &s[..cut])
}

#[derive(Serialize)]
pub struct ScopeEntryOut {
    pub kb_id: Uuid,
    pub name: String,
    pub kind: String,
    pub doc_count: i64,
}

#[derive(Serialize)]
pub struct TriageOptionOut {
    pub label: String,
    /// Libraries this option narrows the scope to (empty ⇒ a non-scope answer).
    pub kb_ids: Vec<Uuid>,
    /// A non-scope clarification (e.g. a timeframe) to append to `refinements`.
    pub refinement: Option<String>,
}

#[derive(Serialize)]
pub struct TriageQuestionOut {
    pub id: String,
    pub prompt: String,
    pub options: Vec<TriageOptionOut>,
}

#[derive(Serialize)]
pub struct PrepareOut {
    pub scope_summary: String,
    pub estimate_minutes_lo: u32,
    pub estimate_minutes_hi: u32,
    /// The readable libraries (corpus modes) — for the scope picker.
    pub scope: Vec<ScopeEntryOut>,
    /// Total documents across the effective scope.
    pub doc_count: i64,
    /// Clarifying chips when the question is ambiguous (corpus modes only).
    pub questions: Vec<TriageQuestionOut>,
}

/// Side-effect-free plan gate: validate → (web/hybrid) egress gate → resolve
/// scope + estimate → ambiguity triage. Returns the scope summary the user
/// confirms with Start, plus any clarifying chips.
pub async fn prepare(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(req): Json<ResearchRequest>,
) -> Result<Json<PrepareOut>> {
    validate(&req)?;
    if needs_egress(&req.source) {
        integrations::guard_egress(&state, &ctx, ConnectorKind::WebSearch).await?;
    }
    let q = req.question.trim();

    // Web-only: no corpus to resolve.
    if req.source == "web" {
        let (lo, hi) = estimate("web", 0);
        return Ok(Json(PrepareOut {
            scope_summary: format!(
                "Web research · \u{201c}{}\u{201d} · ~{lo}–{hi} min",
                ellipsis(q, 70),
            ),
            estimate_minutes_lo: lo,
            estimate_minutes_hi: hi,
            scope: Vec::new(),
            doc_count: 0,
            questions: Vec::new(),
        }));
    }

    // Corpus modes: resolve the readable scope, intersect the request's kb_ids.
    let full = crate::kb::dr_scope(&state.pg, &ctx).await?;
    let effective = crate::kb::intersect_scope(&full, &req.kb_ids);
    if effective.is_empty() {
        return Err(AppError::Validation(
            "no readable libraries are in scope for this research".into(),
        ));
    }
    let doc_count: i64 = effective.iter().map(|k| k.doc_count).sum();
    let lib_count = effective.len();
    let scope: Vec<ScopeEntryOut> = effective
        .iter()
        .map(|k| ScopeEntryOut {
            kb_id: k.id,
            name: k.name.clone(),
            kind: k.kind.clone(),
            doc_count: k.doc_count,
        })
        .collect();
    let (lo, hi) = estimate(&req.source, doc_count);
    let mode_word = if doc_count > DISPLAY_CENSUS_CAP { "sampling" } else { "census" };
    let libs = if lib_count == 1 { "library" } else { "libraries" };
    let scope_summary = if req.source == "files" {
        format!(
            "File research · {lib_count} {libs} ({doc_count} docs) · {mode_word} · ~{lo}–{hi} min",
        )
    } else {
        format!(
            "Files + web · {lib_count} {libs} ({doc_count} docs) · ~{lo}–{hi} min",
        )
    };

    // Ambiguity triage — one cheap LLM call, never blocks (short timeout, degrade
    // to no chips). The frontend re-prepares with skip_triage after answering.
    let questions = if req.skip_triage {
        Vec::new()
    } else {
        let scope_entries: Vec<crate::ml::TriageScopeEntry> = effective
            .iter()
            .enumerate()
            .map(|(i, k)| crate::ml::TriageScopeEntry {
                index: i,
                name: k.name.clone(),
                kind: k.kind.clone(),
                doc_count: k.doc_count,
            })
            .collect();
        let out = crate::ml::research_triage(
            &state.http,
            &state.boot.ml.base_url,
            q,
            &req.source,
            &scope_entries,
            Duration::from_secs(TRIAGE_TIMEOUT_SECS),
        )
        .await;
        // Map scope indices → kb_ids server-side (never trust LLM-emitted ids).
        out.questions
            .into_iter()
            .map(|qn| TriageQuestionOut {
                id: qn.id,
                prompt: qn.prompt,
                options: qn
                    .options
                    .into_iter()
                    .map(|o| {
                        let kb_ids: Vec<Uuid> =
                            o.scope_indices.iter().filter_map(|&i| effective.get(i).map(|k| k.id)).collect();
                        let refinement = if kb_ids.is_empty() { Some(o.label.clone()) } else { None };
                        TriageOptionOut { label: o.label, kb_ids, refinement }
                    })
                    .collect(),
            })
            .collect()
    };

    Ok(Json(PrepareOut {
        scope_summary,
        estimate_minutes_lo: lo,
        estimate_minutes_hi: hi,
        scope,
        doc_count,
        questions,
    }))
}

#[derive(Serialize)]
pub struct StartOut {
    pub chat_id: Uuid,
    pub run_id: Option<Uuid>,
}

/// Start a research run: research chat + durable killable agent-run + the
/// `deep_research` task. Returns the chat to navigate to.
pub async fn start(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(req): Json<ResearchRequest>,
) -> Result<Json<StartOut>> {
    validate(&req)?;
    if needs_egress(&req.source) {
        integrations::guard_egress(&state, &ctx, ConnectorKind::WebSearch).await?;
    }
    let owner = ctx
        .user_id
        .ok_or_else(|| AppError::Forbidden("a research run needs a user owner".into()))?;

    // Corpus modes: require an EXPLICIT library choice (no silent "whole corpus"),
    // then re-resolve + intersect now so a mis-scoped run can't start.
    let (kb_ids, kb_names): (Vec<Uuid>, Vec<String>) = if matches!(req.source.as_str(), "files" | "hybrid") {
        if req.kb_ids.is_empty() {
            return Err(AppError::Validation(
                "choose at least one library to research (or select all explicitly)".into(),
            ));
        }
        let full = crate::kb::dr_scope(&state.pg, &ctx).await?;
        let effective = crate::kb::intersect_scope(&full, &req.kb_ids);
        if effective.is_empty() {
            return Err(AppError::Validation(
                "no readable libraries are in scope for this research".into(),
            ));
        }
        (
            effective.iter().map(|k| k.id).collect(),
            effective.iter().map(|k| k.name.clone()).collect(),
        )
    } else {
        (Vec::new(), Vec::new())
    };

    let chat_id = Uuid::now_v7();
    let turn_id = Uuid::now_v7();
    let q = req.question.trim();
    let title = ellipsis(q, 80);
    // Stash the request params so a finished run can be re-opened prefilled
    // ('Refine' = a fresh run with the same scope; cancel-and-refine).
    let research_params = json!({
        "question": q,
        "source": req.source,
        "template": req.template,
        "kb_ids": kb_ids,
        "kb_names": kb_names,
        "refinements": req.refinements,
    });
    sqlx::query!(
        "INSERT INTO chats (id, owner_user_id, title, mode, research_params) \
         VALUES ($1, $2, $3, 'research', $4)",
        chat_id, owner, title, research_params
    )
    .execute(&state.pg)
    .await?;

    // A real agent-run: durable, auditable, killable (Redis token TTL = the
    // run's wall-clock budget).
    let run_id = if state.boot.features.agents_enabled {
        Some(
            crate::agent::start_run(
                &state, None, ctx.user_id, ctx.role.as_str(),
                Some(chat_id), turn_id, None, None, RESEARCH_WALL_CLOCK_SECS,
            )
            .await?,
        )
    } else {
        None
    };

    crate::scheduler::enqueue(
        &state.pg,
        crate::scheduler::TaskType::DeepResearch,
        json!({
            "run_id": run_id,
            "chat_id": chat_id,
            "turn_id": turn_id,
            "user_id": owner,
            "role": ctx.role.as_str(),
            "question": q,
            "template": req.template,
            "source": req.source,
            "kb_ids": kb_ids,
            "refinements": req.refinements,
        }),
    )
    .await
    .map_err(|e| AppError::Other(anyhow::anyhow!("enqueue deep research: {e}")))?;

    let mut ev = AuditEvent::action("research.started", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("chat".into());
    ev.resource_id = Some(chat_id);
    ev.payload = Some(json!({
        "run_id": run_id, "question": q, "template": req.template,
        "source": req.source, "kb_count": kb_ids.len(),
    }));
    let _ = audit::append(&state.pg, &ev).await;

    Ok(Json(StartOut { chat_id, run_id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(source: &str, template: &str) -> ResearchRequest {
        ResearchRequest {
            question: "what is x".into(),
            source: source.into(),
            template: template.into(),
            kb_ids: Vec::new(),
            refinements: Vec::new(),
            skip_triage: false,
        }
    }

    #[test]
    fn validate_accepts_phase2_sources_and_templates() {
        for s in ["web", "files", "hybrid"] {
            assert!(validate(&req(s, "exploration")).is_ok(), "source {s}");
        }
        for t in ["exploration", "formal", "freeform", "literature"] {
            assert!(validate(&req("files", t)).is_ok(), "template {t}");
        }
        assert!(validate(&req("ftp", "formal")).is_err());
        assert!(validate(&req("web", "memo")).is_err());
        let mut blank = req("web", "formal");
        blank.question = "   ".into();
        assert!(validate(&blank).is_err());
    }

    #[test]
    fn egress_only_for_web_and_hybrid() {
        assert!(needs_egress("web"));
        assert!(needs_egress("hybrid"));
        assert!(!needs_egress("files"), "files-only is air-gap-safe — no egress gate");
    }

    #[test]
    fn estimates_scale_and_cap() {
        // Files scale with document count; everything caps at 30 minutes.
        let (lo, hi) = estimate("files", 0);
        assert_eq!((lo, hi), (5, 15));
        let (lo_big, hi_big) = estimate("files", 10_000);
        assert_eq!((lo_big, hi_big), (20, 30));
        let (_, hh) = estimate("hybrid", 10_000);
        assert!(hh <= 30, "hybrid estimate capped");
    }
}
