// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Research queue. A started run is never replayed with a fresh budget.
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use uuid::Uuid;
use crate::{AppState,Result,auth::AuthContext,model_routing::{*,run::RoutedRun,durable::{Context,Lease}}};
use super::routed::{self,Request,Snapshot};

pub const ENABLED_KEY:&str="routing.research_jobs_enabled";
pub async fn enabled(state:&AppState)->Result<bool> {
    use crate::config::runtime::{self,ConfigValueType};
    match runtime::get(&state.pg,ENABLED_KEY).await? {
        None=>Ok(false),Some(v) if matches!(v.value_type,ConfigValueType::Bool)=>match v.value.as_str() {
            "true"=>Ok(true),"false"=>Ok(false),_=>Err(RoutingError::Invalid("invalid research routing gate").into())},
        Some(_)=>Err(RoutingError::Invalid("research routing gate must be boolean").into()),
    }
}
async fn require_enabled(state:&AppState)->Result<()> {
    if !enabled(state).await? {return Err(RoutingError::Incompatible("routed research jobs are disabled").into());} Ok(())
}
#[derive(Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {pub request:Request,pub selection:ModelSelection,#[serde(default)] pub clarify:bool,
    #[serde(default,skip_serializing)] pub request_id:Option<Uuid>}
#[derive(Serialize,Deserialize)]
struct Input {request:Request,source:Snapshot,public_launch:bool,
    #[serde(default)] clarify:bool,#[serde(default)] clarification:Option<super::triage::Checkpoint>}
#[derive(Serialize)]
pub struct Started {pub job_id:Uuid,pub chat_id:Uuid,pub status_url:String}
fn encode<T:Serialize>(value:&T)->Result<Value> {serde_json::to_value(value).map_err(|_|RoutingError::Invalid("research checkpoint").into())}
fn decode<T:serde::de::DeserializeOwned>(value:Value)->Result<T> {serde_json::from_value(value).map_err(|_|RoutingError::Invalid("research checkpoint").into())}

pub async fn launch(state:&AppState,actor:&AuthContext,body:Launch)->Result<Started> {
    super::history::launch(state,actor,body).await
}
pub(super) async fn launch_once(state:&AppState,actor:&AuthContext,body:Launch)->Result<Started> {
    require_enabled(state).await?;body.request.validate()?;
    if body.clarify && body.request.refinements.len()>5 {
        return Err(RoutingError::Invalid("clarification requires room for three refinements").into());
    }
    let chat=Uuid::now_v7();let id=Uuid::now_v7();
    let context=RoutingContext {actor:actor.clone(),chat_id:Some(chat),project_id:None,run_id:id,purpose:CallPurpose::ResearchPlan};
    // The chat is created only in the enqueue transaction. Planning cannot leave
    // orphan chats, and private corpus contents never become classifier input.
    let source=routed::source_snapshot(state,&context,&body.request).await?;
    let policy_id=match &body.selection {ModelSelection::Auto {policy_id,..}=>*policy_id,_=>None};
    let policy=store::load_policy(state,actor,policy_id).await?;
    let mut constraints=store::deployment_constraints(state).await?.intersect(&policy.constraints);
    for purpose in [CallPurpose::ResearchPlan,CallPurpose::ResearchWrite,CallPurpose::Document,
        CallPurpose::UtilitySummary,CallPurpose::UtilityTitle,CallPurpose::Ocr] {
        let mut stage=context.clone();stage.purpose=purpose;
        constraints=constraints.intersect(&state.inference_policy.constraints(&state.pg,&stage).await?);
    }
    if body.request.web() {
        let mut stage=context.clone();stage.purpose=CallPurpose::Rerank;
        constraints=constraints.intersect(&state.inference_policy.constraints(&state.pg,&stage).await?);
    }
    if body.request.verify {
        let mut stage=context.clone();stage.purpose=CallPurpose::Verify;
        constraints=constraints.intersect(&state.inference_policy.constraints(&state.pg,&stage).await?);
    }
    if constraints.max_total_tokens.is_some() || constraints.max_output_tokens.is_some() || constraints.max_cost_microusd.is_some() {
        return Err(RoutingError::Incompatible("fixed-service token and monetary bounds are not available").into());
    }
    let mut fixed_roles=if body.request.web() {vec![fixed::FixedRole::Ocr,fixed::FixedRole::Rerank]} else {vec![fixed::FixedRole::Ocr]};
    if body.request.verify {fixed_roles.push(fixed::FixedRole::Verify);}
    for role in &fixed_roles {fixed::resolve(state,&context,*role,None,&constraints).await?;}
    let description=json!({"task":"research","source":body.request.source,"context":"metadata_only","documents":source.docs.len(),
        "maximum_text_bytes":32000,"maximum_sections":8,"verify":body.request.verify,"deepen":body.request.deepen}).to_string();
    let mut run=RoutedRun::selected(state,context.clone(),body.selection,
        WorkloadRequirements {task:TaskKind::Research,input_tokens:4096,output_tokens:1024,uncertain:true,..Default::default()},
        &description,&fixed_roles,&tokio::sync::Notify::new()).await?;
    if routed::source_snapshot(state,&context,&body.request).await?!=source {return Err(RoutingError::NeedsReplan.into());}
    run.add_handoff_reason("research_metadata_only")?;
    let frozen=run.freeze()?;frozen.context.validate(&state.pg).await?;require_enabled(state).await?;
    let uid=actor.user_id.ok_or(RoutingError::Forbidden)?;let task=Uuid::now_v7();
    let deadline=frozen.deadline.min(time::OffsetDateTime::now_utc()+time::Duration::minutes(10));
    let mut tx=state.pg.begin().await?;
    sqlx::query("INSERT INTO chats(id,owner_user_id,title,mode,research_params) VALUES($1,$2,'Research','research',$3)")
        .bind(chat).bind(uid).bind(json!({"question":body.request.question,"source":body.request.source,"template":body.request.template,
            "kb_ids":body.request.kb_ids,"refinements":body.request.refinements,"verify":body.request.verify,"deepen":body.request.deepen,"routed_job_id":id}))
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO tasks(id,task_type,payload,max_retries) VALUES($1,'deep_research',$2,1)")
        .bind(task).bind(json!({"routed_job_id":id})).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO routed_jobs(id,task_id,actor_user_id,kind,context,plan,limits,usage,input,deadline) VALUES($1,$2,$3,'deep_research',$4,$5,$6,$7,$8,$9)")
        .bind(id).bind(task).bind(uid).bind(encode(&frozen.context)?).bind(encode(&frozen.plan)?)
        .bind(encode(&frozen.limits)?).bind(encode(&frozen.usage)?)
        .bind(encode(&Input {request:body.request,source,public_launch:true,clarify:body.clarify,clarification:None})?).bind(deadline).execute(&mut *tx).await?;
    let mut event=crate::audit::AuditEvent::action("routing.research.queued",actor.role.as_str());
    event.actor_user_id=Some(uid);event.resource_type=Some("routed_job".into());event.resource_id=Some(id);
    event.payload=Some(json!({"chat_id":chat,"decision_id":frozen.plan.decision.id}));
    crate::audit::append_with(&mut tx,&event).await?;
    if let Some(key)=body.request_id {
        let n=sqlx::query("UPDATE research_launch_receipts SET status='succeeded',job_id=$3 WHERE actor_user_id=$1 AND request_id=$2 AND status='planning' AND deadline>clock_timestamp()")
            .bind(uid).bind(key).bind(id).execute(&mut *tx).await?.rows_affected();
        if n!=1 {return Err(RoutingError::Cancelled.into());}
    }
    tx.commit().await?;
    Ok(Started {job_id:id,chat_id:chat,status_url:format!("/api/research-jobs/{id}")})
}

async fn authorize(state:&AppState,context:&RoutingContext,input:&Input)->Result<()> {
    Context::capture(context)?.validate(&state.pg).await?;
    let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chats WHERE id=$1 AND owner_user_id=$2 AND project_id IS NULL AND mode='research' AND archived_at IS NULL)")
        .bind(context.chat_id).bind(context.actor.user_id).fetch_one(&state.pg).await?;
    if !allowed || context.project_id.is_some() {return Err(RoutingError::Forbidden.into());}
    if routed::template_snapshot(state,context,&input.request).await?!=input.source.template {return Err(RoutingError::NeedsReplan.into());}
    if let Some(expected)=&input.source.web {
        if super::web::policy(state).await?!=*expected {return Err(RoutingError::NeedsReplan.into());}
    }
    let scope=crate::kb::dr_scope(&state.pg,&context.actor).await?;
    if input.request.kb_ids.iter().any(|id|!scope.iter().any(|k|k.id==*id)) {return Err(RoutingError::Forbidden.into());}
    let denied=state.rbac.denied_kb_doc_ids(&state.pg,&context.actor,&input.request.kb_ids).await?;
    if input.source.docs.iter().any(|d|denied.contains(&d.doc_id)) {return Err(RoutingError::Forbidden.into());}
    Ok(())
}

pub async fn status(state:&AppState,actor:&AuthContext,id:Uuid)->Result<Value> {
    if actor.mfa_enroll_only || actor.break_glass {return Err(RoutingError::Forbidden.into());}
    let row:Option<(Value,Value,Value,Value,String,Option<Uuid>,time::OffsetDateTime)>=sqlx::query_as(
        "SELECT context,input,plan,usage,status,result_id,deadline FROM routed_jobs WHERE id=$1 AND actor_user_id=$2 AND kind='deep_research'")
        .bind(id).bind(actor.user_id).fetch_optional(&state.pg).await?;
    let (context,input,plan,usage,status,result,deadline)=row.ok_or(RoutingError::Forbidden)?;
    let context:Context=decode(context)?;let input:Input=decode(input)?;
    if context.actor().role!=actor.role {return Err(RoutingError::Forbidden.into());}
    authorize(state,&context.routing(),&input).await?;
    let plan:ExecutionPlan=decode(plan)?;let usage:budget::Usage=decode(usage)?;
    let verification:Option<Value>=sqlx::query_scalar("SELECT groundedness - 'spans' FROM messages WHERE id=$1 AND chat_id=$2")
        .bind(result).bind(context.routing().chat_id).fetch_optional(&state.pg).await?.flatten();
    Ok(json!({"job_id":id,"chat_id":context.routing().chat_id,"status":status,"message_id":result,
        "selection":plan.selection,"decision_id":plan.decision.id,"reason_codes":plan.decision.reason_codes,
        "usage":usage,"accounting":"conservative_snapshot","verification":verification,
        "options":{"verify":input.request.verify,"deepen":input.request.deepen},
        "deadline":deadline.format(&time::format_description::well_known::Rfc3339).map_err(|_|RoutingError::Invalid("research deadline"))?,
        "clarification":if status=="awaiting_input" {input.clarification.as_ref().map(|c|json!({"id":c.id,"questions":c.questions}))} else {None}}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answers {pub clarification_id:Uuid,pub answers:Vec<String>}

/// An idempotent handoff, never a launch: task identity changes but the original
/// actor, plan, spent ledger, source inventory and absolute deadline do not.
pub async fn resume(state:&AppState,actor:&AuthContext,id:Uuid,body:Answers)->Result<()> {
    if actor.mfa_enroll_only || actor.break_glass {return Err(RoutingError::Forbidden.into());}
    require_enabled(state).await?;
    if body.answers.is_empty() || body.answers.len()>3 || body.answers.iter().any(|s|!super::triage::text_valid(s,256)) {
        return Err(RoutingError::Invalid("answer every clarification with 1-256 UTF-8 bytes").into());
    }
    let mut tx=state.pg.begin().await?;
    let row:Option<(Value,Value,String,bool)>=sqlx::query_as(
        "SELECT context,input,status,deadline>clock_timestamp() FROM routed_jobs WHERE id=$1 AND actor_user_id=$2 AND kind='deep_research' FOR UPDATE")
        .bind(id).bind(actor.user_id).fetch_optional(&mut *tx).await?;
    let (context,input,status,alive)=row.ok_or(RoutingError::Forbidden)?;
    let context:Context=decode(context)?;let mut input:Input=decode(input)?;
    if context.actor().role!=actor.role {return Err(RoutingError::Forbidden.into());}
    authorize(state,&context.routing(),&input).await?;
    let checkpoint=input.clarification.as_mut().ok_or(RoutingError::Invalid("no research clarification"))?;
    if checkpoint.id!=body.clarification_id || checkpoint.questions.len()!=body.answers.len() {
        return Err(RoutingError::Invalid("clarification identity or answer count mismatch").into());
    }
    if let Some(saved)=&checkpoint.answers {
        return if *saved==body.answers {Ok(())} else {Err(RoutingError::Incompatible("clarification already answered").into())};
    }
    if status!="awaiting_input" || !alive {return Err(RoutingError::Incompatible("research is no longer waiting or has expired").into());}
    for (question,answer) in checkpoint.questions.iter().zip(&body.answers) {
        input.request.refinements.push(format!("{}: {}",question.prompt,answer));
    }
    checkpoint.answers=Some(body.answers);
    input.request.validate()?;
    if routed::snapshot(state,&context.routing(),&input.request).await?!=input.source {return Err(RoutingError::NeedsReplan.into());}
    let task=Uuid::now_v7();
    sqlx::query("INSERT INTO tasks(id,task_type,payload,max_retries) VALUES($1,'deep_research',$2,1)")
        .bind(task).bind(json!({"routed_job_id":id})).execute(&mut *tx).await?;
    let n=sqlx::query("UPDATE routed_jobs SET status='queued',task_id=$2,input=$3,updated_at=now() WHERE id=$1 AND status='awaiting_input' AND deadline>clock_timestamp()")
        .bind(id).bind(task).bind(encode(&input)?).execute(&mut *tx).await?.rows_affected();
    if n!=1 {return Err(RoutingError::Cancelled.into());}
    sqlx::query("UPDATE chats SET research_params=jsonb_set(research_params,'{refinements}',$2) WHERE id=$1")
        .bind(context.routing().chat_id).bind(json!(input.request.refinements)).execute(&mut *tx).await?;
    let mut event=crate::audit::AuditEvent::action("routing.research.resumed",actor.role.as_str());
    event.actor_user_id=actor.user_id;event.resource_type=Some("routed_job".into());event.resource_id=Some(id);
    event.payload=Some(json!({"clarification_id":body.clarification_id,"task_id":task}));
    crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;Ok(())
}

async fn pause(state:&AppState,run:&RoutedRun,input:&Input,questions:Vec<super::triage::Question>)->Result<()> {
    let context=run.context(CallPurpose::ResearchPlan);
    let mut saved:Input=decode(encode(input)?)?;
    saved.clarification=Some(super::triage::Checkpoint {id:Uuid::now_v7(),questions,answers:None});
    let mut tx=state.pg.begin().await?;run.fence(&mut tx).await?;
    authorize(state,&context,input).await?;
    if routed::snapshot(state,&context,&input.request).await?!=input.source {return Err(RoutingError::NeedsReplan.into());}
    run.fence(&mut tx).await?;
    sqlx::query("UPDATE routed_jobs SET input=$2,status='awaiting_input',owner=NULL,lease_until=NULL,updated_at=now() WHERE id=$1")
        .bind(context.run_id).bind(encode(&saved)?).execute(&mut *tx).await?;
    sqlx::query("UPDATE tasks SET status='succeeded',finished_at=now(),last_error=NULL WHERE id=(SELECT task_id FROM routed_jobs WHERE id=$1)")
        .bind(context.run_id).execute(&mut *tx).await?;
    let mut event=crate::audit::AuditEvent::action("routing.research.awaiting_input",context.actor.role.as_str());
    event.actor_user_id=context.actor.user_id;event.resource_type=Some("routed_job".into());event.resource_id=Some(context.run_id);
    crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;Ok(())
}
pub async fn cancel(state:&AppState,actor:&AuthContext,id:Uuid)->Result<()> {
    let own:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1 AND actor_user_id=$2 AND kind='deep_research')")
        .bind(id).bind(actor.user_id).fetch_one(&state.pg).await?;
    if !own {return Err(RoutingError::Forbidden.into());}
    // Original ownership is enough to stop spending after a gate/ACL revocation.
    crate::groundedness::durable::cancel(state,actor,id).await
}

pub async fn handle_task(state:&AppState,id:Uuid,task:Uuid)->Result<()> {
    let started=std::time::Instant::now();let owner=Uuid::now_v7();
    let row:Option<(Value,Value,Value,Value,Value,i64)>=sqlx::query_as("UPDATE routed_jobs SET status='running',owner=$2,lease_until=clock_timestamp()+interval '30 seconds',updated_at=now() WHERE id=$1 AND task_id=$3 AND kind='deep_research' AND status='queued' AND deadline>clock_timestamp() RETURNING context,plan,limits,usage,input,(EXTRACT(EPOCH FROM (deadline-clock_timestamp()))*1000)::bigint")
        .bind(id).bind(owner).bind(task).fetch_optional(&state.pg).await?;
    let Some((context,plan,limits,usage,input,remaining))=row else {
        let done:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1 AND task_id=$2 AND kind='deep_research' AND status IN ('succeeded','awaiting_input'))")
            .bind(id).bind(task).fetch_one(&state.pg).await?;
        return if done {Ok(())} else {Err(RoutingError::Incompatible("research job is unavailable or already claimed").into())};
    };
    let result=Box::pin(async {
        let context:Context=decode(context)?;let input:Input=decode(input)?;
        require_enabled(state).await?;authorize(state,&context.routing(),&input).await?;
        let remaining=(remaining.max(0) as u64).saturating_sub(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        let run=crate::model_routing::durable::restore(state,context,decode(plan)?,decode(limits)?,decode(usage)?,remaining,
            Lease {pool:state.pg.clone(),id,owner})?;
        let work=Box::pin(async {
            if routed::snapshot(state,&run.context(CallPurpose::ResearchPlan),&input.request).await?!=input.source {return Err(RoutingError::NeedsReplan.into());}
            if input.clarify && input.clarification.is_none() {
                let questions=super::triage::ask(state,&run,&input.request).await?;
                if !questions.is_empty() {return pause(state,&run,&input,questions).await;}
            }
            let report=routed::execute(state,&run,&input.request).await?;
            publish(state,&run,&input,report).await
        });
        // Runs even when the provider has stopped sending progress or tokens.
        let monitor=Box::pin(async {
            let mut ticks=tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                ticks.tick().await;require_enabled(state).await?;
                authorize(state,&run.context(CallPurpose::ResearchPlan),&input).await?;
                let n=sqlx::query("UPDATE routed_jobs SET lease_until=clock_timestamp()+interval '30 seconds' WHERE id=$1 AND owner=$2 AND status='running' AND deadline>clock_timestamp() AND lease_until>clock_timestamp()")
                    .bind(id).bind(owner).execute(&state.pg).await?.rows_affected();
                if n!=1 {return Err::<(),crate::AppError>(RoutingError::Cancelled.into());}
            }
        });
        tokio::select! {biased; result=monitor=>{run.cancel();result},result=work=>result}
    }).await;
    if result.is_err() {
        let mut tx=state.pg.begin().await?;
        let n=sqlx::query("UPDATE routed_jobs SET status='failed',owner=NULL,lease_until=NULL,updated_at=now() WHERE id=$1 AND owner=$2 AND status='running'")
            .bind(id).bind(owner).execute(&mut *tx).await?.rows_affected();
        if n==1 {
            let mut event=crate::audit::AuditEvent::action("routing.research.failed","system");
            event.resource_type=Some("routed_job".into());event.resource_id=Some(id);
            crate::audit::append_with(&mut tx,&event).await?;
        }
        tx.commit().await?;
    }
    result
}

fn publication(report:&Value)->Result<(String,Vec<crate::ws::protocol::CitationOut>)> {
    let invalid=||RoutingError::Invalid("invalid research publication citations");
    let text=report["report_md"].as_str().ok_or_else(invalid)?;
    let mut records=report["doc_citations"].as_array().ok_or_else(invalid)?.clone();
    if let Some(web)=report["citations"].as_array() {records.extend(web.iter().cloned());}
    let mut markers=Vec::new();let mut order=Vec::new();
    for (start,_) in text.match_indices("[D") {
        let suffix=&text[start+2..];let end=suffix.find(']').ok_or_else(invalid)?;
        if end==0 || !suffix[..end].bytes().all(|b|b.is_ascii_digit()) {return Err(invalid().into());}
        let key=format!("D{}",&suffix[..end]);
        if !order.contains(&key) {order.push(key.clone());}
        markers.push((start,start+end+3,key));
    }
    if order.is_empty() || order.len()!=records.len() {return Err(invalid().into());}
    // History reload appends web citations after documents. Use that same stable
    // grouping for live frames and markers, preserving first appearance per kind.
    order.sort_by_key(|key|records.iter().find(|r|r["reference"]==*key).is_some_and(|r|r.get("url").is_some()));
    let mut citations=Vec::new();
    for key in &order {
        let row=records.iter().find(|r|r["reference"]==*key).ok_or_else(invalid)?;
        let quote_text=row["quote_text"].as_str().ok_or_else(invalid)?.into();
        if let Some(url)=row["url"].as_str() {
            citations.push(crate::ws::protocol::CitationOut {url:Some(url.into()),quote_text,
                title:row["title"].as_str().map(Into::into),domain:row["domain"].as_str().map(Into::into),
                fetched_at:row["fetched_at"].as_str().map(Into::into),snippet_only:Some(false),..Default::default()});
        } else {
            let doc=row["doc_id"].as_str().and_then(|s|Uuid::parse_str(s).ok()).ok_or_else(invalid)?;
            citations.push(crate::ws::protocol::CitationOut {doc_id:Some(doc),quote_text,..Default::default()});
        }
    }
    let mut report=String::with_capacity(text.len());let mut cursor=0;
    for (start,end,key) in markers {
        report.push_str(&text[cursor..start]);report.push_str(&format!("[D{}]",order.iter().position(|k|k==&key).unwrap()+1));cursor=end;
    }
    report.push_str(&text[cursor..]);
    Ok((report,citations))
}

async fn publish(state:&AppState,run:&RoutedRun,input:&Input,report:Value)->Result<()> {
    let context=run.context(CallPurpose::ResearchPlan);let chat=context.chat_id.ok_or(RoutingError::Forbidden)?;
    let (body,citations)=publication(&report)?;
    let title=report["title"].as_str().ok_or(RoutingError::Invalid("research title"))?;
    let text=format!("# {title}\n\n{body}");let message=Uuid::now_v7();let artefact=Uuid::now_v7();
    // Write an immutable, private file before publishing its database pointer.
    // A process crash here can leave an unreferenced file, never a partial report.
    fn io(_:std::io::Error)->crate::AppError {crate::AppError::Unavailable("research artefact storage unavailable".into())}
    let root=crate::storage::resolve_dir(&state.boot.storage.artefacts_dir);
    tokio::fs::create_dir_all(&root).await.map_err(io)?;
    let root=tokio::fs::canonicalize(root).await.map_err(io)?;
    let directory=root.join(chat.to_string());tokio::fs::create_dir_all(&directory).await.map_err(io)?;
    if !tokio::fs::canonicalize(&directory).await.map_err(io)?.starts_with(&root) {return Err(RoutingError::Forbidden.into());}
    let path=directory.join(format!("{artefact}.md"));
    use tokio::io::AsyncWriteExt;
    let mut file=tokio::fs::OpenOptions::new().create_new(true).write(true).open(&path).await.map_err(io)?;
    file.write_all(text.as_bytes()).await.map_err(io)?;file.sync_all().await.map_err(io)?;drop(file);
    let saved=Box::pin(async {
        let mut tx=state.pg.begin().await?;run.fence(&mut tx).await?;
        if let Ok(template)=Uuid::parse_str(&input.request.template) {
            sqlx::query("SELECT id FROM research_templates WHERE id=$1 FOR SHARE").bind(template).execute(&mut *tx).await?;
        }
        if input.source.web.is_some() {
            // Includes inserts of previously absent restrictions, not just row
            // updates. Held only during final database publication.
            sqlx::query("LOCK TABLE config_settings IN SHARE MODE")
                .execute(&mut *tx).await?;
        }
        sqlx::query("SELECT id FROM chats WHERE id=$1 FOR UPDATE").bind(chat).execute(&mut *tx).await?;
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR SHARE").bind(context.actor.user_id).execute(&mut *tx).await?;
        let ids:Vec<_>=input.source.docs.iter().map(|d|d.doc_id).collect();
        sqlx::query("SELECT id FROM kb_documents WHERE id=ANY($1) FOR SHARE").bind(&ids).execute(&mut *tx).await?;
        authorize(state,&context,input).await?;
        if routed::snapshot(state,&context,&input.request).await?!=input.source {return Err(RoutingError::NeedsReplan.into());}
        sqlx::query("INSERT INTO messages(id,chat_id,role,sequence_number,content,turn_id,completed_at) SELECT $1,$2,'assistant',COALESCE(MAX(sequence_number),0)+1,$3,$4,now() FROM messages WHERE chat_id=$2")
            .bind(message).bind(chat).bind(&text).bind(context.run_id).execute(&mut *tx).await?;
        if input.request.verify {
            super::quality::persist(&mut tx,run,message,&text,title,&report).await?;
        }
        for (index,citation) in citations.iter().enumerate() {
            if let Some(url)=&citation.url {
                let fetched=time::OffsetDateTime::parse(citation.fetched_at.as_deref().ok_or(RoutingError::Invalid("web citation time"))?,
                    &time::format_description::well_known::Rfc3339).map_err(|_|RoutingError::Invalid("web citation time"))?;
                sqlx::query("INSERT INTO web_citations(id,message_id,turn_id,url,title,domain,fetched_at,quote_text,snippet_only,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,false,transaction_timestamp()+$9*interval '1 microsecond')")
                    .bind(Uuid::now_v7()).bind(message).bind(context.run_id).bind(url).bind(&citation.title).bind(&citation.domain)
                    .bind(fetched).bind(&citation.quote_text).bind(index as i32).execute(&mut *tx).await?;
                continue;
            }
            sqlx::query("INSERT INTO citations(id,message_id,doc_id,quote_text,additional_metadata,created_at) VALUES($1,$2,$3,$4,$5,transaction_timestamp()+$6*interval '1 microsecond')")
                .bind(Uuid::now_v7()).bind(message).bind(citation.doc_id).bind(&citation.quote_text)
                .bind(json!({"reference":format!("D{}",index+1),"routing_decision_id":run.plan().decision.id}))
                .bind(index as i32).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO generated_artefacts(id,chat_id,turn_id,message_id,kind,title,disk_path,mime,created_by) VALUES($1,$2,$3,$4,'md',$5,$6,'text/markdown',$7)")
            .bind(artefact).bind(chat).bind(context.run_id).bind(message).bind(title)
            .bind(format!("{chat}/{artefact}.md")).bind(context.actor.user_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE chats SET title=$2 WHERE id=$1").bind(chat).bind(title).execute(&mut *tx).await?;
        let mut event=crate::audit::AuditEvent::action("routing.research.succeeded",context.actor.role.as_str());
        event.actor_user_id=context.actor.user_id;event.resource_type=Some("routed_job".into());event.resource_id=Some(context.run_id);
        event.payload=Some(json!({"chat_id":chat,"message_id":message,"artefact_id":artefact,"decision_id":run.plan().decision.id,"usage":run.usage(),
            "verification_requested":input.request.verify,"deepening":report["deepening"]}));
        crate::audit::append_with(&mut tx,&event).await?;
        run.fence(&mut tx).await?;
        sqlx::query("UPDATE routed_jobs SET status='succeeded',owner=NULL,lease_until=NULL,result_id=$2,updated_at=now() WHERE id=$1")
            .bind(context.run_id).bind(message).execute(&mut *tx).await?;
        tx.commit().await?;Ok::<_,crate::AppError>(())
    }).await;
    if saved.is_err() {let _=tokio::fs::remove_file(&path).await;}
    saved?;
    if let Some(uid)=context.actor.user_id {
        state.hub.send_to_user(uid,crate::ws::protocol::ServerFrame::ChatMessagePosted {chat_id:chat,message_id:message,citations});
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn research_publication_preserves_first_reference_order_without_cascading_replacements() {
        let first=Uuid::now_v7();let third=Uuid::now_v7();
        let report=json!({"report_md":"Данные [D3], затем [D1] и снова [D3].","doc_citations":[
            {"reference":"D1","doc_id":first,"quote_text":"First"},{"reference":"D3","doc_id":third,"quote_text":"Third"}]});
        let (text,citations)=publication(&report).unwrap();
        assert_eq!(text,"Данные [D1], затем [D2] и снова [D1].");
        assert_eq!(citations[0].doc_id,Some(third));assert_eq!(citations[1].doc_id,Some(first));
        for text in ["[D9]","[D]","[Draft]","[D3",""] {
            let mut changed=report.clone();changed["report_md"]=json!(text);assert!(publication(&changed).is_err());
        }
    }
}
