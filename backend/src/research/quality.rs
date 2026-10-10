// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Verification and publication share one transaction; no best-effort success.
use serde_json::{Value,json};
use uuid::Uuid;
use crate::{Result,model_routing::{CallPurpose,RoutingError,fixed::FixedRole,run::RoutedRun},groundedness::routed::VerifiedAnswer};

pub(super) async fn persist(tx:&mut sqlx::Transaction<'_,sqlx::Postgres>,run:&RoutedRun,
    message:Uuid,text:&str,title:&str,report:&Value)->Result<()> {
    let result:VerifiedAnswer=serde_json::from_value(report["verification"].clone())
        .map_err(|_|RoutingError::Invalid("research verification missing"))?;
    let raw=report["report_md"].as_str().ok_or(RoutingError::Invalid("research report missing"))?;
    let target=run.plan().fixed_assignments.get(&FixedRole::Verify).ok_or(RoutingError::Invalid("research verifier unplanned"))?;
    crate::groundedness::routed::validate_strict(&result,raw,&target.model)?;
    let chars:Vec<char>=text.chars().collect();let prefix=format!("# {title}\n\n").chars().count();
    // Current bounded reports have at most eight sources: renumbering D1..D8
    // preserves character offsets. Reject any future format that changes this.
    if chars.len()!=prefix+raw.chars().count() {return Err(RoutingError::Invalid("research verification offsets changed").into());}
    let context=run.context(CallPurpose::Verify);let id=Uuid::now_v7();
    let flagged=result.spans.len() as i32;let total=(result.total as i32).max(flagged);
    let contradicted=result.spans.iter().filter(|s|s.label=="contradicted").count() as i32;
    let not_mentioned=flagged-contradicted;
    sqlx::query("INSERT INTO verification_runs(id,target_type,target_id,mode,verifier_model,strictness,faithfulness_score,total_claims,supported,contradicted,not_mentioned,status,created_by,finished_at) VALUES($1,'message',$2,'live',$3,'strict',$4,$5,$6,$7,$8,'succeeded',$9,now())")
        .bind(id).bind(message).bind(&result.model).bind(result.score).bind(total).bind(total-flagged)
        .bind(contradicted).bind(not_mentioned).bind(context.actor.user_id).execute(&mut **tx).await?;
    let mut spans=Vec::new();
    for span in &result.spans {
        let start=span.start+prefix;let end=span.end+prefix;
        let content=chars[start..end].iter().collect::<String>();
        let range=json!({"start":start,"end":end});
        sqlx::query("INSERT INTO claim_verdicts(id,run_id,claim_text,source_span,had_citation,verdict,verifier_score) VALUES($1,$2,$3,$4,false,$5::claim_verdict,$6)")
            .bind(Uuid::now_v7()).bind(id).bind(&content).bind(range).bind(&span.label).bind(span.score).execute(&mut **tx).await?;
        spans.push(json!({"start":start,"end":end,"text":content,"label":span.label}));
    }
    let summary=json!({"method":"source_grounding_v1","score":result.score,"total":total,"flagged":flagged,
        "contradicted":contradicted,"not_mentioned":not_mentioned,"model":result.model,"spans":spans});
    sqlx::query("UPDATE messages SET groundedness=$2 WHERE id=$1").bind(message).bind(summary).execute(&mut **tx).await?;
    let mut event=crate::audit::AuditEvent::action("groundedness.verified",context.actor.role.as_str());
    event.actor_user_id=context.actor.user_id;event.resource_type=Some("message".into());event.resource_id=Some(message);
    event.payload=Some(json!({"mode":"routed_research","run_id":id,"job_id":context.run_id,"target":target,
        "routing_decision_id":run.plan().decision.id,"score":result.score,"flagged":flagged}));
    crate::audit::append_with(tx,&event).await?;Ok(())
}
