// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::*;

/// Explicit selection for a bounded purpose. Default/manual perform no judge
/// call and keep that purpose on the selected model; helpers must be adopted
/// explicitly rather than inheriting unplanned process defaults.
pub async fn selected_plan(state:&crate::AppState,context:&RoutingContext,selection:ModelSelection,
    requirements:WorkloadRequirements,conversation:&str,cancel:&tokio::sync::Notify)->crate::Result<(ExecutionPlan,budget::RunBudget)> {
    selected_plan_in(state,context,selection,requirements,conversation,cancel,None).await
}

/// Only an owned in-process runtime may carry an existing ledger into planning.
/// A serialized plan or usage snapshot is never enough to start this phase.
pub(super) struct PlanningLedger {pub policy_id:uuid::Uuid,pub policy_revision:i64,pub budget:budget::RunBudget}
pub(super) async fn selected_plan_in(state:&crate::AppState,context:&RoutingContext,selection:ModelSelection,
    requirements:WorkloadRequirements,conversation:&str,cancel:&tokio::sync::Notify,ledger:Option<PlanningLedger>)->crate::Result<(ExecutionPlan,budget::RunBudget)> {
    selected_plan_with_retry(state,context,selection,requirements,conversation,cancel,ledger).await.map(|(plan,budget,_)|(plan,budget))
}
/// The effective requirements are an in-process witness, never restored from a
/// client plan or durable snapshot. A retry cannot discard classifier/rule work.
pub(super) async fn selected_plan_with_retry(state:&crate::AppState,context:&RoutingContext,selection:ModelSelection,
    requirements:WorkloadRequirements,conversation:&str,cancel:&tokio::sync::Notify,ledger:Option<PlanningLedger>)
    ->crate::Result<(ExecutionPlan,budget::RunBudget,Option<super::fallback::RetryRequirements>)> {
    if let ModelSelection::Auto {objective,policy_id}=&selection {
        return auto_plan_in(state,context,*policy_id,*objective,requirements,conversation,cancel,ledger).await;
    }
    let policy=store::load_policy(state,&context.actor,None).await?;
    engine::validate_policy(&policy)?;
    let constraints=store::deployment_constraints(state).await?.intersect(&policy.constraints)
        .intersect(&state.inference_policy.constraints(&state.pg,context).await?);
    let constraints=carried_constraints(&ledger,constraints);
    let budget=planning_budget(ledger,&policy,&constraints)?;
    let id=match &selection {ModelSelection::Manual {provider_id}=>*provider_id,
        _=>policy.default_provider_id.ok_or(RoutingError::NoEligibleModel("default is required"))?};
    let profiles=store::current_profiles(store::catalog(state,&context.actor).await?);
    let profile=profiles.iter().find(|p|p.provider_id==id).ok_or(RoutingError::ProviderUnavailable)?;
    if let Some(reason)=engine::target_incompatibility(profile,&requirements,&constraints) {return Err(RoutingError::Incompatible(reason).into());}
    let reference=TargetReference {provider_id:id,provider_revision:profile.provider_revision,profile_revision:profile.revision,
        model:profile.model.clone(),effort:requirements.effort};
    let mut decision=empty_decision(context,&policy,if matches!(selection,ModelSelection::Manual {..}) {"manual_selection"} else {"default_selection"});
    decision.proposed=Some(reference.clone());
    let mut assignments:std::collections::BTreeMap<_,_>=[CallPurpose::Primary,CallPurpose::ToolLoop,CallPurpose::Final,context.purpose]
        .into_iter().map(|p|(p,reference.clone())).collect();
    // Retrieval helpers use the explicit/default target too. Their capabilities
    // are checked at dispatch; utility overrides must not replace this choice.
    if context.purpose==CallPurpose::Primary {
        for purpose in [CallPurpose::RetrievalDecompose,CallPurpose::RetrievalGrade,CallPurpose::UtilitySummary,CallPurpose::UtilityTitle,
            CallPurpose::WebPlan,CallPurpose::WebGrade,CallPurpose::WebSynthesis,CallPurpose::Document] {
            assignments.insert(purpose,reference.clone());
        }
    }
    // The bounded Research workflow explicitly adopts these child purposes.
    if context.purpose==CallPurpose::ResearchPlan {
        for purpose in [CallPurpose::Document,CallPurpose::ResearchWrite,CallPurpose::UtilitySummary,CallPurpose::UtilityTitle] {
            assignments.insert(purpose,reference.clone());
        }
    }
    budget.check_deadline()?;
    Ok((ExecutionPlan {id:context.run_id,schema_version:SCHEMA_VERSION,actor_user_id:context.actor.user_id,selection,decision,
        constraints,assignments,fixed_assignments:Default::default()},budget,None))
}

/// Build an Auto plan. The only possible inference here is ONE explicitly
/// assigned classifier call. Main/tool/final remain pinned to one actual target.
pub async fn auto_plan(
    state: &crate::AppState, context: &RoutingContext, policy_id: Option<uuid::Uuid>,
    objective: RoutingObjective, requirements: WorkloadRequirements,
    conversation: &str, cancel: &tokio::sync::Notify,
) -> crate::Result<(ExecutionPlan, budget::RunBudget)> {
    auto_plan_in(state,context,policy_id,objective,requirements,conversation,cancel,None).await.map(|(plan,budget,_)|(plan,budget))
}
async fn auto_plan_in(
    state:&crate::AppState,context:&RoutingContext,policy_id:Option<uuid::Uuid>,objective:RoutingObjective,
    mut requirements:WorkloadRequirements,conversation:&str,cancel:&tokio::sync::Notify,ledger:Option<PlanningLedger>,
) -> crate::Result<(ExecutionPlan,budget::RunBudget,Option<super::fallback::RetryRequirements>)> {
    let policy = store::load_policy(state, &context.actor, policy_id).await?;
    engine::validate_policy(&policy)?;
    let constraints = store::deployment_constraints(state).await?
        .intersect(&policy.constraints)
        .intersect(&state.inference_policy.constraints(&state.pg, context).await?);
    let constraints=carried_constraints(&ledger,constraints);
    let budget = planning_budget(ledger,&policy,&constraints)?;
    let actual_requirements = requirements.clone();
    let profiles = store::current_profiles(store::catalog(state, &context.actor).await?);
    let mut rule = engine::RuleMatch::default();
    let mut classified = false;
    let mut classifier_failure = false;
    let mut classifier_skip = None;
    let mut single_candidate = false;
    if policy.rollout != RolloutMode::Off {
        if policy.strategy != RoutingStrategy::Classifier {
            rule = engine::apply_rules(&policy, &mut requirements, context.purpose)?;
        }
        let main_constraints=constraints.intersect(&RoutingConstraints {allowed_providers:rule.allowed_providers.clone(),..Default::default()});
        let empty_pool=main_constraints.allowed_providers.as_ref().is_some_and(|pool|pool.is_empty());
        if empty_pool && policy.rollout==RolloutMode::Active {
            return Err(RoutingError::NoEligibleModel("executor pool is empty").into());
        }
        if state.inference_policy.single_candidate_fast_path() && policy.rollout==RolloutMode::Active && !policy.learned_ranking
            && policy.strategy!=RoutingStrategy::Rules && rule.target.is_none() && !rule.classify {
            let eligible:Vec<_>=profiles.iter().filter(|p|engine::incompatibility(p,&requirements,&main_constraints).is_none()).collect();
            // Learned ranking still needs classified workload evidence to enforce
            // its quality floor, even when only one candidate remains.
            // An economy candidate below the initial minimum tier can still be
            // chosen after task classification; it is not an unavailable choice.
            let selectable=profiles.iter().filter(|p|p.auto_enabled && p.quality_status!=EvidenceStatus::Unknown
                && engine::target_incompatibility(p,&engine::auto_requirements(p,&requirements),&main_constraints).is_none()).count();
            // Unknown tasks can become excluded after classification. A sole
            // visible model is not permission to bypass its workload restrictions.
            if eligible.len()==1 && selectable==1
                && (requirements.task!=TaskKind::Unknown || eligible[0].task_exclusions.is_empty()) {
                rule.target=Some(eligible[0].provider_id);rule.effort=requirements.effort.clone();single_candidate=true;
            }
        }
        if !empty_pool && policy.strategy != RoutingStrategy::Rules && rule.target.is_none() {
            // Cancellation wins even when the model would otherwise fall back.
            let never_cancel = tokio::sync::Notify::new();
            let result = tokio::select! {
                biased;
                _ = cancel.notified() => return Err(RoutingError::Cancelled.into()),
                result = classifier::classify_for_plan(state, context, &policy, &profiles, &constraints, &requirements, conversation, &budget, &never_cancel) => result,
            };
            match result {
                Ok(classifier::ClassificationOutcome::Classified(result)) => { requirements = result; classified = true; }
                Ok(classifier::ClassificationOutcome::Skipped(reason)) => {
                    requirements.uncertain=true;
                    classifier_skip=Some(reason);
                }
                Err(error) => {
                    tracing::warn!(%error, "routing classifier unavailable; selecting checked default");
                    requirements.uncertain = true;
                    classifier_failure = true;
                }
            }
        }
    }
    if classifier_skip.is_some() {
        engine::classifier_free_default(&profiles,&requirements,&policy,&constraints)?;
    }
    let use_default = policy.rollout == RolloutMode::Off || (rule.target.is_none() && !classified);
    budget.check_deadline()?;
    let mut decision = if policy.rollout == RolloutMode::Off {
        empty_decision(context, &policy, "routing_off")
    } else {
        match engine::select_for(context.purpose,&profiles, &requirements, &policy, &constraints, objective, &rule, use_default) {
            Ok(decision) => decision,
            Err(_) if policy.rollout == RolloutMode::Shadow => empty_decision(context, &policy, "shadow_no_eligible_proposal"),
            Err(error) => return Err(error.into()),
        }
    };
    if classifier_failure { decision.reason_codes.push("classifier_fallback".into()); }
    if classified { decision.reason_codes.push("classified_requirements".into()); }
    if single_candidate {decision.reason_codes.push("single_eligible_model".into());}
    if let Some(reason)=classifier_skip {decision.reason_codes.push(reason.into());}
    let actual = match policy.rollout {
        RolloutMode::Active => decision.proposed.clone().ok_or(RoutingError::NoEligibleModel("no proposal"))?,
        RolloutMode::Off | RolloutMode::Shadow => {
            decision.reason_codes.push(if policy.rollout == RolloutMode::Off { "routing_off" } else { "shadow_default_planned" }.into());
            engine::checked_default(&profiles,&actual_requirements,&policy,&constraints)?
        }
    };
    let mut assignments = std::collections::BTreeMap::new();
    for purpose in [CallPurpose::Primary, CallPurpose::ToolLoop, CallPurpose::Final, CallPurpose::UtilityTitle,
        CallPurpose::UtilitySummary, CallPurpose::RetrievalDecompose, CallPurpose::RetrievalGrade,
        CallPurpose::WebPlan, CallPurpose::WebGrade, CallPurpose::WebSynthesis, CallPurpose::ResearchPlan,
        CallPurpose::ResearchWrite, CallPurpose::Document, CallPurpose::Voice, CallPurpose::Moderation] {
        let assigned = (policy.rollout == RolloutMode::Active).then(|| policy.utility_assignments.get(&purpose)).flatten();
        let reference = if let Some(id) = assigned {
            let profile = profiles.iter().find(|p| p.provider_id == *id).ok_or(RoutingError::ProviderUnavailable)?;
            let locality = if profile.locality_trusted { profile.locality } else { Locality::Unknown };
            if !constraints.permits(*id, locality) { return Err(RoutingError::Forbidden.into()); }
            TargetReference { provider_id:*id,provider_revision:profile.provider_revision,
                profile_revision:profile.revision,model:profile.model.clone(),effort:profile.auto_effort.clone() }
        } else { actual.clone() };
        assignments.insert(purpose, reference);
    }
    // `actual` is populated by dispatch, not by planning or dry-run.
    decision.actual = None;
    // Only an explicit request/rule effort constrains the next candidate.
    // Resolved profile presets remain on each pinned target, not the retry witness.
    if let Some(effort)=rule.effort {requirements.effort=Some(effort);}
    let retry=((policy.technical_fallback || policy.quality_escalation) && policy.rollout==RolloutMode::Active).then_some(super::fallback::RetryRequirements {
        requirements,allowed_providers:rule.allowed_providers,
    });
    Ok((ExecutionPlan { id:context.run_id,schema_version:SCHEMA_VERSION,actor_user_id:context.actor.user_id,
        selection:ModelSelection::Auto { objective,policy_id:Some(policy.id) },decision,constraints,assignments,fixed_assignments:Default::default() },budget,retry))
}

pub(super) fn empty_decision(context: &RoutingContext, policy: &RoutingPolicy, reason: &str) -> RoutingDecision {
    RoutingDecision { id:context.run_id,schema_version:SCHEMA_VERSION,policy_id:policy.id,policy_revision:policy.revision,
        strategy:policy.strategy,proposed:None,actual:None,reason_codes:vec![reason.into()],excluded_counts:Default::default(),estimated_cost_microusd:None }
}

fn planning_budget(ledger:Option<PlanningLedger>,policy:&RoutingPolicy,constraints:&RoutingConstraints)->crate::Result<budget::RunBudget> {
    let budget=if let Some(ledger)=ledger {
        if ledger.policy_id!=policy.id || ledger.policy_revision!=policy.revision {return Err(RoutingError::NeedsReplan.into());}
        ledger.budget
    } else {budget::RunBudget::new(constraints.clone())};
    budget.restrict(constraints);budget.check_deadline()?;
    if budget.snapshot().exhausted {return Err(RoutingError::BudgetExhausted.into());}
    Ok(budget)
}
fn carried_constraints(ledger:&Option<PlanningLedger>,constraints:RoutingConstraints)->RoutingConstraints {
    ledger.as_ref().map_or(constraints.clone(),|l|constraints.intersect(&l.budget.limits()))
}
