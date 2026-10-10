// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Full-corpus embedding maintenance: fixed services, one durable ledger, no judge.
use std::{sync::Arc,time::Duration,collections::{BTreeMap,BTreeSet}};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use uuid::Uuid;
use crate::{AppState,Result,auth::AuthContext,model_routing::{*,durable::{Context,Lease},run::RoutedRun,
    fixed::FixedRole,fixed_lease::FixedAllowance,transport::StreamOperation}};

#[derive(Clone,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {active_revision:i64,desired_revision:i64,old:String,new:String,kbs:Vec<Uuid>,docs:Vec<Uuid>}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {source:Source}
fn encode<T:Serialize>(value:&T)->Result<Value> {serde_json::to_value(value).map_err(|_|RoutingError::Invalid("rebuild checkpoint").into())}
fn decode<T:serde::de::DeserializeOwned>(value:Value)->Result<T> {serde_json::from_value(value).map_err(|_|RoutingError::Invalid("rebuild checkpoint").into())}

async fn source(state:&AppState,actor:&AuthContext)->Result<Source> {
    source_inner(state,actor,false).await
}
async fn source_inner(state:&AppState,actor:&AuthContext,publication:bool)->Result<Source> {
    state.rbac.require_permission(&state.pg,actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    if !publication {crate::embedding_promotion::require_ready(&state.pg).await?;}
    let row:Option<(i64,i64,String,String,i32,Option<String>)>=sqlx::query_as(
        "SELECT routing_revision,desired_routing_revision,collection_name,desired_model,desired_dim,desired_collection_name FROM embedding_index WHERE id=1 AND desired_model IS NOT NULL AND desired_dim>0")
        .fetch_optional(&state.pg).await?;
    let (active_revision,desired_revision,old,model,dim,new)=row.ok_or(RoutingError::Incompatible("no desired embedding space is pending"))?;
    let new=new.unwrap_or_else(||fixed::desired_collection(dim,&model));
    if old==new {return Err(RoutingError::Incompatible("embedding rebuild requires a separate target").into());}
    let kbs:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM knowledge_bases WHERE archived_at IS NULL ORDER BY id LIMIT 1025").fetch_all(&state.pg).await?;
    if kbs.len()>1024 {return Err(RoutingError::Incompatible("rebuild exceeds 1024 libraries").into());}
    for id in &kbs {crate::kb::require_manage(&state.pg,actor,*id).await?;}
    let docs:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM kb_documents WHERE kb_id=ANY($1) ORDER BY id LIMIT 65537").bind(&kbs).fetch_all(&state.pg).await?;
    if docs.len()>65536 {return Err(RoutingError::Incompatible("rebuild exceeds 65536 document scopes").into());}
    if !state.rbac.denied_kb_doc_ids(&state.pg,actor,&kbs).await?.is_empty() {
        return Err(RoutingError::Forbidden.into());
    }
    Ok(Source {active_revision,desired_revision,old,new,kbs,docs})
}
async fn check(state:&AppState,context:&Context,expected:&Source)->Result<()> {
    context.validate(&state.pg).await?;
    if source(state,&context.actor()).await?!=*expected {return Err(RoutingError::NeedsReplan.into());}Ok(())
}
struct Guard {state:AppState,context:Context,source:Source}
#[async_trait::async_trait]
impl run::AccessGuard for Guard {
    async fn check(&self)->Result<()> {check(&self.state,&self.context,&self.source).await}
}

pub async fn queue(state:&AppState,actor:&AuthContext)->Result<Uuid> {
    let captured=source(state,actor).await?;
    let context=RoutingContext {actor:actor.clone(),run_id:Uuid::now_v7(),chat_id:None,project_id:None,purpose:CallPurpose::Embed};
    Context::capture(&context)?.validate(&state.pg).await?;
    let run=RoutedRun::fixed_rebuild(state,context).await?;
    let id=run.plan().id;let frozen=run.freeze()?;
    check(state,&frozen.context,&captured).await?;
    let mut tx=state.pg.begin().await?;
    sqlx::query("SELECT id FROM embedding_index WHERE id=1 FOR UPDATE").execute(&mut *tx).await?;
    let busy:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE kind='reindex' AND status IN ('queued','running')) OR EXISTS(SELECT 1 FROM tasks WHERE task_type='reindex_embeddings' AND status IN ('queued','running')) OR EXISTS(SELECT 1 FROM embedding_rebuild_attempts WHERE status='built')").fetch_one(&mut *tx).await?;
    if busy {return Err(crate::AppError::Conflict("an embedding rebuild is already queued or running".into()));}
    check(state,&frozen.context,&captured).await?;
    let task=Uuid::now_v7();
    sqlx::query("INSERT INTO tasks(id,task_type,payload,max_retries) VALUES($1,'reindex_embeddings',$2,1)")
        .bind(task).bind(json!({"routed_job_id":id})).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO routed_jobs(id,task_id,actor_user_id,kind,context,plan,limits,usage,input,deadline) VALUES($1,$2,$3,'reindex',$4,$5,$6,$7,$8,$9)")
        .bind(id).bind(task).bind(actor.user_id).bind(encode(&frozen.context)?).bind(encode(&frozen.plan)?).bind(encode(&frozen.limits)?).bind(encode(&frozen.usage)?)
        .bind(encode(&Input {source:captured.clone()})?).bind(frozen.deadline).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO embedding_rebuild_attempts(task_id,desired_revision,routed_job_id) VALUES($1,$2,$3)")
        .bind(task).bind(captured.desired_revision).bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE embedding_index SET status='reindexing',reindex_done=0,reindex_total=0,error=NULL WHERE id=1").execute(&mut *tx).await?;
    let mut event=crate::audit::AuditEvent::action("routing.reindex.queued",actor.role.as_str());event.actor_user_id=actor.user_id;
    event.resource_type=Some("routed_job".into());event.resource_id=Some(id);event.payload=Some(json!({"libraries":captured.kbs.len(),"document_scopes":captured.docs.len(),"classifier_calls":0}));
    crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;Ok(task)
}

pub async fn handle_task(state:&AppState,id:Uuid,task:Uuid)->Result<()> {
    let owner=Uuid::now_v7();let started=std::time::Instant::now();
    let row:Option<(Value,Value,Value,Value,Value,i64)>=sqlx::query_as("UPDATE routed_jobs SET status='running',owner=$2,lease_until=clock_timestamp()+interval '30 seconds' WHERE id=$1 AND task_id=$3 AND kind='reindex' AND status='queued' AND deadline>clock_timestamp() RETURNING context,plan,limits,usage,input,(EXTRACT(EPOCH FROM(deadline-clock_timestamp()))*1000)::bigint")
        .bind(id).bind(owner).bind(task).fetch_optional(&state.pg).await?;
    let Some((context,plan,limits,usage,input,remaining))=row else {
        let complete:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1 AND task_id=$2 AND kind='reindex' AND status='succeeded')").bind(id).bind(task).fetch_one(&state.pg).await?;
        return if complete {Ok(())} else {Err(RoutingError::Incompatible("rebuild delivery unavailable or already consumed").into())};
    };
    let input:Input=decode(input)?;let context:Context=decode(context)?;
    let result=async {
        check(state,&context,&input.source).await?;
        let remaining=(remaining.max(0) as u64).saturating_sub(started.elapsed().as_millis() as u64);
        let run=durable::restore(state,context.clone(),decode(plan)?,decode(limits)?,decode(usage)?,remaining,Lease {pool:state.pg.clone(),id,owner})?
            .with_access(Arc::new(Guard {state:state.clone(),context:context.clone(),source:input.source.clone()}))?;
        let work=async {
            let target=fixed::resolve(state,&run.context(CallPurpose::Embed),FixedRole::EmbedDesired,run.plan().fixed_assignments.get(&FixedRole::EmbedDesired),&run.plan().constraints).await?;
            let cap=run.plan().constraints.max_calls.unwrap_or(128).min(512);
            let body=json!({"new_dim":target.embedding_dimension,"new_model":target.reference.model,"new_base_url":target.base_url,"new_api_key":target.api_key,
                "source_kbs":input.source.kbs,"source_docs":input.source.docs});
            let allowance=FixedAllowance {max_calls:cap,input_bytes:2*1024*1024,output_bytes:8*1024*1024};
            let mut stream=run.stream(state,CallPurpose::Embed,StreamOperation::Reindex,body,&BTreeMap::new(),
                [(FixedRole::EmbedDesired,allowance)].into(),&[FixedRole::Embed].into(),Duration::from_secs(1800)).await?;
            let mut built=None;
            while let Some(event)=stream.recv().await? {
                match event["type"].as_str() {
                    Some("start")|Some("progress")=>{
                        let done=event["done"].as_i64().unwrap_or(0);let total=event["total"].as_i64();
                        if !(0..=32768).contains(&done) || total.is_some_and(|n|!(0..=32768).contains(&n)) {return Err(RoutingError::Invalid("rebuild progress bound").into());}
                        sqlx::query("UPDATE embedding_index SET reindex_done=$1,reindex_total=COALESCE($2,reindex_total) WHERE id=1 AND desired_model IS NOT NULL AND desired_routing_revision=$3 AND EXISTS(SELECT 1 FROM routed_jobs WHERE id=$4 AND owner=$5 AND status='running')")
                            .bind(done as i32).bind(total.map(|v|v as i32)).bind(input.source.desired_revision).bind(id).bind(owner).execute(&state.pg).await?;
                    }
                    Some("built")=>built=Some(event),
                    Some("error")=>return Err(RoutingError::Incompatible("embedding rebuild stopped; no automatic inference replay").into()),
                    _=>{},
                }
            }
            let built=built.ok_or(RoutingError::Invalid("missing terminal rebuild receipt"))?;
            if built["new_collection"]!=input.source.new || built["old_collection"]!=input.source.old {return Err(RoutingError::NeedsReplan.into());}
            check(state,&context,&input.source).await?;
            let digest=built["source_sha256"].as_str().filter(|s|s.len()==64 && s.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                .ok_or(RoutingError::Invalid("missing rebuild corpus receipt"))?;
            let mut tx=state.pg.begin().await?;run.fence(&mut tx).await?;
            sqlx::query("UPDATE embedding_rebuild_attempts SET status='built',new_collection=$2,old_collection=$3,source_sha256=$4 WHERE task_id=$1 AND status='building'")
                .bind(task).bind(&input.source.new).bind(&input.source.old).bind(digest).execute(&mut *tx).await?;
            tx.commit().await?;Ok::<_,crate::AppError>(())
        };
        let monitor=async {
            let mut ticks=tokio::time::interval(Duration::from_secs(2));
            loop {
                ticks.tick().await;check(state,&context,&input.source).await?;
                let n=sqlx::query("UPDATE routed_jobs SET lease_until=clock_timestamp()+interval '30 seconds' WHERE id=$1 AND owner=$2 AND status='running' AND lease_until>clock_timestamp() AND deadline>clock_timestamp()")
                    .bind(id).bind(owner).execute(&state.pg).await?.rows_affected();if n!=1 {return Err::<(),crate::AppError>(RoutingError::Forbidden.into());}
            }
        };
        tokio::select! {biased;result=monitor=>{run.cancel();result},result=work=>result}
    }.await;
    if result.is_ok() {
        // Durable built receipt owns publication; losing its reply can be repaired
        // independently of the inference runtime/deadline by model-free recovery.
        return crate::embedding_promotion::promote_rebuild(state,&input.source.new,&input.source.old,input.source.desired_revision,task).await;
    }
    let failed = sqlx::query("UPDATE routed_jobs SET status='failed',owner=NULL,lease_until=NULL,updated_at=now() WHERE id=$1 AND owner=$2 AND status='running'")
        .bind(id).bind(owner).execute(&state.pg).await?.rows_affected();
    // Cancellation/lease loss already owns the terminal transition. A stale
    // handler must leave both a fresh attempt and published index untouched.
    if failed == 1 {
        sqlx::query("UPDATE embedding_rebuild_attempts SET status='failed' WHERE task_id=$1 AND status='building'").bind(task).execute(&state.pg).await?;
        sqlx::query("UPDATE embedding_index SET status='failed',error='Routed rebuild stopped; explicitly request a new attempt' WHERE id=1 AND desired_model IS NOT NULL AND desired_routing_revision=$1 AND NOT EXISTS(SELECT 1 FROM embedding_promotions WHERE status='pending') AND NOT EXISTS(SELECT 1 FROM routed_jobs newer JOIN routed_jobs original ON original.id=$2 WHERE newer.kind='reindex' AND newer.id<>original.id AND newer.created_at>=original.created_at)")
            .bind(input.source.desired_revision).bind(id).execute(&state.pg).await?;
    }
    result
}

/// Recheck the original routed principal/scope at the model-free publication
/// boundary. Legacy non-routed maintenance retains its existing contract.
pub(crate) async fn publication_authorised(state:&AppState,job:Option<Uuid>,new:&str,revision:i64,digest:Option<&str>)->Result<bool> {
    let Some(job)=job else {return Ok(true);};
    let row:Option<(Value,Value,Option<String>,String,String)>=sqlx::query_as("SELECT j.context,j.input,a.source_sha256,j.status,a.status FROM embedding_rebuild_attempts a JOIN routed_jobs j ON j.id=a.routed_job_id WHERE j.id=$1 AND j.kind='reindex' AND a.new_collection=$2 AND a.desired_revision=$3")
        .bind(job).bind(new).bind(revision).fetch_optional(&state.pg).await?;
    let Some((context,input,stored,status,attempt))=row else {return Ok(false);};
    if attempt!="built" || digest.is_none() || stored.as_deref()!=digest || !matches!(status.as_str(),"running"|"interrupted") {return Ok(false);}
    let context:Context=decode(context)?;let input:Input=decode(input)?;
    context.validate(&state.pg).await?;
    Ok(source_inner(state,&context.actor(),true).await?==input.source)
}

/// Recovery settles already-built work only; never restores an inference phase.
pub(crate) async fn recover(state:&AppState)->Result<()> {
    let rows:Vec<(Uuid,String,String,i64)>=sqlx::query_as("SELECT a.task_id,a.new_collection,a.old_collection,a.desired_revision FROM embedding_rebuild_attempts a JOIN routed_jobs j ON j.id=a.routed_job_id WHERE a.status='built' AND j.status IN ('running','interrupted')")
        .fetch_all(&state.pg).await?;
    for (task,new,old,revision) in rows {crate::embedding_promotion::promote_rebuild(state,&new,&old,revision,task).await?;}
    Ok(())
}
