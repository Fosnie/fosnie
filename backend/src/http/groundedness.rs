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

//! "Verify draft" (groundedness Mode B) REST. Start a durable verification of a
//! generated artefact (`draft`) or a workspace `document`; poll the run + its
//! per-claim verdicts. Evidence is resolved from the caller's readable KBs at
//! request time (intersection RBAC), so the background job never reads anything
//! the requester could not.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::rbac::{Permission, ResourceType};
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::scheduler::{self, TaskType};
use crate::state::AppState;

const FACTCG: &str = "FactCG-DeBERTa-v3-Large";

#[derive(Deserialize)]
pub struct StartBody {
    /// "draft" (generated artefact) | "document" (workspace document).
    pub target_type: String,
    pub target_id: Uuid,
}

/// Resolve a target's text path + mime + the caller's readable evidence KBs,
/// enforcing read access. Returns (disk_path, mime, kb_ids).
async fn resolve_target(
    state: &AppState,
    ctx: &AuthContext,
    target_type: &str,
    target_id: Uuid,
) -> Result<(String, Option<String>, Vec<Uuid>)> {
    match target_type {
        "draft" => {
            let a = sqlx::query!(
                "SELECT chat_id, disk_path, mime FROM generated_artefacts WHERE id = $1",
                target_id
            )
            .fetch_optional(&state.pg)
            .await?
            .ok_or_else(|| AppError::Validation("artefact not found".into()))?;
            crate::http::export::require_chat_read(state, ctx, a.chat_id).await?;
            let chat = sqlx::query!("SELECT project_id, agent_id FROM chats WHERE id = $1", a.chat_id)
                .fetch_one(&state.pg)
                .await?;
            let kb = crate::kb::retrieval_allowlist(&state.pg, ctx, a.chat_id, chat.project_id, chat.agent_id)
                .await?;
            let abs = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &a.disk_path)
                .to_string_lossy()
                .to_string();
            Ok((abs, Some(a.mime), kb))
        }
        "document" => {
            let pid = crate::documents::project_of(&state.pg, target_id).await?;
            state.rbac.require(&state.pg, ctx, ResourceType::Project, pid, Permission::Read).await?;
            let cur = crate::documents::current_version(&state.pg, &state.boot.storage.workspace_dir, target_id).await?;
            // Project's linked KBs ∩ caller-readable, in ONE query (the `can_read`
            // EXISTS shape from kb/mod.rs inlined) instead of a per-KB round-trip
            // loop (avoids the N+1). Admin reads all linked KBs.
            let uid = ctx.user_id;
            let kb: Vec<Uuid> = sqlx::query_scalar!(
                r#"SELECT l.kb_id AS "kb_id!"
                   FROM project_kb_links l
                   JOIN knowledge_bases kb ON kb.id = l.kb_id AND kb.archived_at IS NULL
                   WHERE l.project_id = $1 AND (
                       $2
                       OR kb.owner_id = $3
                       OR EXISTS (
                           SELECT 1 FROM kb_access_grants g WHERE g.kb_id = kb.id AND (
                               (g.principal_type = 'user'  AND g.principal_id = $3)
                            OR (g.principal_type = 'group' AND g.principal_id IN
                                  (SELECT group_id FROM group_members WHERE user_id = $3))
                           )
                       )
                   )"#,
                pid,
                ctx.is_admin(),
                uid,
            )
            .fetch_all(&state.pg)
            .await?;
            Ok((cur.bytes_path, cur.mime, kb))
        }
        _ => Err(AppError::Validation("target_type must be 'draft' or 'document'".into())),
    }
}

/// POST /api/verify-draft — enqueue a draft verification, return its run id.
pub async fn start(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<StartBody>,
) -> Result<Json<serde_json::Value>> {
    if !state.features.enabled_for(&state, &ctx, "groundedness").await {
        return Err(AppError::Forbidden("groundedness verification is disabled".into()));
    }
    let (path, mime, kb_ids) = resolve_target(&state, &ctx, &body.target_type, body.target_id).await?;

    let run_id = Uuid::now_v7();
    sqlx::query!(
        r#"INSERT INTO verification_runs
             (id, target_type, target_id, mode, verifier_model, status, created_by)
           VALUES ($1, ($2::text)::verification_target, $3, ($4::text)::verification_mode, $5, 'queued', $6)"#,
        run_id,
        body.target_type,
        body.target_id,
        "verify_draft",
        FACTCG,
        ctx.user_id,
    )
    .execute(&state.pg)
    .await?;

    let kb_strs: Vec<String> = kb_ids.iter().map(|u| u.to_string()).collect();
    let payload = serde_json::json!({
        "run_id": run_id, "path": path, "mime": mime, "kb_ids": kb_strs,
    });
    scheduler::enqueue(&state.pg, TaskType::VerifyDraft, payload).await?;

    Ok(Json(serde_json::json!({ "run_id": run_id, "status": "queued" })))
}

#[derive(Serialize)]
pub struct ClaimOut {
    pub claim_text: String,
    pub verdict: String,
    pub score: Option<f64>,
    pub evidence: String,
    pub section: String,
    pub had_citation: bool,
    /// The claim's verbatim span in the document, or null if unlocatable.
    /// Drives the inline highlight + is the `find` text for ground-or-cut repair.
    pub source_text: Option<String>,
    /// Set once repaired: 'regenerated' | 'cut' | 'kept'.
    pub repair_action: Option<String>,
}

#[derive(Serialize)]
pub struct RunDetail {
    pub id: Uuid,
    pub target_type: String,
    pub target_id: Uuid,
    pub mode: String,
    pub status: String,
    pub verifier_model: String,
    pub strictness: String,
    pub faithfulness_score: Option<f64>,
    pub total_claims: i32,
    pub supported: i32,
    pub contradicted: i32,
    pub not_mentioned: i32,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub claims: Vec<ClaimOut>,
}

/// Read a run + its verdicts, after re-checking read access to its target.
async fn run_detail(state: &AppState, ctx: &AuthContext, run_id: Uuid) -> Result<RunDetail> {
    let r = sqlx::query!(
        r#"SELECT target_type::text AS "target_type!", target_id, mode::text AS "mode!",
                  status, verifier_model, strictness, faithfulness_score, total_claims, supported,
                  contradicted, not_mentioned, created_by, created_at, finished_at
           FROM verification_runs WHERE id = $1"#,
        run_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("verification run not found".into()))?;

    // RBAC: the creator + admins, else read access to the run's target.
    let is_creator = ctx.user_id.is_some() && ctx.user_id == r.created_by;
    if !is_creator && !ctx.is_admin() {
        resolve_target(state, ctx, &r.target_type, r.target_id).await?;
    }

    let rows = sqlx::query!(
        r#"SELECT claim_text, verdict::text AS "verdict!", verifier_score,
                  bound_evidence_ref, had_citation, source_span, repair_action
           FROM claim_verdicts WHERE run_id = $1 ORDER BY id"#,
        run_id
    )
    .fetch_all(&state.pg)
    .await?;
    let claims = rows
        .into_iter()
        .map(|c| {
            let b = c.bound_evidence_ref.unwrap_or_default();
            let source_text = c
                .source_span
                .as_ref()
                .and_then(|s| s.get("text"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            ClaimOut {
                claim_text: c.claim_text,
                verdict: c.verdict,
                score: c.verifier_score,
                evidence: b.get("evidence").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                section: b.get("section").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                had_citation: c.had_citation,
                source_text,
                repair_action: c.repair_action,
            }
        })
        .collect();

    Ok(RunDetail {
        id: run_id,
        target_type: r.target_type,
        target_id: r.target_id,
        mode: r.mode,
        status: r.status,
        verifier_model: r.verifier_model,
        strictness: r.strictness,
        faithfulness_score: r.faithfulness_score,
        total_claims: r.total_claims,
        supported: r.supported,
        contradicted: r.contradicted,
        not_mentioned: r.not_mentioned,
        created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
        finished_at: r.finished_at.and_then(|t| t.format(&Rfc3339).ok()),
        claims,
    })
}

/// GET /api/verification-runs/{id}
pub async fn get_run(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(run_id): Path<Uuid>,
) -> Result<Json<RunDetail>> {
    Ok(Json(run_detail(&state, &ctx, run_id).await?))
}

#[derive(Deserialize)]
pub struct LatestQuery {
    pub target_type: String,
    pub target_id: Uuid,
}

/// GET /api/verification-runs?target_type=&target_id= — the most recent run for a
/// target (so the UI can show a prior result), or null.
pub async fn latest_for_target(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<LatestQuery>,
) -> Result<Json<Option<RunDetail>>> {
    let run_id = sqlx::query_scalar!(
        r#"SELECT id FROM verification_runs
           WHERE target_type = ($1::text)::verification_target AND target_id = $2
           ORDER BY created_at DESC LIMIT 1"#,
        q.target_type,
        q.target_id
    )
    .fetch_optional(&state.pg)
    .await?;
    match run_id {
        Some(id) => Ok(Json(Some(run_detail(&state, &ctx, id).await?))),
        None => Ok(Json(None)),
    }
}

/// POST /api/verification-runs/{id}/repair — enqueue ground-or-cut repair
/// of a finished verify-draft run on a document. Gated by `features.groundedness`
/// + the `groundedness.repair` knob; document targets only.
pub async fn repair(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(run_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    if !state.features.enabled_for(&state, &ctx, "groundedness").await {
        return Err(AppError::Forbidden("groundedness verification is disabled".into()));
    }
    if !crate::ml::groundedness_repair_enabled(&state.pg).await {
        return Err(AppError::Forbidden("ground-or-cut repair is disabled".into()));
    }
    // RBAC + existence + a succeeded document run, reusing run_detail's access check.
    let r = run_detail(&state, &ctx, run_id).await?;
    if r.target_type != "document" {
        return Err(AppError::Validation("repair applies to workspace documents only".into()));
    }
    if r.status != "succeeded" {
        return Err(AppError::Validation("run is not finished".into()));
    }
    // Tracked changes are rewritten in DOCX XML — repair needs a DOCX document.
    let cur = crate::documents::current_version(&state.pg, &state.boot.storage.workspace_dir, r.target_id).await?;
    let filename: String =
        sqlx::query_scalar!("SELECT original_filename FROM documents WHERE id = $1", r.target_id)
            .fetch_one(&state.pg)
            .await?;
    let is_docx = cur.mime.as_deref().map(|m| m.contains("wordprocessingml")).unwrap_or(false)
        || filename.to_lowercase().ends_with(".docx");
    if !is_docx {
        return Err(AppError::Validation(
            "Repair proposes tracked changes, which require a DOCX document — this document can't be repaired.".into(),
        ));
    }
    scheduler::enqueue(&state.pg, TaskType::RepairRun, json!({ "run_id": run_id })).await?;
    Ok(Json(json!({ "status": "queued" })))
}

/// Render a run as a Markdown report (score, counts, flagged claims + evidence).
fn render_report_md(r: &RunDetail) -> String {
    let pct = r
        .faithfulness_score
        .map(|s| format!("{:.0}% grounded", s * 100.0))
        .unwrap_or_else(|| "not scored".into());
    let mut md = String::from("# Groundedness verification report\n\n");
    md.push_str(&format!("- **Run**: `{}`\n", r.id));
    md.push_str(&format!("- **Target**: {} `{}`\n", r.target_type, r.target_id));
    md.push_str(&format!("- **Mode**: {}\n", r.mode));
    md.push_str(&format!("- **Verifier**: {}\n", r.verifier_model));
    md.push_str(&format!("- **Strictness**: {}\n", r.strictness));
    md.push_str(&format!("- **Created**: {}\n\n", r.created_at));
    md.push_str(&format!("## {pct}\n\n"));
    md.push_str(&format!(
        "{} supported · {} contradicted · {} not mentioned (of {} claims)\n\n",
        r.supported, r.contradicted, r.not_mentioned, r.total_claims
    ));
    let flagged: Vec<&ClaimOut> = r.claims.iter().filter(|c| c.verdict != "supported").collect();
    if flagged.is_empty() {
        md.push_str("Every claim is supported by the provided sources.\n");
    } else {
        md.push_str("## Flagged claims\n\n");
        for c in flagged {
            md.push_str(&format!("### [{}] {}\n\n", c.verdict.replace('_', " "), c.claim_text));
            if !c.section.is_empty() {
                md.push_str(&format!("*{}*\n\n", c.section));
            }
            if !c.evidence.is_empty() {
                md.push_str(&format!("> {}\n\n", c.evidence.replace('\n', " ")));
            }
        }
    }
    md.push_str("\n---\n*Groundedness, not truth — every claim checked against the provided sources; \
                 a qualified human stays in the loop. Keep this report with the matter file.*\n");
    md
}

#[derive(Deserialize)]
pub struct ReportQuery {
    #[serde(default = "default_format")]
    pub format: String,
}

fn default_format() -> String {
    "md".into()
}

/// GET /api/verification-runs/{id}/report?format=md|pdf|docx — a downloadable report.
pub async fn export_report(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(run_id): Path<Uuid>,
    Query(q): Query<ReportQuery>,
) -> Result<Response> {
    let r = run_detail(&state, &ctx, run_id).await?;
    let md = render_report_md(&r);
    let stem = format!("groundedness-{}", &run_id.to_string()[..8]);
    let fmt = q.format.as_str();

    let mut ev = AuditEvent::action("groundedness.report.exported", ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("verification_run".into());
    ev.resource_id = Some(run_id);
    ev.payload = Some(json!({ "format": fmt }));
    let _ = audit::append(&state.pg, &ev).await;

    let dispose = |ext: &str| format!("attachment; filename=\"{stem}.{ext}\"");
    match fmt {
        "md" => Ok((
            [
                (header::CONTENT_TYPE, "text/markdown".to_string()),
                (header::CONTENT_DISPOSITION, dispose("md")),
            ],
            md,
        )
            .into_response()),
        "pdf" | "docx" => {
            // Render via the artefact engine (LibreOffice for PDF), then stream it.
            let root = crate::storage::resolve_dir(&state.boot.storage.exports_dir);
            let out_path = root.join(format!("{run_id}.{fmt}")).to_string_lossy().to_string();
            let (path, mime) = crate::ml::generate_artefact(
                &state.http,
                &state.boot.ml.base_url,
                fmt,
                "Groundedness verification report",
                &md,
                &out_path,
            )
            .await?;
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| AppError::Other(anyhow::anyhow!("read report: {e}")))?;
            Ok((
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CONTENT_DISPOSITION, dispose(fmt)),
                ],
                Body::from(bytes),
            )
                .into_response())
        }
        _ => Err(AppError::Validation("format must be md, pdf or docx".into())),
    }
}
