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

//! Groundedness verification.
//!
//! Mode A (live RAG-answer faithfulness). After a RAG answer finishes streaming,
//! a spawned task sends `(retrieved context, question, answer)` to the verifier
//! (LettuceDetect via the ML `/verify` endpoint), which self-highlights spans of
//! the answer **unsupported by the sources** — *groundedness, not truth*.
//! The result is persisted (a `verification_runs` row + per-span `claim_verdicts`,
//! plus a compact summary denormalised onto the message), audited
//! (`groundedness.verified`), and pushed to the client as a `chat.groundedness`
//! frame.
//!
//! Hard invariants:
//! - **Never blocks TTFT** — this is fire-and-forget, spawned after the answer
//!   already streamed + completed.
//! - **Fail-open** — verifier disabled/unreachable ⇒ no run, no audit, no frame;
//!   the answer is unaffected.
//! - **Respects access by construction** — the verified `context` is the
//!   already-access-filtered retrieval allow-list (no extra read, no cross-wall
//!   surface).
//! - **Every run is a first-class hash-chain audit event.**

use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::state::AppState;

/// Post-stream live groundedness check for one RAG answer. Fire-and-forget:
/// callers spawn this; it never blocks generation and never panics (all errors
/// are logged and swallowed). `context` is the retrieved evidence the answer was
/// composed from; an empty context means RAG didn't run and the caller skips us.
#[allow(clippy::too_many_arguments)]
pub async fn verify_message(
    state: &AppState,
    user_id: Uuid,
    role: String,
    chat_id: Uuid,
    turn_id: Uuid,
    message_id: Uuid,
    question: String,
    answer: String,
    context: String,
) {
    // Ask the ML service. Transport/decode errors fail open (the verifier engine
    // being down is already absorbed there → score=None).
    let overrides = crate::ml::groundedness_overrides(&state.pg).await;
    let res = match crate::ml::verify_live(
        &state.http,
        &state.boot.ml.base_url,
        &context,
        &question,
        &answer,
        &overrides,
        crate::ml::provider_overrides(state, None).await,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "groundedness verify failed; skipping");
            return;
        }
    };

    // score=None ⇒ verifier disabled/unreachable at the ML layer: no run recorded.
    let Some(score) = res.score else {
        return;
    };

    let total = res.total.max(0);
    let flagged = res.flagged.max(0);
    let supported = (total - flagged).max(0);
    // Split the flagged spans by their NLI label: contradicted (the source
    // disagrees — hard fail) vs not_mentioned (the source is silent — soft fail).
    let contradicted = res.spans.iter().filter(|s| s.label == "contradicted").count() as i32;
    let not_mentioned = (flagged - contradicted).max(0);
    let strictness = overrides.strictness.clone().unwrap_or_else(|| "strict".into());

    // 1) The run aggregate.
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
        "live",
        res.model,
        strictness,
        score,
        total,
        supported,
        contradicted,
        not_mentioned,
        user_id,
    )
    .execute(&state.pg)
    .await;
    if let Err(e) = insert_run {
        tracing::warn!(error = %e, "groundedness run insert failed");
        return;
    }

    // 2) One verdict row per flagged span, carrying its NLI label — one UNNEST
    //    insert instead of a per-span round-trip (optimisation audit, L4).
    if !res.spans.is_empty() {
        let ids: Vec<Uuid> = res.spans.iter().map(|_| Uuid::now_v7()).collect();
        let claim_texts: Vec<String> = res.spans.iter().map(|s| s.text.clone()).collect();
        let spans_json: Vec<String> =
            res.spans.iter().map(|s| json!({ "start": s.start, "end": s.end }).to_string()).collect();
        let labels: Vec<String> = res.spans.iter().map(|s| s.label.clone()).collect();
        let scores: Vec<f64> = res.spans.iter().map(|s| s.score).collect();
        let ins = sqlx::query!(
            r#"INSERT INTO claim_verdicts
                 (id, run_id, claim_text, source_span, had_citation, verdict, verifier_score)
               SELECT id, $2, claim_text, span::jsonb, false, verdict::claim_verdict, score
               FROM UNNEST($1::uuid[], $3::text[], $4::text[], $5::text[], $6::float8[])
                  AS t(id, claim_text, span, verdict, score)"#,
            &ids,
            run_id,
            &claim_texts,
            &spans_json,
            &labels,
            &scores,
        )
        .execute(&state.pg)
        .await;
        // A batch failure discards ALL verdicts for the run — surface it (R5).
        if let Err(e) = ins {
            tracing::warn!(error = %e, rows = res.spans.len(), "claim_verdicts span batch insert failed");
        }
    }

    // 3) Denormalise a compact summary onto the message for cheap history load
    //    (mirrors `messages.activity`).
    let spans_json: Vec<_> = res
        .spans
        .iter()
        .map(|s| json!({ "start": s.start, "end": s.end, "text": s.text, "label": s.label }))
        .collect();
    let summary = json!({
        "score": score,
        "total": total,
        "flagged": flagged,
        "contradicted": contradicted,
        "not_mentioned": not_mentioned,
        "model": res.model,
        "spans": spans_json,
    });
    let _ = sqlx::query!(
        "UPDATE messages SET groundedness = $1 WHERE id = $2",
        summary,
        message_id
    )
    .execute(&state.pg)
    .await;

    // 4) Audit — the trust/compliance artefact.
    let mut ev = AuditEvent::action("groundedness.verified", role.as_str());
    ev.actor_user_id = Some(user_id);
    ev.resource_type = Some("message".into());
    ev.resource_id = Some(message_id);
    ev.payload = Some(json!({
        "chat_id": chat_id,
        "run_id": run_id,
        "score": score,
        "total": total,
        "supported": supported,
        "contradicted": contradicted,
        "not_mentioned": not_mentioned,
        "model": res.model,
    }));
    let _ = audit::append(&state.pg, &ev).await;

    // 5) Push the live result to the client.
    state.hub.send_to_user(
        user_id,
        crate::ws::protocol::ServerFrame::ChatGroundedness {
            turn_id,
            message_id,
            score: Some(score),
            total,
            flagged,
            spans: res
                .spans
                .into_iter()
                .map(|s| crate::ws::protocol::GroundSpanOut {
                    start: s.start,
                    end: s.end,
                    text: s.text,
                    label: s.label,
                })
                .collect(),
        },
    );
}

/// Mode B ("Verify draft") background job: decompose the target document into
/// claims and verify each against the caller's sources (resolved at request time,
/// passed in the task payload as `{run_id, path, mime, kb_ids}`). Persists the run
/// + per-claim verdicts, audits, and pushes WS status. Terminal: a verifier outage
/// marks the run `error` and returns Ok (no retry → no duplicate verdicts).
pub async fn verify_draft(
    state: &AppState,
    payload: &serde_json::Value,
) -> std::result::Result<(), crate::error::AppError> {
    let run_id = payload
        .get("run_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| crate::error::AppError::Other(anyhow::anyhow!("verify_draft: bad run_id")))?;
    let path = payload.get("path").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let mime = payload.get("mime").and_then(|v| v.as_str()).map(|s| s.to_string());
    let kb_ids: Vec<String> = payload
        .get("kb_ids")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let created_by: Option<Uuid> = sqlx::query_scalar!(
        "SELECT created_by FROM verification_runs WHERE id = $1",
        run_id
    )
    .fetch_optional(&state.pg)
    .await
    .ok()
    .flatten()
    .flatten();

    let _ = sqlx::query!("UPDATE verification_runs SET status = 'running' WHERE id = $1", run_id)
        .execute(&state.pg)
        .await;
    if let Some(uid) = created_by {
        state.hub.send_to_user(
            uid,
            crate::ws::protocol::ServerFrame::VerificationStatus {
                run_id,
                status: "running".into(),
                progress: None,
            },
        );
    }

    let overrides = crate::ml::groundedness_overrides(&state.pg).await;
    let strictness = overrides.strictness.clone().unwrap_or_else(|| "strict".into());
    let res = match crate::ml::verify_draft(
        &state.http,
        &state.boot.ml.base_url,
        &path,
        mime.as_deref(),
        &kb_ids,
        &overrides,
        crate::ml::provider_overrides(state, None).await,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, %run_id, "verify_draft failed");
            let _ = sqlx::query!(
                "UPDATE verification_runs SET status = 'error', finished_at = now() WHERE id = $1",
                run_id
            )
            .execute(&state.pg)
            .await;
            if let Some(uid) = created_by {
                state.hub.send_to_user(
                    uid,
                    crate::ws::protocol::ServerFrame::VerificationStatus {
                        run_id,
                        status: "error".into(),
                        progress: None,
                    },
                );
            }
            return Ok(()); // terminal — do not retry (avoids duplicate verdicts)
        }
    };

    // One verdict row per claim; evidence + section ride bound_evidence_ref.
    // source_span ({start,end,text}) locates the claim back to the document so
    // the inline highlight + ground-or-cut repair can act on it.
    if !res.claims.is_empty() {
        // One UNNEST insert instead of a per-claim round-trip (optimisation audit,
        // L4). `source_span` stays SQL NULL when absent.
        let ids: Vec<Uuid> = res.claims.iter().map(|_| Uuid::now_v7()).collect();
        let claim_texts: Vec<String> = res.claims.iter().map(|c| c.text.clone()).collect();
        let source_spans: Vec<Option<String>> =
            res.claims.iter().map(|c| c.source_span.as_ref().map(|v| v.to_string())).collect();
        let bounds: Vec<String> = res
            .claims
            .iter()
            .map(|c| json!({ "evidence": c.evidence, "section": c.section }).to_string())
            .collect();
        let had_cites: Vec<bool> = res.claims.iter().map(|c| c.had_citation).collect();
        let verdicts: Vec<String> = res.claims.iter().map(|c| c.verdict.clone()).collect();
        let scores: Vec<f64> = res.claims.iter().map(|c| c.score).collect();
        let ins = sqlx::query!(
            r#"INSERT INTO claim_verdicts
                 (id, run_id, claim_text, source_span, bound_evidence_ref, had_citation, verdict, verifier_score)
               SELECT id, $2, claim_text, source_span::jsonb, bound::jsonb, had_citation,
                      verdict::claim_verdict, score
               FROM UNNEST($1::uuid[], $3::text[], $4::text[], $5::text[], $6::bool[], $7::text[], $8::float8[])
                  AS t(id, claim_text, source_span, bound, had_citation, verdict, score)"#,
            &ids,
            run_id,
            &claim_texts,
            &source_spans as &[Option<String>],
            &bounds,
            &had_cites,
            &verdicts,
            &scores,
        )
        .execute(&state.pg)
        .await;
        if let Err(e) = ins {
            tracing::warn!(error = %e, rows = res.claims.len(), "claim_verdicts claim batch insert failed");
        }
    }

    let _ = sqlx::query!(
        r#"UPDATE verification_runs
             SET status = 'succeeded', faithfulness_score = $2, total_claims = $3,
                 supported = $4, contradicted = $5, not_mentioned = $6, strictness = $7,
                 finished_at = now()
           WHERE id = $1"#,
        run_id,
        res.score,
        res.total,
        res.supported,
        res.contradicted,
        res.not_mentioned,
        strictness,
    )
    .execute(&state.pg)
    .await;

    let mut ev = AuditEvent::action("groundedness.verified", "system");
    ev.actor_user_id = created_by;
    ev.resource_type = Some("verification_run".into());
    ev.resource_id = Some(run_id);
    ev.payload = Some(json!({
        "mode": "verify_draft",
        "score": res.score,
        "total": res.total,
        "supported": res.supported,
        "contradicted": res.contradicted,
        "not_mentioned": res.not_mentioned,
    }));
    let _ = audit::append(&state.pg, &ev).await;

    if let Some(uid) = created_by {
        state.hub.send_to_user(
            uid,
            crate::ws::protocol::ServerFrame::VerificationComplete {
                run_id,
                score: res.score,
                total: res.total,
                supported: res.supported,
                contradicted: res.contradicted,
                not_mentioned: res.not_mentioned,
            },
        );
    }
    Ok(())
}

/// Emit the terminal repair frame so the viewer always clears (success or failure).
fn emit_repair_done(
    state: &AppState,
    uid: Uuid,
    run_id: Uuid,
    doc_id: Uuid,
    counts: (i64, i64, i64),
    error: Option<String>,
) {
    state.hub.send_to_user(
        uid,
        crate::ws::protocol::ServerFrame::RepairComplete {
            run_id,
            document_id: doc_id,
            regenerated: counts.0,
            cut: counts.1,
            kept: counts.2,
            error,
        },
    );
}

/// Ground-or-cut repair of a finished verify-draft run on a
/// **document**: re-retrieve + regenerate (or cut) each flagged claim, re-verify
/// the new citation (never trust a fresh one; an un-groundable claim is
/// cut, not rewritten), and surface each as a **tracked-change proposal** via the
/// existing accept/reject HITL. Gated by the `groundedness.repair` knob upstream.
/// Terminal: ALWAYS emits a `repair.complete` frame (so the UI never hangs) and
/// returns Ok (no duplicate proposals / no retry storm on a non-repairable doc).
pub async fn repair_run(
    state: &AppState,
    payload: &serde_json::Value,
) -> std::result::Result<(), crate::error::AppError> {
    let run_id = payload
        .get("run_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| crate::error::AppError::Other(anyhow::anyhow!("repair_run: bad run_id")))?;

    let run = sqlx::query!(
        r#"SELECT target_type::text AS "target_type!", target_id, status, strictness, created_by
           FROM verification_runs WHERE id = $1"#,
        run_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| crate::error::AppError::Validation("verification run not found".into()))?;

    if run.target_type != "document" || run.status != "succeeded" {
        tracing::warn!(%run_id, "repair_run: not a succeeded document run; skipping");
        return Ok(());
    }
    let doc_id = run.target_id;
    let Some(created_by) = run.created_by else {
        tracing::warn!(%run_id, "repair_run: run has no creator; skipping");
        return Ok(());
    };
    let ctx = crate::auth::load_context(&state.pg, created_by).await?;

    state.hub.send_to_user(
        created_by,
        crate::ws::protocol::ServerFrame::VerificationStatus {
            run_id,
            status: "running".into(),
            progress: Some("repairing".into()),
        },
    );

    // Tracked changes are rewritten in DOCX XML — repair only works on a DOCX
    // workspace document. A PDF/other upload can be highlighted but not repaired.
    let cur = crate::documents::current_version(&state.pg, &state.boot.storage.workspace_dir, doc_id).await?;
    let filename: String =
        sqlx::query_scalar!("SELECT original_filename FROM documents WHERE id = $1", doc_id)
            .fetch_one(&state.pg)
            .await?;
    let is_docx = cur.mime.as_deref().map(|m| m.contains("wordprocessingml")).unwrap_or(false)
        || filename.to_lowercase().ends_with(".docx");
    if !is_docx {
        emit_repair_done(
            state,
            created_by,
            run_id,
            doc_id,
            (0, 0, 0),
            Some("Repair proposes tracked changes, which require a DOCX document — this document can't be repaired.".into()),
        );
        return Ok(());
    }

    // Do the fallible work in a core that may error; whatever happens, emit a
    // terminal frame so the viewer clears.
    match repair_core(state, &ctx, run_id, doc_id, created_by, &run.strictness, &cur).await {
        Ok(counts) => emit_repair_done(state, created_by, run_id, doc_id, counts, None),
        Err(e) => {
            tracing::warn!(error = %e, %run_id, "repair_run core failed");
            emit_repair_done(
                state,
                created_by,
                run_id,
                doc_id,
                (0, 0, 0),
                Some("Repair failed unexpectedly — see the server logs.".into()),
            );
        }
    }
    Ok(())
}

/// The fallible core of [`repair_run`]: resolve KBs, regenerate/cut + re-verify
/// each flagged claim, apply the surviving edits as tracked-change proposals, and
/// record per-claim outcomes. Returns `(regenerated, cut, kept)`.
async fn repair_core(
    state: &AppState,
    ctx: &crate::auth::AuthContext,
    run_id: Uuid,
    doc_id: Uuid,
    created_by: Uuid,
    strictness: &str,
    cur: &crate::documents::CurrentVersion,
) -> std::result::Result<(i64, i64, i64), crate::error::AppError> {
    use crate::ml::RepairClaimInput;

    // Re-resolve the creator's readable KBs for this document (intersection RBAC,
    // exactly as the verify-draft request did) — repair never reads beyond them.
    let pid = crate::documents::project_of(&state.pg, doc_id).await?;
    let linked = sqlx::query_scalar!("SELECT kb_id FROM project_kb_links WHERE project_id = $1", pid)
        .fetch_all(&state.pg)
        .await?;
    let mut kb_ids: Vec<String> = Vec::new();
    for k in linked {
        if crate::kb::can_read(&state.pg, ctx, k).await? {
            kb_ids.push(k.to_string());
        }
    }

    // Flagged + locatable claims only (un-locatable ones cannot be cited or cut).
    let rows = sqlx::query!(
        r#"SELECT id, claim_text, verdict::text AS "verdict!", verifier_score,
                  source_span->>'text' AS source_text,
                  bound_evidence_ref->>'evidence' AS evidence
           FROM claim_verdicts
           WHERE run_id = $1 AND verdict::text <> 'supported' AND source_span->>'text' IS NOT NULL
           ORDER BY id"#,
        run_id
    )
    .fetch_all(&state.pg)
    .await?;
    if rows.is_empty() {
        return Ok((0, 0, 0));
    }

    let inputs: Vec<RepairClaimInput> = rows
        .iter()
        .map(|r| RepairClaimInput {
            text: r.claim_text.clone(),
            source_text: r.source_text.clone(),
            verdict: r.verdict.clone(),
            evidence: r.evidence.clone(),
            score: r.verifier_score,
        })
        .collect();

    let results = crate::ml::repair_draft(
        &state.http,
        &state.boot.ml.base_url,
        &inputs,
        &kb_ids,
        Some(strictness),
    )
    .await?;

    // Build the tracked-change edits from non-`kept` results: a grounded rewrite,
    // or a deletion (empty replace) for an un-groundable claim.
    let mut edits: Vec<crate::ml::EditInput> = Vec::new();
    for res in &results {
        if res.action == "kept" {
            continue;
        }
        if let Some(find) = &res.source_text {
            edits.push(crate::ml::EditInput {
                find: find.clone(),
                replace: res.replacement.clone().unwrap_or_default(),
                context_before: None,
                context_after: None,
            });
        }
    }

    // Apply them as tracked changes → a new version + one document_edits row each,
    // exactly like the `edit_document` tool path (reuses accept/reject HITL).
    let mut find_to_wid: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if !edits.is_empty() {
        let out = std::env::temp_dir()
            .join(format!("pai_repair_{}.docx", Uuid::now_v7()))
            .to_string_lossy()
            .to_string();
        let applied = crate::ml::apply_tracked_changes(
            &state.http,
            &state.boot.ml.base_url,
            &cur.bytes_path,
            &out,
            &edits,
            "Groundedness",
        )
        .await?;
        if !applied.changes.is_empty() {
            let bytes = tokio::fs::read(&out)
                .await
                .map_err(|e| crate::error::AppError::Other(anyhow::anyhow!("read repaired docx: {e}")))?;
            let (ver_id, _n) = crate::documents::add_version(
                state, ctx, doc_id, "assistant_edit", &bytes, Some(created_by),
            )
            .await?;
            // One UNNEST insert instead of a per-change round-trip (optimisation
            // audit, L4); the find→w_id map is built separately (no DB).
            let edit_ids: Vec<Uuid> = applied.changes.iter().map(|_| Uuid::now_v7()).collect();
            let w_ids: Vec<String> = applied.changes.iter().map(|ch| ch.w_id.clone()).collect();
            let finds: Vec<String> = applied.changes.iter().map(|ch| ch.find.clone()).collect();
            let replaces: Vec<String> = applied.changes.iter().map(|ch| ch.replace.clone()).collect();
            let ins = sqlx::query!(
                "INSERT INTO document_edits \
                 (id, document_id, document_version_id, w_id, author, find_text, replace_text) \
                 SELECT id, $2, $3, w_id, 'assistant', find_text, replace_text \
                 FROM UNNEST($1::uuid[], $4::text[], $5::text[], $6::text[]) AS t(id, w_id, find_text, replace_text)",
                &edit_ids,
                doc_id,
                ver_id,
                &w_ids,
                &finds,
                &replaces,
            )
            .execute(&state.pg)
            .await;
            if let Err(e) = ins {
                tracing::warn!(error = %e, rows = applied.changes.len(), "document_edits batch insert failed");
            }
            for ch in &applied.changes {
                find_to_wid.entry(ch.find.clone()).or_insert_with(|| ch.w_id.clone());
            }
        }
        let _ = tokio::fs::remove_file(&out).await;
    }

    // Record the per-claim outcome: repair_action + the rewrite/evidence/w_id.
    let (mut regenerated, mut cut, mut kept) = (0i64, 0i64, 0i64);
    for (row, res) in rows.iter().zip(results.iter()) {
        match res.action.as_str() {
            "regenerated" => regenerated += 1,
            "cut" => cut += 1,
            _ => kept += 1,
        }
        let w_id = res.source_text.as_ref().and_then(|f| find_to_wid.get(f)).cloned();
        let merge = json!({
            "repair_text": res.replacement,
            "repair_evidence": res.evidence,
            "w_id": w_id,
            "citation_ref": res.citation_ref,
            "reverify_verdict": res.reverify_verdict,
        });
        let _ = sqlx::query!(
            "UPDATE claim_verdicts SET repair_action = $2, \
             bound_evidence_ref = COALESCE(bound_evidence_ref, '{}'::jsonb) || $3::jsonb WHERE id = $1",
            row.id,
            res.action,
            merge,
        )
        .execute(&state.pg)
        .await;
    }

    let mut ev = AuditEvent::action("groundedness.repaired", ctx.role.as_str());
    ev.actor_user_id = Some(created_by);
    ev.resource_type = Some("verification_run".into());
    ev.resource_id = Some(run_id);
    ev.payload = Some(json!({
        "document_id": doc_id,
        "regenerated": regenerated,
        "cut": cut,
        "kept": kept,
    }));
    let _ = audit::append(&state.pg, &ev).await;

    Ok((regenerated, cut, kept))
}
