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

//! Super-admin (ephemeral break-glass) endpoints — the UI panel lives in a JIT
//! break-glass session. Every route is gated by
//! the [`SuperAdmin`] extractor (`X-Break-Glass` → Redis grant), mounted OUTSIDE
//! Keycloak so it works even when Keycloak is down. Every use is hash-chain audited
//! by the extractor itself. Zero standing privilege: no persistent super-admin.
//!
//! Stage A: the grant's session metadata for the panel header.
//! Stage B: tweak dynamic (non-boot) tuning knobs — RAG/reranker/ingest — over the
//! audited `config_settings` store; the backend feeds these to the ML service per
//! request (the ML→platform boundary stays clean).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::breakglass::{self, SuperAdmin};
use crate::config::runtime::{self, ConfigValueType};
use crate::error::{AppError, Result};
use crate::state::AppState;

/// Active grant metadata for the panel header (label/reason + remaining TTL the UI
/// counts down client-side, so it doesn't poll — each poll would audit a "use").
pub async fn session(
    State(state): State<AppState>,
    SuperAdmin(ctx): SuperAdmin,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let token = headers
        .get("x-break-glass")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string());
    let grant = match token {
        Some(t) => breakglass::list_active(&state).await?.into_iter().find(|g| g.grant_id == t),
        None => None,
    };
    // The token is never echoed back (the panel already holds it) — only the
    // session's label/reason and remaining TTL, for the header + countdown.
    Ok(Json(json!({
        "role": ctx.role.as_str(),
        "break_glass": ctx.break_glass,
        "grant": grant.map(|g| json!({
            "label": g.label,
            "reason": g.reason,
            "ttl_secs": g.ttl_secs,
        })),
    })))
}

// --- Dynamic tuning knobs ----------------------------------------------------

/// A runtime-tweakable setting: not boot config, fed to the ML service per request.
struct Knob {
    key: &'static str,
    label: &'static str,
    desc: &'static str,
    value_type: ConfigValueType,
    default: &'static str,
    min: Option<i64>,
    max: Option<i64>,
}

/// The registry of settings a super-admin may tweak from the panel. Bounds are
/// enforced on write. Defaults mirror the ML service defaults (ml/app/config.py);
/// an unset key means "ML uses its own default" (no override sent).
const KNOBS: &[Knob] = &[
    Knob { key: "rag.top_k", label: "RAG Top-K", desc: "Chunks kept after rerank and fed to generation.", value_type: ConfigValueType::Int, default: "8", min: Some(1), max: Some(100) },
    Knob { key: "rag.over_retrieval", label: "Over-retrieval multiplier", desc: "How many more than Top-K to search before reranking.", value_type: ConfigValueType::Int, default: "4", min: Some(1), max: Some(20) },
    Knob { key: "rag.max_rounds", label: "Agentic max rounds", desc: "Cap on the decompose → re-query loop.", value_type: ConfigValueType::Int, default: "2", min: Some(1), max: Some(8) },
    Knob { key: "rag.query_variants", label: "Query variants", desc: "Multi-query expansions generated per question.", value_type: ConfigValueType::Int, default: "3", min: Some(1), max: Some(10) },
    Knob { key: "rag.rerank_enabled", label: "Reranker enabled", desc: "Use the cross-encoder reranker after hybrid search.", value_type: ConfigValueType::Bool, default: "true", min: None, max: None },
    Knob { key: "rag.grade_skip_threshold", label: "Grade-skip rerank score", desc: "Skip only the per-sub-question LLM grade CALL when the top reranker score clears this (it never marks the round resolved, so a reformulate round still runs). Scale is reranker-dependent (Jina ~0.5, llama.cpp raw logit) — CALIBRATE from the eval best_rerank distribution; 0 = off (always LLM-grade).", value_type: ConfigValueType::Float, default: "0", min: None, max: None },
    Knob { key: "rag.max_subqueries", label: "Max sub-questions", desc: "How many atomic sub-questions a prompt decomposes into (each gets its own search). Raise for complex multi-part prompts so no question is dropped.", value_type: ConfigValueType::Int, default: "10", min: Some(1), max: Some(20) },
    Knob { key: "rag.max_parents", label: "Max context sections", desc: "Distinct parent sections fed to generation after merge/pool-dedup. Raise so several different sections (e.g. one per question) all fit; costs context length.", value_type: ConfigValueType::Int, default: "16", min: Some(1), max: Some(40) },
    Knob { key: "rag.max_context_chunks", label: "Max merged chunks", desc: "Total retrieved chunks kept after merging all sub-questions, before building context.", value_type: ConfigValueType::Int, default: "24", min: Some(1), max: Some(100) },
    Knob { key: "rag.pool_uncited_per_subq", label: "Pool: uncited chunks/sub-Q", desc: "Beyond the chunks a sub-answer actually cited, also keep this many of its top reranked UNcited chunks in the final [D#] pool — so relevant passages a terse mini-answer skipped are not lost (recall). 0 = cited-only.", value_type: ConfigValueType::Int, default: "3", min: Some(0), max: Some(20) },
    Knob { key: "rag.pool_per_subq_budget", label: "Pool: budget per sub-Q", desc: "The [D#] synthesis pool scales with the sub-question count: budget = min(hard cap, max(merged-chunks, sub-Qs × this)). Ensures a 5-6 part prompt isn't starved by the earlier sub-questions (~3 cited + 3 uncited each).", value_type: ConfigValueType::Int, default: "6", min: Some(1), max: Some(20) },
    Knob { key: "rag.pool_hard_cap", label: "Pool: hard cap", desc: "Absolute ceiling on pooled child chunks before parent-expansion, bounding worst-case final-prompt tokens on a very multi-part prompt.", value_type: ConfigValueType::Int, default: "48", min: Some(1), max: Some(120) },
    Knob { key: "rag.pool_crossref_reserve", label: "Pool: cross-ref reserve", desc: "Reserved pool slots for deterministically-fetched statutory sections — cross-references, ±N neighbours, required anchors and the topic-to-section (table-of-contents) channel — ON TOP of the per-sub-question budget, so a precisely-fetched operative section can never be evicted by a generic uncited chunk. Additive (never lowers cited/uncited recall). Raise if a heavy multi-part prompt still drops edge provisions.", value_type: ConfigValueType::Int, default: "40", min: Some(0), max: Some(80) },
    Knob { key: "rag.answer_reasoning_effort", label: "RAG answer reasoning effort", desc: "Reasoning effort for the FINAL synthesised answer on a retrieval turn (minimal | low | medium | high | xhigh). Capped DOWN only — a lower per-turn choice is respected. Once the per-sub-question mini-answers have done the local work, medium is ~as good as high on the synthesis pass and cuts minutes off the answer. Blank = medium.", value_type: ConfigValueType::String, default: "medium", min: None, max: None },
    Knob { key: "rag.synthesis_idle_timeout_secs", label: "Synthesis: stall timeout", desc: "Max seconds with NO streamed event before an answer/part is treated as stalled and ended fail-soft (any token OR reasoning delta resets it). Catches a SILENT wedged stream. 0 disables.", value_type: ConfigValueType::Int, default: "120", min: Some(0), max: Some(600) },
    Knob { key: "rag.synthesis_part_max_secs", label: "Synthesis: total budget", desc: "Absolute wall-clock budget per answer/part — reasoning deltas do NOT reset it, so this is the guard against a reasoning-runaway (a model streaming summary deltas for minutes with zero output tokens, which the idle timeout never trips). On expiry the part ends fail-soft with a notice, its ML request is aborted and its concurrency permit freed so later per_part parts still run. Generous default; 0 disables.", value_type: ConfigValueType::Int, default: "300", min: Some(0), max: Some(1800) },
    Knob { key: "rag.synthesis_mode", label: "Synthesis mode", desc: "How the final answer is written on a multi-part retrieval turn. 'unified' = one synthesis over all sections (best on flagship models at high effort). 'per_part' = a small synthesis per numbered part, run in parallel and joined — moves work from reasoning to structure so medium/local models stop dropping questions; latency stays ≈ unified because parts run concurrently.", value_type: ConfigValueType::String, default: "unified", min: None, max: None },
    Knob { key: "rag.anchor_lookup_max", label: "Anchor look-ups per turn", desc: "Deterministic retrieval expansion: when a sub-question or the prompt names specific sections (its 'required set'), directly fetch any that retrieval missed — regardless of whether the sub-answer succeeded. This caps such look-ups per turn. Pure Qdrant filter, no LLM.", value_type: ConfigValueType::Int, default: "8", min: Some(0), max: Some(40) },
    Knob { key: "rag.neighbor_span", label: "Neighbour section span (±N)", desc: "For each section found or required, also fetch its ±N numeric neighbours (e.g. s443A → s443, s444) so a provision's operative context isn't split across chunks. 0 = off. Pure Qdrant filter.", value_type: ConfigValueType::Int, default: "1", min: Some(0), max: Some(5) },
    Knob { key: "rag.crossref_max_sections", label: "Cross-ref sections per sub-Q", desc: "Cap on cross-referenced + neighbouring sections fetched per sub-question (the sections a sub-question's top chunks point at, followed one hop). Bounds the deterministic expansion. Pure Qdrant filter.", value_type: ConfigValueType::Int, default: "8", min: Some(0), max: Some(40) },
    Knob { key: "rag.toc_max_sections", label: "TOC channel sweep width", desc: "Topic-to-section channel: a topical, numberless sub-question ('authority to allot… pre-emption') is matched to a statute chapter by title, then this many contiguous section numbers are swept from the chapter start (statute chapters are adjacent, so ~24 reaches the sibling chapter). 0 = channel off. Pure Qdrant, no LLM.", value_type: ConfigValueType::Int, default: "24", min: Some(0), max: Some(60) },
    Knob { key: "rag.late_anchor_cap", label: "Late-anchor recoveries per part", desc: "Last-resort guardrail: before a per-part answer says a section is 'not reproduced', if the part NAMES that section's number but its slice lacks it, force a direct fetch (or reuse an already-pooled block) and add it — up to this many per part. Stops false 'not found' on an obviously-named section. 0 = off. Pure Qdrant, no LLM.", value_type: ConfigValueType::Int, default: "4", min: Some(0), max: Some(12) },
    Knob { key: "rag.show_diagnostics", label: "Show retrieval diagnostics", desc: "On: the chat activity panel includes the retrieval Coverage step (parts covered, sub-questions, documents/sections, expansion counts) — useful when tuning retrieval. Off (default): only the human progress steps are shown; the Coverage step is hidden. Display only; retrieval and its telemetry are unchanged, and this toggles live without a restart.", value_type: ConfigValueType::Bool, default: "false", min: None, max: None },
    Knob { key: "rag.gap_round_enabled", label: "Iterative retrieval", desc: "Iterative retrieval: after the sub-answers and before writing the answer, the model checks whether each part's retrieved evidence is enough and names any specific provisions still missing; a deterministic fetch (sections + BM25 + table-of-contents) tops up the evidence, looping across rounds until sufficient or the corpus is exhausted. No tool-loop in the answer stream.", value_type: ConfigValueType::Bool, default: "true", min: None, max: None },
    Knob { key: "rag.gap_rounds", label: "Iterative retrieval rounds", desc: "Maximum gap-check → fill iterations the iterative-retrieval loop may run before writing the answer. The loop stops early on sufficiency, corpus exhaustion, the reserve/deadline budget or diminishing returns — this is the ceiling. Raise (up to 8) for deep multi-hop research; costs latency per extra round. Do not raise beyond eval-validated values.", value_type: ConfigValueType::Int, default: "3", min: Some(0), max: Some(8) },
    Knob { key: "rag.gap_reserve", label: "Pool: iterative-retrieval reserve", desc: "Maximum additional sections the iterative-retrieval loop may append to the evidence per turn, TOTAL across all rounds — a reserved, non-evictable budget on top of the normal pool. Bounds the extra context length. (Default raised 12→40 on the 15 Jul eval: mean recall 0.60→0.67 at flat latency; the first gap round saturates it, so it is the effective recall dial.)", value_type: ConfigValueType::Int, default: "40", min: Some(0), max: Some(40) },
    Knob { key: "rag.gap_deadline_secs", label: "Iterative retrieval: deadline", desc: "Wall-clock budget for the whole iterative-retrieval phase; when it expires the loop stops fail-soft and synthesis proceeds with what was gathered. Keep well below the total retrieval timeout. 0 = no deadline (only the round/reserve/diminishing stops apply).", value_type: ConfigValueType::Int, default: "60", min: Some(0), max: Some(300) },
    Knob { key: "rag.gap_diminishing_unseen", label: "Iterative retrieval: diminishing-returns floor", desc: "Stop the loop when a round's fraction of NEW (previously-unseen) fetched chunks drops below this — the round is re-surfacing material already retrieved (anti-thrash). 0..1; 0 disables the criterion. Range is not enforced by the numeric bounds below (a float knob) — the ML service clamps to 0..1.", value_type: ConfigValueType::Float, default: "0.2", min: None, max: None },
    Knob { key: "rag.gap_escalate", label: "Iterative retrieval: escalation pass", desc: "On a second attempt at a still-missing item, widen the net: a full hybrid search over the need text (not the judge's search phrase), one query reformulation, and a ±1 neighbour sweep — so a later round is not a verbatim repeat of an earlier one. Off = repeat the same deterministic fetch each round.", value_type: ConfigValueType::Bool, default: "true", min: None, max: None },
    Knob { key: "rag.model_search_mode", label: "Model-driven library search", desc: "Whether the MAIN answering model may call a `search_library` tool to top up evidence when the automatic first pass fell short. 'gaps_only' (default) offers it only when the iterative retrieval left unresolved gaps — healthy turns keep their fast path with zero extra latency. 'always' offers it on every retrieval turn (research agents; costs one extra non-streamed step before the answer). 'off' disables it. Independent of the deployment tool kill-switch and per-agent tool selection.", value_type: ConfigValueType::String, default: "gaps_only", min: None, max: None },
    Knob { key: "rag.model_search_max_calls", label: "Model-driven search: max calls", desc: "How many times the model may call `search_library` in one turn before it is told to answer from what it has (anti-thrash).", value_type: ConfigValueType::Int, default: "4", min: Some(1), max: Some(10) },
    Knob { key: "rag.model_search_deadline_secs", label: "Model-driven search: per-call deadline", desc: "Wall-clock budget for a single `search_library` call. Each call runs a LIGHT retrieval (the model is the outer loop), so this is short by design.", value_type: ConfigValueType::Int, default: "20", min: Some(5), max: Some(120) },
    Knob { key: "rag.model_search_show_commentary", label: "Model-driven search: show reasoning", desc: "Surface the model's brief one-line reason for each library top-up as a live activity detail. Off hides it (the search steps still show).", value_type: ConfigValueType::Bool, default: "true", min: None, max: None },
    Knob { key: "rag.model_search_locus", label: "Model-driven search: where it runs", desc: "Where the `search_library` top-up happens. 'loop' (default) runs it in a non-streamed step BEFORE the answer streams. 'midstream' (experimental) hands the tool to the streaming answer instead, so the model can search between segments of the same reply — saving one step before the first token on gap-turns. Midstream applies to unified synthesis only, and falls back to a plain stream if a provider can't stream tool calls.", value_type: ConfigValueType::String, default: "loop", min: None, max: None },
    Knob { key: "chat.answer_max_continuations", label: "Answer auto-continue passes", desc: "When an answer is truncated at the model's output-token limit (long multi-part answers at higher reasoning effort), how many times to automatically continue it before showing a truncation notice. Each pass resumes where it stopped. 0 = never continue (truncate + notice).", value_type: ConfigValueType::Int, default: "4", min: Some(0), max: Some(10) },
    Knob { key: "ingest.chunk_size", label: "Chunk size (chars)", desc: "Applied to documents ingested from now on.", value_type: ConfigValueType::Int, default: "1500", min: Some(200), max: Some(8000) },
    Knob { key: "ingest.chunk_overlap", label: "Chunk overlap (chars)", desc: "Applied to documents ingested from now on.", value_type: ConfigValueType::Int, default: "400", min: Some(0), max: Some(2000) },
    Knob { key: "ingest.pdfplumber", label: "Table-aware PDF parser", desc: "On: extract PDFs with pdfplumber and append detected tables as Markdown, recovering row/column structure pypdf flattens (fixes table-heavy docs). Off: faster pypdf text only. Applies to documents ingested from now on — re-ingest to apply.", value_type: ConfigValueType::Bool, default: "true", min: None, max: None },
    Knob { key: "workflows.pause_all", label: "Pause all workflows", desc: "Fleet kill-switch: halts all event-driven workflow dispatch and runs.", value_type: ConfigValueType::Bool, default: "false", min: None, max: None },
    Knob { key: "workflows.max_depth", label: "Workflow max depth", desc: "Loop circuit-breaker: how many workflow hops a causal chain may go.", value_type: ConfigValueType::Int, default: "3", min: Some(1), max: Some(50) },
    Knob { key: "groundedness.strict", label: "Strict groundedness", desc: "On: any unsupported claim fails (legal default). Off (lenient): only a contradiction fails — a claim merely not in the sources is tolerated.", value_type: ConfigValueType::Bool, default: "true", min: None, max: None },
    Knob { key: "groundedness.threshold", label: "Flag confidence floor", desc: "Minimum verifier confidence (0–1) to flag a live span; below it the span is treated as grounded.", value_type: ConfigValueType::Float, default: "0", min: None, max: None },
    Knob { key: "groundedness.hhem_filter", label: "HHEM second opinion", desc: "Cross-check each flagged claim with HHEM; rescue (mark supported) any the consistency model judges grounded. Reduces false positives.", value_type: ConfigValueType::Bool, default: "false", min: None, max: None },
    Knob { key: "groundedness.repair", label: "Ground-or-cut repair", desc: "Enable the 'Repair' action on a verified document: regenerate each flagged claim to cite a source (re-verifying the new citation) or cut it, surfaced as tracked-change proposals. Quality is model-dependent; off by default.", value_type: ConfigValueType::Bool, default: "false", min: None, max: None },
    Knob { key: "voice.silence_threshold_ms", label: "Voice turn silence (ms)", desc: "Live voice: trailing-silence the platform waits before ending the speaker's turn. The single highest-leverage latency dial — tune in 100 ms steps. Lower = snappier but risks cutting the user off; higher = feels slow.", value_type: ConfigValueType::Int, default: "600", min: Some(200), max: Some(2000) },
    Knob { key: "voice.ptt_default", label: "Push-to-talk default", desc: "Live voice: default to push-to-talk (explicit hold-to-speak) rather than an open VAD-gated mic. On is the professional default — an always-open mic in a shared room risks capturing privileged third-party speech.", value_type: ConfigValueType::Bool, default: "true", min: None, max: None },
    Knob { key: "voice.aec_required", label: "Require echo cancellation", desc: "Live voice: require browser acoustic echo cancellation before honouring barge-in (so the assistant's own audio can't self-interrupt). Off → barge-in falls back to push-to-talk release.", value_type: ConfigValueType::Bool, default: "true", min: None, max: None },
    Knob { key: "voice.turn_detection", label: "Semantic turn detection", desc: "Live voice: consult the turn-detection sidecar (Silero VAD + Smart-Turn) so a mid-thought pause holds instead of ending the turn. Off (or sidecar absent) → the silence threshold alone decides.", value_type: ConfigValueType::Bool, default: "false", min: None, max: None },
    Knob { key: "research.verify", label: "Deep Research verification", desc: "Run an in-pipeline citation verification + ground-or-cut over each Deep Research report (decompose → verify each claim against the cited evidence → cut contradicted sentences, strip unsupported markers). Requires the groundedness feature + verifier sidecar; fails open. Off by default.", value_type: ConfigValueType::Bool, default: "false", min: None, max: None },
    Knob { key: "research.max_minutes", label: "Research wall-clock (min)", desc: "Per-run Deep Research time budget; beyond it the run delivers best-effort from what it gathered. Must stay below the Rust stream timeout (45 min).", value_type: ConfigValueType::Int, default: "20", min: Some(2), max: Some(45) },
    Knob { key: "research.census_cap", label: "Corpus census cap (docs)", desc: "At or below this many documents a files/hybrid run reads every document (census); above it, it falls back to retrieval sampling with an honest coverage appendix. Tune low on a slow local LLM.", value_type: ConfigValueType::Int, default: "500", min: Some(10), max: Some(5000) },
    Knob { key: "research.notes_concurrency", label: "Research notes concurrency", desc: "Concurrent per-source / per-document note-extraction calls during a Deep Research run.", value_type: ConfigValueType::Int, default: "4", min: Some(1), max: Some(16) },
    Knob { key: "research.deepen_enabled", label: "Per-section deepening", desc: "Before writing, judge each report section's evidence and run a bounded targeted dig for the gaps (web or corpus), binding new sources to that section. Skipped on a small model context and time-boxed within the run; fails open. On by default.", value_type: ConfigValueType::Bool, default: "true", min: None, max: None },
    Knob { key: "research.deepen_concurrency", label: "Deepening concurrency", desc: "Concurrent sections deepened at once during the pre-write deepening stage. A dig is heavier than a note, so this is separate from (and lower than) the notes concurrency.", value_type: ConfigValueType::Int, default: "2", min: Some(1), max: Some(16) },
];

/// Allowed values for an enum (String) knob — rendered as a `<select>` and validated on
/// write. None ⇒ a free String / numeric knob.
fn knob_options(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "rag.answer_reasoning_effort" => Some(&["minimal", "low", "medium", "high", "xhigh"]),
        "rag.synthesis_mode" => Some(&["unified", "per_part"]),
        "rag.model_search_mode" => Some(&["off", "gaps_only", "always"]),
        "rag.model_search_locus" => Some(&["loop", "midstream"]),
        _ => None,
    }
}

#[derive(Serialize)]
pub struct KnobOut {
    key: String,
    label: String,
    desc: String,
    value_type: String,
    value: String,
    is_default: bool,
    min: Option<i64>,
    max: Option<i64>,
    /// Enum choices for a select input; absent for numeric/free-text knobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Vec<String>>,
}

/// The knob registry with each setting's current value (or its default if unset).
pub async fn list_config(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
) -> Result<Json<Vec<KnobOut>>> {
    let mut out = Vec::with_capacity(KNOBS.len());
    for k in KNOBS {
        let cur = runtime::get(&state.pg, k.key).await?;
        let (value, is_default) = match cur {
            Some(e) => (e.value, false),
            None => (k.default.to_string(), true),
        };
        out.push(KnobOut {
            key: k.key.into(),
            label: k.label.into(),
            desc: k.desc.into(),
            value_type: k.value_type.as_str().into(),
            value,
            is_default,
            min: k.min,
            max: k.max,
            options: knob_options(k.key).map(|o| o.iter().map(|s| s.to_string()).collect()),
        });
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct SetBody {
    pub value: String,
}

/// Set one knob. Rejects unknown keys and out-of-bounds values; `runtime::set`
/// validates the type and audits `config.changed` atomically.
pub async fn set_config(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
    Path(key): Path<String>,
    Json(body): Json<SetBody>,
) -> Result<StatusCode> {
    let knob = KNOBS
        .iter()
        .find(|k| k.key == key)
        .ok_or_else(|| AppError::Validation(format!("unknown setting {key:?}")))?;

    if knob.value_type == ConfigValueType::Int {
        let n: i64 = body
            .value
            .parse()
            .map_err(|_| AppError::Validation(format!("{} must be a whole number", knob.label)))?;
        if let Some(min) = knob.min {
            if n < min {
                return Err(AppError::Validation(format!("{} must be ≥ {min}", knob.label)));
            }
        }
        if let Some(max) = knob.max {
            if n > max {
                return Err(AppError::Validation(format!("{} must be ≤ {max}", knob.label)));
            }
        }
    }
    // Enum (String) knobs: reject any value outside the allowed set.
    if let Some(opts) = knob_options(&key) {
        if !opts.contains(&body.value.as_str()) {
            return Err(AppError::Validation(format!("{} must be one of: {}", knob.label, opts.join(", "))));
        }
    }

    // actor: ephemeral super-admin (no user id) → audited as the super_admin role.
    runtime::set(&state.pg, knob.key, &body.value, knob.value_type, "global", None, "super_admin").await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Reset one knob to its default by removing the override row —
/// restores `is_default=true`. Audited as `config.changed` via `runtime::unset`.
pub async fn reset_config(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
    Path(key): Path<String>,
) -> Result<StatusCode> {
    KNOBS
        .iter()
        .find(|k| k.key == key)
        .ok_or_else(|| AppError::Validation(format!("unknown setting {key:?}")))?;
    runtime::unset(&state.pg, &key, "super_admin").await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod knob_tests {
    use super::{ConfigValueType, KNOBS};

    #[test]
    fn research_knobs_registered_with_types_and_bounds() {
        let by_key = |k: &str| KNOBS.iter().find(|x| x.key == k);
        // The Deep Research budget + verification knobs are all present.
        for k in [
            "research.verify",
            "research.max_minutes",
            "research.census_cap",
            "research.notes_concurrency",
            "research.deepen_enabled",
            "research.deepen_concurrency",
        ] {
            assert!(by_key(k).is_some(), "knob {k} registered");
        }
        // verify is a boolean toggle, default off (Phases 1-2 behaviour unchanged).
        let v = by_key("research.verify").unwrap();
        assert_eq!(v.value_type, ConfigValueType::Bool);
        assert_eq!(v.default, "false");
        // The numeric budgets carry bounds enforced on write.
        let mm = by_key("research.max_minutes").unwrap();
        assert_eq!(mm.value_type, ConfigValueType::Int);
        assert_eq!((mm.min, mm.max), (Some(2), Some(45)));
        let cap = by_key("research.census_cap").unwrap();
        assert_eq!((cap.min, cap.max), (Some(10), Some(5000)));
        // Deepening: a default-on boolean plus a bounded concurrency dial.
        let de = by_key("research.deepen_enabled").unwrap();
        assert_eq!(de.value_type, ConfigValueType::Bool);
        assert_eq!(de.default, "true");
        let dc = by_key("research.deepen_concurrency").unwrap();
        assert_eq!(dc.value_type, ConfigValueType::Int);
        assert_eq!((dc.min, dc.max), (Some(1), Some(16)));
    }

    #[test]
    fn knob_keys_unique() {
        let mut keys: Vec<&str> = KNOBS.iter().map(|k| k.key).collect();
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n, "no duplicate knob keys");
    }

    #[test]
    fn iterative_retrieval_knobs_registered_with_types_and_bounds() {
        let by_key = |k: &str| KNOBS.iter().find(|x| x.key == k);
        // The whole iterative-retrieval family is present.
        for k in ["rag.gap_round_enabled", "rag.gap_rounds", "rag.gap_reserve",
                  "rag.gap_deadline_secs", "rag.gap_diminishing_unseen", "rag.gap_escalate"] {
            assert!(by_key(k).is_some(), "knob {k} registered");
        }
        // Rounds raised to a default of 3 with an 8-round ceiling ("as many rounds as it takes").
        let rounds = by_key("rag.gap_rounds").unwrap();
        assert_eq!(rounds.value_type, ConfigValueType::Int);
        assert_eq!(rounds.default, "3");
        assert_eq!((rounds.min, rounds.max), (Some(0), Some(8)));
        // Deadline is a bounded seconds Int (0 = off) enforced on write.
        let dl = by_key("rag.gap_deadline_secs").unwrap();
        assert_eq!(dl.value_type, ConfigValueType::Int);
        assert_eq!((dl.min, dl.max), (Some(0), Some(300)));
        // Diminishing floor is a Float (0..1 clamped ML-side, so no i64 bounds here).
        let dim = by_key("rag.gap_diminishing_unseen").unwrap();
        assert_eq!(dim.value_type, ConfigValueType::Float);
        assert_eq!(dim.default, "0.2");
        assert_eq!((dim.min, dim.max), (None, None));
        // Escalation is a boolean toggle, default on.
        let esc = by_key("rag.gap_escalate").unwrap();
        assert_eq!(esc.value_type, ConfigValueType::Bool);
        assert_eq!(esc.default, "true");
    }

    #[test]
    fn model_search_knobs_registered_with_types_and_bounds() {
        let by_key = |k: &str| KNOBS.iter().find(|x| x.key == k);
        for k in ["rag.model_search_mode", "rag.model_search_max_calls",
                  "rag.model_search_deadline_secs", "rag.model_search_show_commentary",
                  "rag.model_search_locus"] {
            assert!(by_key(k).is_some(), "knob {k} registered");
        }
        // Mode is an enum, default gaps_only, validated against the three-way option set.
        let mode = by_key("rag.model_search_mode").unwrap();
        assert_eq!(mode.value_type, ConfigValueType::String);
        assert_eq!(mode.default, "gaps_only");
        assert_eq!(super::knob_options("rag.model_search_mode"), Some(&["off", "gaps_only", "always"][..]));
        // Locus is an enum defaulting to the non-streamed loop; midstream is opt-in.
        let locus = by_key("rag.model_search_locus").unwrap();
        assert_eq!((locus.value_type, locus.default), (ConfigValueType::String, "loop"));
        assert_eq!(super::knob_options("rag.model_search_locus"), Some(&["loop", "midstream"][..]));
        // Caps + deadline are bounded Ints enforced on write.
        let mc = by_key("rag.model_search_max_calls").unwrap();
        assert_eq!((mc.value_type, mc.min, mc.max), (ConfigValueType::Int, Some(1), Some(10)));
        let dl = by_key("rag.model_search_deadline_secs").unwrap();
        assert_eq!((dl.min, dl.max), (Some(5), Some(120)));
    }
}

// --- Accounts + cross-user chat viewing -------------------------------------

use crate::audit::{self, AuditEvent};
use uuid::Uuid as Uid;

fn audit_super(action: &str, target: Uid) -> AuditEvent {
    let mut ev = AuditEvent::action(action, "super_admin");
    ev.resource_type = Some("user".into());
    ev.resource_id = Some(target);
    ev.risk_anomaly_flag = true; // cross-user / destructive — always notable
    ev
}

#[derive(Serialize)]
pub struct UserRow {
    id: Uid,
    email: String,
    display_name: String,
    role: String,
    deactivated: bool,
}

/// Every user (for the Accounts + Chats tabs).
pub async fn list_users(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
) -> Result<Json<Vec<UserRow>>> {
    let rows = sqlx::query!(
        r#"SELECT id, email, display_name, role::text AS "role!", deactivated_at
           FROM users ORDER BY created_at"#
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| UserRow {
                id: r.id,
                email: r.email,
                display_name: r.display_name,
                role: r.role,
                deactivated: r.deactivated_at.is_some(),
            })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct ChatRow {
    id: Uid,
    title: String,
    created_at: String,
}

/// Any user's LLM chats. Bypasses the owner check by design — audited.
pub async fn user_chats(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
    Path(user_id): Path<Uid>,
) -> Result<Json<Vec<ChatRow>>> {
    let rows = sqlx::query!(
        "SELECT id, title, created_at FROM chats \
         WHERE owner_user_id = $1 AND archived_at IS NULL ORDER BY created_at DESC",
        user_id
    )
    .fetch_all(&state.pg)
    .await?;
    let _ = audit::append(&state.pg, &audit_super("admin.chats.listed", user_id)).await;
    Ok(Json(
        rows.into_iter()
            .map(|r| ChatRow { id: r.id, title: r.title, created_at: r.created_at.to_string() })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct MsgRow {
    role: String,
    content: String,
    created_at: String,
}

/// The messages of any chat (LLM history is plaintext). Audited as a chat view.
pub async fn chat_messages(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
    Path(chat_id): Path<Uid>,
) -> Result<Json<Vec<MsgRow>>> {
    let owner = sqlx::query_scalar!("SELECT owner_user_id FROM chats WHERE id = $1", chat_id)
        .fetch_optional(&state.pg)
        .await?;
    let rows = sqlx::query!(
        "SELECT role::text AS \"role!\", content, created_at FROM messages \
         WHERE chat_id = $1 AND role IN ('user','assistant') ORDER BY sequence_number",
        chat_id
    )
    .fetch_all(&state.pg)
    .await?;
    let mut ev = audit_super("admin.chat.viewed", owner.unwrap_or(chat_id));
    ev.resource_type = Some("chat".into());
    ev.resource_id = Some(chat_id);
    let _ = audit::append(&state.pg, &ev).await;
    Ok(Json(
        rows.into_iter()
            .map(|r| MsgRow { role: r.role, content: r.content, created_at: r.created_at.to_string() })
            .collect(),
    ))
}

/// A mandatory operator-supplied reason for a destructive action (audited).
#[derive(Deserialize)]
pub struct ReasonQuery {
    #[serde(default)]
    pub reason: String,
}

fn require_reason(q: &ReasonQuery) -> Result<String> {
    let r = q.reason.trim();
    if r.is_empty() {
        return Err(AppError::Validation("a reason is required for this action".into()));
    }
    Ok(r.to_string())
}

/// Soft-deactivate an account (kept; Keycloak stays authoritative). Reuses the
/// client-admin logic but via the break-glass path. Requires a reason (audited).
pub async fn deactivate_user(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
    Path(user_id): Path<Uid>,
    Query(q): Query<ReasonQuery>,
) -> Result<Json<serde_json::Value>> {
    let reason = require_reason(&q)?;
    crate::cache::rate_limit_guard(&state.redis, "sa-destructive", 20, 60).await?;
    sqlx::query!(
        "UPDATE users SET deactivated_at = now() WHERE id = $1 AND deactivated_at IS NULL",
        user_id
    )
    .execute(&state.pg)
    .await?;
    state.hub.close_user(user_id);
    let mut ev = audit_super("user.deactivated", user_id);
    ev.payload = Some(json!({ "reason": reason }));
    let _ = audit::append(&state.pg, &ev).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- GDPR erasure ------------------------------------------------------------

/// Is any of the user's project/document data under an active legal hold? Erasure
/// must refuse while held (litigation > right-to-erasure).
async fn under_hold(pg: &sqlx::PgPool, user_id: Uid) -> Result<bool> {
    let held = sqlx::query_scalar!(
        r#"SELECT EXISTS(
            SELECT 1 FROM legal_holds lh WHERE lh.active AND (
              (lh.resource_type = 'project'  AND lh.resource_id IN (SELECT id FROM projects  WHERE owner_user_id = $1))
              OR (lh.resource_type = 'document' AND lh.resource_id IN (SELECT id FROM documents WHERE created_by   = $1))
            )) AS "held!""#,
        user_id
    )
    .fetch_one(pg)
    .await?;
    Ok(held)
}

/// Best-effort unlink of a user's on-disk artefacts (documents, attachments,
/// generated files, exports). A missing file is fine.
async fn unlink_user_files(state: &AppState, user_id: Uid) {
    let s = &state.boot.storage;
    // Each column stores a category-relative suffix — resolve before unlinking.
    let jobs: [(&str, &str); 5] = [
        ("SELECT disk_path FROM generated_artefacts WHERE created_by = $1", s.artefacts_dir.as_str()),
        ("SELECT disk_path FROM message_attachments WHERE uploaded_by = $1", s.message_attachments_dir.as_str()),
        ("SELECT disk_path FROM exports WHERE requested_by = $1", s.exports_dir.as_str()),
        ("SELECT bytes_path FROM kb_documents WHERE created_by = $1", s.documents_dir.as_str()),
        ("SELECT avatar_path FROM users WHERE id = $1", s.avatars_dir.as_str()),
    ];
    for (q, dir) in jobs {
        let rows: Vec<Option<String>> =
            sqlx::query_scalar(q).bind(user_id).fetch_all(&state.pg).await.unwrap_or_default();
        for p in rows.into_iter().flatten() {
            if !p.is_empty() {
                let abs = crate::storage::resolve_file(dir, &p);
                let _ = tokio::fs::remove_file(&abs).await;
            }
        }
    }
}

/// GDPR right-to-erasure: purge everything tied to the account, anonymise the
/// users row, and write a single high-risk audit event. Audit itself is never
/// touched (append-only, immutable). Refuses while data is under legal hold.
pub async fn erase_user(
    State(state): State<AppState>,
    SuperAdmin(_ctx): SuperAdmin,
    Path(user_id): Path<Uid>,
    Query(q): Query<ReasonQuery>,
) -> Result<Json<serde_json::Value>> {
    let reason = require_reason(&q)?;
    crate::cache::rate_limit_guard(&state.redis, "sa-destructive", 20, 60).await?;
    let ev = audit_super("gdpr.erasure", user_id);
    let report = purge_user(&state, user_id, &reason, ev).await?;
    Ok(Json(report))
}

/// GDPR/erasure core: purge everything tied to `user_id`, anonymise the users row,
/// and write the caller-supplied audit event atomically with the deletes. Refuses
/// while any of the account's data is under an active legal hold. Reused by the
/// super-admin console (`erase_user`) and the SCIM `DELETE …?delete_behaviour=purge`
/// path — callers pass their own audit event (action + actor) so the log reflects
/// who purged and why. Audit itself is never touched (append-only, immutable).
pub async fn purge_user(
    state: &AppState,
    user_id: Uid,
    reason: &str,
    mut ev: crate::audit::AuditEvent,
) -> Result<serde_json::Value> {
    if under_hold(&state.pg, user_id).await? {
        return Err(AppError::Validation(
            "this account has data under an active legal hold — clear the hold before erasing".into(),
        ));
    }

    // Drop on-disk files first (the rows that point at them go next).
    unlink_user_files(state, user_id).await;
    state.hub.close_user(user_id);

    let mut tx = state.pg.begin().await?;
    let mut report = serde_json::Map::new();

    // Two chat/message/project references have no ON DELETE CASCADE, so they must be
    // cleared before the owning rows are deleted (else the delete violates the FK):
    //  • automation_runs.output_chat_id → chats: unlink runs that targeted this
    //    user's chats (keeps other users' run rows intact).
    let nulled = sqlx::query(
        "UPDATE automation_runs SET output_chat_id = NULL \
         WHERE output_chat_id IN (SELECT id FROM chats WHERE owner_user_id = $1)",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if nulled > 0 {
        report.insert("automation_runs_unlinked".into(), serde_json::json!(nulled));
    }

    // Explicit deletes — most user FKs are deliberately not ON DELETE CASCADE so a
    // deletion can't break the graph; we purge in dependency-safe order. Child rows
    // that DO cascade (messages←chats, group_chat_messages←group_chats, etc.) ride
    // along. Order: leaf/owned content → grants → owned roots.
    let deletes: &[(&str, &str)] = &[
        // moderation_flags → chats/messages/projects (no cascade): clear first so the
        // chat/message/project deletes below don't trip its FKs.
        ("moderation_flags", "DELETE FROM moderation_flags WHERE user_id = $1 \
            OR chat_id IN (SELECT id FROM chats WHERE owner_user_id = $1) \
            OR project_id IN (SELECT id FROM projects WHERE owner_user_id = $1) \
            OR team_id IN (SELECT id FROM projects WHERE owner_user_id = $1)"),
        ("message_reactions", "DELETE FROM message_reactions WHERE user_id = $1"),
        ("group_chat_messages_authored", "DELETE FROM group_chat_messages WHERE sender_user_id = $1"),
        ("group_chat_members", "DELETE FROM group_chat_members WHERE user_id = $1"),
        ("group_chats", "DELETE FROM group_chats WHERE created_by = $1"),
        ("chats", "DELETE FROM chats WHERE owner_user_id = $1"),
        ("feedback", "DELETE FROM feedback WHERE user_id = $1"),
        ("memory_facts", "DELETE FROM memory_facts WHERE owner_user_id = $1"),
        ("automations", "DELETE FROM automations WHERE owner_user_id = $1"),
        ("generated_artefacts", "DELETE FROM generated_artefacts WHERE created_by = $1"),
        ("message_attachments", "DELETE FROM message_attachments WHERE uploaded_by = $1"),
        ("exports", "DELETE FROM exports WHERE requested_by = $1"),
        ("prompts", "DELETE FROM prompts WHERE created_by = $1"),
        ("agents", "DELETE FROM agents WHERE created_by = $1"),
        ("skills", "DELETE FROM skills WHERE created_by = $1"),
        ("kb_access_grants", "DELETE FROM kb_access_grants WHERE principal_type = 'user' AND principal_id = $1"),
        ("access_grants", "DELETE FROM access_grants WHERE principal_type = 'user' AND principal_id = $1"),
        ("knowledge_bases", "DELETE FROM knowledge_bases WHERE owner_id = $1"),
        ("documents", "DELETE FROM documents WHERE created_by = $1"),
        ("group_members", "DELETE FROM group_members WHERE user_id = $1"),
        ("projects", "DELETE FROM projects WHERE owner_user_id = $1"),
    ];
    for (name, sql) in deletes {
        let n = sqlx::query(*sql).bind(user_id).execute(&mut *tx).await?.rows_affected();
        if n > 0 {
            report.insert((*name).into(), serde_json::json!(n));
        }
    }

    // Anonymise the account record itself (keep the id so audit actor refs resolve).
    sqlx::query!(
        "UPDATE users SET email = 'erased-' || id || '@gdpr.invalid', display_name = 'Erased user', \
         deactivated_at = COALESCE(deactivated_at, now()) WHERE id = $1",
        user_id
    )
    .execute(&mut *tx)
    .await?;

    let mut payload = report.clone();
    payload.insert("reason".into(), json!(reason));
    ev.resource_type = Some("user".into());
    ev.resource_id = Some(user_id);
    ev.payload = Some(serde_json::Value::Object(payload));
    audit::append_with(&mut tx, &ev).await?;

    tx.commit().await?;
    Ok(serde_json::json!({ "ok": true, "erased": report }))
}
