// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Safe-point retry for the stateless public text API only. Stateful callers
//! keep their pinned plan: they cannot replay a tool loop or a durable phase.
use super::*;
use crate::model_routing::fallback::{Attempt,AttemptState,FailureKind};

impl RoutedRun {
    async fn retry_plan(&self,state:&crate::AppState,failure:FailureKind)->crate::Result<Option<(RoutingConstraints,Attempt)>> {
        let Some(witness)=self.0.retry.clone() else {return Ok(None);};
        let requirements=&witness.requirements;
        self.guard(async {
            let policy=store::load_policy(state,&self.0.context.actor,Some(self.plan().decision.policy_id)).await?;
            if policy.revision!=self.plan().decision.policy_revision {return Err(RoutingError::NeedsReplan.into());}
            if !AttemptState::default().can_retry(&policy,&self.plan().selection,failure) {return Ok(None);}
            engine::validate_policy(&policy)?;
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
            // Rank against remaining allowance; retain absolute caps in the
            // ledger. Dispatch atomically reserves the second call.
            let mut remaining=constraints.clone();
            remaining.max_calls=remaining.max_calls.map(|n|n.saturating_sub(used.calls));
            remaining.max_total_tokens=remaining.max_total_tokens.map(|n|n.saturating_sub(used.tokens));
            remaining.max_cost_microusd=remaining.max_cost_microusd.map(|n|n.saturating_sub(used.cost_microusd));
            let failed=self.plan().assignments[&CallPurpose::Final].clone();
            let profiles:Vec<_>=store::current_profiles(store::catalog(state,&context.actor).await?)
                .into_iter().filter(|p|p.provider_id!=failed.provider_id).collect();
            let objective=match self.plan().selection {ModelSelection::Auto{objective,..}=>objective,_=>return Ok(None)};
            let rule=engine::RuleMatch{allowed_providers:witness.allowed_providers.clone(),..Default::default()};
            let profiles=super::super::health::filter(state,profiles,CallPurpose::Final).await?;
            let selected=engine::select(&profiles,requirements,&policy,&remaining,objective,&rule,false)?;
            let target=selected.proposed.ok_or(RoutingError::ProviderUnavailable)?;
            Ok(Some((constraints,Attempt {retry_index:1,failed_target:failed,planned_target:target,failure})))
        }).await
    }
    /// Only the private general-text browser witness may use this fork. The
    /// old runtime remains immutable; cancellation and already spent budget stay shared.
    pub(crate) async fn retry_browser_before_output(&self,state:&crate::AppState,failure:FailureKind)->crate::Result<Option<Self>> {
        self.validate_access().await?;
        if self.browser_chat()!=self.0.context.chat_id || self.browser_chat().is_none()
            || !self.0.access.as_ref().is_some_and(|a|a.plain_text_retry())
            || self.0.durable.is_some() || self.0.artefact.is_some() || !self.plan().fixed_assignments.is_empty()
            || self.0.children.load(std::sync::atomic::Ordering::SeqCst)!=0 || self.0.browser_retry_claimed.load(std::sync::atomic::Ordering::SeqCst)
            || self.0.retry.as_ref().is_none_or(|w|w.requirements.tools) {
            return Ok(None);
        }
        let _idle=self.0.serial.try_lock().map_err(|_|RoutingError::Incompatible("retry requires an idle text run"))?;
        let Some((constraints,attempt))=self.retry_plan(state,failure).await? else {return Ok(None);};
        if self.0.browser_retry_claimed.compare_exchange(false,true,std::sync::atomic::Ordering::SeqCst,std::sync::atomic::Ordering::SeqCst).is_err() {
            return Ok(None);
        }
        if self.0.children.load(std::sync::atomic::Ordering::SeqCst)!=0 {return Err(RoutingError::Incompatible("a helper started during retry planning").into());}
        let mut event=crate::audit::AuditEvent::action("routing.fallback",self.0.context.actor.role.as_str());
        event.actor_user_id=self.0.context.actor.user_id;event.resource_type=Some("chat".into());event.resource_id=self.0.context.chat_id;
        event.model_agent_traceability=Some(serde_json::json!({"run_id":self.plan().id,"attempt":attempt,"usage":self.usage(),"scope":"browser_plain_text_before_output"}));
        self.guard(async {crate::audit::append(&state.pg,&event).await?;Ok(())}).await?;
        let mut plan=self.plan().clone();
        for purpose in [CallPurpose::Primary,CallPurpose::ToolLoop,CallPurpose::Final] {
            plan.assignments.insert(purpose,attempt.planned_target.clone());
        }
        // Explicit helper assignments are not replaced. No helpers are started
        // on this bounded retry path; browser title generation is deferred/skipped.
        plan.constraints=constraints;plan.decision.reason_codes.push("technical_fallback".into());plan.decision.estimated_cost_microusd=None;
        Ok(Some(Self(std::sync::Arc::new(Inner {context:self.0.context.clone(),plan,budget:self.0.budget.clone(),
            client:super::transport::PipelineClient::from_state(state)?,cancelled:self.0.cancelled.clone(),durable:None,
            serial:self.0.serial.clone(),access:self.0.access.clone(),artefact:None,
            children:std::sync::atomic::AtomicUsize::new(0),children_done:tokio::sync::Notify::new(),retry:None,
            fallback:Some(attempt),quality:None,browser_retry_claimed:self.0.browser_retry_claimed.clone()}))))
    }
    pub(crate) fn browser_retried(&self)->bool {self.0.browser_retry_claimed.load(std::sync::atomic::Ordering::SeqCst)}

    pub(crate) fn fallback_attempt(&self)->Option<&Attempt> {self.0.fallback.as_ref()}

    /// Caller must have dropped the failed stream before its first event. Unique
    /// runtime ownership also proves there are no outstanding helpers/streams.
    pub(crate) async fn retry_stateless(&mut self,state:&crate::AppState,failure:FailureKind)->crate::Result<bool> {
        self.check()?;
        let Some(witness)=self.0.retry.clone() else {return Ok(false);};
        let requirements=&witness.requirements;
        if Arc::strong_count(&self.0)!=1 || self.0.durable.is_some() || self.0.access.is_some()
            || self.0.artefact.is_some() || self.0.context.chat_id.is_some() || self.0.context.project_id.is_some()
            || self.0.context.purpose!=CallPurpose::Primary || !self.0.plan.fixed_assignments.is_empty()
            || requirements.tools || self.0.children.load(std::sync::atomic::Ordering::SeqCst)!=0 {
            return Err(RoutingError::Incompatible("technical retry requires an exclusive stateless text run").into());
        }
        let planned=self.retry_plan(state,failure).await?;
        let Some((constraints,attempt))=planned else {return Ok(false);};
        // An audit error cannot permit a third attempt. This witness is never
        // serialized into a resumable plan.
        Arc::get_mut(&mut self.0).ok_or(RoutingError::NeedsReplan)?.retry.take();
        self.guard(async {
            let mut event=crate::audit::AuditEvent::action("routing.fallback",self.0.context.actor.role.as_str());
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
        inner.plan.decision.reason_codes.push("technical_fallback".into());
        inner.plan.decision.estimated_cost_microusd=None;
        inner.fallback=Some(attempt);
        Ok(true)
    }
}
