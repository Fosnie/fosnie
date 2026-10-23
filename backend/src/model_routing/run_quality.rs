// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! One completed, buffered answer may be replaced before any external output.
use super::*;
use crate::model_routing::quality::{QualityAttempt,QualityFailure};

impl RoutedRun {
    pub(crate) fn quality_attempt(&self)->Option<&QualityAttempt> {self.0.quality.as_ref()}

    pub(crate) async fn require_quality_escalation(&self,state:&crate::AppState)->crate::Result<()> {
        if self.plan().profile_snapshot.is_some() {return Err(RoutingError::Incompatible("v2 escalation requires calibrated optimizer").into());}
        self.guard(async {
            let policy=store::load_policy_supported(state,&self.0.context.actor,Some(self.plan().decision.policy_id),self.plan().schema_version).await?;
            if policy.revision!=self.plan().decision.policy_revision {return Err(RoutingError::NeedsReplan.into());}
            if !policy.quality_escalation || policy.rollout!=RolloutMode::Active
                || !matches!(self.plan().selection,ModelSelection::Auto{..}) {
                return Err(RoutingError::Invalid("quality contracts require an active Auto policy with quality escalation enabled").into());
            }
            engine::validate_policy(&policy)?;Ok(())
        }).await
    }

    pub(crate) async fn escalate_stateless(&mut self,state:&crate::AppState,failure:QualityFailure)->crate::Result<bool> {
        self.check()?;
        if self.plan().vnext_context.as_ref().is_some_and(|v|v.recovery.is_some()) {return self.retry_vnext_stateless(state,super::fallback::FailureKind::InvalidRequest,Some(failure)).await;}
        let Some(witness)=self.0.retry.clone() else {return Ok(false);};
        let requirements=&witness.requirements;
        if Arc::strong_count(&self.0)!=1 || self.0.durable.is_some() || self.0.access.is_some()
            || self.0.artefact.is_some() || self.0.context.chat_id.is_some() || self.0.context.project_id.is_some()
            || self.0.context.purpose!=CallPurpose::Primary || !self.0.plan.fixed_assignments.is_empty()
            || requirements.tools || self.0.children.load(std::sync::atomic::Ordering::SeqCst)!=0 {
            return Err(RoutingError::Incompatible("quality escalation requires an exclusive stateless text run").into());
        }
        self.require_quality_escalation(state).await?;
        let (constraints,attempt)=self.guard(async {
            let policy=store::load_policy_supported(state,&self.0.context.actor,Some(self.plan().decision.policy_id),self.plan().schema_version).await?;
            if policy.revision!=self.plan().decision.policy_revision {return Err(RoutingError::NeedsReplan.into());}
            let context=self.context(CallPurpose::Final);
            let constraints=self.plan().constraints.intersect(&policy.constraints)
                .intersect(&store::deployment_constraints(state).await?)
                .intersect(&state.inference_policy.constraints(&state.pg,&self.0.context).await?)
                .intersect(&state.inference_policy.constraints(&state.pg,&context).await?);
            self.0.budget.restrict(&constraints);
            let used=self.usage();
            if constraints.max_cost_microusd.is_some() && used.unknown_cost_calls>0
                || constraints.max_total_tokens.is_some() && used.unknown_token_calls>0 {
                return Err(RoutingError::BudgetExhausted.into());
            }
            let mut remaining=constraints.clone();
            remaining.max_calls=remaining.max_calls.map(|n|n.saturating_sub(used.calls));
            remaining.max_total_tokens=remaining.max_total_tokens.map(|n|n.saturating_sub(used.tokens));
            remaining.max_cost_microusd=remaining.max_cost_microusd.map(|n|n.saturating_sub(used.cost_microusd));
            let failed=self.plan().assignments[&CallPurpose::Final].clone();
            let profiles=store::current_profiles(store::catalog(state,&context.actor).await?);
            let original=profiles.iter().find(|p|p.provider_id==failed.provider_id
                && p.provider_revision==failed.provider_revision && p.revision==failed.profile_revision)
                .ok_or(RoutingError::NeedsReplan)?;
            let tier=|p:&ModelProfile|p.task_tiers.get(&requirements.task).copied().unwrap_or(p.tier);
            let previous=tier(original);
            let profiles:Vec<_>=profiles.into_iter().filter(|p|p.provider_id!=failed.provider_id && tier(p)>previous).collect();
            let objective=match self.plan().selection {ModelSelection::Auto{objective,..}=>objective,_=>return Err(RoutingError::Forbidden.into())};
            let rule=engine::RuleMatch{allowed_providers:witness.allowed_providers.clone(),..Default::default()};
            let selected=engine::select_for(self.0.context.purpose,&profiles,requirements,&policy,&remaining,objective,&rule,false)?;
            let target=selected.proposed.ok_or(RoutingError::ProviderUnavailable)?;
            Ok((constraints,QualityAttempt{failed_target:failed,planned_target:target,failure}))
        }).await?;
        // Shared with technical fallback: neither path can buy a third attempt.
        Arc::get_mut(&mut self.0).ok_or(RoutingError::NeedsReplan)?.retry.take();
        self.guard(async {
            let mut event=crate::audit::AuditEvent::action("routing.quality_escalation",self.0.context.actor.role.as_str());
            event.actor_user_id=self.0.context.actor.user_id;
            event.resource_type=Some("api_completion".into());event.resource_id=Some(self.plan().id);
            event.model_agent_traceability=Some(serde_json::json!({"schema_version":1,"run_id":self.plan().id,
                "decision_id":self.plan().decision.id,"policy_id":self.plan().decision.policy_id,
                "policy_revision":self.plan().decision.policy_revision,"purpose":"final","attempt":attempt,"usage":self.usage()}));
            crate::audit::append(&state.pg,&event).await.map_err(crate::AppError::from)
        }).await?;
        self.check()?;
        let inner=Arc::get_mut(&mut self.0).ok_or(RoutingError::NeedsReplan)?;
        for reference in inner.plan.assignments.values_mut() {
            if reference.provider_id==attempt.failed_target.provider_id {*reference=attempt.planned_target.clone();}
        }
        inner.plan.constraints=constraints;
        inner.plan.decision.reason_codes.push("quality_escalation".into());
        inner.plan.decision.estimated_cost_microusd=None;
        inner.quality=Some(attempt);
        Ok(true)
    }
}
