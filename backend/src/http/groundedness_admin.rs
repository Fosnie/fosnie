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

//! Client-admin groundedness/verification dashboard (BACKLOG A1). Read-only
//! aggregation over `verification_runs` — it surfaces the otherwise-invisible
//! verification moat as a governance artefact: per-interaction trust scores,
//! source traceability, and answer-quality-over-time. Segmented by mode: live
//! chat (Mode A) and "Verify draft" document/artefact checks (Mode B).
//!
//! No new tables: every metric is derived from `verification_runs` (run-level
//! scores + verdict mix, populated for both modes — see
//! `groundedness/mod.rs::verify_message`), joined to `messages`/`chats`/`agents`
//! for the live drill-down, plus the `citations` tables for source traceability.

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::auth::keycloak::AuthUser;
use crate::auth::permissions;
use crate::error::Result;
use crate::state::AppState;

#[derive(Serialize)]
pub struct VerdictMix {
    pub supported: i64,
    pub contradicted: i64,
    pub not_mentioned: i64,
}

/// One day of a 30-day verification series (contiguous; empty days are zero runs
/// and a null average).
#[derive(Serialize)]
pub struct GroundednessDay {
    pub day: String,
    pub avg_score: Option<f64>,
    pub runs: i64,
}

/// Average grounding for the answers produced under one Agent (live mode).
#[derive(Serialize)]
pub struct AgentGrounding {
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub avg_score: Option<f64>,
    pub runs: i64,
}

/// A live interaction in the drill-down (lowest-grounded first); `chat_id` links
/// the admin straight to the conversation.
#[derive(Serialize)]
pub struct LiveInteraction {
    pub run_id: Uuid,
    pub message_id: Uuid,
    pub chat_id: Uuid,
    pub snippet: String,
    pub score: Option<f64>,
    pub flagged: i32,
    pub created_at: String,
}

/// A draft/document verification run in the Mode-B drill-down.
#[derive(Serialize)]
pub struct DraftRun {
    pub run_id: Uuid,
    pub target_type: String,
    pub status: String,
    pub score: Option<f64>,
    pub supported: i32,
    pub contradicted: i32,
    pub not_mentioned: i32,
    pub created_at: String,
}

#[derive(Serialize)]
pub struct StatusCount {
    pub status: String,
    pub count: i64,
}

#[derive(Serialize)]
pub struct GroundednessAnalytics {
    // ── Mode A — live chat ──
    pub live_runs: i64,
    pub live_avg_score: Option<f64>,
    pub live_verdicts: VerdictMix,
    /// Fraction of live-verified answers carrying ≥1 citation (source traceability).
    pub live_cited_fraction: Option<f64>,
    pub live_series: Vec<GroundednessDay>,
    pub per_agent: Vec<AgentGrounding>,
    pub lowest_interactions: Vec<LiveInteraction>,
    // ── Mode B — draft / document ──
    pub draft_runs: i64,
    pub draft_avg_score: Option<f64>,
    pub draft_verdicts: VerdictMix,
    pub draft_by_status: Vec<StatusCount>,
    pub draft_series: Vec<GroundednessDay>,
    pub recent_runs: Vec<DraftRun>,
}

pub async fn analytics(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<GroundednessAnalytics>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::GROUNDEDNESS_VIEW).await?;

    // ── Live (Mode A) totals + verdict mix. AVG ignores failed/null-score runs;
    //    the verdict columns are summed across every run.
    let live = sqlx::query!(
        r#"SELECT COUNT(*) AS "runs!: i64",
                  AVG(faithfulness_score) FILTER (
                    WHERE status = 'succeeded' AND faithfulness_score IS NOT NULL
                  ) AS "avg_score: f64",
                  COALESCE(SUM(supported), 0)::bigint     AS "supported!: i64",
                  COALESCE(SUM(contradicted), 0)::bigint  AS "contradicted!: i64",
                  COALESCE(SUM(not_mentioned), 0)::bigint AS "not_mentioned!: i64"
           FROM verification_runs
           WHERE mode::text = 'live'"#
    )
    .fetch_one(&state.pg)
    .await?;

    // Source traceability: of the live-verified answers, how many cite a source
    // (Project Knowledge / workspace doc, or a web source).
    let cited = sqlx::query_scalar!(
        r#"SELECT (COUNT(*) FILTER (
                     WHERE EXISTS (SELECT 1 FROM citations c WHERE c.message_id = vr.target_id)
                        OR EXISTS (SELECT 1 FROM web_citations w WHERE w.message_id = vr.target_id)
                   ))::float8
                 / NULLIF(COUNT(*), 0)::float8 AS "frac: f64"
           FROM verification_runs vr
           WHERE vr.mode::text = 'live' AND vr.target_type::text = 'message'"#
    )
    .fetch_one(&state.pg)
    .await?;

    let live_series = sqlx::query!(
        r#"SELECT to_char(d, 'YYYY-MM-DD') AS "day!",
                  AVG(vr.faithfulness_score) FILTER (WHERE vr.status = 'succeeded') AS "avg_score: f64",
                  COUNT(vr.id) AS "runs!: i64"
           FROM generate_series((now() - interval '29 days')::date, now()::date, interval '1 day') d
           LEFT JOIN verification_runs vr
             ON vr.mode::text = 'live'
            AND date_trunc('day', vr.created_at)::date = d::date
           GROUP BY d ORDER BY d"#
    )
    .fetch_all(&state.pg)
    .await?;

    // Per-Agent grounding: live runs target a message; join through the chat to the
    // Agent (null = a chat run without a named Agent).
    let per_agent = sqlx::query!(
        r#"SELECT ag.id::text AS "agent_id?: String",
                  ag.name AS "agent_name?",
                  AVG(vr.faithfulness_score) FILTER (WHERE vr.status = 'succeeded') AS "avg_score: f64",
                  COUNT(vr.id) AS "runs!: i64"
           FROM verification_runs vr
           JOIN messages m ON m.id = vr.target_id
           JOIN chats c ON c.id = m.chat_id
           LEFT JOIN agents ag ON ag.id = c.agent_id
           WHERE vr.mode::text = 'live' AND vr.target_type::text = 'message'
           GROUP BY ag.id, ag.name
           ORDER BY COUNT(vr.id) DESC"#
    )
    .fetch_all(&state.pg)
    .await?;

    // Drill-down: the lowest-grounded interactions, freshest first within a score.
    let lowest = sqlx::query!(
        r#"SELECT vr.id AS "run_id!", m.id AS "message_id!", c.id AS "chat_id!",
                  left(m.content, 160) AS "snippet!",
                  vr.faithfulness_score AS "score: f64",
                  (COALESCE(vr.contradicted, 0) + COALESCE(vr.not_mentioned, 0)) AS "flagged!: i32",
                  vr.created_at
           FROM verification_runs vr
           JOIN messages m ON m.id = vr.target_id
           JOIN chats c ON c.id = m.chat_id
           WHERE vr.mode::text = 'live' AND vr.target_type::text = 'message'
             AND vr.status = 'succeeded' AND vr.faithfulness_score IS NOT NULL
           ORDER BY vr.faithfulness_score ASC, vr.created_at DESC
           LIMIT 10"#
    )
    .fetch_all(&state.pg)
    .await?;

    // ── Draft / document (Mode B) ──
    let draft = sqlx::query!(
        r#"SELECT COUNT(*) AS "runs!: i64",
                  AVG(faithfulness_score) FILTER (
                    WHERE status = 'succeeded' AND faithfulness_score IS NOT NULL
                  ) AS "avg_score: f64",
                  COALESCE(SUM(supported), 0)::bigint     AS "supported!: i64",
                  COALESCE(SUM(contradicted), 0)::bigint  AS "contradicted!: i64",
                  COALESCE(SUM(not_mentioned), 0)::bigint AS "not_mentioned!: i64"
           FROM verification_runs
           WHERE mode::text = 'verify_draft'"#
    )
    .fetch_one(&state.pg)
    .await?;

    let draft_by_status = sqlx::query!(
        r#"SELECT status AS "status!", COUNT(*) AS "count!: i64"
           FROM verification_runs
           WHERE mode::text = 'verify_draft'
           GROUP BY status ORDER BY COUNT(*) DESC"#
    )
    .fetch_all(&state.pg)
    .await?;

    let draft_series = sqlx::query!(
        r#"SELECT to_char(d, 'YYYY-MM-DD') AS "day!",
                  AVG(vr.faithfulness_score) FILTER (WHERE vr.status = 'succeeded') AS "avg_score: f64",
                  COUNT(vr.id) AS "runs!: i64"
           FROM generate_series((now() - interval '29 days')::date, now()::date, interval '1 day') d
           LEFT JOIN verification_runs vr
             ON vr.mode::text = 'verify_draft'
            AND date_trunc('day', vr.created_at)::date = d::date
           GROUP BY d ORDER BY d"#
    )
    .fetch_all(&state.pg)
    .await?;

    let recent = sqlx::query!(
        r#"SELECT id AS "run_id!", target_type::text AS "target_type!", status AS "status!",
                  faithfulness_score AS "score: f64",
                  COALESCE(supported, 0)::int     AS "supported!: i32",
                  COALESCE(contradicted, 0)::int  AS "contradicted!: i32",
                  COALESCE(not_mentioned, 0)::int AS "not_mentioned!: i32",
                  created_at
           FROM verification_runs
           WHERE mode::text = 'verify_draft'
           ORDER BY created_at DESC LIMIT 15"#
    )
    .fetch_all(&state.pg)
    .await?;

    Ok(Json(GroundednessAnalytics {
        live_runs: live.runs,
        live_avg_score: live.avg_score,
        live_verdicts: VerdictMix {
            supported: live.supported,
            contradicted: live.contradicted,
            not_mentioned: live.not_mentioned,
        },
        live_cited_fraction: cited,
        live_series: live_series
            .into_iter()
            .map(|r| GroundednessDay { day: r.day, avg_score: r.avg_score, runs: r.runs })
            .collect(),
        per_agent: per_agent
            .into_iter()
            .map(|r| AgentGrounding {
                agent_id: r.agent_id,
                agent_name: r.agent_name,
                avg_score: r.avg_score,
                runs: r.runs,
            })
            .collect(),
        lowest_interactions: lowest
            .into_iter()
            .map(|r| LiveInteraction {
                run_id: r.run_id,
                message_id: r.message_id,
                chat_id: r.chat_id,
                snippet: r.snippet,
                score: r.score,
                flagged: r.flagged,
                created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
            })
            .collect(),
        draft_runs: draft.runs,
        draft_avg_score: draft.avg_score,
        draft_verdicts: VerdictMix {
            supported: draft.supported,
            contradicted: draft.contradicted,
            not_mentioned: draft.not_mentioned,
        },
        draft_by_status: draft_by_status
            .into_iter()
            .map(|r| StatusCount { status: r.status, count: r.count })
            .collect(),
        draft_series: draft_series
            .into_iter()
            .map(|r| GroundednessDay { day: r.day, avg_score: r.avg_score, runs: r.runs })
            .collect(),
        recent_runs: recent
            .into_iter()
            .map(|r| DraftRun {
                run_id: r.run_id,
                target_type: r.target_type,
                status: r.status,
                score: r.score,
                supported: r.supported,
                contradicted: r.contradicted,
                not_mentioned: r.not_mentioned,
                created_at: r.created_at.format(&Rfc3339).unwrap_or_default(),
            })
            .collect(),
    }))
}
