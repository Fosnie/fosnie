// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Version-bound document verification and tracked repair on the original runtime.
//! Handles are intentionally not serializable: restoring one must never reset spend.
use std::{collections::BTreeSet,time::Duration};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use uuid::Uuid;
use crate::{AppState,Result,auth::rbac::Permission,model_routing::{*,run::RoutedRun,
    fixed::FixedRole,fixed_lease::FixedAllowance,dispatch::StageRequest,transport::JsonOperation}};

#[derive(Debug,Deserialize,Serialize)]
struct Span {start:usize,end:usize,text:String}
#[derive(Debug,Deserialize,Serialize)]
struct Claim {text:String,verdict:String,score:f64,evidence:String,evidence_doc_ids:Vec<Uuid>,
    section:String,had_citation:bool,source_span:Option<Span>}
#[derive(Deserialize,Serialize)]
struct Draft {claims:Vec<Claim>,document_text:String,model:String,score:Option<f64>,total:usize,
    supported:usize,contradicted:usize,not_mentioned:usize}
#[derive(PartialEq,Serialize,Deserialize)]
pub(super) struct Source {pub(super) doc:Uuid,#[serde(default)] pub(super) kind:TargetKind,
    #[serde(default)] chat_id:Option<Uuid>,version:Uuid,path:String,mime:Option<String>,pub(super) kbs:Vec<Uuid>,denied:Vec<Uuid>,hash:Vec<u8>}
#[derive(Debug,Default,Clone,Copy,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum TargetKind {#[default] Document,Draft}
impl TargetKind {pub fn as_str(self)->&'static str {match self {Self::Document=>"document",Self::Draft=>"draft"}}}

/// Only a successful verification can create a handle for a later repair.
pub struct VerifiedDraft {pub id:Uuid,run_id:Uuid,decision:Uuid,source:Source,draft:Draft,claim_ids:Vec<Uuid>,strictness:String}
// Only the server's durable worker can decode this private, versioned format.
#[derive(Deserialize)]
struct Checkpoint {schema_version:u8,id:Uuid,run_id:Uuid,decision:Uuid,source:Source,draft:Draft,claim_ids:Vec<Uuid>,strictness:String}
pub(super) async fn restore_verified(state:&AppState,run:&RoutedRun,value:Value)->Result<VerifiedDraft> {
    let c:Checkpoint=serde_json::from_value(value).map_err(|_|RoutingError::Invalid("invalid verified checkpoint"))?;
    if c.schema_version!=1 || Some(c.id)!=run.durable_id() || c.run_id!=run.context(CallPurpose::Document).run_id
        || c.decision!=run.plan().decision.id || c.claim_ids.len()!=c.draft.claims.len()
        || c.claim_ids.iter().collect::<BTreeSet<_>>().len()!=c.claim_ids.len() {
        return Err(RoutingError::Invalid("verified checkpoint identity mismatch").into());
    }
    let model=&run.plan().fixed_assignments.get(&FixedRole::VerifyClaims).ok_or(RoutingError::NeedsReplan)?.model;
    validate(&c.draft,model,&c.strictness,&c.source.denied)?;
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM claim_verdicts WHERE run_id=$1").bind(c.id).fetch_one(&state.pg).await?;
    if count!=c.claim_ids.len() as i64 {return Err(RoutingError::NeedsReplan.into());}
    for (id,claim) in c.claim_ids.iter().zip(&c.draft.claims) {
        let matches:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM claim_verdicts WHERE id=$1 AND run_id=$2 AND claim_text=$3 AND source_span IS NOT DISTINCT FROM $4 AND verdict::text=$5 AND repair_action IS NULL AND bound_evidence_ref=$6)")
            .bind(id).bind(c.id).bind(&claim.text).bind(claim.source_span.as_ref().map(|s|json!(s))).bind(&claim.verdict)
            .bind(json!({"evidence":claim.evidence,"section":claim.section,"doc_ids":claim.evidence_doc_ids,"document_version_id":c.source.version,"routing_decision_id":c.decision}))
            .fetch_one(&state.pg).await?;
        if !matches {return Err(RoutingError::NeedsReplan.into());}
    }
    evidence_access(state,&c.source,c.draft.claims.iter().flat_map(|claim|claim.evidence_doc_ids.clone()).collect()).await?;
    Ok(VerifiedDraft {id:c.id,run_id:c.run_id,decision:c.decision,source:c.source,draft:c.draft,claim_ids:c.claim_ids,strictness:c.strictness})
}
fn io(_:std::io::Error)->crate::AppError {crate::AppError::Unavailable("routed document storage unavailable".into())}

/// Legacy queue jobs cannot reconstruct an actor/ledger for routed results.
pub async fn reject_legacy_resume(state:&AppState,id:Uuid)->Result<()> {
    let routed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM audit_events WHERE resource_id=$1 AND action_type='groundedness.verified' AND payload->>'routing_decision_id' IS NOT NULL)")
        .bind(id).fetch_one(&state.pg).await?;
    if routed {return Err(RoutingError::Incompatible("routed draft repair requires the original runtime").into());}
    Ok(())
}

/// The explicit caller keeps ownership across both phases. Public HTTP selection
/// and durable resume are separate gates; this does not enqueue a fresh-budget job.
pub async fn execute(state:&AppState,run:&RoutedRun,doc:Uuid,repair:bool)->Result<Uuid> {
    execute_target(state,run,doc,TargetKind::Document,repair).await
}
pub(super) async fn execute_target(state:&AppState,run:&RoutedRun,doc:Uuid,kind:TargetKind,repair:bool)->Result<Uuid> {
    if kind==TargetKind::Draft && repair {return Err(RoutingError::Incompatible("artefact repair requires a workspace document").into());}
    let verified=verify_target(state,run,doc,kind).await?;
    if repair {
        if let Err(error)=repair_document(state,run,&verified).await {
            if let Some(uid)=run.context(CallPurpose::Document).actor.user_id {
                super::emit_repair_done(state,uid,verified.id,doc,(0,0,0),Some("Repair could not complete. Refresh the document before retrying.".into()));
            }
            return Err(error);
        }
    }
    Ok(verified.id)
}

pub(super) async fn source(state:&AppState,run:&RoutedRun,doc:Uuid,write:bool)->Result<Source> {
    source_kind(state,run,doc,TargetKind::Document,write).await
}
pub(super) async fn source_kind(state:&AppState,run:&RoutedRun,doc:Uuid,kind:TargetKind,write:bool)->Result<Source> {
    let ctx=run.context(CallPurpose::Document);
    if kind==TargetKind::Document && ctx.chat_id.is_some() {crate::chat::routing::authorize(state,run).await?;}
    snapshot_kind(state,&ctx,doc,kind,write).await
}
pub(super) async fn snapshot_kind(state:&AppState,ctx:&RoutingContext,id:Uuid,kind:TargetKind,write:bool)->Result<Source> {
    match kind {
        TargetKind::Document=>snapshot(state,ctx,id,write).await,
        TargetKind::Draft=>{
            if write {return Err(RoutingError::Incompatible("artefact repair requires a workspace document").into());}
            if !state.features.enabled_for(state,&ctx.actor,"groundedness").await {return Err(RoutingError::Forbidden.into());}
            let (chat,path,mime,format):(Uuid,String,String,String)=sqlx::query_as("SELECT chat_id,disk_path,mime,kind::text FROM generated_artefacts WHERE id=$1")
                .bind(id).fetch_optional(&state.pg).await?.ok_or(RoutingError::Forbidden)?;
            if !matches!(format.as_str(),"md"|"docx"|"pdf") {return Err(RoutingError::Incompatible("unsupported artefact verification format").into());}
            let (project,_)=crate::http::export::require_chat_read(state,&ctx.actor,chat).await?;
            if ctx.chat_id!=Some(chat) || ctx.project_id!=project {return Err(RoutingError::NeedsReplan.into());}
            let agent:Option<Uuid>=sqlx::query_scalar("SELECT agent_id FROM chats WHERE id=$1").bind(chat).fetch_one(&state.pg).await?;
            let mut kbs=crate::kb::retrieval_allowlist(&state.pg,&ctx.actor,chat,project,agent).await?;kbs.sort();kbs.dedup();
            if kbs.is_empty() || kbs.len()>64 {return Err(RoutingError::Forbidden.into());}
            let mut denied=state.rbac.denied_kb_doc_ids(&state.pg,&ctx.actor,&kbs).await?;denied.sort();denied.dedup();
            let root=tokio::fs::canonicalize(crate::storage::resolve_dir(&state.boot.storage.artefacts_dir)).await.map_err(io)?;
            let path=crate::storage::resolve_file(&state.boot.storage.artefacts_dir,&path);
            let canonical=tokio::fs::canonicalize(&path).await.map_err(io)?;
            if !canonical.starts_with(root) || tokio::fs::metadata(&canonical).await.map_err(io)?.len()>4*1024*1024 {return Err(RoutingError::Invalid("artefact file exceeds scope or size").into());}
            let bytes=tokio::fs::read(&canonical).await.map_err(io)?;
            if bytes.is_empty() || bytes.len()>4*1024*1024 {return Err(RoutingError::Invalid("invalid artefact file").into());}
            Ok(Source {doc:id,kind,chat_id:Some(chat),version:id,path:path.to_string_lossy().into(),mime:Some(mime),kbs,denied,hash:Sha256::digest(bytes).to_vec()})
        }
    }
}
pub(super) async fn snapshot(state:&AppState,ctx:&RoutingContext,doc:Uuid,write:bool)->Result<Source> {
    if !state.features.enabled_for(state,&ctx.actor,"groundedness").await {return Err(RoutingError::Forbidden.into());}
    let project=crate::documents::project_of(&state.pg,doc).await?;
    if ctx.project_id!=Some(project) {return Err(RoutingError::Forbidden.into());}
    state.rbac.require_project(&state.pg,&ctx.actor,project,if write {Permission::Write} else {Permission::Read}).await?;
    // Project access does not imply access to every connected-source document.
    // Check the injected Enterprise ACL before reading bytes or planning a judge.
    if state.rbac.document_access(&state.pg,&ctx.actor,project,doc).await?==crate::ext::DocAccess::Denied {
        return Err(RoutingError::Forbidden.into());
    }
    let current=crate::documents::current_version(&state.pg,&state.boot.storage.workspace_dir,doc).await?;
    let mut kbs=Vec::new();
    let linked:Vec<Uuid>=sqlx::query_scalar("SELECT kb_id FROM project_kb_links WHERE project_id=$1 ORDER BY kb_id")
        .bind(project).fetch_all(&state.pg).await?;
    for kb in linked {if crate::kb::can_read(&state.pg,&ctx.actor,kb).await? {kbs.push(kb);}}
    if kbs.is_empty() || kbs.len()>64 {return Err(RoutingError::Forbidden.into());}
    let mut denied=state.rbac.denied_kb_doc_ids(&state.pg,&ctx.actor,&kbs).await?;denied.sort();denied.dedup();
    let root=tokio::fs::canonicalize(crate::storage::resolve_dir(&state.boot.storage.workspace_dir)).await.map_err(io)?;
    let path=tokio::fs::canonicalize(&current.bytes_path).await.map_err(io)?;
    if !path.starts_with(root) || tokio::fs::metadata(&path).await.map_err(io)?.len()>4*1024*1024 {return Err(RoutingError::Invalid("draft file exceeds scope or size").into());}
    let bytes=tokio::fs::read(&path).await.map_err(io)?;
    if bytes.is_empty() || bytes.len()>4*1024*1024 {return Err(RoutingError::Invalid("invalid draft file").into());}
    Ok(Source {doc,kind:TargetKind::Document,chat_id:None,version:current.version_id,path:current.bytes_path,mime:current.mime,kbs,denied,hash:Sha256::digest(bytes).to_vec()})
}
pub(super) async fn options(state:&AppState)->Result<super::routed::Options> {
    let opts=super::routed::options(state).await?;
    if opts.threshold!=0.0 {return Err(RoutingError::Incompatible("draft confidence filtering requires separate adoption").into());}
    Ok(opts)
}
fn valid_score(score:f64)->bool {score.is_finite() && (0.0..=1.0).contains(&score)}
async fn evidence_access(state:&AppState,source:&Source,ids:Vec<Uuid>)->Result<()> {
    let ids:Vec<_>=ids.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM kb_documents WHERE id=ANY($1) AND kb_id=ANY($2)")
        .bind(&ids).bind(&source.kbs).fetch_one(&state.pg).await?;
    if count!=ids.len() as i64 || ids.iter().any(|id|source.denied.contains(id)) {return Err(RoutingError::Forbidden.into());}
    Ok(())
}
fn report(value:&Value,nonempty:bool)->Result<()> {
    let llm=value["routing_report"]["calls"].as_array().ok_or(RoutingError::Invalid("missing draft report"))?;
    let fixed=value["fixed_routing_report"]["calls"].as_array().ok_or(RoutingError::Invalid("missing draft fixed report"))?;
    if llm.iter().chain(fixed).any(|c|c["status"]!="completed")
        || (nonempty && !fixed.iter().any(|c|c["role"]=="verify_claims")) {
        return Err(RoutingError::Invalid("draft inference did not complete").into());
    }
    Ok(())
}
async fn dispatch(state:&AppState,run:&RoutedRun,operation:JsonOperation,body:Value,repair:bool)->Result<Value> {
    let purpose=CallPurpose::Document;let (capacity,output)=run.capacity(state,purpose).await?;
    let output=output.min(if repair {400} else {1024});
    if output<if repair {400} else {1024} {return Err(RoutingError::Incompatible("draft output capacity").into());}
    let input=capacity.checked_sub(output).filter(|n|*n>=4096).ok_or(RoutingError::Incompatible("draft context capacity"))?;
    let stages=[(purpose,StageRequest {max_calls:if repair {16} else {4},requirements:WorkloadRequirements {
        input_tokens:input,output_tokens:output,effort:run.plan().assignments[&purpose].effort.clone(),..Default::default()}})].into();
    let mut fixed=[(FixedRole::Embed,16),(FixedRole::Rerank,16),(FixedRole::VerifyClaims,if repair {16} else {1})]
        .into_iter().map(|(r,n)|(r,FixedAllowance {max_calls:n,input_bytes:2*1024*1024,output_bytes:2*1024*1024})).collect::<std::collections::BTreeMap<_,_>>();
    if !repair {fixed.insert(FixedRole::Ocr,FixedAllowance {max_calls:1,input_bytes:8*1024*1024,output_bytes:2*1024*1024});}
    run.json(state,purpose,operation,body,&stages,fixed,&Default::default(),Duration::from_secs(120)).await
}
fn validate(d:&Draft,model:&str,strictness:&str,denied:&[Uuid])->Result<()> {
    let chars:Vec<char>=d.document_text.chars().collect();
    if d.document_text.trim().is_empty() || d.document_text.len()>16000 || d.model!=model || d.total!=d.claims.len() || d.total>16 {
        return Err(RoutingError::Invalid("invalid draft result").into());
    }
    let mut counts=[0;3];
    for c in &d.claims {
        let index=match c.verdict.as_str() {"supported"=>0,"contradicted"=>1,"not_mentioned"=>2,_=>return Err(RoutingError::Invalid("invalid claim verdict").into())};counts[index]+=1;
        if c.text.trim().is_empty() || c.text.len()>2048 || !valid_score(c.score) || c.evidence.len()>16384 || c.section.len()>64
            || c.evidence_doc_ids.len()>3 || c.evidence_doc_ids.iter().any(|id|denied.contains(id))
            || c.had_citation!=!c.evidence.trim().is_empty() || (c.had_citation && c.evidence_doc_ids.is_empty()) {
            return Err(RoutingError::Invalid("invalid draft claim").into());
        }
        if let Some(s)=&c.source_span {
            if s.start>=s.end || s.end>chars.len() || chars[s.start..s.end].iter().collect::<String>()!=s.text {
                return Err(RoutingError::Invalid("invalid draft source span").into());
            }
        }
    }
    let expected=if d.total==0 {None} else {Some((counts[0]+if strictness=="lenient" {counts[2]} else {0}) as f64/d.total as f64)};
    if counts!=[d.supported,d.contradicted,d.not_mentioned] || expected!=d.score {return Err(RoutingError::Invalid("inconsistent draft score").into());}
    Ok(())
}

/// Internal opt-in caller. The caller must have pinned Embed/Rerank/Ocr/VerifyClaims.
/// Successful persistence is atomic; empty-claim results retain score=None.
pub async fn verify_document(state:&AppState,run:&RoutedRun,doc:Uuid)->Result<VerifiedDraft> {
    verify_target(state,run,doc,TargetKind::Document).await
}
pub async fn verify_target(state:&AppState,run:&RoutedRun,doc:Uuid,kind:TargetKind)->Result<VerifiedDraft> {
    tokio::time::timeout(Duration::from_secs(120),run.guard(Box::pin(async {
        let ctx=run.context(CallPurpose::Document);let uid=ctx.actor.user_id.ok_or(RoutingError::Forbidden)?;
        let original=source_kind(state,run,doc,kind,false).await?;let opts=options(state).await?;
        let model=&run.plan().fixed_assignments.get(&FixedRole::VerifyClaims).ok_or(RoutingError::NeedsReplan)?.model;
        let value=dispatch(state,run,JsonOperation::VerifyDraft,json!({"path":original.path,"mime":original.mime,
            "kb_ids":original.kbs,"deny_doc_ids":original.denied,"max_claims":16,"strictness":opts.strictness,"threshold":0,"hhem_filter":false}),false).await?;
        report(&value,value["total"].as_u64().is_some_and(|n|n>0))?;
        if value["routing_report"]["calls"].as_array().is_none_or(|v|v.is_empty()) {return Err(RoutingError::Invalid("draft decomposition did not run").into());}
        let fixed_calls=value["fixed_routing_report"]["calls"].as_array().unwrap();
        if fixed_calls.iter().filter(|c|c["role"]=="embed").count()!=value["total"].as_u64().unwrap_or(u64::MAX) as usize
            || fixed_calls.iter().filter(|c|c["role"]=="verify_claims").count()!=usize::from(value["total"].as_u64().is_some_and(|n|n>0)) {
            return Err(RoutingError::Invalid("draft evidence accounting mismatch").into());
        }
        let draft:Draft=serde_json::from_value(value).map_err(|_|RoutingError::Invalid("invalid draft response"))?;
        validate(&draft,model,opts.strictness,&original.denied)?;
        evidence_access(state,&original,draft.claims.iter().flat_map(|c|c.evidence_doc_ids.clone()).collect()).await?;
        if source_kind(state,run,doc,kind,false).await?!=original || options(state).await?!=opts {return Err(RoutingError::NeedsReplan.into());}
        let id=run.durable_id().unwrap_or_else(Uuid::now_v7);let claim_ids:Vec<_>=draft.claims.iter().map(|_|Uuid::now_v7()).collect();
        let mut tx=state.pg.begin().await?;
        run.fence(&mut tx).await?;
        if kind==TargetKind::Document {
            let current:Option<Uuid>=sqlx::query_scalar("SELECT current_version_id FROM documents WHERE id=$1 AND deleted_at IS NULL FOR UPDATE")
                .bind(doc).fetch_optional(&mut *tx).await?;
            if current!=Some(original.version) {return Err(RoutingError::NeedsReplan.into());}
        } else {
            let row:Option<(Uuid,String,String)>=sqlx::query_as("SELECT chat_id,disk_path,mime FROM generated_artefacts WHERE id=$1 FOR UPDATE")
                .bind(doc).fetch_optional(&mut *tx).await?;
            let (chat,path,mime)=row.ok_or(RoutingError::NeedsReplan)?;
            if Some(chat)!=original.chat_id || Some(mime)!=original.mime
                || crate::storage::resolve_file(&state.boot.storage.artefacts_dir,&path).to_string_lossy()!=original.path {return Err(RoutingError::NeedsReplan.into());}
        }
        sqlx::query("INSERT INTO verification_runs(id,target_type,target_id,mode,verifier_model,strictness,faithfulness_score,total_claims,supported,contradicted,not_mentioned,status,created_by,finished_at) VALUES($1,$11::verification_target,$2,'verify_draft',$3,$4,$5,$6,$7,$8,$9,'succeeded',$10,now())")
            .bind(id).bind(doc).bind(model).bind(opts.strictness).bind(draft.score).bind(draft.total as i32).bind(draft.supported as i32)
            .bind(draft.contradicted as i32).bind(draft.not_mentioned as i32).bind(uid).bind(kind.as_str()).execute(&mut *tx).await?;
        for (claim_id,c) in claim_ids.iter().zip(&draft.claims) {
            sqlx::query("INSERT INTO claim_verdicts(id,run_id,claim_text,source_span,bound_evidence_ref,had_citation,verdict,verifier_score) VALUES($1,$2,$3,$4,$5,$6,$7::claim_verdict,$8)")
                .bind(claim_id).bind(id).bind(&c.text).bind(c.source_span.as_ref().map(|s|json!(s)))
                .bind(json!({"evidence":c.evidence,"section":c.section,"doc_ids":c.evidence_doc_ids,"document_version_id":if kind==TargetKind::Document {Some(original.version)} else {None},"routing_decision_id":run.plan().decision.id}))
                .bind(c.had_citation).bind(&c.verdict).bind(c.score).execute(&mut *tx).await?;
        }
        let event=event(run,"groundedness.verified",id,json!({"mode":"verify_draft","target_type":kind,"target_id":doc,
            "document_id":if kind==TargetKind::Document {Some(doc)} else {None},
            "document_version_id":if kind==TargetKind::Document {Some(original.version)} else {None},
            "source_hash":original.hash,"total":draft.total,"score":draft.score}));
        if run.durable_id().is_some() {
            sqlx::query("UPDATE routed_jobs SET verified_checkpoint=$2 WHERE id=$1")
                .bind(id).bind(json!({"schema_version":1,"id":id,"run_id":ctx.run_id,"decision":run.plan().decision.id,
                    "source":original,"draft":draft,"claim_ids":claim_ids,"strictness":opts.strictness}))
                .execute(&mut *tx).await?;
        }
        crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;
        state.hub.send_to_user(uid,crate::ws::protocol::ServerFrame::VerificationComplete {run_id:id,score:draft.score,total:draft.total as i32,
            supported:draft.supported as i32,contradicted:draft.contradicted as i32,not_mentioned:draft.not_mentioned as i32});
        Ok(VerifiedDraft {id,run_id:ctx.run_id,decision:run.plan().decision.id,source:original,draft,claim_ids,strictness:opts.strictness.into()})
    }))).await.map_err(|_|crate::AppError::Unavailable("routed draft deadline exceeded".into()))?
}

fn event(run:&RoutedRun,action:&str,id:Uuid,details:Value)->crate::audit::AuditEvent {
    let ctx=run.context(CallPurpose::Document);let usage=run.usage();
    let mut ev=crate::audit::AuditEvent::action(action,ctx.actor.role.as_str());ev.actor_user_id=ctx.actor.user_id;
    ev.resource_type=Some("verification_run".into());ev.resource_id=Some(id);
    ev.payload=Some(json!({"details":details,"routing_decision_id":run.plan().decision.id,"resumable":false,
        "document_target":run.plan().assignments[&CallPurpose::Document],"verifier_target":run.plan().fixed_assignments.get(&FixedRole::VerifyClaims),
        "routing_usage_snapshot":{"calls":usage.calls,"tokens":usage.tokens,"unknown_token_calls":usage.unknown_token_calls,
        "unknown_cost_calls":usage.unknown_cost_calls,"cost_microusd":usage.cost_microusd}}));ev
}

#[derive(Deserialize)]
struct Repair {source_text:String,claim_text:String,action:String,replacement:Option<String>,evidence:String,
    evidence_doc_ids:Vec<Uuid>,reverify_verdict:String,reverify_score:f64}

/// Propose tracked changes, using the exact verified version. No edits are
/// accepted automatically; the normal document accept/reject UI remains in charge.
pub async fn repair_document(state:&AppState,run:&RoutedRun,verified:&VerifiedDraft)->Result<(i64,i64,i64)> {
    tokio::time::timeout(Duration::from_secs(120),run.guard(Box::pin(async {
        let ctx=run.context(CallPurpose::Document);let uid=ctx.actor.user_id.ok_or(RoutingError::Forbidden)?;
        let doc=verified.source.doc;
        if verified.source.kind!=TargetKind::Document {return Err(RoutingError::Incompatible("artefact repair requires a workspace document").into());}
        if verified.run_id!=ctx.run_id || verified.decision!=run.plan().decision.id {return Err(RoutingError::Forbidden.into());}
        let enabled=crate::config::runtime::get(&state.pg,"groundedness.repair").await?.is_some_and(|v|v.value=="true");
        if !enabled {return Err(RoutingError::Forbidden.into());}
        let opts=options(state).await?;
        if opts.strictness!=verified.strictness || source(state,run,doc,true).await?!=verified.source {return Err(RoutingError::NeedsReplan.into());}
        let filename:String=sqlx::query_scalar("SELECT original_filename FROM documents WHERE id=$1").bind(doc).fetch_one(&state.pg).await?;
        if !filename.to_ascii_lowercase().ends_with(".docx") {return Err(RoutingError::Incompatible("tracked repair needs a DOCX document").into());}
        // Ambiguous/repeated/overlapping spans cannot safely drive a text find.
        let mut selected=Vec::new();let mut ranges=Vec::new();
        for (id,c) in verified.claim_ids.iter().zip(&verified.draft.claims) {
            if c.verdict=="supported" {continue;}
            let Some(s)=&c.source_span else {continue;};
            if s.text.len()>4096 || verified.draft.document_text.matches(&s.text).count()!=1
                || ranges.iter().any(|&(start,end)|s.start<end && start<s.end) {return Err(RoutingError::Incompatible("ambiguous repair span").into());}
            ranges.push((s.start,s.end));selected.push((*id,c,s));
        }
        if selected.is_empty() {
            super::emit_repair_done(state,uid,verified.id,doc,(0,0,0),None);
            return Ok((0,0,0));
        }
        let ids:Vec<_>=selected.iter().map(|(id,_,_)|*id).collect();
        let pending:i64=sqlx::query_scalar("SELECT count(*) FROM claim_verdicts WHERE id=ANY($1) AND run_id=$2 AND repair_action IS NULL")
            .bind(&ids).bind(verified.id).fetch_one(&state.pg).await?;
        if pending!=ids.len() as i64 {return Err(RoutingError::NeedsReplan.into());}
        let claims:Vec<_>=selected.iter().map(|(_,c,s)|json!({"text":c.text,"source_text":s.text,"verdict":c.verdict,"score":c.score})).collect();
        let value=dispatch(state,run,JsonOperation::RepairDraft,json!({"claims":claims,"kb_ids":verified.source.kbs,
            "deny_doc_ids":verified.source.denied,"strictness":verified.strictness}),true).await?;
        report(&value,false)?;
        let result:Vec<Repair>=serde_json::from_value(value["results"].clone()).map_err(|_|RoutingError::Invalid("invalid repair result"))?;
        if result.len()!=selected.len() {return Err(RoutingError::Invalid("repair result count mismatch").into());}
        let mut edits=Vec::new();let mut counts=(0,0,0);
        for ((_,c,s),r) in selected.iter().zip(&result) {
            if r.claim_text!=c.text || r.source_text!=s.text || r.evidence.len()>16384 || r.evidence_doc_ids.len()>3
                || !valid_score(r.reverify_score) || !matches!(r.reverify_verdict.as_str(),"supported"|"contradicted"|"not_mentioned") {
                return Err(RoutingError::Invalid("invalid repair provenance").into());
            }
            match r.action.as_str() {
                "regenerated" if r.replacement.as_ref().is_some_and(|t|!t.trim().is_empty() && t.len()<=4096 && t!=&s.text)
                    && r.reverify_verdict=="supported" && !r.evidence.trim().is_empty() && !r.evidence_doc_ids.is_empty()=>counts.0+=1,
                "cut" if r.replacement.is_none()=>counts.1+=1,
                "kept" if r.replacement.is_none() && r.reverify_verdict=="supported" && !r.evidence.trim().is_empty() && !r.evidence_doc_ids.is_empty()=>counts.2+=1,
                _=>return Err(RoutingError::Invalid("invalid repair action").into()),
            }
            if r.action!="kept" {edits.push(crate::ml::EditInput {find:s.text.clone(),replace:r.replacement.clone().unwrap_or_default(),context_before:None,context_after:None});}
        }
        let fixed_calls=value["fixed_routing_report"]["calls"].as_array().unwrap();
        if fixed_calls.iter().filter(|c|c["role"]=="embed").count()!=selected.len()
            || fixed_calls.iter().filter(|c|c["role"]=="verify_claims").count()<(counts.0+counts.2) as usize
            || value["routing_report"]["calls"].as_array().unwrap().len()<result.iter().filter(|r|!r.evidence.trim().is_empty()).count() {
            return Err(RoutingError::Invalid("repair evidence accounting mismatch").into());
        }
        evidence_access(state,&verified.source,result.iter().flat_map(|r|r.evidence_doc_ids.clone()).collect()).await?;
        let version=Uuid::now_v7();
        let root=crate::documents::workspace_root(state)?;
        let path=crate::documents::version_path(&root,doc,version,".docx");
        let mut applied=crate::ml::ApplyResult {changes:vec![],errors:vec![]};
        let mut size=0;
        if !edits.is_empty() {
            if source(state,run,doc,true).await?!=verified.source {return Err(RoutingError::NeedsReplan.into());}
            tokio::fs::create_dir_all(path.parent().unwrap()).await.map_err(io)?;
            applied=crate::ml::apply_tracked_changes(&state.http,&state.boot.ml.base_url,&verified.source.path,&path.to_string_lossy(),&edits,"Groundedness").await?;
            let mut wids=BTreeSet::new();
            if !applied.errors.is_empty() || applied.changes.len()!=edits.len() || applied.changes.iter().zip(&edits)
                .any(|(c,e)|c.find!=e.find || c.replace!=e.replace || c.w_id.is_empty() || !wids.insert(c.w_id.clone())) {
                return Err(RoutingError::Invalid("tracked changes do not match repair proposals").into());
            }
            size=tokio::fs::metadata(&path).await.map_err(io)?.len();
            if size==0 || size>8*1024*1024 {return Err(RoutingError::Invalid("invalid repaired document size").into());}
        }
        if source(state,run,doc,true).await?!=verified.source || options(state).await?!=opts
            || !crate::config::runtime::get(&state.pg,"groundedness.repair").await?.is_some_and(|v|v.value=="true") {
            return Err(RoutingError::NeedsReplan.into());
        }
        let mut tx=state.pg.begin().await?;
        run.fence(&mut tx).await?;
        let current:Option<Uuid>=sqlx::query_scalar("SELECT current_version_id FROM documents WHERE id=$1 AND deleted_at IS NULL FOR UPDATE")
            .bind(doc).fetch_optional(&mut *tx).await?;
        if current!=Some(verified.source.version) {return Err(RoutingError::NeedsReplan.into());}
        let pending:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM claim_verdicts WHERE id=ANY($1) AND run_id=$2 AND repair_action IS NULL FOR UPDATE")
            .bind(&ids).bind(verified.id).fetch_all(&mut *tx).await?;
        if pending.len()!=ids.len() {return Err(RoutingError::NeedsReplan.into());}
        if !edits.is_empty() {
            let n:i32=sqlx::query_scalar("SELECT COALESCE(MAX(version_number),0)+1 FROM document_versions WHERE document_id=$1").bind(doc).fetch_one(&mut *tx).await?;
            let relative=crate::storage::relativise(&state.boot.storage.workspace_dir,&path);
            sqlx::query("INSERT INTO document_versions(id,document_id,version_number,source,bytes_path,byte_size,created_by) VALUES($1,$2,$3,'assistant_edit',$4,$5,$6)")
                .bind(version).bind(doc).bind(n).bind(relative).bind(size as i64).bind(uid).execute(&mut *tx).await?;
            sqlx::query("UPDATE documents SET current_version_id=$2 WHERE id=$1").bind(doc).bind(version).execute(&mut *tx).await?;
            for c in &applied.changes {
                sqlx::query("INSERT INTO document_edits(id,document_id,document_version_id,w_id,author,find_text,replace_text) VALUES($1,$2,$3,$4,'assistant',$5,$6)")
                    .bind(Uuid::now_v7()).bind(doc).bind(version).bind(&c.w_id).bind(&c.find).bind(&c.replace).execute(&mut *tx).await?;
            }
        }
        for ((id,_,_),r) in selected.iter().zip(&result) {
            let wid=applied.changes.iter().find(|c|c.find==r.source_text).map(|c|&c.w_id);
            sqlx::query("UPDATE claim_verdicts SET repair_action=$2,bound_evidence_ref=bound_evidence_ref || $3::jsonb WHERE id=$1")
                .bind(id).bind(&r.action).bind(json!({"repair":{"replacement":r.replacement,"evidence":r.evidence,"doc_ids":r.evidence_doc_ids,
                    "reverify_verdict":r.reverify_verdict,"reverify_score":r.reverify_score,"w_id":wid}})).execute(&mut *tx).await?;
        }
        crate::audit::append_with(&mut tx,&event(run,"groundedness.repaired",verified.id,json!({"document_id":doc,
            "base_version":verified.source.version,"version_id":if edits.is_empty() {None} else {Some(version)},"regenerated":counts.0,"cut":counts.1,"kept":counts.2}))).await?;
        tx.commit().await?;
        super::emit_repair_done(state,uid,verified.id,doc,counts,None);
        Ok(counts)
    }))).await.map_err(|_|crate::AppError::Unavailable("routed repair deadline exceeded".into()))?
}
