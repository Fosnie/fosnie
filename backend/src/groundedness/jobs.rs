// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Public bounded document jobs, behind a separate default-off deployment gate.
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use uuid::Uuid;
use crate::{AppState,AppError,Result,auth::{AuthContext,rbac::Permission},model_routing::{*,run::RoutedRun,fixed::FixedRole}};
use super::routed_draft::TargetKind;
pub const ENABLED_KEY:&str="routing.document_jobs_enabled";

pub async fn enabled(state:&AppState)->Result<bool> {
    use crate::config::runtime::{self,ConfigValueType};
    match runtime::get(&state.pg,ENABLED_KEY).await? {
        None=>Ok(false),Some(v) if matches!(v.value_type,ConfigValueType::Bool)=>match v.value.as_str() {
            "true"=>Ok(true),"false"=>Ok(false),_=>Err(RoutingError::Invalid("invalid document routing gate").into())},
        Some(_)=>Err(RoutingError::Invalid("document routing gate must be boolean").into()),
    }
}
pub(super) async fn require_enabled(state:&AppState)->Result<()> {
    if !enabled(state).await? {return Err(RoutingError::Incompatible("routed document jobs are disabled").into());} Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub document_id:Uuid,
    pub selection:ModelSelection,
    #[serde(default)] pub repair:bool,
}

pub async fn launch(state:&AppState,actor:&AuthContext,request:Launch)->Result<Uuid> {
    launch_target(state,actor,request,TargetKind::Document).await
}
pub async fn launch_target(state:&AppState,actor:&AuthContext,request:Launch,kind:TargetKind)->Result<Uuid> {
    require_enabled(state).await?;
    if kind==TargetKind::Draft && request.repair {return Err(RoutingError::Incompatible("artefact repair requires a workspace document").into());}
    let (chat_id,project_id)=match kind {
        TargetKind::Document=>(None,Some(crate::documents::project_of(&state.pg,request.document_id).await?)),
        TargetKind::Draft=>{
            let chat:Uuid=sqlx::query_scalar("SELECT chat_id FROM generated_artefacts WHERE id=$1").bind(request.document_id)
                .fetch_optional(&state.pg).await?.ok_or(RoutingError::Forbidden)?;
            let (project,_)=crate::http::export::require_chat_read(state,actor,chat).await?;
            (Some(chat),project)
        }
    };
    let context=RoutingContext {actor:actor.clone(),chat_id,project_id,run_id:Uuid::now_v7(),purpose:CallPurpose::Document};
    crate::model_routing::durable::Context::capture(&context)?.validate(&state.pg).await?;
    // Before any possible classifier call: file/source permissions, settings,
    // supported repair format, and availability of the fixed inference lanes.
    let original=super::routed_draft::snapshot_kind(state,&context,request.document_id,kind,request.repair).await?;
    super::routed_draft::options(state).await?;
    if request.repair {
        if !crate::config::runtime::get(&state.pg,"groundedness.repair").await?.is_some_and(|v|v.value=="true") {return Err(RoutingError::Forbidden.into());}
        let name:String=sqlx::query_scalar("SELECT original_filename FROM documents WHERE id=$1").bind(request.document_id).fetch_one(&state.pg).await?;
        if !name.to_ascii_lowercase().ends_with(".docx") {return Err(RoutingError::Incompatible("tracked repair needs DOCX").into());}
    }
    let policy_id=match &request.selection {ModelSelection::Auto {policy_id,..}=>*policy_id,_=>None};
    let policy=store::load_policy(state,actor,policy_id).await?;
    let constraints=store::deployment_constraints(state).await?.intersect(&policy.constraints)
        .intersect(&state.inference_policy.constraints(&state.pg,&context).await?);
    if constraints.max_total_tokens.is_some() || constraints.max_output_tokens.is_some() || constraints.max_cost_microusd.is_some() {
        return Err(RoutingError::Incompatible("fixed-service token and monetary bounds are not available").into());
    }
    let roles=[FixedRole::Embed,FixedRole::Rerank,FixedRole::Ocr,FixedRole::VerifyClaims];
    for role in roles {fixed::resolve(state,&context,role,None,&constraints).await?;}
    // The classifier gets a task description, never document content or a path.
    // It cannot infer document-specific complexity from this bounded metadata.
    let description=json!({"task":"verify_draft","target_type":kind,"repair":request.repair,
        "context":"metadata_only","maximum_text_bytes":16000,"maximum_claims":16}).to_string();
    let mut run=RoutedRun::selected(state,context.clone(),request.selection,
        WorkloadRequirements {task:TaskKind::Extraction,input_tokens:4096,output_tokens:1024,uncertain:true,..Default::default()},
        &description,&roles,&tokio::sync::Notify::new()).await?;
    // Preserve the request-time source snapshot even if planning used a judge.
    if super::routed_draft::snapshot_kind(state,&context,request.document_id,kind,request.repair).await?!=original {return Err(RoutingError::NeedsReplan.into());}
    require_enabled(state).await?;
    // This reason describes the classifier input, not a quality measurement.
    run.add_handoff_reason("document_metadata_only")?;
    super::durable::enqueue_public(state,run,request.document_id,request.repair,original).await
}

#[derive(Serialize)]
pub struct Status {
    pub id:Uuid,pub document_id:Option<Uuid>,pub target_id:Uuid,pub target_type:TargetKind,pub status:String,pub phase:String,pub repair:bool,
    pub selection:ModelSelection,pub decision_id:Uuid,pub reason_codes:Vec<String>,
    pub planned_document_target:Option<TargetReference>,pub usage:budget::Usage,
    pub accounting:&'static str,pub result_id:Option<Uuid>,pub created_at:String,pub updated_at:String,
}
pub async fn status(state:&AppState,actor:&AuthContext,id:Uuid)->Result<Status> {
    if actor.mfa_enroll_only || actor.break_glass {return Err(RoutingError::Forbidden.into());}
    let uid=actor.user_id.ok_or(RoutingError::Forbidden)?;
    let row:Option<(Uuid,bool,String,Value,Value,time::OffsetDateTime,time::OffsetDateTime,bool,String,Value)>=sqlx::query_as(
        "SELECT (input->'source'->>'doc')::uuid,(input->>'repair')::bool,status,plan,usage,created_at,updated_at,EXISTS(SELECT 1 FROM verification_runs v WHERE v.id=j.id),phase,COALESCE(input->'source'->'kind','\"document\"'::jsonb) FROM routed_jobs j WHERE id=$1 AND actor_user_id=$2 AND kind='verify_draft'")
        .bind(id).bind(uid).fetch_optional(&state.pg).await?;
    let (doc,repair,status,plan,usage,created,updated,has_result,phase,kind)=row.ok_or_else(||AppError::NotFound("document job unavailable".into()))?;
    let kind:TargetKind=serde_json::from_value(kind).map_err(|_|RoutingError::Invalid("stored target kind"))?;
    authorize_target_result(state,actor,id,doc,kind).await?;
    let plan:ExecutionPlan=serde_json::from_value(plan).map_err(|_|RoutingError::Invalid("stored job plan"))?;
    let visible=store::catalog(state,actor).await?;
    let target=plan.assignments.get(&CallPurpose::Document).filter(|t|visible.iter().any(|p|p.id==t.provider_id)).cloned();
    let format=time::format_description::well_known::Rfc3339;
    Ok(Status {id,document_id:(kind==TargetKind::Document).then_some(doc),target_id:doc,target_type:kind,status,phase,repair,selection:plan.selection,decision_id:plan.decision.id,
        reason_codes:plan.decision.reason_codes,planned_document_target:target,
        usage:serde_json::from_value(usage).map_err(|_|RoutingError::Invalid("stored job usage"))?,accounting:"conservative_snapshot",
        result_id:has_result.then_some(id),created_at:created.format(&format).unwrap_or_default(),updated_at:updated.format(&format).unwrap_or_default()})
}

pub async fn is_routed(state:&AppState,id:Uuid)->Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1) OR EXISTS(SELECT 1 FROM audit_events WHERE resource_id=$1 AND action_type='groundedness.verified' AND payload->>'routing_decision_id' IS NOT NULL)")
        .bind(id).fetch_one(&state.pg).await?)
}

/// Result reads must not inherit the creator/admin bypass of legacy reports.
/// Recheck the document and every evidence source, including repaired claims.
pub async fn authorize_result(state:&AppState,actor:&AuthContext,id:Uuid,doc:Uuid)->Result<()> {
    authorize_target_result(state,actor,id,doc,TargetKind::Document).await
}
pub async fn authorize_target_result(state:&AppState,actor:&AuthContext,id:Uuid,doc:Uuid,kind:TargetKind)->Result<()> {
    if actor.mfa_enroll_only || !state.features.enabled_for(state,actor,"groundedness").await {return Err(RoutingError::Forbidden.into());}
    match kind {
        TargetKind::Document=>{
            let project=crate::documents::project_of(&state.pg,doc).await?;
            state.rbac.require_project(&state.pg,actor,project,Permission::Read).await?;
        }
        TargetKind::Draft=>{
            let chat:Uuid=sqlx::query_scalar("SELECT chat_id FROM generated_artefacts WHERE id=$1").bind(doc)
                .fetch_optional(&state.pg).await?.ok_or(RoutingError::Forbidden)?;
            crate::http::export::require_chat_read(state,actor,chat).await?;
        }
    }
    let rows:Vec<Value>=sqlx::query_scalar("SELECT bound_evidence_ref FROM claim_verdicts WHERE run_id=$1 AND bound_evidence_ref IS NOT NULL")
        .bind(id).fetch_all(&state.pg).await?;
    let mut ids=std::collections::BTreeSet::new();
    for row in rows {
        for value in [&row["doc_ids"],&row["repair"]["doc_ids"]] {
            if value.is_null() {continue;}
            let documents:Vec<Uuid>=serde_json::from_value(value.clone()).map_err(|_|RoutingError::Invalid("stored evidence identities"))?;
            ids.extend(documents);
        }
    }
    let ids:Vec<_>=ids.into_iter().collect();
    let documents:Vec<(Uuid,Uuid)>=sqlx::query_as("SELECT id,kb_id FROM kb_documents WHERE id=ANY($1)").bind(&ids).fetch_all(&state.pg).await?;
    if documents.len()!=ids.len() {return Err(RoutingError::Forbidden.into());}
    let libraries:Vec<_>=documents.iter().map(|(_,kb)|*kb).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    for kb in &libraries {if !crate::kb::can_read(&state.pg,actor,*kb).await? {return Err(RoutingError::Forbidden.into());}}
    let denied=state.rbac.denied_kb_doc_ids(&state.pg,actor,&libraries).await?;
    if ids.iter().any(|id|denied.contains(id)) {return Err(RoutingError::Forbidden.into());} Ok(())
}
