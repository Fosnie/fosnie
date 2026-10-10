// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Opt-in uploaded-document ingestion with a fenced, non-replayable ledger.
use std::{collections::BTreeMap,time::Duration,sync::Arc};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use uuid::Uuid;
use crate::{AppState,Result,auth::AuthContext,model_routing::{*,run::RoutedRun,
    durable::{Context,Lease},dispatch::StageRequest,fixed::FixedRole,fixed_lease::FixedAllowance,
    transport::JsonOperation}};

pub const ENABLED_KEY:&str="routing.ingestion_enabled";
pub const CONNECTOR_ENABLED_KEY:&str="routing.connector_ingestion_enabled";
pub const MAX_DOCUMENT_BYTES:usize=4*1024*1024;

pub async fn enabled(state:&AppState)->Result<bool> {
    gate(state,ENABLED_KEY).await
}
pub async fn connector_enabled(state:&AppState)->Result<bool> {gate(state,CONNECTOR_ENABLED_KEY).await}
async fn gate(state:&AppState,key:&str)->Result<bool> {
    use crate::config::runtime::{self,ConfigValueType};
    match runtime::get(&state.pg,key).await? {
        None=>Ok(false),Some(v) if matches!(v.value_type,ConfigValueType::Bool)=>match v.value.as_str() {
            "true"=>Ok(true),"false"=>Ok(false),_=>Err(RoutingError::Invalid("invalid ingestion routing gate").into())},
        Some(_)=>Err(RoutingError::Invalid("ingestion routing gate must be boolean").into()),
    }
}
async fn require_enabled(state:&AppState,origin:&str)->Result<()> {
    let enabled=if origin=="connector_import" {connector_enabled(state).await?} else {enabled(state).await?};
    if !enabled {return Err(RoutingError::Incompatible("routed ingestion is disabled").into());} Ok(())
}

#[derive(Clone,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {doc:Uuid,kb:Uuid,path:String,mime:Option<String>,filename:String,created_by:Option<Uuid>,
    dimension:i32,project:Option<Uuid>,parent_child:bool,sha256:String,
    #[serde(default="upload_origin")] origin:String,#[serde(default)] connector_witness:Option<String>,#[serde(default)] index_revision:Option<i64>}
fn upload_origin()->String {"upload".into()}
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {source:Source,snapshot:String,public_launch:bool,#[serde(default)] system_launch:bool}
fn encode<T:Serialize>(v:&T)->Result<Value> {serde_json::to_value(v).map_err(|_|RoutingError::Invalid("ingestion checkpoint").into())}
fn decode<T:serde::de::DeserializeOwned>(v:Value)->Result<T> {serde_json::from_value(v).map_err(|_|RoutingError::Invalid("ingestion checkpoint").into())}

async fn source(state:&AppState,actor:&AuthContext,doc:Uuid)->Result<(Source,Vec<u8>)> {
    let row:Option<(Uuid,String,Option<String>,String,Option<Uuid>,i32,Option<Uuid>,bool,String)>=sqlx::query_as(
        "SELECT d.kb_id,d.bytes_path,d.mime,d.original_filename,d.created_by,k.embedding_dimension,k.origin_project_id,k.parent_child,d.source FROM kb_documents d JOIN knowledge_bases k ON k.id=d.kb_id AND k.archived_at IS NULL WHERE d.id=$1")
        .bind(doc).fetch_optional(&state.pg).await?;
    let (kb,path,mime,filename,created_by,dimension,project,parent_child,origin)=row.ok_or(RoutingError::Forbidden)?;
    if !matches!(origin.as_str(),"upload"|"connector_import") || dimension<=0 {return Err(RoutingError::Incompatible("routed ingestion requires a supported document and active index").into());}
    crate::kb::require_manage(&state.pg,actor,kb).await?;
    if let Some(id)=project {state.rbac.require(&state.pg,actor,crate::auth::rbac::ResourceType::Project,id,crate::auth::rbac::Permission::Write).await?;}
    let connector_witness=if origin=="connector_import" {
        Some(state.rbac.connector_ingestion_witness(&state.pg,actor,doc).await?)
    } else {None};
    let abs=crate::storage::resolve_file(&state.boot.storage.documents_dir,&path);
    let abs=crate::upload::ensure_within_storage(&state.boot.storage.documents_dir,&abs.to_string_lossy())?;
    if tokio::fs::metadata(&abs).await.map_err(|_|RoutingError::Forbidden)?.len()>MAX_DOCUMENT_BYTES as u64 {
        return Err(RoutingError::Incompatible("routed ingestion document exceeds 4 MiB").into());
    }
    let bytes=tokio::fs::read(abs).await.map_err(|_|RoutingError::Forbidden)?;
    if bytes.is_empty() || bytes.len()>MAX_DOCUMENT_BYTES {return Err(RoutingError::Incompatible("invalid bounded ingestion document").into());}
    let sha256=hex::encode(Sha256::digest(&bytes));
    let index_revision:Option<i64>=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_optional(&state.pg).await?;
    Ok((Source {doc,kb,path,mime,filename,created_by,dimension,project,parent_child,sha256,origin,connector_witness,index_revision},bytes))
}
async fn check(state:&AppState,context:&Context,expected:&Source)->Result<()> {
    crate::embedding_promotion::require_ready(&state.pg).await?;
    require_enabled(state,&expected.origin).await?;context.validate(&state.pg).await?;
    if source(state,&context.actor(),expected.doc).await?.0!=*expected {return Err(RoutingError::NeedsReplan.into());} Ok(())
}
async fn remove_snapshot(state:&AppState,id:Uuid,name:&str)->Result<()> {
    if !name.starts_with(&format!("{id}__ingest_snapshot.")) || name.contains(['/', '\\']) {return Err(RoutingError::Invalid("ingestion snapshot identity").into());}
    let root=std::fs::canonicalize(crate::storage::resolve_dir(&state.boot.storage.documents_dir)).map_err(|_|RoutingError::Forbidden)?;
    let abs=root.join(name);
    if !abs.starts_with(&root) {return Err(RoutingError::Forbidden.into());}
    match tokio::fs::remove_file(abs).await {
        Ok(())=>{},Err(error) if error.kind()==std::io::ErrorKind::NotFound=>{},
        Err(_)=>return Err(RoutingError::Invalid("ingestion snapshot cleanup unavailable").into()),
    }
    sqlx::query("UPDATE routed_jobs SET ingestion_snapshot_removed=true WHERE id=$1 AND kind='ingest' AND status IN ('succeeded','failed','cancelled','interrupted')")
        .bind(id).execute(&state.pg).await?;Ok(())
}

/// Human uploads and explicitly staged connector imports share one runtime.
/// Task payloads contain only the durable job ID, never credentials or a plan.
pub async fn queue(state:&AppState,actor:&AuthContext,doc:Uuid)->Result<Uuid> {
    let origin:Option<String>=sqlx::query_scalar("SELECT source FROM kb_documents WHERE id=$1").bind(doc).fetch_optional(&state.pg).await?;
    if origin.as_deref()==Some("connector_import") && connector_enabled(state).await? {
        return Err(RoutingError::Incompatible("stage connector bytes and capture ACLs before queueing").into());
    }
    if !enabled(state).await? || origin.as_deref()!=Some("upload") {
        return crate::scheduler::enqueue(&state.pg,crate::scheduler::TaskType::Ingest,json!({"doc_id":doc})).await.map_err(Into::into);
    }
    queue_routed(state,actor,doc,false).await
}

/// Call only after connector provenance and the source ACL have been captured.
/// A same-version re-import must not call this: reindexing requires an explicit
/// request. Scheduled sync supplies its connection owner's current context.
pub async fn queue_connector(state:&AppState,actor:&AuthContext,doc:Uuid,system_launch:bool)->Result<Uuid> {
    let origin:Option<String>=sqlx::query_scalar("SELECT source FROM kb_documents WHERE id=$1").bind(doc).fetch_optional(&state.pg).await?;
    if origin.as_deref()!=Some("connector_import") {return Err(RoutingError::Forbidden.into());}
    if !connector_enabled(state).await? {
        return crate::scheduler::enqueue(&state.pg,crate::scheduler::TaskType::Ingest,json!({"doc_id":doc})).await.map_err(Into::into);
    }
    queue_routed(state,actor,doc,system_launch).await
}

#[derive(Clone)]
struct SourceGuard {state:AppState,context:Context,source:Source}
#[async_trait::async_trait]
impl run::AccessGuard for SourceGuard {
    async fn check(&self)->Result<()> {check(&self.state,&self.context,&self.source).await}
}

async fn queue_routed(state:&AppState,actor:&AuthContext,doc:Uuid,system_launch:bool)->Result<Uuid> {
    let context=RoutingContext {actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Primary};
    let captured=Context::capture(&context)?;captured.validate(&state.pg).await?;
    let (source,bytes)=source(state,actor,doc).await?;
    let busy:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE kind='ingest' AND input->'source'->>'doc'=$1 AND status IN ('queued','running'))")
        .bind(doc.to_string()).fetch_one(&state.pg).await?;
    if busy {return Err(crate::AppError::Conflict("document ingestion is already queued or running".into()));}
    let mut context=context;context.project_id=source.project;
    let policy=store::load_policy(state,actor,None).await?;
    let active=crate::embedding_index::active(&state.pg,state.message_key).await?.ok_or(RoutingError::Incompatible("active embedding index is required"))?;
    let mut roles=vec![FixedRole::Embed,FixedRole::Ocr];if active.reindexing {roles.push(FixedRole::EmbedDesired);}
    // Check fixed availability before any possible metadata-only classifier.
    let limits=store::deployment_constraints(state).await?.intersect(&policy.constraints);
    for role in &roles {fixed::resolve(state,&context,*role,None,&limits).await?;}
    let description=json!({"task":"document_ingestion","source":source.origin,"context":"metadata_only",
        "maximum_document_bytes":MAX_DOCUMENT_BYTES,"maximum_helper_calls":33}).to_string();
    let run=RoutedRun::selected_capped(state,context,ModelSelection::Auto {objective:policy.objective,policy_id:Some(policy.id)},
        WorkloadRequirements {task:TaskKind::Extraction,input_tokens:16384,output_tokens:512,..Default::default()},
        &description,(policy.id,policy.revision),RoutingConstraints {max_calls:Some(128),max_duration_ms:Some(300000),..Default::default()}).await?;
    let run=run.adopt_fixed_roles(state,&roles).await?;
    check(state,&captured,&source).await?;
    let ext=std::path::Path::new(&source.filename).extension().and_then(|v|v.to_str()).ok_or(RoutingError::Invalid("ingestion extension"))?;
    if ext.len()>8 || !ext.bytes().all(|b|b.is_ascii_alphanumeric()) {return Err(RoutingError::Invalid("ingestion extension").into());}
    let id=run.plan().id;let snapshot=format!("{id}__ingest_snapshot.{ext}");
    // Canonicalise the existing parent; create_new rejects an existing symlink
    // or file. The server-generated basename cannot escape this parent.
    let root=std::fs::canonicalize(crate::storage::resolve_dir(&state.boot.storage.documents_dir)).map_err(|_|RoutingError::Forbidden)?;
    let abs=root.join(&snapshot);
    if !abs.starts_with(&root) {return Err(RoutingError::Forbidden.into());}
    use tokio::io::AsyncWriteExt;
    let mut file=tokio::fs::OpenOptions::new().write(true).create_new(true).open(abs).await.map_err(|_|RoutingError::Invalid("ingestion snapshot"))?;
    let written=async {file.write_all(&bytes).await?;file.sync_all().await}.await;
    drop(file);
    if written.is_err() {let _=remove_snapshot(state,id,&snapshot).await;return Err(RoutingError::Invalid("ingestion snapshot write failed").into());}
    let result=async {
        let frozen=run.freeze()?;let task=Uuid::now_v7();let uid=actor.user_id.ok_or(RoutingError::Forbidden)?;
        check(state,&frozen.context,&source).await?;
        let mut tx=state.pg.begin().await?;
        sqlx::query("SELECT id FROM kb_documents WHERE id=$1 FOR UPDATE").bind(doc).execute(&mut *tx).await?;
        let busy:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE kind='ingest' AND input->'source'->>'doc'=$1 AND status IN ('queued','running'))")
            .bind(doc.to_string()).fetch_one(&mut *tx).await?;
        if busy {return Err(crate::AppError::Conflict("document ingestion is already queued or running".into()));}
        check(state,&frozen.context,&source).await?;
        sqlx::query("INSERT INTO tasks(id,task_type,payload,max_retries) VALUES($1,'ingest',$2,1)")
            .bind(task).bind(json!({"routed_job_id":id})).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO routed_jobs(id,task_id,actor_user_id,kind,context,plan,limits,usage,input,deadline) VALUES($1,$2,$3,'ingest',$4,$5,$6,$7,$8,$9)")
            .bind(id).bind(task).bind(uid).bind(encode(&frozen.context)?).bind(encode(&frozen.plan)?).bind(encode(&frozen.limits)?)
            .bind(encode(&frozen.usage)?).bind(encode(&Input {source:source.clone(),snapshot:snapshot.clone(),public_launch:true,system_launch})?).bind(frozen.deadline).execute(&mut *tx).await?;
        let mut event=crate::audit::AuditEvent::action("routing.ingestion.queued",actor.role.as_str());
        event.actor_user_id=Some(uid);event.resource_type=Some("routed_job".into());event.resource_id=Some(id);
        event.payload=Some(json!({"document_id":doc,"kb_id":source.kb,"decision_id":frozen.plan.decision.id,"metadata_only_classifier":true}));
        crate::audit::append_with(&mut tx,&event).await?;
        sqlx::query("UPDATE kb_documents SET ingest_status='uploaded' WHERE id=$1").bind(doc).execute(&mut *tx).await?;
        tx.commit().await?;Ok::<_,crate::AppError>(task)
    }.await;
    if result.is_err() {let _=remove_snapshot(state,id,&snapshot).await;}result
}

pub async fn handle_task(state:&AppState,id:Uuid,task:Uuid)->Result<()> {
    let claimed=std::time::Instant::now();let owner=Uuid::now_v7();
    let row:Option<(Value,Value,Value,Value,Value,i64)>=sqlx::query_as(
        "UPDATE routed_jobs SET status='running',owner=$2,lease_until=clock_timestamp()+interval '30 seconds',updated_at=now() WHERE id=$1 AND task_id=$3 AND kind='ingest' AND status='queued' AND deadline>clock_timestamp() RETURNING context,plan,limits,usage,input,(EXTRACT(EPOCH FROM (deadline-clock_timestamp()))*1000)::bigint")
        .bind(id).bind(owner).bind(task).fetch_optional(&state.pg).await?;
    let Some((context,plan,limits,usage,input,remaining))=row else {
        let done:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1 AND task_id=$2 AND kind='ingest' AND status='succeeded')").bind(id).bind(task).fetch_one(&state.pg).await?;
        return if done {Ok(())} else {Err(RoutingError::Incompatible("ingestion job is unavailable or already claimed").into())};
    };
    let input:Input=decode(input)?;let doc=input.source.doc;let kb=input.source.kb;
    let result=async {
        let context:Context=decode(context)?;check(state,&context,&input.source).await?;
        let remaining=(remaining.max(0) as u64).saturating_sub(claimed.elapsed().as_millis() as u64);
        let run=durable::restore(state,context.clone(),decode(plan)?,decode(limits)?,decode(usage)?,remaining,Lease {pool:state.pg.clone(),id,owner})?
            .with_access(Arc::new(SourceGuard {state:state.clone(),context,source:input.source.clone()}))?;
        let work=async {
            sqlx::query("UPDATE kb_documents SET ingest_status='extracting' WHERE id=$1").bind(doc).execute(&state.pg).await?;
            sqlx::query("UPDATE knowledge_bases SET status='indexing' WHERE id=$1").bind(kb).execute(&state.pg).await?;
            crate::scheduler::emit_ingest_status(state,input.source.created_by,doc,kb,"extracting",None);
            let abs=crate::storage::resolve_file(&state.boot.storage.documents_dir,&input.snapshot);
            let abs=crate::upload::ensure_within_storage(&state.boot.storage.documents_dir,&abs.to_string_lossy())?;
            let bytes=tokio::fs::read(&abs).await.map_err(|_|RoutingError::Forbidden)?;
            if bytes.len()>MAX_DOCUMENT_BYTES || hex::encode(Sha256::digest(&bytes))!=input.source.sha256 {return Err(RoutingError::NeedsReplan.into());}
            let (capacity,maximum)=run.capacity(state,CallPurpose::UtilitySummary).await?;
            let output=maximum.min(512);let input_tokens=capacity.saturating_sub(output).min(16384);
            let stages:BTreeMap<_,_>=[(CallPurpose::UtilitySummary,StageRequest {max_calls:33,
                requirements:WorkloadRequirements {task:TaskKind::Extraction,input_tokens,output_tokens:output,
                    effort:run.plan().assignments[&CallPurpose::UtilitySummary].effort.clone(),..Default::default()}})].into();
            let allowance=FixedAllowance {max_calls:32,input_bytes:8*1024*1024,output_bytes:8*1024*1024};
            let mut fixed:BTreeMap<_,_>=[(FixedRole::Embed,allowance.clone()),(FixedRole::Ocr,FixedAllowance {max_calls:8,input_bytes:8*1024*1024,output_bytes:2*1024*1024})].into();
            let dual=if let Some(reference)=run.plan().fixed_assignments.get(&FixedRole::EmbedDesired) {
                fixed.insert(FixedRole::EmbedDesired,allowance);
                let desired=fixed::resolve(state,&run.context(CallPurpose::Embed),FixedRole::EmbedDesired,Some(reference),&run.plan().constraints).await?;
                Some(json!({"model":desired.reference.model,"dim":desired.embedding_dimension,"base_url":desired.base_url,"api_key":desired.api_key}))
            } else {None};
            let read_int=|key:&'static str|async move {crate::config::runtime::get(&state.pg,key).await.ok().flatten().and_then(|v|v.value.parse::<i64>().ok())};
            let body=json!({"doc_id":doc,"kb_id":kb,"path":abs.to_string_lossy(),"mime":input.source.mime,"dimension":input.source.dimension,
                "chunk_size":read_int("ingest.chunk_size").await,"chunk_overlap":read_int("ingest.chunk_overlap").await,
                "pdfplumber":crate::config::runtime::get(&state.pg,"ingest.pdfplumber").await?.map(|v|v.value=="true"),
                "parent_child":input.source.parent_child,"storage_recovery":true,"dual":dual});
            crate::scheduler::emit_ingest_status(state,input.source.created_by,doc,kb,"indexing",None);
            let value=run.json(state,CallPurpose::Embed,JsonOperation::Ingest,body,&stages,fixed,&Default::default(),Duration::from_secs(240)).await?;
            let chunks=value["chunks"].as_i64().filter(|n|*n>0 && *n<=100000).ok_or(RoutingError::Invalid("ingestion produced no bounded chunks"))?;
            check(state,&Context::capture(&run.context(CallPurpose::Primary))?,&input.source).await?;
            let date=value["effective_date"].as_str().and_then(crate::scheduler::parse_iso_date);
            let mut tx=state.pg.begin().await?;run.fence(&mut tx).await?;
            sqlx::query("SELECT id FROM kb_documents WHERE id=$1 FOR UPDATE").bind(doc).execute(&mut *tx).await?;
            sqlx::query("UPDATE kb_documents SET ingest_status='ready',effective_date=$2 WHERE id=$1").bind(doc).bind(date).execute(&mut *tx).await?;
            sqlx::query("UPDATE knowledge_bases SET status='ready',last_ingest_at=now() WHERE id=$1").bind(kb).execute(&mut *tx).await?;
            let event=crate::events::NewEvent::new(crate::events::DOCUMENT_INGESTED,if input.system_launch {crate::events::ActorType::System} else {crate::events::ActorType::Human})
                .actor(if input.system_launch {None} else {run.context(CallPurpose::Primary).actor.user_id}).resource("kb_document",doc).project(input.source.project)
                .payload(json!({"filename":input.source.filename,"mime":input.source.mime,"kb_id":kb.to_string(),"chunks":chunks}));
            crate::events::emit_with(&mut tx,&event).await?;
            // Document readiness, domain event and successful job publication
            // commit together. A crash cannot turn a published result into a
            // fresh retry or an ambiguous interrupted job.
            let changed=sqlx::query("UPDATE routed_jobs SET status='succeeded',owner=NULL,lease_until=NULL,result_id=$3,updated_at=now() WHERE id=$1 AND owner=$2 AND status='running'")
                .bind(id).bind(owner).bind(doc).execute(&mut *tx).await?.rows_affected();
            if changed!=1 {return Err(RoutingError::Forbidden.into());}
            let mut finished=crate::audit::AuditEvent::action("routing.ingestion.succeeded","system");
            finished.resource_type=Some("routed_job".into());finished.resource_id=Some(id);finished.payload=Some(json!({"document_id":doc,"chunks":chunks}));
            crate::audit::append_with(&mut tx,&finished).await?;tx.commit().await?;
            crate::scheduler::emit_ingest_status(state,input.source.created_by,doc,kb,"ready",None);Ok::<_,crate::AppError>(chunks)
        };
        let monitor=async {
            let mut ticks=tokio::time::interval(Duration::from_secs(2));
            loop {
                ticks.tick().await;check(state,&Context::capture(&run.context(CallPurpose::Primary))?,&input.source).await?;
                let n=sqlx::query("UPDATE routed_jobs SET lease_until=clock_timestamp()+interval '30 seconds' WHERE id=$1 AND owner=$2 AND status='running' AND deadline>clock_timestamp() AND lease_until>clock_timestamp()")
                    .bind(id).bind(owner).execute(&state.pg).await?.rows_affected();
                if n!=1 {return Err::<i64,crate::AppError>(RoutingError::Forbidden.into());}
            }
        };
        tokio::select! {biased;result=monitor=>{run.cancel();result},result=work=>result}
    }.await;
    let published:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1 AND task_id=$2 AND kind='ingest' AND status='succeeded' AND result_id=$3)")
        .bind(id).bind(task).bind(doc).fetch_one(&state.pg).await?;
    if published {let _=remove_snapshot(state,id,&input.snapshot).await;return Ok(());}
    let status=if result.is_ok() {"succeeded"} else {"failed"};
    let mut tx=state.pg.begin().await?;
    let n=sqlx::query("UPDATE routed_jobs SET status=$3,owner=NULL,lease_until=NULL,result_id=$4,updated_at=now() WHERE id=$1 AND owner=$2 AND status='running'")
        .bind(id).bind(owner).bind(status).bind(result.as_ref().ok().map(|_|doc)).execute(&mut *tx).await?.rows_affected();
    if n==1 {
        if result.is_err() {
            sqlx::query("UPDATE kb_documents SET ingest_status='error' WHERE id=$1 AND NOT EXISTS(SELECT 1 FROM routed_jobs WHERE kind='ingest' AND input->'source'->>'doc'=$2 AND created_at>(SELECT created_at FROM routed_jobs WHERE id=$3))")
                .bind(doc).bind(doc.to_string()).bind(id).execute(&mut *tx).await?;
            sqlx::query("UPDATE knowledge_bases SET status='error' WHERE id=$1 AND EXISTS(SELECT 1 FROM kb_documents WHERE id=$2 AND ingest_status='error')").bind(kb).bind(doc).execute(&mut *tx).await?;
        }
        let mut event=crate::audit::AuditEvent::action(&format!("routing.ingestion.{status}"),"system");
        event.resource_type=Some("routed_job".into());event.resource_id=Some(id);event.payload=Some(json!({"document_id":doc,"chunks":result.as_ref().ok()}));
        crate::audit::append_with(&mut tx,&event).await?;
    }
    tx.commit().await?;
    if n!=1 {return Err(RoutingError::Forbidden.into());}
    let _=remove_snapshot(state,id,&input.snapshot).await;
    if result.is_err() {crate::scheduler::emit_ingest_status(state,input.source.created_by,doc,kb,"error",Some("Routed ingestion stopped; no automatic replay".into()));}
    result.map(|_|())
}

pub(crate) async fn recover(state:&AppState)->Result<()> {
    sqlx::query("UPDATE kb_documents d SET ingest_status='error' FROM routed_jobs j WHERE j.kind='ingest' AND j.status IN ('failed','cancelled','interrupted') AND d.id::text=j.input->'source'->>'doc' AND d.ingest_status IN ('uploaded','extracting','indexing') AND NOT EXISTS(SELECT 1 FROM routed_jobs newer WHERE newer.kind='ingest' AND newer.input->'source'->>'doc'=d.id::text AND newer.created_at>j.created_at)")
        .execute(&state.pg).await?;
    let snapshots:Vec<(Uuid,String)>=sqlx::query_as("SELECT id,input->>'snapshot' FROM routed_jobs WHERE kind='ingest' AND status IN ('succeeded','failed','cancelled','interrupted') AND NOT ingestion_snapshot_removed AND input->>'snapshot' IS NOT NULL ORDER BY updated_at LIMIT 128")
        .fetch_all(&state.pg).await?;
    for (id,name) in snapshots {let _=remove_snapshot(state,id,&name).await;}
    Ok(())
}
