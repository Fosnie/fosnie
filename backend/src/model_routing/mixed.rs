// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! One preparation and reservation for a request containing both model families.
use std::{collections::{BTreeMap,BTreeSet},time::{Duration,Instant}};
use serde::Deserialize;
use super::{*,budget::RunBudget,dispatch::StageRequest,fixed::{FixedRole,FixedTarget},
    fixed_lease::{FixedAllowance,FixedGrant,FixedReport},lease::{Allowance,GrantedLease,LeaseReport}};

#[cfg(test)]
#[path="mixed_tests.rs"]
mod tests;

#[derive(Debug,Default,Deserialize)]
pub struct Reports {
    #[serde(default)] pub routing_report:Option<LeaseReport>,
    #[serde(default)] pub fixed_routing_report:Option<FixedReport>,
}

pub struct MixedGrant {
    llm:Option<GrantedLease>,fixed:Option<FixedGrant>,budget:RunBudget,deadline:Instant,
}
impl MixedGrant {
    pub(super) fn envelope(&self,decision_id:uuid::Uuid,purpose:CallPurpose,
        assignments:BTreeMap<CallPurpose,InferenceTarget>,fixed_assignments:BTreeMap<FixedRole,FixedTarget>)->InferenceEnvelope {
        InferenceEnvelope {schema_version:SCHEMA_VERSION,decision_id,purpose,assignments,fixed_assignments,
            budget:self.llm.as_ref().map(|g|g.wire.clone()),fixed_budget:self.fixed.as_ref().map(|g|g.wire.clone())}
    }
    pub fn reserve(budget:&RunBudget,decision_id:uuid::Uuid,llm_targets:&BTreeMap<CallPurpose,InferenceTarget>,profiles:&[ModelProfile],
        llm_allowances:BTreeMap<CallPurpose,Allowance>,fixed_targets:&BTreeMap<FixedRole,FixedTarget>,
        fixed_allowances:BTreeMap<FixedRole,FixedAllowance>,timeout:Duration) -> Result<Self,RoutingError> {
        let timeout=budget.remaining(timeout).min(Duration::from_secs(86_400));
        let deadline=Instant::now()+timeout;
        let llm=if llm_allowances.is_empty() { None } else { Some(GrantedLease::spec(budget,decision_id,llm_targets,profiles,llm_allowances,timeout)?) };
        let fixed=if fixed_allowances.is_empty() { None } else { Some(FixedGrant::spec(budget,decision_id,fixed_targets,fixed_allowances,timeout)?) };
        let requests=llm.as_ref().map(|s|s.requests.as_slice()).unwrap_or(&[]);
        let fixed_count=fixed.as_ref().map_or(0,|s|s.count);
        let count=requests.len()+fixed_count as usize;
        if count==0 || count>1024 { return Err(RoutingError::Invalid("mixed request must grant 1 to 1024 calls")); }
        let mut reserved=budget.reserve_mixed(requests,fixed_count)?;
        let fixed_reserved=reserved.split_off(requests.len());
        Ok(Self { llm:llm.map(|s|s.bind(budget,reserved)),fixed:fixed.map(|s|s.bind(budget,fixed_reserved)),budget:budget.clone(),deadline })
    }
    pub fn remaining(&self) -> Result<Duration,RoutingError> {
        if self.budget.snapshot().exhausted { return Err(RoutingError::BudgetExhausted); }
        let remaining=self.budget.remaining(self.deadline.saturating_duration_since(Instant::now()));
        if remaining.is_zero() { return Err(RoutingError::BudgetExhausted); }
        Ok(remaining)
    }
    /// False means at least one report is missing/incomplete: the entire request
    /// stays reserved. Validate both lanes and cross-lane IDs before any refund.
    pub fn finish(self,reports:&Reports) -> Result<bool,RoutingError> {
        if (self.llm.is_none() && reports.routing_report.is_some()) || (self.fixed.is_none() && reports.fixed_routing_report.is_some()) {
            return Err(RoutingError::Invalid("unexpected inference report lane"));
        }
        let mut overrun=false;
        if let (Some(grant),Some(report))=(&self.llm,&reports.routing_report) { overrun|=grant.validate(report)?; }
        if let (Some(grant),Some(report))=(&self.fixed,&reports.fixed_routing_report) { overrun|=grant.validate(report)?; }
        let mut ids=BTreeSet::new();
        for id in reports.routing_report.iter().flat_map(|r|r.calls.iter().map(|c|c.call_id))
            .chain(reports.fixed_routing_report.iter().flat_map(|r|r.calls.iter().map(|c|c.call_id))) {
            if !ids.insert(id) { return Err(RoutingError::Invalid("duplicate call id across inference lanes")); }
        }
        if overrun { self.budget.exhaust(); return Err(RoutingError::BudgetExhausted); }
        if (self.llm.is_some() && !reports.routing_report.as_ref().is_some_and(|r|r.complete))
            || (self.fixed.is_some() && !reports.fixed_routing_report.as_ref().is_some_and(|r|r.complete)) { return Ok(false); }
        if let Some(grant)=self.llm { grant.finish(reports.routing_report.as_ref())?; }
        if let Some(grant)=self.fixed { grant.finish(reports.fixed_routing_report.as_ref())?; }
        Ok(true)
    }
}

pub struct PreparedMixedRequest { pub envelope:InferenceEnvelope,pub(super) grant:MixedGrant }
impl PreparedMixedRequest {
    pub fn finish(self,reports:&Reports) -> crate::Result<bool> { self.grant.finish(reports).map_err(Into::into) }
}

/// Stages without calls (for example an active index used only as the source of
/// a desired-index rebuild) can be explicitly included as read-only references.
pub async fn prepare(state:&crate::AppState,context:&RoutingContext,plan:&ExecutionPlan,budget:&RunBudget,
    stages:&BTreeMap<CallPurpose,StageRequest>,fixed_allowances:BTreeMap<FixedRole,FixedAllowance>,
    fixed_references:&BTreeSet<FixedRole>,timeout:Duration) -> crate::Result<PreparedMixedRequest> {
    if plan.schema_version!=SCHEMA_VERSION || plan.id!=context.run_id || plan.actor_user_id!=context.actor.user_id {
        return Err(RoutingError::Forbidden.into());
    }
    if !stages.contains_key(&context.purpose) && !fixed_allowances.keys().any(|r|r.purpose()==context.purpose) {
        return Err(RoutingError::Invalid("missing mixed request purpose").into());
    }
    let policy=store::load_policy(state,&context.actor,Some(plan.decision.policy_id)).await?;
    if policy.revision!=plan.decision.policy_revision { return Err(RoutingError::NeedsReplan.into()); }
    let mut constraints=plan.constraints.intersect(&policy.constraints).intersect(&store::deployment_constraints(state).await?);
    let profiles=store::current_profiles(store::catalog(state,&context.actor).await?);
    let roles:BTreeSet<_>=fixed_allowances.keys().copied().chain(fixed_references.iter().copied()).collect();
    for &purpose in stages.keys() {
        if matches!(purpose,CallPurpose::Embed|CallPurpose::Rerank|CallPurpose::Verify|CallPurpose::Ocr|CallPurpose::Stt|CallPurpose::Tts) {
            return Err(RoutingError::Invalid("fixed role in LLM stage map").into());
        }
    }
    for purpose in stages.keys().copied().chain(roles.iter().map(|r|r.purpose())) {
        let mut ctx=context.clone(); ctx.purpose=purpose;
        constraints=constraints.intersect(&state.inference_policy.constraints(&state.pg,&ctx).await?);
    }
    for (&purpose,stage) in stages {
        let reference=plan.assignments.get(&purpose).ok_or(RoutingError::Invalid("unplanned LLM purpose"))?;
        let profile=profiles.iter().find(|p|p.provider_id==reference.provider_id && p.provider_revision==reference.provider_revision
            && p.revision==reference.profile_revision && p.model==reference.model).ok_or(RoutingError::NeedsReplan)?;
        if stage.requirements.effort!=reference.effort { return Err(RoutingError::Incompatible("request effort differs from plan").into()); }
        if let Some(reason)=engine::target_incompatibility(profile,&stage.requirements,&constraints) { return Err(RoutingError::Incompatible(reason).into()); }
    }
    budget.restrict(&constraints);
    let mut assignments=BTreeMap::new(); let mut fixed_assignments=BTreeMap::new();
    for &purpose in stages.keys() {
        let mut ctx=context.clone(); ctx.purpose=purpose;
        assignments.insert(purpose,resolve_target(state,&ctx,&plan.assignments[&purpose],&constraints).await?);
    }
    for &role in &roles {
        let reference=plan.fixed_assignments.get(&role).ok_or(RoutingError::Invalid("unpinned fixed stage"))?;
        fixed_assignments.insert(role,fixed::resolve(state,context,role,Some(reference),&constraints).await?);
    }
    // A target resolved early may have changed while another registry awaited.
    for target in assignments.values() {
        let current:Option<(i64,i64)>=sqlx::query_as("SELECT p.routing_revision,m.revision FROM provider_configs p JOIN model_profiles m ON m.provider_id=p.id WHERE p.id=$1")
            .bind(target.provider_id).fetch_optional(&state.pg).await?;
        if current!=Some((target.provider_revision,target.profile_revision)) { return Err(RoutingError::NeedsReplan.into()); }
    }
    for target in fixed_assignments.values() {
        let r=&target.reference;
        let current:Option<(i64,i64)>=sqlx::query_as("SELECT p.routing_revision,f.revision FROM provider_configs p JOIN fixed_inference_profiles f ON f.provider_id=p.id AND f.slot=$2 WHERE p.id=$1")
            .bind(r.provider_id).bind(r.role.slot()).fetch_optional(&state.pg).await?;
        if current!=Some((r.provider_revision,r.profile_revision)) { return Err(RoutingError::NeedsReplan.into()); }
        if r.active_index_revision.is_some() || r.desired_index_revision.is_some() {
            let indices:Option<(i64,i64)>=sqlx::query_as("SELECT routing_revision,desired_routing_revision FROM embedding_index WHERE id=1").fetch_optional(&state.pg).await?;
            if indices.is_none_or(|(active,desired)|r.active_index_revision.is_some_and(|v|v!=active) || r.desired_index_revision.is_some_and(|v|v!=desired)) {
                return Err(RoutingError::NeedsReplan.into());
            }
        }
    }
    if store::load_policy(state,&context.actor,Some(policy.id)).await?.revision!=policy.revision { return Err(RoutingError::NeedsReplan.into()); }
    let allowances=stages.iter().map(|(&p,s)|(p,Allowance { max_calls:s.max_calls,input_tokens:s.requirements.input_tokens,output_tokens:s.requirements.output_tokens })).collect();
    let grant=MixedGrant::reserve(budget,plan.decision.id,&assignments,&profiles,allowances,&fixed_assignments,fixed_allowances,timeout)?;
    let envelope=grant.envelope(plan.decision.id,context.purpose,assignments,fixed_assignments);
    Ok(PreparedMixedRequest { envelope,grant })
}
