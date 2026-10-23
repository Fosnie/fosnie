// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Negotiated v2 uses the same resolver, plan, stage affinity and RunBudget.
use super::super::*;
use super::{catalogue::*, pool::PoolSnapshot, profile::*, profiles, variants};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantBinding {
    pub variant: VariantRef,
    pub model_config_revision: i64,
    pub behaviour_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContext {
    pub intent: fosnie_protocol::routing_vnext::RoutingIntentV2,
    pub workload: WorkloadRequirements,
    #[serde(default)]
    pub difficulty: DifficultySignal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<super::recovery::Plan>,
}
impl VariantBinding {
    pub fn of(v: &ModelVariant) -> Self {
        Self {
            variant: VariantRef {
                id: v.id,
                revision: v.revision,
            },
            model_config_revision: v.model_config_revision,
            behaviour_sha256: v.behaviour_sha256.clone(),
        }
    }
}

pub async fn current_variant(
    state: &crate::AppState,
    actor: &crate::auth::AuthContext,
    reference: &TargetReference,
) -> crate::Result<Option<ModelVariant>> {
    let Some(binding) = &reference.variant else {
        return Ok(None);
    };
    let c = variants::catalogue(state, actor).await?;
    let v = c
        .variants
        .into_iter()
        .find(|e| {
            e.variant.id == binding.variant.id && e.configuration_current && e.binding_current
        })
        .ok_or(RoutingError::NeedsReplan)?
        .variant;
    if VariantBinding::of(&v) != *binding
        || v.provider_id != reference.provider_id
        || v.behaviour.effort.value() != reference.effort.as_deref()
    {
        return Err(RoutingError::NeedsReplan.into());
    }
    variants::validate_adapter_generation(&v.behaviour)?;
    Ok(Some(v))
}
fn reference(v: &ModelVariant, p: &ModelProfile) -> TargetReference {
    TargetReference {
        provider_id: p.provider_id,
        provider_revision: p.provider_revision,
        profile_revision: p.revision,
        model: p.model.clone(),
        effort: v.behaviour.effort.value().map(str::to_owned),
        variant: Some(VariantBinding::of(v)),
    }
}


pub async fn plan(state:&crate::AppState,context:&RoutingContext,
    intent:fosnie_protocol::routing_vnext::RoutingIntentV2,req:WorkloadRequirements,
    conversation:&str,cancel:&tokio::sync::Notify)->crate::Result<(ExecutionPlan,budget::RunBudget)> {
    plan_with_ledger(state,context,intent,req,conversation,cancel,None).await
}
pub(crate) async fn plan_with_ledger(state:&crate::AppState,context:&RoutingContext,
    intent:fosnie_protocol::routing_vnext::RoutingIntentV2,req:WorkloadRequirements,
    conversation:&str,cancel:&tokio::sync::Notify,ledger:Option<service::PlanningLedger>)
    ->crate::Result<(ExecutionPlan,budget::RunBudget)> {
    Box::pin(plan_internal(state,context,intent,req,conversation,cancel,ledger)).await
}
async fn plan_internal(state:&crate::AppState,context:&RoutingContext,
    intent:fosnie_protocol::routing_vnext::RoutingIntentV2,mut req:WorkloadRequirements,
    conversation:&str,cancel:&tokio::sync::Notify,ledger:Option<service::PlanningLedger>)
    ->crate::Result<(ExecutionPlan,budget::RunBudget)> {
    intent.require_support(&[1,2],&[1,2]).map_err(RoutingError::Invalid)?;
    let narrowed=crate::chat::browser::constrain(state.clone(),&intent.options.clone().unwrap_or_default());
    let state=&narrowed;
    if let Some(e)=intent.options.as_ref().and_then(|o|o.effort.clone()) {req.effort=Some(e);}
    if intent.variant.is_none() && intent.required_profile_version==1 {
        let (mut p,b,_)=service::selected_plan_with_retry_supported(state,context,intent.selection.clone(),req.clone(),conversation,cancel,ledger,2).await?;
        p.schema_version=2;p.decision.schema_version=2;
        p.vnext_context=Some(ExecutionContext{intent,workload:req,difficulty:DifficultySignal::Unknown,recovery:None});
        return Ok((p,b));
    }
    let policy_id=match &intent.selection {ModelSelection::Auto{policy_id,..}=>*policy_id,_=>None};
    let policy=store::load_policy_supported(state,&context.actor,policy_id,2).await?;
    engine::validate_policy(&policy)?;
    let mut constraints=store::deployment_constraints(state).await?.intersect(&policy.constraints)
        .intersect(&state.inference_policy.constraints(&state.pg,context).await?);
    if intent.options.as_ref().is_some_and(|o|o.local_only){constraints.local_only=true;}
    let budget=if let Some(owned)=ledger {
        if owned.policy_id!=policy.id || owned.policy_revision!=policy.revision{return Err(RoutingError::NeedsReplan.into());}
        owned.budget
    }else{budget::RunBudget::new(constraints.clone())};
    budget.restrict(&constraints);budget.check_deadline()?;
    let catalogue=variants::catalogue(state,&context.actor).await?;
    let models=store::current_profiles(store::catalog(state,&context.actor).await?);
    let config=if intent.required_profile_version==2 {
        if policy.rollout!=RolloutMode::Active{return Err(RoutingError::Invalid("activate the routing profile before using Auto").into());}
        Some(profiles::load(state,&context.actor,policy.id).await?)
    }else{None};
    let mut rule=engine::RuleMatch::default();let mut difficulty=DifficultySignal::Unknown;
    let mut classifier_reason=None;
    if config.is_some() {
        rule=engine::apply_rules(&policy,&mut req,context.purpose)?;
        if rule.target.is_none() && policy.strategy!=RoutingStrategy::Rules {
            let never_cancel=tokio::sync::Notify::new();
            let result=tokio::select!{biased;_=cancel.notified()=>return Err(RoutingError::Cancelled.into()),
                value=classifier::classify_for_plan(state,context,&policy,&models,&constraints,&req,conversation,&budget,&never_cancel)=>value};
            match result {
                Ok(classifier::ClassificationOutcome::Classified(r,d))=>{req=r;difficulty=d;classifier_reason=Some("task_classified");}
                Ok(classifier::ClassificationOutcome::Skipped(reason))=>{req.uncertain=true;classifier_reason=Some(reason);}
                Err(_)=>{req.uncertain=true;classifier_reason=Some("classifier_unavailable_default");}
            }
            rule=engine::apply_rules(&policy,&mut req,context.purpose)?;
        }
        if let Some(e)=&rule.effort{req.effort=Some(e.clone());}
        constraints=constraints.intersect(&RoutingConstraints{allowed_providers:rule.allowed_providers.clone(),..Default::default()});
        budget.restrict(&constraints);
    }
    let mut authorised=BTreeMap::new();
    for e in &catalogue.variants {
        let v=&e.variant;
        if !e.configuration_current || !e.binding_current {continue;}
        let Some(p)=models.iter().find(|p|p.provider_id==v.provider_id)else{continue;};
        let mut effective=req.clone();effective.effort=v.behaviour.effort.value().map(str::to_owned);
        effective.output_tokens=effective.output_tokens.min(v.behaviour.generation.max_output_tokens.unwrap_or(effective.output_tokens));
        if req.effort.as_ref().is_some_and(|s|Some(s.as_str())!=v.behaviour.effort.value())
            || engine::target_incompatibility(p,&effective,&constraints).is_some(){continue;}
        if config.is_some() && (!p.auto_enabled || p.task_exclusions.contains(&req.task)
            || (req.task==TaskKind::Unknown && !p.task_exclusions.is_empty())){continue;}
        if health::blocked(state,&reference(v,p),context.purpose).await?.is_some(){continue;}
        authorised.insert(v.id,(v,p));
    }
    let mut decision=service::empty_decision(context,&policy,"manual_variant");decision.schema_version=2;
    let mut profile_snapshot=None;let mut recovery=None;
    let selected=if let Some(manual)=&intent.variant {
        let (v,p)=authorised.get(&manual.id).ok_or(RoutingError::ProviderUnavailable)?;
        if v.revision!=manual.expected_revision || !matches!(intent.selection,ModelSelection::Manual{provider_id} if provider_id==v.provider_id){return Err(RoutingError::NeedsReplan.into());}
        reference(v,p)
    }else{
        let config=config.as_ref().ok_or(RoutingError::Invalid("routing profile required"))?;
        let aliases=authorised.iter().map(|(id,(v,_))|(*id,(v.provider_id,v.behaviour_sha256.clone()))).collect();
        let groups=config.resolve_groups_with_aliases(&aliases,&req,context.purpose,difficulty)?;
        let legacy_pin=if let Some(id)=rule.target {
            let mut matching:Vec<_>=authorised.values().filter(|(v,_)|v.provider_id==id && groups.allowed_variants.contains(&v.id)).collect();
            matching.sort_by_key(|(v,_)|v.id);
            let (v,_)=matching.first().ok_or(RoutingError::NoEligibleModel("rule model needs an available pool variant"))?;
            if matching.iter().any(|(other,_)|other.behaviour_sha256!=v.behaviour_sha256){return Err(RoutingError::Invalid("ambiguous model assignment; select an exact variant").into());}
            Some(VariantRef{id:v.id,revision:v.revision})
        }else{None};
        let objective=match intent.selection{ModelSelection::Auto{objective,..}=>objective,_=>policy.objective};
        let (selected,reason)=super::selection::choose(config,&groups,&authorised,&req,objective,legacy_pin)?;
        decision.reason_codes=vec![reason.into()];
        if let Some(reason)=classifier_reason{decision.reason_codes.push(reason.into());}
        for id in &groups.matched_groups{decision.reason_codes.push(format!("task_group:{id}"));}
        let mut active=groups.allowed_variants.clone();active.extend(groups.reference_variants.iter().copied());active.extend(groups.fallback_variants.iter().copied());
        let mut seen=BTreeSet::new();let mut candidates=Vec::new();
        let mut ordered:Vec<_>=active.into_iter().collect();ordered.sort_by_key(|id|(*id!=selected.id,*id));
        for id in ordered {
            let (v,p)=authorised[&id];if !seen.insert((v.provider_id,v.behaviour_sha256.clone())){continue;}
            let d=catalogue.deployments.iter().find(|d|d.id==v.provider_id).ok_or(RoutingError::NeedsReplan)?;
            candidates.push(CandidateRef{provider_id:v.provider_id,provider_revision:p.provider_revision,profile_revision:p.revision,
                model_config_revision:v.model_config_revision,variant:VariantRef{id:v.id,revision:v.revision},effort:v.behaviour.effort.clone(),behaviour_sha256:v.behaviour_sha256.clone(),connection:d.connection.clone()});
        }
        let snapshot=PoolSnapshot::freeze(policy.id,policy.revision,catalogue.catalogue_revision,
            super::digest("fosnie.actor.scope.v2",&(context.actor.user_id,&constraints))?,config.fingerprint()?,candidates,config.max_active_variants)?;
        snapshot.require_variant(selected)?;
        if policy.technical_fallback && groups.pin.is_none() && legacy_pin.is_none() {
            if let Some(id)=groups.fallback_variants.iter().find(|id|**id!=selected.id && snapshot.candidates.iter().any(|c|c.variant.id==**id)) {
                let (v,p)=authorised[id];recovery=Some(super::recovery::Plan{reference:reference(v,p),reference_role:false,technical:true});
            }
        }
        decision.reason_codes.push(format!("pool_snapshot:{}",snapshot.hash));profile_snapshot=Some(snapshot);
        let (v,p)=authorised[&selected.id];reference(v,p)
    };
    decision.proposed=Some(selected.clone());decision.actual=None;
    let mut assignments=BTreeMap::new();
    for purpose in [CallPurpose::Primary,CallPurpose::ToolLoop,CallPurpose::Final,CallPurpose::UtilityTitle,CallPurpose::UtilitySummary,
        CallPurpose::RetrievalDecompose,CallPurpose::RetrievalGrade,CallPurpose::WebPlan,CallPurpose::WebGrade,CallPurpose::WebSynthesis,
        CallPurpose::ResearchPlan,CallPurpose::ResearchWrite,CallPurpose::Document,context.purpose] {
        let target=if let Some(id)=policy.utility_assignments.get(&purpose) {
            let p=models.iter().find(|p|p.provider_id==*id).ok_or(RoutingError::ProviderUnavailable)?;
            let effective=engine::auto_requirements(p,&req);
            if engine::target_incompatibility(p,&effective,&constraints).is_some(){return Err(RoutingError::NoEligibleModel("helper assignment is incompatible").into());}
            TargetReference{provider_id:p.provider_id,provider_revision:p.provider_revision,profile_revision:p.revision,model:p.model.clone(),effort:effective.effort,variant:None}
        }else{selected.clone()};
        assignments.insert(purpose,target);
    }
    budget.check_deadline()?;
    Ok((ExecutionPlan{id:context.run_id,schema_version:2,actor_user_id:context.actor.user_id,selection:intent.selection.clone(),decision,
        constraints,assignments,fixed_assignments:Default::default(),profile_snapshot,
        vnext_context:Some(ExecutionContext{intent,workload:req,difficulty,recovery})},budget))
}

pub async fn validate_plan(
    state: &crate::AppState,
    context: &RoutingContext,
    plan: &ExecutionPlan,
) -> crate::Result<()> {
    if let Some(v) = &plan.vnext_context {
        v.intent
            .require_support(&[1, 2], &[1, 2])
            .map_err(RoutingError::Invalid)?;
        let same = v.intent.selection == plan.selection
            || matches!((&v.intent.selection,&plan.selection),
            (ModelSelection::Auto{objective:a,policy_id:None},ModelSelection::Auto{objective:b,policy_id:Some(id)}) if a==b && *id==plan.decision.policy_id);
        if plan.schema_version != 2 || !same {
            return Err(RoutingError::NeedsReplan.into());
        }
    }
    if let Some(snapshot) = &plan.profile_snapshot {
        if plan.schema_version != 2 {
            return Err(RoutingError::Invalid("snapshot requires v2 plan").into());
        }
        let config = profiles::load(state, &context.actor, plan.decision.policy_id).await?;
        snapshot.validate(config.max_active_variants)?;
        if snapshot.policy_id != plan.decision.policy_id
            || snapshot.policy_revision != plan.decision.policy_revision
            || snapshot.profile_config_sha256 != config.fingerprint()?
            || variants::catalogue(state, &context.actor)
                .await?
                .catalogue_revision
                != snapshot.catalogue_revision
        {
            return Err(RoutingError::NeedsReplan.into());
        }
    }
    Ok(())
}
pub fn validate_constraints(
    context: &RoutingContext,
    plan: &ExecutionPlan,
    constraints: &RoutingConstraints,
) -> Result<(), RoutingError> {
    if let Some(snapshot) = &plan.profile_snapshot {
        let hash = super::digest(
            "fosnie.actor.scope.v2",
            &(context.actor.user_id, constraints),
        )?;
        if hash != snapshot.authorisation_scope_sha256 {
            return Err(RoutingError::NeedsReplan);
        }
    }
    Ok(())
}
pub async fn validate_stage(
    state: &crate::AppState,
    context: &RoutingContext,
    plan: &ExecutionPlan,
    _req: &WorkloadRequirements,
    reference: &TargetReference,
) -> crate::Result<()> {
    if plan.profile_snapshot.is_some() && reference.variant.is_none() {
        let policy=store::load_policy_supported(state,&context.actor,Some(plan.decision.policy_id),2).await?;
        if policy.revision!=plan.decision.policy_revision || policy.utility_assignments.get(&context.purpose)!=Some(&reference.provider_id){
            return Err(RoutingError::NeedsReplan.into());
        }
        return Ok(());
    }
    if let Some(snapshot) = &plan.profile_snapshot {
        let config = profiles::load(state, &context.actor, plan.decision.policy_id).await?;
        let binding = reference
            .variant
            .as_ref()
            .ok_or(RoutingError::NeedsReplan)?;
        snapshot.require_variant(binding.variant)?;
        let workload = &plan
            .vnext_context
            .as_ref()
            .ok_or(RoutingError::NeedsReplan)?
            .workload;
        let group = config.resolve_groups(
            &snapshot.candidates.iter().map(|c| c.variant.id).collect(),
            workload,
            context.purpose,
            plan.vnext_context.as_ref().map(|v|v.difficulty).unwrap_or(DifficultySignal::Unknown),
        )?;
        let declared_fallback = plan
            .decision
            .reason_codes
            .iter()
            .any(|r| r == "declared_explicit_fallback");
        let permitted = if declared_fallback {
            group.fallback_variants.contains(&binding.variant.id)
        } else if plan
            .decision
            .reason_codes
            .iter()
            .any(|s| s == "uncertain_task_default" || s == "no_matching_role_default")
        {
            group.reference_variants.contains(&binding.variant.id)
        } else {
            group.allowed_variants.contains(&binding.variant.id)
        };
        if !permitted || group.pin.is_some_and(|v| v != binding.variant) {
            return Err(RoutingError::NoEligibleModel(
                "stage conflicts with task group assignment",
            )
            .into());
        }
    }
    Ok(())
}

/// The original RunBudget still reserves from caps at dispatch. Extended
/// reasoning/cache rates are projected conservatively, never from forecasts.
pub(crate) async fn reservation_profiles(
    state: &crate::AppState,
    context: &RoutingContext,
    assignments: &BTreeMap<CallPurpose, InferenceTarget>,
    profiles: &mut [ModelProfile],
) -> crate::Result<()> {
    if !assignments.values().any(|t| t.variant.is_some()) {
        return Ok(());
    }
    let catalogue = variants::catalogue(state, &context.actor).await?;
    for target in assignments.values().filter(|t| t.variant.is_some()) {
        let deployment = catalogue
            .deployments
            .iter()
            .find(|d| d.id == target.provider_id)
            .ok_or(RoutingError::NeedsReplan)?;
        let profile = profiles
            .iter_mut()
            .find(|p| p.provider_id == target.provider_id)
            .ok_or(RoutingError::NeedsReplan)?;
        if deployment.provider_revision != target.provider_revision {
            return Err(RoutingError::NeedsReplan.into());
        }
        if deployment
            .tariffs
            .reasoning_output_microusd_per_million
            .is_some()
            || deployment
                .tariffs
                .cached_input_microusd_per_million
                .is_some()
        {
            profile.pricing = super::cost::reservation_pricing(&deployment.tariffs);
        }
    }
    Ok(())
}
