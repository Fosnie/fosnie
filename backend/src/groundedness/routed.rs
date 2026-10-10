// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Strict live verifier executor. Product callers must retain source provenance
//! and persist/publish only its successful result; this is not a job-resume API.
use crate::{
    model_routing::{
        fixed::FixedRole, fixed_lease::FixedAllowance, run::RoutedRun, transport::JsonOperation,
        CallPurpose, RoutingError,
    },
    AppState, Result,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub label: String,
    pub score: f64,
}
#[derive(Debug, Deserialize)]
pub struct VerifiedAnswer {
    pub spans: Vec<Span>,
    pub score: f64,
    pub total: usize,
    pub flagged: usize,
    pub model: String,
}

#[derive(PartialEq)]
pub(super) struct Options {
    pub(super) strictness: &'static str,
    pub(super) threshold: f64,
}
pub(super) async fn options(state: &AppState) -> Result<Options> {
    let read = |key: &'static str| crate::config::runtime::get(&state.pg, key);
    let strictness = match read("groundedness.strict")
        .await?
        .as_ref()
        .map(|e| e.value.as_str())
    {
        None | Some("true") => "strict",
        Some("false") => "lenient",
        _ => return Err(RoutingError::Invalid("invalid groundedness strictness").into()),
    };
    let threshold = match read("groundedness.threshold").await? {
        None => 0.0,
        Some(v) => v
            .value
            .parse::<f64>()
            .map_err(|_| RoutingError::Invalid("invalid groundedness threshold"))?,
    };
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(RoutingError::Invalid("invalid groundedness threshold").into());
    }
    match read("groundedness.hhem_filter")
        .await?
        .as_ref()
        .map(|e| e.value.as_str())
    {
        None | Some("false") => {}
        _ => {
            return Err(RoutingError::Incompatible(
                "routed HHEM second opinion requires separate adoption",
            )
            .into())
        }
    }
    Ok(Options {
        strictness,
        threshold,
    })
}

pub(crate) async fn validate_options(state: &AppState) -> Result<()> {
    options(state).await.map(|_| ())
}

/// Snapshot of the actual persisted answer and the evidence supplied to its LLM.
pub struct LiveCheck {
    pub message_id: Uuid,
    pub content: String,
    pub question: String,
    pub evidence: String,
    pub kbs: Vec<Uuid>,
    pub denied: Vec<Uuid>,
}

/// A successful commit is the verification's durable completion. Socket delivery
/// is best effort after that commit; reconnecting clients read the same summary.
pub async fn verify_message(state: &AppState, run: &RoutedRun, check: &LiveCheck) -> Result<()> {
    run.guard(Box::pin(async {
        let ctx=run.context(CallPurpose::Verify);
        let uid=ctx.actor.user_id.ok_or(RoutingError::Forbidden)?;
        crate::chat::routing::validate_live_grounding(state,run).await?;
        sources(state,run,&check.kbs,&check.denied).await?;
        let sql="SELECT groundedness FROM messages WHERE id=$1 AND chat_id=$2 AND turn_id=$3 AND role='assistant' AND content=$4 AND completed_at IS NOT NULL AND interrupted_at IS NULL AND routing_decision->>'id'=$5";
        let existing:Option<(Option<serde_json::Value>,)>=sqlx::query_as(sql).bind(check.message_id).bind(ctx.chat_id).bind(ctx.run_id)
            .bind(&check.content).bind(run.plan().decision.id.to_string()).fetch_optional(&state.pg).await?;
        let Some((existing,))=existing else {return Err(RoutingError::NeedsReplan.into());};
        if existing.is_some() {return Ok(());}
        let opts=options(state).await?;
        let answer=crate::chat::strip_think(&check.content);
        let result=Box::pin(verify(state,run,&check.kbs,&check.denied,&check.evidence,&check.question,&answer)).await?;
        if options(state).await?!=opts {return Err(RoutingError::NeedsReplan.into());}
        crate::chat::routing::validate_live_grounding(state,run).await?;
        sources(state,run,&check.kbs,&check.denied).await?;
        let mut tx=state.pg.begin().await?;
        // Row lock arbitrates duplicate callbacks and prevents a stale answer
        // snapshot from overwriting a later edit/regeneration.
        let row:Option<(Option<serde_json::Value>,)>=sqlx::query_as("SELECT groundedness FROM messages WHERE id=$1 AND chat_id=$2 AND turn_id=$3 AND role='assistant' AND content=$4 AND completed_at IS NOT NULL AND interrupted_at IS NULL AND routing_decision->>'id'=$5 FOR UPDATE").bind(check.message_id).bind(ctx.chat_id).bind(ctx.run_id)
            .bind(&check.content).bind(run.plan().decision.id.to_string()).fetch_optional(&mut *tx).await?;
        let Some((existing,))=row else {return Err(RoutingError::NeedsReplan.into());};
        if existing.is_some() {return Ok(());}
        let id=Uuid::now_v7();let total=result.total as i32;let flagged=result.flagged as i32;
        let contradicted=result.spans.iter().filter(|s|s.label=="contradicted").count() as i32;
        let supported=(total-flagged).max(0);let not_mentioned=flagged-contradicted;
        let spans:Vec<_>=result.spans.iter().map(|s|json!({"start":s.start,"end":s.end,"text":s.text,"label":s.label})).collect();
        let summary=json!({"score":result.score,"total":total,"flagged":flagged,"contradicted":contradicted,
            "not_mentioned":not_mentioned,"model":result.model,"spans":spans});
        sqlx::query("INSERT INTO verification_runs(id,target_type,target_id,mode,verifier_model,strictness,faithfulness_score,total_claims,supported,contradicted,not_mentioned,status,created_by,finished_at) VALUES($1,'message',$2,'live',$3,$4,$5,$6,$7,$8,$9,'succeeded',$10,now())")
            .bind(id).bind(check.message_id).bind(&result.model).bind(opts.strictness).bind(result.score).bind(total).bind(supported).bind(contradicted).bind(not_mentioned).bind(uid).execute(&mut *tx).await?;
        if !result.spans.is_empty() {
            let ids:Vec<_>=result.spans.iter().map(|_|Uuid::now_v7()).collect();
            let texts:Vec<_>=result.spans.iter().map(|s|s.text.clone()).collect();
            let ranges:Vec<_>=result.spans.iter().map(|s|json!({"start":s.start,"end":s.end}).to_string()).collect();
            let labels:Vec<_>=result.spans.iter().map(|s|s.label.clone()).collect();
            let scores:Vec<_>=result.spans.iter().map(|s|s.score).collect();
            sqlx::query("INSERT INTO claim_verdicts(id,run_id,claim_text,source_span,had_citation,verdict,verifier_score) SELECT id,$2,claim_text,span::jsonb,false,verdict::claim_verdict,score FROM UNNEST($1::uuid[],$3::text[],$4::text[],$5::text[],$6::float8[]) AS t(id,claim_text,span,verdict,score)")
                .bind(ids).bind(id).bind(texts).bind(ranges).bind(labels).bind(scores).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE messages SET groundedness=$2 WHERE id=$1").bind(check.message_id).bind(summary).execute(&mut *tx).await?;
        let mut event=crate::audit::AuditEvent::action("groundedness.verified",ctx.actor.role.as_str());
        event.actor_user_id=Some(uid);event.resource_type=Some("message".into());event.resource_id=Some(check.message_id);
        let usage=run.usage();
        event.payload=Some(json!({"chat_id":ctx.chat_id,"run_id":id,"score":result.score,"total":total,"supported":supported,
            "contradicted":contradicted,"not_mentioned":not_mentioned,"model":result.model,"routing_decision_id":run.plan().decision.id,
            "target":run.plan().fixed_assignments[&FixedRole::Verify],"routing_usage_snapshot":{"calls":usage.calls,"tokens":usage.tokens,
            "unknown_token_calls":usage.unknown_token_calls,"unknown_cost_calls":usage.unknown_cost_calls,"cost_microusd":usage.cost_microusd},"resumable":false}));
        crate::audit::append_with(&mut tx,&event).await?;
        tx.commit().await?;
        state.hub.send_to_user(uid,crate::ws::protocol::ServerFrame::ChatGroundedness {
            turn_id:ctx.run_id,message_id:check.message_id,score:Some(result.score),total,flagged,
            spans:result.spans.into_iter().map(|s|crate::ws::protocol::GroundSpanOut {start:s.start as i32,end:s.end as i32,text:s.text,label:s.label}).collect(),
        });
        Ok(())
    })).await
}

async fn sources(state: &AppState, run: &RoutedRun, kbs: &[Uuid], denied: &[Uuid]) -> Result<()> {
    let (chat, _) = crate::chat::routing::authorize(state, run).await?;
    let ctx = run.context(CallPurpose::Verify);
    let agent: Option<Uuid> = sqlx::query_scalar("SELECT agent_id FROM chats WHERE id=$1")
        .bind(chat)
        .fetch_one(&state.pg)
        .await?;
    let allowed =
        crate::kb::retrieval_allowlist(&state.pg, &ctx.actor, chat, ctx.project_id, agent).await?;
    if kbs.is_empty() || kbs.len() > 64 || kbs.iter().any(|kb| !allowed.contains(kb)) {
        return Err(RoutingError::Forbidden.into());
    }
    if state
        .rbac
        .denied_kb_doc_ids(&state.pg, &ctx.actor, kbs)
        .await?
        .iter()
        .any(|id| !denied.contains(id))
    {
        return Err(RoutingError::NeedsReplan.into());
    }
    Ok(())
}

pub(crate) fn validate_strict(result:&VerifiedAnswer,answer:&str,model:&str)->Result<()> {
    validate(result,answer,model,&Options {strictness:"strict",threshold:0.0})
}
fn validate(result: &VerifiedAnswer, answer: &str, model: &str, opts: &Options) -> Result<()> {
    let chars: Vec<char> = answer.chars().collect();
    if result.model != model
        || !result.score.is_finite()
        || !(0.0..=1.0).contains(&result.score)
        || result.total == 0
        || result.total > chars.len()
        || result.spans.len() > 1024
        || result.flagged != result.spans.len()
    {
        return Err(RoutingError::Invalid("invalid routed verification result").into());
    }
    let mut end = 0;
    let mut flagged = 0;
    for span in &result.spans {
        if span.start < end
            || span.start >= span.end
            || span.end > chars.len()
            || !span.score.is_finite()
            || !(opts.threshold..=1.0).contains(&span.score)
            || !matches!(span.label.as_str(), "contradicted" | "not_mentioned")
            || (opts.strictness == "lenient" && span.label != "contradicted")
            || chars[span.start..span.end].iter().collect::<String>() != span.text
        {
            return Err(RoutingError::Invalid("invalid routed verification span").into());
        }
        end = span.end;
        flagged += span.end - span.start;
    }
    if (result.score - (1.0 - flagged as f64 / chars.len() as f64)).abs() > 1e-9 {
        return Err(RoutingError::Invalid("verification score contradicts spans").into());
    }
    Ok(())
}

/// `context` must be the already-filtered retrieval evidence from `kbs`, with
/// `denied` captured at retrieval. Rechecks protect delayed/background callers.
pub async fn verify(
    state: &AppState,
    run: &RoutedRun,
    kbs: &[Uuid],
    denied: &[Uuid],
    context: &str,
    question: &str,
    answer: &str,
) -> Result<VerifiedAnswer> {
    let timeout = std::time::Duration::from_secs(30);
    tokio::time::timeout(
        timeout,
        run.guard(Box::pin(async {
            for (text, cap) in [(context, 1024 * 1024), (question, 65536), (answer, 262144)] {
                if text.trim().is_empty() || text.len() > cap {
                    return Err(
                        RoutingError::Invalid("empty or oversized verification input").into(),
                    );
                }
            }
            sources(state, run, kbs, denied).await?;
            let opts = options(state).await?;
            let model = &run
                .plan()
                .fixed_assignments
                .get(&FixedRole::Verify)
                .ok_or(RoutingError::NeedsReplan)?
                .model;
            let value = run
                .json(
                    state,
                    CallPurpose::Verify,
                    JsonOperation::Verify,
                    json!({"context":context,"question":question,"answer":answer,
            "strictness":opts.strictness,"threshold":opts.threshold,"hhem_filter":false}),
                    &Default::default(),
                    [(
                        FixedRole::Verify,
                        FixedAllowance {
                            max_calls: 1,
                            input_bytes: 8 * 1024 * 1024,
                            output_bytes: 2 * 1024 * 1024,
                        },
                    )]
                    .into(),
                    &Default::default(),
                    timeout,
                )
                .await?;
            // A nonempty successful verification requires exactly one completed attempt.
            let calls = value["fixed_routing_report"]["calls"]
                .as_array()
                .ok_or(RoutingError::Invalid("missing verification calls"))?;
            if calls.len() != 1 || calls[0]["status"] != "completed" {
                return Err(RoutingError::Invalid("verification did not complete").into());
            }
            let result: VerifiedAnswer = serde_json::from_value(value)
                .map_err(|_| RoutingError::Invalid("invalid verification result"))?;
            validate(&result, answer, model, &opts)?;
            sources(state, run, kbs, denied).await?;
            if options(state).await? != opts {
                return Err(RoutingError::NeedsReplan.into());
            }
            Ok(result)
        })),
    )
    .await
    .map_err(|_| crate::AppError::Unavailable("routed verification timeout".into()))?
}
