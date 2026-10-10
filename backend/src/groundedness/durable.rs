// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Durable verify/repair jobs. A started phase is never automatically replayed.
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use uuid::Uuid;
use crate::{AppState,Result,auth::AuthContext,model_routing::{*,run::RoutedRun,durable::{Context,Lease}}};
use super::routed_draft::{self,Source};

#[derive(Serialize,Deserialize)]
struct Input {source:Source,repair:bool,#[serde(default)] public_launch:bool}

/// Internal opt-in handoff. Consumes exclusive ownership of the original run;
/// caller clones or outstanding reservations are rejected, never reset.
/// Public model selection remains gated pending the other routing entry points.
pub async fn enqueue(state:&AppState,run:RoutedRun,doc:Uuid,repair:bool)->Result<Uuid> {
    enqueue_inner(state,run,doc,repair,false,None).await
}
pub(super) async fn enqueue_public(state:&AppState,run:RoutedRun,doc:Uuid,repair:bool,source:Source)->Result<Uuid> {
    super::jobs::require_enabled(state).await?;
    enqueue_inner(state,run,doc,repair,true,Some(source)).await
}
async fn enqueue_inner(state:&AppState,run:RoutedRun,doc:Uuid,repair:bool,public_launch:bool,expected:Option<Source>)->Result<Uuid> {
    let kind=expected.as_ref().map(|s|s.kind).unwrap_or_default();
    let source=routed_draft::source_kind(state,&run,doc,kind,repair).await?;
    if expected.is_some_and(|s|s!=source) {return Err(RoutingError::NeedsReplan.into());}
    if repair && !crate::config::runtime::get(&state.pg,"groundedness.repair").await?.is_some_and(|v|v.value=="true") {
        return Err(RoutingError::Forbidden.into());
    }
    let frozen=run.freeze()?;
    frozen.context.validate(&state.pg).await?;
    let id=frozen.plan.id;let task=Uuid::now_v7();
    let uid=frozen.context.actor().user_id.ok_or(RoutingError::Forbidden)?;
    let mut tx=state.pg.begin().await?;
    sqlx::query("INSERT INTO tasks(id,task_type,payload,max_retries) VALUES($1,'verify_draft',$2,1)")
        .bind(task).bind(json!({"routed_job_id":id})).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO routed_jobs(id,task_id,actor_user_id,kind,context,plan,limits,usage,input,deadline) VALUES($1,$2,$3,'verify_draft',$4,$5,$6,$7,$8,$9)")
        .bind(id).bind(task).bind(uid).bind(encode(&frozen.context)?).bind(encode(&frozen.plan)?)
        .bind(encode(&frozen.limits)?).bind(encode(&frozen.usage)?).bind(encode(&Input {source,repair,public_launch})?)
        .bind(frozen.deadline).execute(&mut *tx).await?;
    let mut event=crate::audit::AuditEvent::action("routing.job.queued",frozen.context.actor().role.as_str());
    event.actor_user_id=Some(uid);event.resource_type=Some("routed_job".into());event.resource_id=Some(id);
    event.payload=Some(json!({"decision_id":frozen.plan.decision.id,"target_type":kind,"target_id":doc,
        "document_id":if kind==routed_draft::TargetKind::Document {Some(doc)} else {None},"repair":repair}));
    crate::audit::append_with(&mut tx,&event).await?;
    tx.commit().await?;Ok(id)
}
fn encode<T:Serialize>(value:&T)->Result<Value> {serde_json::to_value(value).map_err(|_|RoutingError::Invalid("durable job checkpoint").into())}
fn decode<T:serde::de::DeserializeOwned>(value:Value)->Result<T> {serde_json::from_value(value).map_err(|_|RoutingError::Invalid("durable job checkpoint").into())}

/// Explicit continuation of a confirmed verify-only result. The task changes;
/// the original actor, plan, accounting and absolute deadline do not.
pub async fn enqueue_repair(state:&AppState,actor:&AuthContext,id:Uuid)->Result<()> {
    if actor.mfa_enroll_only || actor.break_glass {return Err(RoutingError::Forbidden.into());}
    super::jobs::require_enabled(state).await?;
    let uid=actor.user_id.ok_or(RoutingError::Forbidden)?;
    let mut tx=state.pg.begin().await?;
    let row:Option<(Uuid,String,String,Value,Value,Value,Value,Option<Value>,i64)>=sqlx::query_as(
        "SELECT task_id,status,phase,context,input,limits,usage,verified_checkpoint,(EXTRACT(EPOCH FROM (deadline-clock_timestamp()))*1000)::bigint FROM routed_jobs WHERE id=$1 AND actor_user_id=$2 AND kind='verify_draft' FOR UPDATE")
        .bind(id).bind(uid).fetch_optional(&mut *tx).await?;
    let (old_task,status,phase,context,input,limits,usage,checkpoint,remaining)=row.ok_or(RoutingError::Forbidden)?;
    let context:Context=decode(context)?;context.validate(&state.pg).await?;
    if context.actor().role!=actor.role {return Err(RoutingError::Forbidden.into());}
    let mut input:Input=decode(input)?;
    if input.source.kind!=routed_draft::TargetKind::Document {return Err(RoutingError::Incompatible("artefact repair requires a workspace document").into());}
    if !input.public_launch {return Err(RoutingError::Incompatible("repair continuation requires a public document job").into());}
    super::jobs::authorize_result(state,actor,id,input.source.doc).await?;
    if phase=="repair" && matches!(status.as_str(),"queued"|"running"|"succeeded") {return Ok(());}
    if phase!="verify" || status!="succeeded" || input.repair || checkpoint.is_none() {
        return Err(RoutingError::Incompatible("repair requires a completed verify-only job; stopped phases cannot be replayed").into());
    }
    // Validate the remaining ledger now; the worker restores it again at claim.
    budget::RunBudget::restore(decode(limits)?,decode(usage)?,remaining.max(0) as u64)?;
    if !crate::config::runtime::get(&state.pg,"groundedness.repair").await?.is_some_and(|v|v.value=="true") {
        return Err(RoutingError::Forbidden.into());
    }
    let opts=routed_draft::options(state).await?;
    if checkpoint.as_ref().and_then(|v|v["strictness"].as_str())!=Some(opts.strictness)
        || routed_draft::snapshot(state,&context.routing(),input.source.doc,true).await?!=input.source {
        return Err(RoutingError::NeedsReplan.into());
    }
    let filename:String=sqlx::query_scalar("SELECT original_filename FROM documents WHERE id=$1").bind(input.source.doc).fetch_one(&state.pg).await?;
    if !filename.to_ascii_lowercase().ends_with(".docx") {return Err(RoutingError::Incompatible("tracked repair needs a DOCX document").into());}
    let task=Uuid::now_v7();input.repair=true;
    sqlx::query("UPDATE tasks SET status='succeeded',finished_at=now(),last_error=NULL WHERE id=$1").bind(old_task).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO tasks(id,task_type,payload,max_retries) VALUES($1,'verify_draft',$2,1)")
        .bind(task).bind(json!({"routed_job_id":id})).execute(&mut *tx).await?;
    sqlx::query("UPDATE routed_jobs SET phase='repair',status='queued',task_id=$2,input=$3,updated_at=now() WHERE id=$1")
        .bind(id).bind(task).bind(encode(&input)?).execute(&mut *tx).await?;
    let mut event=crate::audit::AuditEvent::action("routing.job.repair_queued",actor.role.as_str());
    event.actor_user_id=Some(uid);event.resource_type=Some("routed_job".into());event.resource_id=Some(id);
    event.payload=Some(json!({"previous_task_id":old_task,"task_id":task}));
    crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;Ok(())
}

/// Cancels pending/running work without granting any additional inference rights.
pub async fn cancel(state:&AppState,actor:&AuthContext,id:Uuid)->Result<()> {
    if actor.mfa_enroll_only {return Err(RoutingError::Forbidden.into());}
    let uid=actor.user_id.ok_or(RoutingError::Forbidden)?;
    let mut tx=state.pg.begin().await?;
    let task:Option<Uuid>=sqlx::query_scalar("UPDATE routed_jobs SET status='cancelled',owner=NULL,lease_until=NULL,updated_at=now() WHERE id=$1 AND actor_user_id=$2 AND status IN ('queued','running','awaiting_input') RETURNING task_id")
        .bind(id).bind(uid).fetch_optional(&mut *tx).await?;
    if let Some(task)=task {
        sqlx::query("UPDATE tasks SET status='dead_letter',finished_at=now(),last_error='routed job cancelled' WHERE id=$1 AND status IN ('queued','running')")
            .bind(task).execute(&mut *tx).await?;
        let mut event=crate::audit::AuditEvent::action("routing.job.cancelled",actor.role.as_str());
        event.actor_user_id=Some(uid);event.resource_type=Some("routed_job".into());event.resource_id=Some(id);
        crate::audit::append_with(&mut tx,&event).await?;
    } else {
        let own:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1 AND actor_user_id=$2)").bind(id).bind(uid).fetch_one(&mut *tx).await?;
        if !own {return Err(RoutingError::Forbidden.into());}
    }
    tx.commit().await?;Ok(())
}

/// Called by the existing scheduler before claiming tasks. Time spent waiting
/// still counts; expired work never gets a new deadline or budget.
pub async fn recover(state:&AppState)->Result<u64> {
    let mut tx=state.pg.begin().await?;
    let tasks:Vec<Uuid>=sqlx::query_scalar("UPDATE routed_jobs SET status=CASE WHEN status='running' THEN 'interrupted' ELSE 'failed' END,owner=NULL,lease_until=NULL,updated_at=now() WHERE (status='running' AND (lease_until<=clock_timestamp() OR deadline<=clock_timestamp())) OR (status IN ('queued','awaiting_input') AND deadline<=clock_timestamp()) RETURNING task_id")
        .fetch_all(&mut *tx).await?;
    if !tasks.is_empty() {
        sqlx::query("UPDATE tasks SET status='dead_letter',finished_at=now(),last_error='routed job expired or interrupted; automatic replay is disabled' WHERE id=ANY($1) AND status IN ('queued','running')")
            .bind(&tasks).execute(&mut *tx).await?;
    }
    // A scheduler may die after claiming its batch but before entering this
    // handler. The job is still queued, so no inference reservation was sent.
    sqlx::query("UPDATE tasks t SET status='queued',started_at=NULL,next_attempt_at=now() FROM routed_jobs j WHERE j.task_id=t.id AND j.status='queued' AND t.status='running' AND t.started_at<clock_timestamp()-interval '30 seconds'")
        .execute(&mut *tx).await?;
    sqlx::query("UPDATE tasks t SET status=CASE WHEN j.status IN ('succeeded','awaiting_input') THEN 'succeeded'::task_status ELSE 'dead_letter'::task_status END,finished_at=now(),last_error=CASE WHEN j.status IN ('succeeded','awaiting_input') THEN NULL ELSE 'routed job stopped; automatic replay is disabled' END FROM routed_jobs j WHERE j.task_id=t.id AND j.status IN ('succeeded','awaiting_input','failed','cancelled','interrupted') AND t.status IN ('queued','running')")
        .execute(&mut *tx).await?;
    tx.commit().await?;crate::workflows::routing::recover(state).await?;crate::model_routing::ingestion::recover(state).await?;Ok(tasks.len() as u64)
}

/// Queue status follows the fenced job, not whichever duplicate handler returns
/// first. A still-running owner must not be marked succeeded/dead by a duplicate.
pub(crate) async fn sync_task(state:&AppState,task:Uuid,id:Uuid)->Result<()> {
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1 AND task_id=$2)")
        .bind(id).bind(task).fetch_one(&state.pg).await?;
    if !exists {
        let completed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tasks WHERE id=$1 AND status='succeeded' AND payload->>'routed_job_id'=$2)")
            .bind(task).bind(id.to_string()).fetch_one(&state.pg).await?;
        return if completed {Ok(())} else {Err(RoutingError::Invalid("routed task identity mismatch").into())};
    }
    sqlx::query("UPDATE tasks t SET status=CASE WHEN j.status IN ('succeeded','awaiting_input') THEN 'succeeded'::task_status ELSE 'dead_letter'::task_status END,finished_at=now(),last_error=CASE WHEN j.status IN ('succeeded','awaiting_input') THEN NULL ELSE 'routed job stopped; automatic replay is disabled' END FROM routed_jobs j WHERE j.id=$1 AND j.task_id=t.id AND t.id=$2 AND j.status IN ('succeeded','awaiting_input','failed','cancelled','interrupted')")
        .bind(id).bind(task).execute(&state.pg).await?;Ok(())
}

/// Worker entry point: reconstruct only from the server checkpoint, never from
/// client-supplied actor/plan/usage in a task payload. Duplicate delivery is inert.
pub async fn handle(state:&AppState,id:Uuid)->Result<()> {
    let task:Uuid=sqlx::query_scalar("SELECT task_id FROM routed_jobs WHERE id=$1 AND kind='verify_draft'").bind(id).fetch_one(&state.pg).await?;
    handle_task(state,id,task).await
}
/// Scheduler must pass the actual delivery identity; an old verify task cannot
/// claim a later repair phase even though both share the same job ID.
pub async fn handle_task(state:&AppState,id:Uuid,task:Uuid)->Result<()> {
    let claimed_at=std::time::Instant::now();
    let owner=Uuid::now_v7();
    let row:Option<(Value,Value,Value,Value,Value,i64,String,Option<Value>)>=sqlx::query_as("UPDATE routed_jobs SET status='running',owner=$2,lease_until=clock_timestamp()+interval '30 seconds',updated_at=now() WHERE id=$1 AND task_id=$3 AND kind='verify_draft' AND status='queued' AND deadline>clock_timestamp() RETURNING context,plan,limits,usage,input,(EXTRACT(EPOCH FROM (deadline-clock_timestamp()))*1000)::bigint,phase,verified_checkpoint")
        .bind(id).bind(owner).bind(task).fetch_optional(&state.pg).await?;
    let Some((context,plan,limits,usage,input,remaining,phase,checkpoint))=row else {
        let status:Option<String>=sqlx::query_scalar("SELECT status FROM routed_jobs WHERE id=$1").bind(id).fetch_optional(&state.pg).await?;
        return if status.as_deref()==Some("succeeded") {Ok(())} else {Err(RoutingError::Incompatible("routed job is unavailable or already claimed").into())};
    };
    let result=Box::pin(async {
        let context:Context=decode(context)?;
        context.validate(&state.pg).await?;
        let input:Input=decode(input)?;
        if input.public_launch {super::jobs::require_enabled(state).await?;}
        let remaining=(remaining.max(0) as u64).saturating_sub(claimed_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        let run=crate::model_routing::durable::restore(state,context,decode(plan)?,decode(limits)?,decode(usage)?,remaining,
            Lease {pool:state.pg.clone(),id,owner})?;
        let work=Box::pin(async {
            if routed_draft::source_kind(state,&run,input.source.doc,input.source.kind,input.repair).await?!=input.source {return Err(RoutingError::NeedsReplan.into());}
            if phase=="repair" {
                let verified=routed_draft::restore_verified(state,&run,checkpoint.ok_or(RoutingError::Invalid("missing verified checkpoint"))?).await?;
                routed_draft::repair_document(state,&run,&verified).await?;
                return Ok(verified.id);
            }
            routed_draft::execute_target(state,&run,input.source.doc,input.source.kind,input.repair).await
        });
        let monitor=Box::pin(async {
            let mut ticks=tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                ticks.tick().await;
                if input.public_launch {super::jobs::require_enabled(state).await?;}
                Context::capture(&run.context(CallPurpose::Document))?.validate(&state.pg).await?;
                let n=sqlx::query("UPDATE routed_jobs SET lease_until=clock_timestamp()+interval '30 seconds' WHERE id=$1 AND owner=$2 AND status='running' AND deadline>clock_timestamp() AND lease_until>clock_timestamp()")
                    .bind(id).bind(owner).execute(&state.pg).await?.rows_affected();
                if n!=1 {return Err::<Uuid,crate::AppError>(crate::AppError::Unavailable("durable worker lost its lease".into()));}
            }
        });
        tokio::select! {biased; result=monitor=>{run.cancel();result},result=work=>result}
    }).await;
    // Before/after-dispatch checkpoints already contain charged work. If this
    // final write fails, recovery stops the job; it cannot repeat the effects.
    let status=if result.is_ok() {"succeeded"} else {"failed"};
    let mut tx=state.pg.begin().await?;
    let n=sqlx::query("UPDATE routed_jobs SET status=$3,owner=NULL,lease_until=NULL,result_id=COALESCE($4,(SELECT id FROM verification_runs WHERE id=$1)),updated_at=now() WHERE id=$1 AND owner=$2 AND status='running'")
        .bind(id).bind(owner).bind(status).bind(result.as_ref().ok().copied()).execute(&mut *tx).await?.rows_affected();
    if n==1 {
        let mut event=crate::audit::AuditEvent::action(&format!("routing.job.{status}"),"system");
        event.resource_type=Some("routed_job".into());event.resource_id=Some(id);
        event.payload=Some(json!({"result_id":result.as_ref().ok()}));
        crate::audit::append_with(&mut tx,&event).await?;
    }
    tx.commit().await?;
    if n!=1 {return Err(crate::AppError::Unavailable("durable completion lost its lease".into()));}
    result.map(|_|())
}
