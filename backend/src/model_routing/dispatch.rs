// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Resolve only the purposes used by this ML request, just before dispatch.
//! Decrypted targets never enter the durable execution plan.
use std::{collections::BTreeMap, time::Duration};
use super::{*, budget::RunBudget, lease::{Allowance, GrantedLease, LeaseReport}};

pub struct StageRequest { pub requirements: WorkloadRequirements, pub max_calls: u32 }

pub struct PreparedRequest {
    pub envelope: InferenceEnvelope,
    grant: GrantedLease,
}
impl PreparedRequest {
    pub fn finish(self, report: Option<&LeaseReport>) -> crate::Result<()> {
        self.grant.finish(report).map_err(Into::into)
    }
}

pub async fn prepare(
    state: &crate::AppState, context: &RoutingContext, plan: &ExecutionPlan, budget: &RunBudget,
    stages: &BTreeMap<CallPurpose,StageRequest>, timeout: Duration,
) -> crate::Result<PreparedRequest> {
    if plan.schema_version != SCHEMA_VERSION || plan.id != context.run_id || plan.actor_user_id != context.actor.user_id {
        return Err(RoutingError::Forbidden.into());
    }
    if !stages.contains_key(&context.purpose) { return Err(RoutingError::Invalid("missing request purpose").into()); }
    let policy = store::load_policy(state,&context.actor,Some(plan.decision.policy_id)).await?;
    if policy.revision != plan.decision.policy_revision { return Err(RoutingError::NeedsReplan.into()); }
    let mut constraints = plan.constraints.intersect(&policy.constraints).intersect(&store::deployment_constraints(state).await?);
    let profiles = store::current_profiles(store::catalog(state,&context.actor).await?);
    // Collect restrictions for the whole fan-out before decrypting any target.
    for &purpose in stages.keys() {
        if matches!(purpose,CallPurpose::Embed|CallPurpose::Rerank|CallPurpose::Ocr|CallPurpose::Stt|CallPurpose::Tts|CallPurpose::Verify) {
            return Err(RoutingError::Invalid("fixed capability requires its own dispatch contract").into());
        }
        let mut purpose_context = context.clone(); purpose_context.purpose=purpose;
        constraints = constraints.intersect(&state.inference_policy.constraints(&state.pg,&purpose_context).await?);
    }
    for (&purpose,stage) in stages {
        let reference = plan.assignments.get(&purpose).ok_or(RoutingError::Invalid("unplanned purpose"))?;
        let profile = profiles.iter().find(|p| p.provider_id==reference.provider_id
            && p.revision==reference.profile_revision && p.provider_revision==reference.provider_revision
            && p.model==reference.model).ok_or(RoutingError::NeedsReplan)?;
        if stage.requirements.effort != reference.effort { return Err(RoutingError::Incompatible("request effort differs from plan").into()); }
        if let Some(reason) = engine::target_incompatibility(profile,&stage.requirements,&constraints) {
            return Err(RoutingError::Incompatible(reason).into());
        }
    }
    budget.restrict(&constraints);
    let mut assignments = BTreeMap::new();
    for &purpose in stages.keys() {
        let mut purpose_context=context.clone(); purpose_context.purpose=purpose;
        let target=resolve_target(state,&purpose_context,&plan.assignments[&purpose],&constraints).await?;
        assignments.insert(purpose,target);
    }
    if store::load_policy(state,&context.actor,Some(policy.id)).await?.revision != policy.revision {
        return Err(RoutingError::NeedsReplan.into());
    }
    let allowances=stages.iter().map(|(&purpose,s)| (purpose,Allowance { max_calls:s.max_calls,
        input_tokens:s.requirements.input_tokens,output_tokens:s.requirements.output_tokens })).collect();
    let grant=GrantedLease::reserve(budget,plan.decision.id,&assignments,&profiles,allowances,timeout)?;
    let envelope=InferenceEnvelope { schema_version:SCHEMA_VERSION,decision_id:plan.decision.id,
        purpose:context.purpose,assignments,budget:Some(grant.wire.clone()),fixed_assignments:Default::default(),fixed_budget:None };
    Ok(PreparedRequest { envelope,grant })
}

/// One planned non-streaming call. Tool execution and persistence belong to the
/// orchestrator; this function never retries or executes returned tools.
pub async fn chat_step(
    state: &crate::AppState, context: &RoutingContext, plan: &ExecutionPlan, budget: &RunBudget,
    messages: &[crate::ml::Message], tools: Option<&[serde_json::Value]>, sampling: &crate::ml::Sampling,
    cancel: &tokio::sync::Notify,
) -> crate::Result<crate::ml::ChatStep> {
    let future = async {
        let reference=plan.assignments.get(&context.purpose).ok_or(RoutingError::Invalid("unplanned purpose"))?;
        let requirements=call_requirements(reference,messages,tools,sampling)?;
        let prepared=prepare(state,context,plan,budget,&[(context.purpose,StageRequest { requirements,max_calls:1 })].into(),Duration::from_secs(120)).await?;
        let overrides=prepared.envelope.apply(Default::default())?;
        let result=tokio::time::timeout(budget.remaining(Duration::from_secs(120)),
            crate::ml::chat_step(&state.http,&state.boot.ml.base_url,messages,tools,sampling,overrides))
            .await.map_err(|_|crate::AppError::Unavailable("routed inference timed out".into()))??;
        prepared.finish(result.routing_report.as_ref())?;
        Ok(result)
    };
    tokio::select! {
        biased;
        _ = cancel.notified() => Err(RoutingError::Cancelled.into()),
        result = future => result,
    }
}

pub(super) fn call_requirements(reference: &TargetReference, messages: &[crate::ml::Message],
    tools: Option<&[serde_json::Value]>, sampling: &crate::ml::Sampling) -> Result<WorkloadRequirements,RoutingError> {
    let output=u32::try_from(sampling.max_tokens.ok_or(RoutingError::Invalid("routed output limit is required"))?)
        .map_err(|_|RoutingError::Invalid("routed output limit must be positive"))?;
    if output==0 { return Err(RoutingError::Invalid("routed output limit must be positive")); }
    if sampling.reasoning_effort.as_ref().is_some_and(|e|Some(e)!=reference.effort.as_ref()) {
        return Err(RoutingError::Incompatible("sampling effort conflicts with execution plan"));
    }
    let extra=schema::overhead(sampling.response_schema.as_ref())?;
    let input=u32::try_from(serde_json::to_vec(&serde_json::json!({"messages":messages,"tools":tools}))
        .map_err(|_|RoutingError::Invalid("invalid model input"))?.len()+1024+extra).map_err(|_|RoutingError::BudgetExhausted)?;
    if messages.iter().any(|m| m.get("content").and_then(|v|v.as_array()).is_some_and(|parts|
        parts.iter().any(|p|p.get("type").and_then(|v|v.as_str())!=Some("text")))) {
        return Err(RoutingError::Incompatible("bounded multimodal inference requires token accounting"));
    }
    Ok(WorkloadRequirements { input_tokens:input,output_tokens:output,
        tools:tools.is_some_and(|t|!t.is_empty()),structured_output:sampling.response_schema.is_some(),effort:reference.effort.clone(),..Default::default() })
}

pub struct PlannedStream {
    source: Option<crate::ml::GenStream>,
    prepared: Option<PreparedRequest>,
    budget: RunBudget,
    deadline: tokio::time::Instant,
}
impl PlannedStream {
    /// Terminal/error/deadline closes upstream immediately. EOF without Done is
    /// an error and retains the reservation; it is never a successful free call.
    pub async fn recv(&mut self, cancel: &tokio::sync::Notify) -> crate::Result<Option<crate::ml::GenEvent>> {
        let Some(source)=self.source.as_mut() else { return Ok(None); };
        let wait=self.budget.remaining(self.deadline.saturating_duration_since(tokio::time::Instant::now()));
        if wait.is_zero() {
            self.source.take(); self.prepared.take();
            return Err(RoutingError::BudgetExhausted.into());
        }
        let event=tokio::select! {
            biased;
            _ = cancel.notified() => {
                self.source.take(); self.prepared.take();
                return Err(RoutingError::Cancelled.into());
            }
            event = tokio::time::timeout(wait,source.recv()) => event,
        };
        match event {
            Ok(Some(event @ crate::ml::GenEvent::Done { .. })) => {
                self.source.take();
                if let crate::ml::GenEvent::Done { ref routing_report,.. }=event {
                    self.prepared.take().unwrap().finish(routing_report.as_ref())?;
                }
                Ok(Some(event))
            }
            Ok(Some(event @ crate::ml::GenEvent::Error { .. })) => {
                self.source.take(); self.prepared.take(); Ok(Some(event))
            }
            Ok(Some(event)) => Ok(Some(event)),
            _ => {
                self.source.take(); self.prepared.take();
                Err(crate::AppError::Unavailable("routed stream ended before completion".into()))
            }
        }
    }
}

pub async fn generate(
    state: &crate::AppState, context: &RoutingContext, plan: &ExecutionPlan, budget: &RunBudget,
    mut request: crate::ml::GenerateRequest, cancel: &tokio::sync::Notify,
) -> crate::Result<PlannedStream> {
    let future=async {
        let timeout=Duration::from_secs(120);
        let reference=plan.assignments.get(&context.purpose).ok_or(RoutingError::Invalid("unplanned purpose"))?;
        if request.model.as_ref().is_some_and(|m|m!=&reference.model) {
            return Err(RoutingError::Incompatible("generation model conflicts with execution plan").into());
        }
        let requirements=call_requirements(reference,&request.messages,request.tools.as_deref(),&request.sampling)?;
        let prepared=prepare(state,context,plan,budget,&[(context.purpose,StageRequest { requirements,max_calls:1 })].into(),timeout).await?;
        request.overrides=prepared.envelope.apply(request.overrides)?;
        let deadline=tokio::time::Instant::now()+budget.remaining(timeout);
        let source=tokio::time::timeout_at(deadline,crate::ml::generate(&state.http,&state.boot.ml.base_url,&request)).await
            .map_err(|_|crate::AppError::Unavailable("routed stream connection timed out".into()))??;
        Ok(PlannedStream { source:Some(source),prepared:Some(prepared),budget:budget.clone(),deadline })
    };
    tokio::select! {
        biased;
        _ = cancel.notified() => Err(RoutingError::Cancelled.into()),
        result = future => result,
    }
}
