// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! v2 recovery uses the same runtime witness, retry bit, leases and RunBudget.
use super::*;
use crate::model_routing::{
    fallback::{Attempt, FailureKind},
    quality::QualityFailure,
};
impl RoutedRun {
    pub(super) async fn vnext_retry_plan(
        &self,
        state: &crate::AppState,
        failure: FailureKind,
    ) -> crate::Result<Option<(RoutingConstraints, Attempt)>> {
        self.vnext_replacement(state, failure, None).await
    }
    async fn vnext_replacement(
        &self,
        state: &crate::AppState,
        failure: FailureKind,
        quality: Option<QualityFailure>,
    ) -> crate::Result<Option<(RoutingConstraints, Attempt)>> {
        if quality.is_some(){return Ok(None);}
        let Some(v) = &self.plan().vnext_context else {
            return Ok(None);
        };
        let Some(r) = &v.recovery else {
            return Ok(None);
        };
        let failed = self.plan().assignments[&CallPurpose::Final].clone();
        if self.browser_retried()
            || failed.provider_id == r.reference.provider_id
                && failed.variant == r.reference.variant
            || !matches!(self.plan().selection, ModelSelection::Auto { .. })
            || (quality.is_none()
                && (!r.technical
                    || !matches!(
                        failure,
                        FailureKind::Connection
                            | FailureKind::RateLimited
                            | FailureKind::Unavailable
                            | FailureKind::Timeout
                            | FailureKind::QuotaExhausted
                            | FailureKind::PaymentRequired
                            | FailureKind::ModelUnavailable
                    )))
        {
            return Ok(None);
        }
        self.guard(async {
            let policy = store::load_policy_supported(
                state,
                &self.0.context.actor,
                Some(self.plan().decision.policy_id),
                2,
            )
            .await?;
            if policy.revision != self.plan().decision.policy_revision {
                return Err(RoutingError::NeedsReplan.into());
            }
            if quality.is_some() && !policy.quality_escalation
                || quality.is_none() && !policy.technical_fallback
            {
                return Ok(None);
            }
            vnext::execution::validate_plan(state, &self.0.context, self.plan()).await?;
            let config = vnext::profiles::load(state, &self.0.context.actor, policy.id).await?;
            let snapshot = self
                .plan()
                .profile_snapshot
                .as_ref()
                .ok_or(RoutingError::NeedsReplan)?;
            let binding = r
                .reference
                .variant
                .as_ref()
                .ok_or(RoutingError::NeedsReplan)?;
            snapshot.require_variant(binding.variant)?;
            let groups = config.resolve_groups(
                &snapshot.candidates.iter().map(|c| c.variant.id).collect(),
                &v.workload,
                self.0.context.purpose,
                v.difficulty,
            )?;
            if groups.pin.is_some()
                || !groups.fallback_variants.contains(&binding.variant.id)
            {
                return Err(RoutingError::NoEligibleModel(
                    "recovery reference excluded by task groups",
                )
                .into());
            }
            let context = self.context(CallPurpose::Final);
            if super::super::health::blocked(state, &r.reference, CallPurpose::Final)
                .await?
                .is_some()
            {
                return Err(RoutingError::NoEligibleModel(
                    "fallback model is unavailable",
                )
                .into());
            }
            let constraints = self
                .plan()
                .constraints
                .intersect(&policy.constraints)
                .intersect(&store::deployment_constraints(state).await?)
                .intersect(
                    &state
                        .inference_policy
                        .constraints(&state.pg, &self.0.context)
                        .await?,
                )
                .intersect(
                    &state
                        .inference_policy
                        .constraints(&state.pg, &context)
                        .await?,
                );
            self.0.budget.restrict(&constraints);
            let used = self.usage();
            if constraints.max_cost_microusd.is_some() && used.unknown_cost_calls > 0
                || constraints.max_total_tokens.is_some() && used.unknown_token_calls > 0
            {
                return Err(RoutingError::BudgetExhausted.into());
            }
            self.0.budget.check_deadline()?;
            Ok(Some((
                constraints,
                Attempt {
                    retry_index: 1,
                    failed_target: failed,
                    planned_target: r.reference.clone(),
                    failure,
                },
            )))
        })
        .await
    }

    pub(super) fn apply_vnext_replacement(plan:&mut ExecutionPlan,attempt:&Attempt,_quality:Option<QualityFailure>) {
        for purpose in [CallPurpose::Primary,CallPurpose::Final,CallPurpose::ToolLoop] {
            plan.assignments.insert(purpose,attempt.planned_target.clone());
        }
        plan.decision.proposed=Some(attempt.planned_target.clone());plan.decision.actual=None;
        plan.decision.estimated_cost_microusd=None;
        plan.decision.reason_codes.extend(["technical_fallback".into(),"declared_explicit_fallback".into()]);
        if let Some(r)=plan.vnext_context.as_mut().and_then(|v|v.recovery.as_mut()){r.technical=false;}
    }
    pub(super) async fn retry_vnext_stateless(
        &mut self,
        state: &crate::AppState,
        failure: FailureKind,
        quality: Option<QualityFailure>,
    ) -> crate::Result<bool> {
        if quality.is_some(){return Ok(false);}
        self.check()?;
        if Arc::strong_count(&self.0) != 1
            || self.0.durable.is_some()
            || self.0.access.is_some()
            || self.0.artefact.is_some()
            || self.0.context.chat_id.is_some()
            || self.0.context.project_id.is_some()
            || self.0.children.load(std::sync::atomic::Ordering::SeqCst) != 0
            || !self.plan().fixed_assignments.is_empty()
        {
            return Err(RoutingError::Incompatible(
                "recovery requires an exclusive portable text boundary",
            )
            .into());
        }
        let Some((constraints, attempt)) = self.vnext_replacement(state, failure, quality).await?
        else {
            return Ok(false);
        };
        if self
            .0
            .browser_retry_claimed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return Ok(false);
        }
        self.guard(async {let mut event=crate::audit::AuditEvent::action("routing.reference_recovery",self.0.context.actor.role.as_str());event.actor_user_id=self.0.context.actor.user_id;event.resource_type=Some("api_completion".into());event.resource_id=Some(self.plan().id);event.model_agent_traceability=Some(serde_json::json!({"attempt":attempt,"quality_failure":quality,"usage":self.usage()}));crate::audit::append(&state.pg,&event).await?;Ok(())}).await?;
        let inner = Arc::get_mut(&mut self.0).ok_or(RoutingError::NeedsReplan)?;
        inner.retry = None;
        inner.plan.constraints = constraints;
        Self::apply_vnext_replacement(&mut inner.plan, &attempt, quality);
        if let Some(failure) = quality {
            inner.quality = Some(super::super::quality::QualityAttempt {
                failed_target: attempt.failed_target.clone(),
                planned_target: attempt.planned_target.clone(),
                failure,
            });
        }
        inner.fallback = Some(attempt);
        Ok(true)
    }
}
