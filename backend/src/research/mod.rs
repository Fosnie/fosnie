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

//! Deep Research background runs. The REST
//! entry-point (`http/research.rs`) creates a `mode='research'` chat, mints a
//! durable, killable agent-run and enqueues a `deep_research` task; this module
//! is the task handler — it streams the ML synthesis pipeline, broadcasts
//! `research.progress` frames, and on completion posts the full report as an
//! assistant message with its citations and an always-created MD artefact.
//! Modelled on `web_search::run_deep`.
//!
//! Clock ordering: ML `research_max_minutes` (20 min) < the per-request stream
//! timeout below (45 min) ≈ the agent-run kill-token TTL minted at start.

use std::time::Duration;

use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::error::{AppError, Result};
use crate::state::AppState;
use crate::web_search::persist_web_citations;
use crate::ws::protocol::{CitationOut, ServerFrame};

/// Generous per-request ceiling for the research ML call (the shared ML client
/// sets none). Matches the kill-token TTL minted by the start endpoint.
pub const RESEARCH_ML_TIMEOUT_SECS: u64 = 2_700;

/// Kill-token TTL / wall-clock budget for a research agent-run.
pub const RESEARCH_WALL_CLOCK_SECS: u64 = 2_700;

/// Upper bound on the census inventory sent to ML. Comfortably above the
/// default `research_census_cap` (500) so the full census case is never
/// truncated; above the cap ML switches to retrieval sampling and only needs
/// this list for document names. Keeps the request body bounded.
const RESEARCH_INVENTORY_CAP: i64 = 1_000;

struct ResearchPayload {
    run_id: Option<Uuid>,
    chat_id: Uuid,
    turn_id: Uuid,
    user_id: Option<Uuid>,
    role: String,
    question: String,
    template: Option<String>,
    /// A user-defined template snapshot, frozen when the run was enqueued (so a
    /// later edit or archive cannot rewrite a queued/running report). Absent for
    /// the built-ins, which the research service owns.
    template_spec: Option<serde_json::Value>,
    source: String,
    kb_ids: Vec<Uuid>,
    refinements: Vec<String>,
}

fn parse(payload: &serde_json::Value) -> Result<ResearchPayload> {
    let uuid = |k: &str| payload.get(k).and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
    let chat_id = uuid("chat_id").ok_or_else(|| AppError::Validation("deep research: missing chat_id".into()))?;
    let turn_id = uuid("turn_id").ok_or_else(|| AppError::Validation("deep research: missing turn_id".into()))?;
    let question = payload
        .get("question")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|q| !q.trim().is_empty())
        .ok_or_else(|| AppError::Validation("deep research: missing question".into()))?;
    let kb_ids = payload
        .get("kb_ids")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).filter_map(|s| Uuid::parse_str(s).ok()).collect())
        .unwrap_or_default();
    let refinements = payload
        .get("refinements")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default();
    Ok(ResearchPayload {
        run_id: uuid("run_id"),
        chat_id,
        turn_id,
        user_id: uuid("user_id"),
        role: payload.get("role").and_then(|v| v.as_str()).unwrap_or("user").to_string(),
        question,
        template: payload.get("template").and_then(|v| v.as_str()).map(str::to_string),
        template_spec: payload.get("template_spec").filter(|v| !v.is_null()).cloned(),
        source: payload.get("source").and_then(|v| v.as_str()).unwrap_or("web").to_string(),
        kb_ids,
        refinements,
    })
}

/// Resolve the run's document inventory at EXECUTION time: re-intersect the
/// requested `kb_ids` with the owner's current DR scope (fail-closed against a
/// grant revoked since `start`), then list ready documents. Returns the capped
/// inventory + the true total count (for honest coverage). Web-only runs skip
/// this entirely.
async fn resolve_inventory(
    state: &AppState,
    p: &ResearchPayload,
) -> (Vec<crate::ml::DocEntry>, Vec<String>, Option<i64>) {
    if !matches!(p.source.as_str(), "files" | "hybrid") {
        return (Vec::new(), Vec::new(), None);
    }
    let Some(uid) = p.user_id else { return (Vec::new(), Vec::new(), Some(0)) };
    let ctx = match crate::auth::load_context(&state.pg, uid).await {
        Ok(c) => c,
        Err(_) => return (Vec::new(), Vec::new(), Some(0)),
    };
    let scope = crate::kb::dr_scope(&state.pg, &ctx).await.unwrap_or_default();
    let effective = crate::kb::intersect_scope(&scope, &p.kb_ids);
    let kb_ids: Vec<Uuid> = effective.iter().map(|k| k.id).collect();
    let kb_id_strings: Vec<String> = kb_ids.iter().map(|i| i.to_string()).collect();
    if kb_ids.is_empty() {
        return (Vec::new(), kb_id_strings, Some(0));
    }
    let names: std::collections::HashMap<Uuid, String> =
        effective.iter().map(|k| (k.id, k.name.clone())).collect();

    let total: i64 = sqlx::query_scalar!(
        "SELECT count(*) AS \"c!\" FROM kb_documents WHERE kb_id = ANY($1) AND ingest_status = 'ready'",
        &kb_ids,
    )
    .fetch_one(&state.pg)
    .await
    .unwrap_or(0);

    let rows = sqlx::query!(
        "SELECT id, kb_id, original_filename, mime, bytes_path \
         FROM kb_documents WHERE kb_id = ANY($1) AND ingest_status = 'ready' \
         ORDER BY created_at ASC, id ASC LIMIT $2",
        &kb_ids,
        RESEARCH_INVENTORY_CAP,
    )
    .fetch_all(&state.pg)
    .await
    .unwrap_or_default();

    let docs = rows
        .into_iter()
        .map(|r| crate::ml::DocEntry {
            doc_id: r.id,
            kb_id: r.kb_id,
            kb_name: names.get(&r.kb_id).cloned().unwrap_or_default(),
            path: crate::storage::resolve_file(&state.boot.storage.documents_dir, &r.bytes_path)
                .to_string_lossy()
                .to_string(),
            mime: r.mime,
            filename: r.original_filename,
        })
        .collect();
    (docs, kb_id_strings, Some(total))
}

fn web_citations_out(citations: &[crate::ml::WebCitation]) -> Vec<CitationOut> {
    citations
        .iter()
        .map(|c| CitationOut {
            quote_text: c.quote_text.clone(),
            url: Some(c.url.clone()),
            title: c.title.clone(),
            domain: Some(c.domain.clone()),
            published_date: c.published_date.clone(),
            fetched_at: c.fetched_at.clone(),
            snippet_only: Some(c.snippet_only),
            ..Default::default()
        })
        .collect()
}

fn doc_citations_out(citations: &[crate::ml::Citation]) -> Vec<CitationOut> {
    citations
        .iter()
        .map(|c| CitationOut {
            doc_id: c.doc_id,
            quote_text: c.quote_text.clone(),
            page_number: c.page_number,
            clause_section_ref: c.clause_section_ref.clone(),
            ..Default::default()
        })
        .collect()
}

/// Create the report's MD artefact (always — resolved decision 6) and link it
/// to the posted message. Best-effort: a failed artefact never fails the run.
async fn create_md_artefact(
    state: &AppState,
    p: &ResearchPayload,
    message_id: Uuid,
    title: &str,
    report_md: &str,
) {
    let artefact_id = Uuid::now_v7();
    // Store the RELATIVE suffix under `artefacts_dir`; resolve for the ML call only.
    let rel = format!("{}/{artefact_id}.md", p.chat_id);
    let out_path = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &rel)
        .to_string_lossy()
        .to_string();

    match crate::ml::generate_artefact(
        &state.http, &state.boot.ml.base_url, "md", title, report_md, &out_path,
    )
    .await
    {
        Ok((_path, mime)) => {
            let _ = sqlx::query!(
                "INSERT INTO generated_artefacts (id, chat_id, turn_id, message_id, kind, title, disk_path, mime, created_by) \
                 VALUES ($1, $2, $3, $4, 'md', $5, $6, $7, $8)",
                artefact_id, p.chat_id, p.turn_id, message_id, title, rel, mime, p.user_id,
            )
            .execute(&state.pg)
            .await;
            let mut ev = AuditEvent::action("artefact.generated", &p.role);
            ev.actor_user_id = p.user_id;
            ev.resource_type = Some("artefact".into());
            ev.resource_id = Some(artefact_id);
            ev.payload = Some(json!({ "chat_id": p.chat_id, "kind": "md", "title": title, "research": true }));
            let _ = audit::append(&state.pg, &ev).await;
        }
        Err(e) => {
            tracing::warn!(error = %e, "research MD artefact generation failed (report still posted)");
        }
    }
}

/// Notify the user's sockets that the report message was posted.
fn notify(state: &AppState, p: &ResearchPayload, message_id: Uuid, citations: Vec<CitationOut>) {
    if let Some(uid) = p.user_id {
        state.hub.send_to_user(
            uid,
            ServerFrame::ChatMessagePosted { chat_id: p.chat_id, message_id, citations },
        );
    }
}

/// Persist the Phase-3 verification run for a delivered report message and light
/// the groundedness pill — mirrors `groundedness::verify_message`'s persistence
/// (reuses `verification_runs`/`claim_verdicts`/`messages.groundedness` and the
/// `ChatGroundedness` frame). `mode='verify_draft'` (the enum has no 'research'
/// value; the audit payload carries `"mode":"research"` to disambiguate).
/// Best-effort: every error is logged — the report is already posted. `None`
/// (an unverified run) is a no-op.
async fn persist_verification(
    state: &AppState,
    p: &ResearchPayload,
    message_id: Uuid,
    prefix_chars: i32,
    verification: Option<&crate::ml::ResearchVerification>,
) {
    let Some(v) = verification else { return };
    // Shift ML's report-relative span offsets to index the stored message
    // content ("# {title}\n\n{report_md}").
    let spans: Vec<crate::ws::protocol::GroundSpanOut> = v
        .spans
        .iter()
        .map(|s| crate::ws::protocol::GroundSpanOut {
            start: s.start + prefix_chars,
            end: s.end + prefix_chars,
            text: s.text.clone(),
            label: if s.label.is_empty() { "not_mentioned".into() } else { s.label.clone() },
        })
        .collect();
    let flagged = spans.len() as i32;

    let run_id = Uuid::now_v7();
    let insert_run = sqlx::query!(
        r#"INSERT INTO verification_runs
             (id, target_type, target_id, mode, verifier_model, strictness,
              faithfulness_score, total_claims, supported, contradicted, not_mentioned,
              status, created_by, finished_at)
           VALUES ($1, ($2::text)::verification_target, $3, ($4::text)::verification_mode,
                   $5, $6, $7, $8, $9, $10, $11, 'succeeded', $12, now())"#,
        run_id,
        "message",
        message_id,
        "verify_draft",
        v.model,
        "strict",
        v.score,
        v.total,
        v.supported,
        v.contradicted,
        v.not_mentioned,
        p.user_id,
    )
    .execute(&state.pg)
    .await;
    if let Err(e) = insert_run {
        tracing::warn!(error = %e, "research verification run insert failed");
        return;
    }

    for s in &spans {
        let span = json!({ "start": s.start, "end": s.end });
        let _ = sqlx::query!(
            r#"INSERT INTO claim_verdicts
                 (id, run_id, claim_text, source_span, had_citation, verdict, verifier_score)
               VALUES ($1, $2, $3, $4, true, ($5::text)::claim_verdict, $6)"#,
            Uuid::now_v7(),
            run_id,
            s.text,
            span,
            s.label,
            0.0_f64,
        )
        .execute(&state.pg)
        .await;
    }

    let spans_json: Vec<_> = spans
        .iter()
        .map(|s| json!({ "start": s.start, "end": s.end, "text": s.text, "label": s.label }))
        .collect();
    let summary = json!({
        "score": v.score,
        "total": v.total,
        "flagged": flagged,
        "contradicted": v.contradicted,
        "not_mentioned": v.not_mentioned,
        "model": v.model,
        "spans": spans_json,
    });
    let _ = sqlx::query!(
        "UPDATE messages SET groundedness = $1 WHERE id = $2",
        summary,
        message_id
    )
    .execute(&state.pg)
    .await;

    let mut ev = AuditEvent::action("groundedness.verified", &p.role);
    ev.actor_user_id = p.user_id;
    ev.resource_type = Some("message".into());
    ev.resource_id = Some(message_id);
    ev.payload = Some(json!({
        "chat_id": p.chat_id, "run_id": run_id, "mode": "research",
        "score": v.score, "total": v.total, "supported": v.supported,
        "contradicted": v.contradicted, "not_mentioned": v.not_mentioned, "model": v.model,
    }));
    let _ = audit::append(&state.pg, &ev).await;

    if let Some(uid) = p.user_id {
        state.hub.send_to_user(
            uid,
            ServerFrame::ChatGroundedness {
                turn_id: p.turn_id,
                message_id,
                score: Some(v.score),
                total: v.total,
                flagged,
                spans,
            },
        );
    }
}

/// Task handler. Terminal on every outcome: a failure posts an honest message
/// and returns `Ok` (a retry would double-post). All `Err` returns happen
/// BEFORE the first DB write.
pub async fn run_research(state: &AppState, payload: &serde_json::Value) -> Result<()> {
    let p = parse(payload)?;

    // Killed/expired before we started → nothing to do.
    if let Some(run_id) = p.run_id {
        if state.boot.features.agents_enabled && !crate::agent::alive(state, run_id).await {
            return Ok(());
        }
    }

    // Runtime web overrides resolve at execution time (latest admin config).
    let overrides = crate::ml::web_overrides(&state.pg).await;
    // Research budget knobs + the verify gate (features.groundedness AND the
    // research.verify runtime knob) — resolved fresh so admin changes take hold.
    let research = crate::ml::research_overrides(&state.pg).await;
    let verify = state.features.enabled_for_user(state, p.user_id, "groundedness").await
        && crate::ml::research_verify_enabled(&state.pg).await;
    // Corpus inventory + scope re-resolve at execution (fail-closed). Empty for
    // a web-only run — which therefore performs zero corpus work.
    let (docs, kb_ids, total_docs) = resolve_inventory(state, &p).await;

    // Create the streaming assistant message UP FRONT so the report types into the
    // chat as it is written and a reload mid-run resumes from the row. The streamed
    // section tokens are a live draft; the terminal `report_md` is authoritative and
    // replaces the content on finalise (the client refetches on `chat.message_posted`).
    let message_id = crate::chat::start_assistant_message(&state.pg, p.chat_id).await?;
    if let Some(uid) = p.user_id {
        state.hub.send_to_user(
            uid,
            ServerFrame::ChatMessageStarted { chat_id: p.chat_id, message_id, agent: None },
        );
    }
    let mut acc = String::new();
    let mut last_flush = std::time::Instant::now();
    // Roadmap history, persisted on the finished message so it survives a reload:
    // the section list (captured when the outline event arrives) + a phase
    // timeline (one entry per progress event).
    let run_started = std::time::Instant::now();
    let mut roadmap_sections: Option<Vec<String>> = None;
    let mut roadmap_phases: Vec<serde_json::Value> = Vec::new();

    type Outcome = (
        String,
        String,
        Vec<crate::ml::WebCitation>,
        Vec<crate::ml::Citation>,
        Option<crate::ml::ResearchVerification>,
    );
    let result: Result<Outcome> = async {
        let mut stream = crate::ml::research_stream(
            &state.http,
            &state.boot.ml.base_url,
            &p.question,
            p.template.as_deref(),
            p.template_spec.as_ref(),
            &p.source,
            &kb_ids,
            &docs,
            total_docs,
            &p.refinements,
            verify,
            &overrides,
            &research,
            crate::ml::provider_overrides(state, None).await,
            Some(Duration::from_secs(RESEARCH_ML_TIMEOUT_SECS)),
        )
        .await?;
        let mut ticks: u32 = 0;
        loop {
            match stream.recv().await {
                Some(crate::ml::ResearchEvent::Progress {
                    phase, detail, sources_read, sections_done, sections_total, sections,
                }) => {
                    // Mid-stream cancellation: poll the kill-token every few
                    // events so a Stop aborts promptly (dropping `stream` cancels
                    // the upstream body → ML task cancels). Cheap (~1 Redis hit/8).
                    ticks += 1;
                    if ticks % 8 == 0 {
                        if let Some(run_id) = p.run_id {
                            if state.boot.features.agents_enabled
                                && !crate::agent::alive(state, run_id).await
                            {
                                return Err(AppError::Other(anyhow::anyhow!("cancelled")));
                            }
                        }
                    }
                    // Record the roadmap: the full section list (once), and a phase
                    // timeline entry for every event (seconds since the run started).
                    if let Some(ref s) = sections {
                        roadmap_sections = Some(s.clone());
                    }
                    roadmap_phases.push(serde_json::json!({
                        "phase": phase,
                        "detail": detail,
                        "at": run_started.elapsed().as_secs(),
                    }));
                    if let (Some(uid), Some(run_id)) = (p.user_id, p.run_id) {
                        state.hub.send_to_user(
                            uid,
                            ServerFrame::ResearchProgress {
                                chat_id: p.chat_id,
                                run_id,
                                phase,
                                detail,
                                sources_read,
                                sections_done,
                                sections_total,
                                sections,
                            },
                        );
                    }
                }
                Some(crate::ml::ResearchEvent::Token { delta }) => {
                    acc.push_str(&delta);
                    if let Some(uid) = p.user_id {
                        state.hub.send_to_user(
                            uid,
                            ServerFrame::ChatMessageToken { chat_id: p.chat_id, message_id, delta },
                        );
                    }
                    // Throttled persist (mirrors the chat turn's ~750 ms flush) so a
                    // reload mid-write resumes the partial report from the row.
                    if last_flush.elapsed() >= Duration::from_millis(750) {
                        crate::chat::flush_assistant_message(&state.pg, message_id, &acc).await;
                        last_flush = std::time::Instant::now();
                    }
                }
                Some(crate::ml::ResearchEvent::Done { title, report_md, citations, doc_citations, verification }) => {
                    break Ok((title, report_md, citations, doc_citations, verification));
                }
                Some(crate::ml::ResearchEvent::Error { message }) => {
                    break Err(AppError::Other(anyhow::anyhow!("ml deep research: {message}")));
                }
                None => {
                    break Err(AppError::Other(anyhow::anyhow!(
                        "the research stream ended unexpectedly"
                    )));
                }
            }
        }
    }
    .await;

    // A kill issued during the run → settle the partial message (so it stops
    // showing as streaming) and discard the rest.
    if let Some(run_id) = p.run_id {
        if state.boot.features.agents_enabled && !crate::agent::alive(state, run_id).await {
            let _ = crate::chat::finish_assistant_message(&state.pg, message_id, &acc, None).await;
            return Ok(());
        }
    }

    match result {
        Ok((title, report_md, citations, doc_citations, verification)) => {
            // The terminal report_md (post-coherence/renumber, with references) is
            // authoritative — finalise the streamed message with it; the client
            // reconciles its live draft on `chat.message_posted`.
            let content = format!("# {title}\n\n{report_md}");
            // Persist the roadmap (section list + phase timeline) on the message's
            // `activity` so the "Research steps" block survives a reload.
            let activity = roadmap_sections.as_ref().map(|secs| {
                json!({ "research_roadmap": {
                    "sections": secs,
                    "sections_total": secs.len(),
                    "phases": roadmap_phases,
                }})
            });
            crate::chat::finish_assistant_message(&state.pg, message_id, &content, activity).await?;
            // Replace the placeholder chat title (the truncated prompt set at start)
            // with the pipeline's generated title. Best-effort; the client refetches
            // the chat lists on `chat.message_posted` and prefers the persisted title.
            match sqlx::query!("UPDATE chats SET title = $1 WHERE id = $2", title, p.chat_id)
                .execute(&state.pg)
                .await
            {
                Ok(r) if r.rows_affected() == 1 => {
                    tracing::info!(chat_id = %p.chat_id, %title, "research: chat title updated");
                }
                Ok(r) => {
                    tracing::warn!(chat_id = %p.chat_id, rows = r.rows_affected(), "research: title UPDATE matched no chat");
                }
                Err(e) => {
                    tracing::warn!(chat_id = %p.chat_id, error = %e, "research: title UPDATE failed");
                }
            }
            // Web citations in W1..Wn order, then document citations in D1..Dn
            // order — frame order = reference numbering in each namespace.
            persist_web_citations(&state.pg, p.turn_id, Some(message_id), &citations).await;
            if !doc_citations.is_empty() {
                let _ = crate::chat::persist_citations(&state.pg, message_id, &doc_citations).await;
            }
            create_md_artefact(state, &p, message_id, &title, &content).await;
            // Persist the verification run + light the groundedness pill
            // (best-effort; the report is already posted). ML resolved spans
            // against report_md; the message prepends "# {title}\n\n", so shift
            // span offsets by that prefix to index the stored message content.
            let prefix_chars = format!("# {title}\n\n").chars().count() as i32;
            persist_verification(state, &p, message_id, prefix_chars, verification.as_ref()).await;

            let mut ev = AuditEvent::action("research.completed", &p.role);
            ev.actor_user_id = p.user_id;
            ev.resource_type = Some("chat".into());
            ev.resource_id = Some(p.chat_id);
            ev.payload = Some(json!({
                "turn_id": p.turn_id,
                "message_id": message_id,
                "title": title,
                "source": p.source,
                "citation_count": citations.len(),
                "doc_citation_count": doc_citations.len(),
                "verified": verification.is_some(),
                "urls": citations.iter().map(|c| c.url.as_str()).collect::<Vec<_>>(),
            }));
            let _ = audit::append(&state.pg, &ev).await;

            // Inline both namespaces on the posted frame (the messages API
            // returns no citations; a separate refetch would race).
            let mut out = web_citations_out(&citations);
            out.extend(doc_citations_out(&doc_citations));
            notify(state, &p, message_id, out);
            if let Some(run_id) = p.run_id {
                crate::agent::finish(state, run_id, "completed").await;
            }
            Ok(())
        }
        Err(e) => {
            // Settle the already-streaming message with an honest note (keep any
            // partial draft already streamed); terminal (no retry).
            let content = if acc.trim().is_empty() {
                format!("Deep research on \u{201c}{}\u{201d} could not be completed: {e}.", p.question)
            } else {
                format!("{acc}\n\n*Deep research did not finish cleanly: {e}.*")
            };
            let _ = crate::chat::finish_assistant_message(&state.pg, message_id, &content, None).await;
            notify(state, &p, message_id, Vec::new());
            let mut ev = AuditEvent::action("research.failed", &p.role);
            ev.actor_user_id = p.user_id;
            ev.resource_type = Some("chat".into());
            ev.resource_id = Some(p.chat_id);
            ev.outcome = audit::AuditOutcome::Failure;
            ev.outcome_reason = Some(e.to_string());
            let _ = audit::append(&state.pg, &ev).await;
            if let Some(run_id) = p.run_id {
                crate::agent::finish(state, run_id, "failed").await;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_requires_chat_turn_question() {
        let chat = Uuid::now_v7();
        let turn = Uuid::now_v7();
        let kb = Uuid::now_v7();
        let tmpl_id = Uuid::now_v7().to_string();
        let spec = json!({ "id": tmpl_id, "label": "Ours", "outline_mode": "constrained" });
        let ok = parse(&json!({
            "chat_id": chat, "turn_id": turn, "question": "what is x",
            "template": tmpl_id, "template_spec": spec.clone(), "role": "user",
            "source": "files", "kb_ids": [kb.to_string()], "refinements": ["Last year"],
        }))
        .expect("valid payload");
        assert_eq!(ok.chat_id, chat);
        assert_eq!(ok.question, "what is x");
        assert_eq!(ok.template.as_deref(), Some(tmpl_id.as_str()));
        assert_eq!(ok.template_spec.as_ref(), Some(&spec));
        assert_eq!(ok.source, "files");
        assert_eq!(ok.kb_ids, vec![kb]);
        assert_eq!(ok.refinements, vec!["Last year".to_string()]);
        assert!(ok.run_id.is_none());

        // Built-in path: a bare template id and no spec (spec stays None, and an
        // explicit null must not become Some(Null)).
        let builtin = parse(&json!({
            "chat_id": chat, "turn_id": turn, "question": "q",
            "template": "literature", "template_spec": serde_json::Value::Null,
        }))
        .unwrap();
        assert_eq!(builtin.template.as_deref(), Some("literature"));
        assert!(builtin.template_spec.is_none());

        // Web default: no source/kb_ids/refinements present.
        let web = parse(&json!({ "chat_id": chat, "turn_id": turn, "question": "q" })).unwrap();
        assert_eq!(web.source, "web");
        assert!(web.kb_ids.is_empty() && web.refinements.is_empty());

        assert!(parse(&json!({ "turn_id": turn, "question": "x" })).is_err());
        assert!(parse(&json!({ "chat_id": chat, "question": "x" })).is_err());
        assert!(parse(&json!({ "chat_id": chat, "turn_id": turn })).is_err());
        assert!(parse(&json!({ "chat_id": chat, "turn_id": turn, "question": "  " })).is_err());
    }

    #[test]
    fn research_event_lines_parse() {
        use crate::ml::ResearchEvent;
        let p: ResearchEvent = serde_json::from_str(
            r#"{"type":"progress","phase":"write","detail":"Findings","sections_done":2,"sections_total":6}"#,
        )
        .unwrap();
        assert!(matches!(
            p,
            ResearchEvent::Progress { ref phase, sections_done: Some(2), sections_total: Some(6), .. } if phase == "write"
        ));
        let p2: ResearchEvent =
            serde_json::from_str(r#"{"type":"progress","phase":"plan"}"#).unwrap();
        assert!(matches!(p2, ResearchEvent::Progress { detail: None, sources_read: None, .. }));
        // Phase-1 Done (no doc_citations) → serde default empty.
        let d: ResearchEvent = serde_json::from_str(
            r#"{"type":"done","title":"T","report_md":"section one body","citations":[]}"#,
        )
        .unwrap();
        assert!(matches!(d, ResearchEvent::Done { ref title, ref doc_citations, .. } if title == "T" && doc_citations.is_empty()));
        // Phase-2 Done carries a document citation.
        let d2: ResearchEvent = serde_json::from_str(
            r#"{"type":"done","title":"T2","report_md":"b","citations":[],"doc_citations":[{"doc_id":"00000000-0000-0000-0000-000000000000","quote_text":"q","page_number":null,"chunk_index":null,"clause_section_ref":null}]}"#,
        )
        .unwrap();
        assert!(matches!(d2, ResearchEvent::Done { ref doc_citations, .. } if doc_citations.len() == 1));
        // Phase-1/2 Done (no verification) → None; Phase-3 Done carries it.
        assert!(matches!(d, ResearchEvent::Done { ref verification, .. } if verification.is_none()));
        let d3: ResearchEvent = serde_json::from_str(
            r#"{"type":"done","title":"T3","report_md":"b","citations":[],"verification":{"score":0.8,"total":5,"supported":4,"contradicted":1,"not_mentioned":0,"model":"factcg","spans":[{"start":3,"end":9,"text":"a claim","label":"not_mentioned","score":0.2}]}}"#,
        )
        .unwrap();
        match d3 {
            ResearchEvent::Done { verification: Some(v), .. } => {
                assert!((v.score - 0.8).abs() < 1e-9 && v.total == 5 && v.supported == 4);
                assert_eq!(v.spans.len(), 1);
                assert_eq!(v.spans[0].label, "not_mentioned");
            }
            _ => panic!("expected Done with verification"),
        }
        // Report-writing token (streamed sections).
        let t: ResearchEvent = serde_json::from_str(r#"{"type":"token","delta":"section "}"#).unwrap();
        assert!(matches!(t, ResearchEvent::Token { ref delta } if delta == "section "));
        let e: ResearchEvent = serde_json::from_str(r#"{"type":"error","message":"boom"}"#).unwrap();
        assert!(matches!(e, ResearchEvent::Error { ref message } if message == "boom"));
    }
}
