// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Core-owned management, non-inferencing preview and explicit bounded probes.
use axum::{Json, extract::{Path, Query, State}};
use serde::Deserialize;
use uuid::Uuid;
use crate::{AppState, Result, auth::keycloak::AuthUser, model_routing::{*, store, engine}};

pub async fn management(State(state):State<AppState>, AuthUser(ctx):AuthUser)->Result<Json<serde_json::Value>> {
    let context=RoutingContext {actor:ctx.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::RouterClassify};
    let actor=durable::Context::capture(&context)?;
    actor.validate(&state.pg).await?;
    Ok(Json(serde_json::json!({"schema_version":1,
        "deployment_manage":state.rbac.has_permission(&state.pg,&ctx,crate::auth::permissions::PROVIDERS_MANAGE).await?,
        "user_byok_enabled":store::byok_enabled(&state).await?,"personal_policies":true})))
}

pub async fn ingestion_gate(State(state):State<AppState>,AuthUser(_actor):AuthUser)->Result<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({"enabled":ingestion::enabled(&state).await?,"maximum_document_bytes":ingestion::MAX_DOCUMENT_BYTES})))
}
pub async fn set_ingestion_gate(State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<crate::http::voice_routing::Enabled>)->Result<Json<serde_json::Value>> {
    let context=RoutingContext {actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Primary};
    durable::Context::capture(&context)?.validate(&state.pg).await?;
    state.rbac.require_permission(&state.pg,&actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    crate::config::runtime::set(&state.pg,ingestion::ENABLED_KEY,if body.enabled {"true"} else {"false"},crate::config::runtime::ConfigValueType::Bool,"global",actor.user_id,actor.role.as_str()).await?;
    Ok(Json(serde_json::json!({"enabled":body.enabled})))
}

pub async fn connector_ingestion_gate(State(state):State<AppState>,AuthUser(_):AuthUser)->Result<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({"enabled":ingestion::connector_enabled(&state).await?,"maximum_document_bytes":ingestion::MAX_DOCUMENT_BYTES})))
}
pub async fn set_connector_ingestion_gate(State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<crate::http::voice_routing::Enabled>)->Result<Json<serde_json::Value>> {
    let ctx=RoutingContext {actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Primary};
    durable::Context::capture(&ctx)?.validate(&state.pg).await?;
    state.rbac.require_permission(&state.pg,&actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    crate::config::runtime::set(&state.pg,ingestion::CONNECTOR_ENABLED_KEY,if body.enabled {"true"} else {"false"},crate::config::runtime::ConfigValueType::Bool,"global",actor.user_id,actor.role.as_str()).await?;
    Ok(Json(serde_json::json!({"enabled":body.enabled})))
}
/// Explicit reindex after ACL mapping or an interrupted connector job. A new
/// request owns a new ledger; consumed allocations are never resumed/refunded.
pub async fn queue_connector_ingestion(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(doc):Path<Uuid>)->Result<Json<serde_json::Value>> {
    if !ingestion::connector_enabled(&state).await? {return Err(RoutingError::Incompatible("connector ingestion routing is disabled").into());}
    let task=ingestion::queue_connector(&state,&actor,doc,false).await?;
    Ok(Json(serde_json::json!({"task_id":task})))
}

pub async fn catalog(State(state): State<AppState>, AuthUser(ctx): AuthUser) -> Result<Json<Vec<store::CatalogEntry>>> {
    Ok(Json(store::catalog(&state, &ctx).await?))
}
pub async fn fixed_catalog(State(state): State<AppState>, AuthUser(ctx): AuthUser) -> Result<Json<Vec<fixed::FixedCatalogEntry>>> {
    Ok(Json(fixed::catalog(&state,&ctx).await?))
}
pub async fn save_fixed_profile(State(state): State<AppState>, AuthUser(ctx): AuthUser, Path(id):Path<Uuid>, Json(body):Json<fixed::SaveFixedProfile>) -> Result<Json<fixed::FixedProfile>> {
    Ok(Json(fixed::save_profile(&state,&ctx,id,body).await?))
}
#[derive(Deserialize)]
pub struct FixedRevision { pub expected_revision:i64,pub role:Option<fixed::FixedRole> }
pub async fn delete_fixed_profile(State(state):State<AppState>, AuthUser(ctx):AuthUser, Path(id):Path<Uuid>, Query(revision):Query<FixedRevision>) -> Result<Json<serde_json::Value>> {
    fixed::delete_profile_stage(&state,&ctx,id,revision.expected_revision,revision.role).await?;
    Ok(Json(serde_json::json!({"ok":true})))
}
pub async fn save_profile(State(state): State<AppState>, AuthUser(ctx): AuthUser, Path(id): Path<Uuid>, Json(body): Json<store::SaveProfile>) -> Result<Json<ModelProfile>> {
    Ok(Json(store::save_profile(&state, &ctx, id, body).await?))
}
pub async fn policies(State(state): State<AppState>, AuthUser(ctx): AuthUser) -> Result<Json<Vec<store::PolicyEntry>>> {
    Ok(Json(store::policies(&state, &ctx).await?))
}
pub async fn save_policy(State(state): State<AppState>, AuthUser(ctx): AuthUser, Path(id): Path<Uuid>, Json(body): Json<store::SavePolicy>) -> Result<Json<RoutingPolicy>> {
    Ok(Json(store::save_policy(&state, &ctx, id, body).await?))
}
#[derive(Deserialize)]
pub struct Revision { pub expected_revision: i64 }
pub async fn delete_profile(State(state): State<AppState>, AuthUser(ctx): AuthUser, Path(id): Path<Uuid>, Query(revision): Query<Revision>) -> Result<Json<serde_json::Value>> {
    store::delete_profile(&state, &ctx, id, revision.expected_revision).await?;
    Ok(Json(serde_json::json!({"ok":true})))
}
pub async fn delete_policy(State(state): State<AppState>, AuthUser(ctx): AuthUser, Path(id): Path<Uuid>, Query(revision): Query<Revision>) -> Result<Json<serde_json::Value>> {
    store::delete_policy(&state, &ctx, id, revision.expected_revision).await?;
    Ok(Json(serde_json::json!({"ok":true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub policy_id: Option<Uuid>,
    pub expected_policy_revision:Option<i64>,
    pub objective: RoutingObjective,
    pub purpose: CallPurpose,
    pub requirements: WorkloadRequirements,
}

pub async fn preview(State(state): State<AppState>, AuthUser(ctx): AuthUser, Json(mut body): Json<Preview>) -> Result<Json<serde_json::Value>> {
    let policy = store::load_policy(&state, &ctx, body.policy_id).await?;
    if body.expected_policy_revision.is_some_and(|r|r!=policy.revision) {return Err(RoutingError::NeedsReplan.into());}
    let context = RoutingContext { actor: ctx.clone(), chat_id: None, project_id: None, run_id: Uuid::now_v7(), purpose: body.purpose };
    let constraints = store::deployment_constraints(&state).await?
        .intersect(&state.inference_policy.constraints(&state.pg, &context).await?);
    let profiles = store::current_profiles(store::catalog(&state, &ctx).await?);
    let baseline=body.requirements.clone();
    let rule = if policy.rollout == RolloutMode::Off || policy.strategy == RoutingStrategy::Classifier { engine::RuleMatch::default() }
        else { engine::apply_rules(&policy, &mut body.requirements, body.purpose)? };
    let main_constraints=constraints.intersect(&policy.constraints).intersect(&RoutingConstraints {
        allowed_providers:rule.allowed_providers.clone(),..Default::default()
    });
    let empty_pool=main_constraints.allowed_providers.as_ref().is_some_and(|pool|pool.is_empty());
    let classifier_required = !empty_pool && policy.rollout != RolloutMode::Off && policy.strategy != RoutingStrategy::Rules && rule.target.is_none();
    // Preview receives declared requirements. It neither decrypts keys nor runs
    // the classifier; its missing inference is clearly represented in the result.
    let use_default = rule.target.is_none();
    let decision = if policy.rollout==RolloutMode::Off {
        RoutingDecision {id:context.run_id,schema_version:SCHEMA_VERSION,policy_id:policy.id,policy_revision:policy.revision,
            strategy:policy.strategy,proposed:None,actual:None,reason_codes:vec!["routing_off".into()],excluded_counts:Default::default(),estimated_cost_microusd:None}
    } else {match engine::select_for(body.purpose,&profiles,&body.requirements,&policy,&constraints,body.objective,&rule,use_default) {
        Ok(d)=>d,
        Err(_) if policy.rollout==RolloutMode::Shadow=>RoutingDecision {id:context.run_id,schema_version:SCHEMA_VERSION,policy_id:policy.id,policy_revision:policy.revision,
            strategy:policy.strategy,proposed:None,actual:None,reason_codes:vec!["shadow_no_eligible_proposal".into()],excluded_counts:Default::default(),estimated_cost_microusd:None},
        Err(e)=>return Err(e.into()),
    }};
    let execution_target=match policy.rollout {
        RolloutMode::Off|RolloutMode::Shadow=>Some(engine::checked_default(&profiles,&baseline,&policy,&constraints)?),
        RolloutMode::Active if !classifier_required=>decision.proposed.clone(),
        RolloutMode::Active=>None,
    };
    Ok(Json(serde_json::json!({"dry_run":true,"inference_calls":0,"classifier_required":classifier_required,
        "rollout":policy.rollout,"execution_target":execution_target,"decision":decision,"requirements":body.requirements,
        "constraints":constraints.intersect(&policy.constraints),"executor_allowed_providers":main_constraints.allowed_providers})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifierProbe {
    pub policy_id:Uuid,
    pub expected_policy_revision:i64,
    pub conversation:String,
    pub max_cost_microusd:u64,
}

/// Test the configured judge, even while rollout is off. One call, no tools or
/// answer generation, no fallback/retry. Never accepts targets from the prompt.
pub async fn classifier_probe(State(state):State<AppState>,AuthUser(ctx):AuthUser,Json(body):Json<ClassifierProbe>)->Result<Json<serde_json::Value>> {
    if body.conversation.trim().is_empty() || body.conversation.chars().count()>12_000 {
        return Err(RoutingError::Invalid("judge test requires 1..12000 characters").into());
    }
    let context=RoutingContext {actor:ctx.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::RouterClassify};
    let actor=durable::Context::capture(&context)?;actor.validate(&state.pg).await?;
    let entry=store::policies(&state,&ctx).await?.into_iter().find(|p|p.id==body.policy_id).ok_or(RoutingError::Forbidden)?;
    if entry.scope=="deployment" {
        state.rbac.require_permission(&state.pg,&ctx,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    }
    let policy=store::load_policy(&state,&ctx,Some(body.policy_id)).await?;
    if policy.revision!=body.expected_policy_revision {return Err(RoutingError::NeedsReplan.into());}
    let profiles=store::current_profiles(store::catalog(&state,&ctx).await?);
    let judge=profiles.iter().find(|p|Some(p.provider_id)==policy.classifier_provider_id).ok_or(RoutingError::ProviderUnavailable)?;
    let judge_revision=(judge.provider_id,judge.provider_revision,judge.revision);
    let deployment=store::deployment_constraints(&state).await?;
    let edition=state.inference_policy.constraints(&state.pg,&context).await?;
    let constraints=deployment.intersect(&edition).intersect(&policy.constraints).intersect(&RoutingConstraints {
        max_calls:Some(1),max_output_tokens:Some(policy.classifier.max_output_tokens),
        max_duration_ms:Some(if judge.locality_trusted && judge.locality==Locality::Local {policy.classifier.local_timeout_ms}else{policy.classifier.cloud_timeout_ms}),
        max_cost_microusd:Some(body.max_cost_microusd),..Default::default()
    });
    let budget=budget::RunBudget::new(constraints.clone());
    let baseline=WorkloadRequirements {minimum_tier:ModelTier::Economy,
        input_tokens:u32::try_from(body.conversation.len()).map_err(|_|RoutingError::BudgetExhausted)?,output_tokens:1024,..Default::default()};
    actor.validate(&state.pg).await?;
    let result=classifier::classify(&state,&context,&policy,&profiles,&constraints,&baseline,&body.conversation,&budget,&tokio::sync::Notify::new()).await;
    actor.validate(&state.pg).await?;
    if entry.scope=="deployment" {state.rbac.require_permission(&state.pg,&ctx,crate::auth::permissions::PROVIDERS_MANAGE).await?;}
    let current=store::load_policy(&state,&ctx,Some(policy.id)).await?;
    let current_profiles=store::current_profiles(store::catalog(&state,&ctx).await?);
    if current.revision!=policy.revision || !current_profiles.iter().any(|p|(p.provider_id,p.provider_revision,p.revision)==judge_revision)
        || store::deployment_constraints(&state).await?!=deployment || state.inference_policy.constraints(&state.pg,&context).await?!=edition {
        return Err(RoutingError::NeedsReplan.into());
    }
    // Failed attempts retain conservative reservations in the ledger and audit.
    // A failed classifier never manufactures requirements or runs a default.
    Ok(Json(match result {
        Ok(requirements)=>serde_json::json!({"ok":true,"judge_model":judge.model,"policy_revision":policy.revision,
            "requirements":requirements,"usage":budget.snapshot(),"answer_calls":0,"quality_evaluated":false}),
        Err(error)=>{
            tracing::warn!(%error,"explicit routing judge test failed");
            serde_json::json!({"ok":false,"error":if budget.snapshot().calls==0 {"judge_test_rejected_before_dispatch"} else {"judge_test_failed"},
                "usage":budget.snapshot(),"answer_calls":0,"quality_evaluated":false})
        },
    }))
}

#[derive(Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ProbeKind { Connectivity, ToolCall, JsonResponse }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe { pub kind: ProbeKind, pub expected_profile_revision: i64, pub max_cost_microusd: Option<u64> }

/// Explicit operator action: one short synthetic request, never a user prompt,
/// never executed by dry-run. The report is compatibility evidence, not quality.
pub async fn probe(State(state): State<AppState>, AuthUser(ctx): AuthUser, Path(id): Path<Uuid>, Json(body): Json<Probe>) -> Result<Json<serde_json::Value>> {
    let entries = store::catalog(&state, &ctx).await?;
    let entry = entries.into_iter().find(|entry| entry.id == id).ok_or(RoutingError::ProviderUnavailable)?;
    if entry.scope == "deployment" {
        state.rbac.require_permission(&state.pg, &ctx, crate::auth::permissions::PROVIDERS_MANAGE).await?;
    }
    let profile = store::current_profiles(vec![entry]).pop().ok_or(RoutingError::NeedsReplan)?;
    if profile.revision != body.expected_profile_revision { return Err(RoutingError::NeedsReplan.into()); }
    let context = RoutingContext { actor:ctx,chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::ToolLoop };
    let constraints = store::deployment_constraints(&state).await?.intersect(&RoutingConstraints {
        max_calls:Some(1),max_output_tokens:Some(128),max_duration_ms:Some(10_000),max_cost_microusd:body.max_cost_microusd,..Default::default()
    }).intersect(&state.inference_policy.constraints(&state.pg,&context).await?);
    let target = resolve_target(&state,&context,&TargetReference { provider_id:id,provider_revision:profile.provider_revision,
        profile_revision:profile.revision,model:profile.model.clone(),effort:None , variant:None },&constraints).await?;
    let (prompt, tools) = match body.kind {
        ProbeKind::Connectivity => ("Reply with exactly READY", None),
        ProbeKind::JsonResponse => ("Return only the JSON object {\"ready\":true}", None),
        ProbeKind::ToolCall => ("Call routing_probe with ready=true. Do not answer in prose.", Some(vec![serde_json::json!({"type":"function","function":{"name":"routing_probe","description":"Synthetic compatibility probe. No action will be executed.","parameters":{"type":"object","properties":{"ready":{"type":"boolean"}},"required":["ready"],"additionalProperties":false}}})])),
    };
    let budget = budget::RunBudget::new(constraints);
    let assignments = [(CallPurpose::ToolLoop,target)].into();
    let grant = lease::GrantedLease::reserve(&budget,context.run_id,&assignments,std::slice::from_ref(&profile),
        [(CallPurpose::ToolLoop,lease::Allowance { max_calls:1,input_tokens:1024,output_tokens:128 })].into(),
        std::time::Duration::from_secs(10))?;
    let overrides = InferenceEnvelope { schema_version:SCHEMA_VERSION,decision_id:context.run_id,purpose:CallPurpose::ToolLoop,
        assignments,budget:Some(grant.wire.clone()),fixed_assignments:Default::default(),fixed_budget:None }.apply(Default::default())?;
    let messages = [serde_json::json!({"role":"user","content":prompt})];
    let sampling = crate::ml::Sampling { max_tokens:Some(128),temperature:Some(0.0),..Default::default() };
    let result = tokio::time::timeout(budget.remaining(std::time::Duration::from_secs(10)), crate::ml::chat_step(&state.http,&state.boot.ml.base_url,&messages,tools.as_deref(),&sampling,overrides))
        .await.map_err(|_| crate::AppError::Unavailable("model probe timed out".into()))??;
    grant.finish(result.routing_report.as_ref())?;
    let compatible = match body.kind {
        ProbeKind::Connectivity => result.content.trim() == "READY",
        ProbeKind::JsonResponse => serde_json::from_str::<serde_json::Value>(&result.content).ok() == Some(serde_json::json!({"ready":true})),
        ProbeKind::ToolCall => result.tool_calls.iter().any(|call| call.name=="routing_probe" && call.arguments==serde_json::json!({"ready":true})),
    };
    Ok(Json(serde_json::json!({"compatible":compatible,"provider_revision":profile.provider_revision,"profile_revision":profile.revision,
        "quality_evaluated":false,"inference_calls":1,"usage":result.usage})))
}
