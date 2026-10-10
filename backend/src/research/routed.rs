// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit, bounded file/web/hybrid Research. The caller owns the plan and ledger.
//! No public launch or legacy scheduler fallback is enabled by this module.
use std::{collections::BTreeSet,time::Duration};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use uuid::Uuid;
use crate::{AppState,Result,model_routing::{*,run::RoutedRun,dispatch::StageRequest,
    fixed::FixedRole,fixed_lease::FixedAllowance,transport::StreamOperation}};

#[derive(Clone,Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub question:String,pub kb_ids:Vec<Uuid>,pub template:String,
    #[serde(default="files_source")] pub source:String,
    #[serde(default)] pub refinements:Vec<String>,
    #[serde(default)] pub verify:bool,
    #[serde(default)] pub deepen:bool,
}
fn files_source()->String {"files".into()}
impl Request {
    pub(crate) fn web(&self)->bool {self.source!="files"}
    pub(crate) fn validate(&self)->Result<u32> {
        let sections=match self.template.as_str() {"exploration"=>4,"formal"=>5,"literature"=>6,"freeform"=>8,
            id if Uuid::parse_str(id).is_ok()=>8,
            _=>return Err(RoutingError::Incompatible("unsupported bounded research template").into())};
        if !matches!(self.source.as_str(),"files"|"web"|"hybrid") || (self.source=="web" && !self.kb_ids.is_empty())
            || (self.source!="web" && self.kb_ids.is_empty()) || self.question.trim().is_empty() || self.question.len()>4096 || self.kb_ids.len()>4
            || self.kb_ids.iter().collect::<BTreeSet<_>>().len()!=self.kb_ids.len() || self.refinements.len()>8
            || self.refinements.iter().any(|s|s.trim().is_empty() || s.len()>512) {
            return Err(RoutingError::Invalid("invalid bounded research request").into());
        }
        Ok(sections)
    }
}

#[derive(Clone,PartialEq,Serialize,Deserialize)]
pub(crate) struct Source {pub doc_id:Uuid,pub kb_id:Uuid,path:String,mime:Option<String>,sha256:String}
#[derive(Clone,PartialEq,Serialize,Deserialize)]
pub(crate) struct Snapshot {pub docs:Vec<Source>,denied:Vec<Uuid>,#[serde(default)] pub web:Option<Value>,
    #[serde(default)] pub template:Option<Value>}

/// Reuse original-actor visibility (including archived templates for Refine),
/// then admit only template semantics implemented by this bounded executor.
pub(crate) async fn template_snapshot(state:&AppState,context:&RoutingContext,req:&Request)->Result<Option<Value>> {
    if Uuid::parse_str(&req.template).is_err() {return Ok(None);}
    let spec=crate::http::research::resolve_template(state,&context.actor,&req.template).await?.spec
        .ok_or(RoutingError::Invalid("missing custom research template"))?;
    if spec.to_string().len()>8192 {return Err(RoutingError::Invalid("research template exceeds 8192 bytes").into());}
    let sections=spec["skeleton"].as_array().ok_or(RoutingError::Invalid("research template skeleton"))?;
    if sections.len()>8 || !matches!(spec["outline_mode"].as_str(),Some("free"|"constrained"))
        || (spec["outline_mode"]=="free" && !sections.is_empty()) || (spec["outline_mode"]=="constrained" && sections.is_empty()) {
        return Err(RoutingError::Incompatible("bounded templates need 1-8 fixed sections or a free outline with no skeleton").into());
    }
    for (key,limit) in [("label",512),("writing_instructions",4096)] {
        if !spec[key].as_str().is_some_and(|s|!s.trim().is_empty() && s.len()<=limit && !s.contains('\0')) {
            return Err(RoutingError::Invalid("custom template needs a label and non-empty writing instructions within byte limits").into());
        }
    }
    let mut seen=BTreeSet::new();
    for section in sections {
        for (key,limit) in [("heading",160),("brief",1024)] {
            if !section[key].as_str().is_some_and(|s|!s.trim().is_empty() && s.len()<=limit && !s.contains('\0')) {
                return Err(RoutingError::Invalid("custom template needs bounded non-empty headings and briefs").into());
            }
        }
        let heading=section["heading"].as_str().unwrap();
        if heading.contains(['\n','\r']) || !seen.insert(heading.trim().to_lowercase())
            || section["expandable"].as_bool().unwrap_or(false) || section["exec_summary"].as_bool().unwrap_or(false) {
            return Err(RoutingError::Incompatible("bounded custom templates require unique single-line fixed headings without expansion or summary placeholders").into());
        }
    }
    Ok(Some(spec))
}

fn storage_error(_:std::io::Error)->crate::AppError {crate::AppError::Unavailable("research source storage unavailable".into())}
/// Both editions resolve the original actor and document denials here. A changed
/// corpus is a replan, never a silent substitution or partial successful census.
pub(crate) async fn snapshot(state:&AppState,context:&RoutingContext,req:&Request)->Result<Snapshot> {
    let chat=context.chat_id.ok_or(RoutingError::Forbidden)?;
    let (project,_)=crate::http::export::require_chat_read(state,&context.actor,chat).await?;
    if project!=context.project_id {return Err(RoutingError::NeedsReplan.into());}
    source_snapshot(state,context,req).await
}
pub(crate) async fn source_snapshot(state:&AppState,context:&RoutingContext,req:&Request)->Result<Snapshot> {
    req.validate()?;
    crate::model_routing::durable::Context::capture(context)?.validate(&state.pg).await?;
    let template=template_snapshot(state,context,req).await?;
    let web=if req.web() {Some(super::web::policy(state).await?)} else {None};
    if req.source=="web" {return Ok(Snapshot {docs:vec![],denied:vec![],web,template});}
    let scope=crate::kb::dr_scope(&state.pg,&context.actor).await?;
    if req.kb_ids.iter().any(|id|!scope.iter().any(|kb|kb.id==*id)) {return Err(RoutingError::Forbidden.into());}
    let mut denied=state.rbac.denied_kb_doc_ids(&state.pg,&context.actor,&req.kb_ids).await?;
    denied.sort();denied.dedup();
    if denied.len()>100000 {return Err(RoutingError::Invalid("research denial scope exceeds limit").into());}
    let rows:Vec<(Uuid,Uuid,String,Option<String>)>=sqlx::query_as("SELECT id,kb_id,bytes_path,mime FROM kb_documents WHERE kb_id=ANY($1) AND NOT(id=ANY($2)) AND ingest_status='ready' ORDER BY id LIMIT 5")
        .bind(&req.kb_ids).bind(&denied).fetch_all(&state.pg).await?;
    if rows.is_empty() || rows.len()>4 {return Err(RoutingError::Incompatible("bounded research needs one to four readable documents").into());}
    let root=tokio::fs::canonicalize(crate::storage::resolve_dir(&state.boot.storage.documents_dir)).await.map_err(storage_error)?;
    let mut docs=Vec::new();
    for (doc_id,kb_id,path,mime) in rows {
        let path=tokio::fs::canonicalize(crate::storage::resolve_file(&state.boot.storage.documents_dir,&path)).await.map_err(storage_error)?;
        if !path.starts_with(&root) {return Err(RoutingError::Forbidden.into());}
        use tokio::io::AsyncReadExt;
        let file=tokio::fs::File::open(&path).await.map_err(storage_error)?;
        let mut bytes=Vec::new();file.take(4*1024*1024+1).read_to_end(&mut bytes).await.map_err(storage_error)?;
        if bytes.is_empty() || bytes.len()>4*1024*1024 {return Err(RoutingError::Invalid("research file exceeds size limit").into());}
        docs.push(Source {doc_id,kb_id,path:path.to_string_lossy().into(),mime,sha256:hex::encode(Sha256::digest(bytes))});
    }
    Ok(Snapshot {docs,denied,web,template})
}

/// Returns only a validated final report. Provisional tokens are intentionally
/// not published: the durable worker must commit result + citations + audit under
/// its lease. This function never creates a plan, resets a budget or retries work.
pub async fn execute(state:&AppState,run:&RoutedRun,req:&Request)->Result<Value> {
    run.guard(Box::pin(async {
        let mut sections=req.validate()?;
        let context=run.context(CallPurpose::ResearchPlan);
        let source=snapshot(state,&context,req).await?;
        if let Some(spec)=&source.template {
            let count=spec["skeleton"].as_array().unwrap().len() as u32;
            sections=if count==0 {8} else {count};
        }
        let mut stages=std::collections::BTreeMap::new();
        for (purpose,max_calls,max_output) in [
            (CallPurpose::Document,1,512),
            (CallPurpose::ResearchPlan,source.docs.len() as u32+1+if req.web() {4} else {0}+if req.deepen {sections} else {0},1536),
            (CallPurpose::ResearchWrite,sections+1,4096),
            (CallPurpose::UtilitySummary,sections,256),(CallPurpose::UtilityTitle,1,96),
        ] {
            let (capacity,output)=run.capacity(state,purpose).await?;
            let output=output.min(max_output);
            let input=capacity.checked_sub(output).filter(|n|*n>2048)
                .ok_or(RoutingError::Incompatible("research model context capacity"))?.min(128000);
            stages.insert(purpose,StageRequest {max_calls,requirements:WorkloadRequirements {
                input_tokens:input,output_tokens:output,effort:run.plan().assignments[&purpose].effort.clone(),..Default::default()
            }});
        }
        let mut fixed=std::collections::BTreeMap::from([(FixedRole::Ocr,FixedAllowance {max_calls:source.docs.len() as u32+if req.web() {4} else {0},input_bytes:8*1024*1024,output_bytes:2*1024*1024})]);
        if req.web() {fixed.insert(FixedRole::Rerank,FixedAllowance {max_calls:1,input_bytes:64*1024,output_bytes:64*1024});}
        if req.verify {fixed.insert(FixedRole::Verify,FixedAllowance {max_calls:1,input_bytes:512*1024,output_bytes:512*1024});}
        let mut body=json!({"question":req.question,"template":req.template,"source":req.source,"kb_ids":req.kb_ids,
            "docs":source.docs,"total_docs":source.docs.len(),"deny_doc_ids":source.denied,"refinements":req.refinements,
            "verify":req.verify,"research_limits":{"schema_version":1,"deepen":req.deepen}});
        if let Some(spec)=&source.template {body["template_spec"]=spec.clone();}
        if let Some(policy)=&source.web {
            crate::integrations::guard_egress(state,&context.actor,crate::integrations::ConnectorKind::WebSearch).await?;
            body.as_object_mut().unwrap().extend(policy.as_object().unwrap().clone());
        }
        let mut stream=run.stream(state,CallPurpose::ResearchPlan,StreamOperation::Research,body,&stages,fixed,
            &Default::default(),Duration::from_secs(600)).await?;
        let mut bytes=0usize;
        while let Some(value)=stream.recv().await? {
            bytes=bytes.saturating_add(value.to_string().len());
            if bytes>2*1024*1024 {return Err(RoutingError::Invalid("research stream exceeds limit").into());}
            match value["type"].as_str() {
                Some("progress"|"token")=>{},
                Some("done")=>{
                    validate_report(&value,&source,req,run)?;
                    if snapshot(state,&context,req).await?!=source {return Err(RoutingError::NeedsReplan.into());}
                    return Ok(value);
                },
                _=>return Err(RoutingError::Invalid("unexpected research event").into()),
            }
        }
        Err(RoutingError::Invalid("research ended without a report").into())
    })).await
}

fn validate_report(value:&Value,source:&Snapshot,req:&Request,run:&RoutedRun)->Result<()> {
    let invalid=||RoutingError::Invalid("invalid bounded research report");
    for (key,limit) in [("title",512),("report_md",24000)] {
        if !value[key].as_str().is_some_and(|s|!s.trim().is_empty() && s.len()<=limit) {return Err(invalid().into());}
    }
    if req.verify {
        let verified:crate::groundedness::routed::VerifiedAnswer=serde_json::from_value(value["verification"].clone()).map_err(|_|invalid())?;
        let model=&run.plan().fixed_assignments.get(&FixedRole::Verify).ok_or_else(invalid)?.model;
        crate::groundedness::routed::validate_strict(&verified,value["report_md"].as_str().unwrap(),model)?;
        if !value["fixed_routing_report"]["calls"].as_array().is_some_and(|calls|calls.iter().any(|c|c["role"]=="verify" && c["status"]=="completed")) {
            return Err(invalid().into());
        }
    } else if !value["verification"].is_null() {return Err(invalid().into());}
    if req.deepen {
        let sections=value["report_md"].as_str().unwrap().lines().filter(|s|s.starts_with("## ")).count();
        let summary=&value["deepening"];
        if sections==0 || sections>8 || summary["method"]!="existing_sources_v1" || summary["sections"]!=sections
            || !summary["sections_with_gaps"].as_u64().is_some_and(|n|n<=sections as u64) {return Err(invalid().into());}
        let required=source.docs.len()+value["web_sources"].as_array().map_or(0,Vec::len)+1+sections;
        if value["routing_report"]["calls"].as_array().map_or(0,|calls|calls.iter().filter(|c|c["purpose"]=="research_plan" && c["status"]=="completed").count())<required {
            return Err(invalid().into());
        }
    } else if !value["deepening"].is_null() {return Err(invalid().into());}
    if value["coverage"]!=json!({"selected":source.docs.len(),"read":source.docs.len(),"sampled":false}) {return Err(invalid().into());}
    let citations=value["doc_citations"].as_array().ok_or_else(invalid)?;
    let mut seen=BTreeSet::new();
    if (!source.docs.is_empty() && citations.is_empty()) || citations.len()>source.docs.len() {return Err(invalid().into());}
    if let Some(policy)=&source.web {super::web::validate_sources(value,source.docs.len(),policy)?;}
    else if value["citations"]!=json!([]) || !value["web_sources"].is_null() {return Err(invalid().into());}
    for citation in citations {
        let id=citation["doc_id"].as_str().and_then(|s|Uuid::parse_str(s).ok()).ok_or_else(invalid)?;
        let index=source.docs.iter().position(|d|d.doc_id==id).ok_or_else(invalid)?;
        if !seen.insert(id) || citation["reference"]!=format!("D{}",index+1)
            || !citation["quote_text"].as_str().is_some_and(|s|s.len()<=800)
            || ["page_number","chunk_index","clause_section_ref"].iter().any(|k|!citation[*k].is_null()) {return Err(invalid().into());}
    }
    Ok(())
}
