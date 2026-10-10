// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Fixed service grants count HTTP attempts and bytes, never fabricated tokens.
use std::{collections::{BTreeMap,BTreeSet}, time::Duration};
use serde::{Serialize,Deserialize};
use uuid::Uuid;
use super::{*,fixed::{FixedRole,FixedReference,FixedTarget},budget::{RunBudget,Reservation},lease::CallStatus};

#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedAllowance { pub max_calls:u32,pub input_bytes:u32,pub output_bytes:u32 }
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedLease { pub id:Uuid,pub timeout_ms:u64,pub allowances:BTreeMap<FixedRole,FixedAllowance> }
#[derive(Debug,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedCallRecord {
    pub call_id:Uuid,pub lease_id:Uuid,pub decision_id:Uuid,pub role:FixedRole,
    pub provider_id:Uuid,pub provider_revision:i64,pub profile_revision:i64,
    pub active_index_revision:Option<i64>,
    #[serde(default)] pub desired_index_revision:Option<i64>,
    pub status:CallStatus,pub latency_ms:u64,
    pub input_bytes:u64,pub output_bytes:Option<u64>,
}
#[derive(Debug,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixedReport { pub lease_id:Uuid,pub complete:bool,pub calls:Vec<FixedCallRecord> }

pub struct FixedGrant {
    pub wire:FixedLease,budget:RunBudget,decision_id:Uuid,
    targets:BTreeMap<FixedRole,FixedReference>,reservations:BTreeMap<FixedRole,Vec<Reservation>>,
}
pub(super) struct FixedSpec { pub wire:FixedLease,decision_id:Uuid,targets:BTreeMap<FixedRole,FixedReference>,pub count:u32 }
impl FixedSpec {
    pub fn bind(self,budget:&RunBudget,reserved:Vec<Reservation>) -> FixedGrant {
        assert_eq!(reserved.len(),self.count as usize);
        let mut reserved=reserved.into_iter();
        let reservations=self.wire.allowances.iter().map(|(&role,a)|(role,reserved.by_ref().take(a.max_calls as usize).collect())).collect();
        FixedGrant { wire:self.wire,budget:budget.clone(),decision_id:self.decision_id,targets:self.targets,reservations }
    }
}
impl FixedGrant {
    pub fn reserve(budget:&RunBudget,decision_id:Uuid,targets:&BTreeMap<FixedRole,FixedTarget>,
        allowances:BTreeMap<FixedRole,FixedAllowance>,timeout:Duration) -> Result<Self,RoutingError> {
        let spec=Self::spec(budget,decision_id,targets,allowances,timeout)?;
        let reserved=budget.reserve_unmetered_many(spec.count)?;
        Ok(spec.bind(budget,reserved))
    }
    pub(super) fn spec(budget:&RunBudget,decision_id:Uuid,targets:&BTreeMap<FixedRole,FixedTarget>,
        allowances:BTreeMap<FixedRole,FixedAllowance>,timeout:Duration) -> Result<FixedSpec,RoutingError> {
        let count:u64=allowances.values().map(|a|u64::from(a.max_calls)).sum();
        if count==0 || count>1024 { return Err(RoutingError::Invalid("fixed lease must grant 1 to 1024 calls")); }
        let timeout_ms=budget.remaining(timeout).as_millis().min(86_400_000) as u64;
        if timeout_ms==0 { return Err(RoutingError::BudgetExhausted); }
        let mut references=BTreeMap::new();
        for (&role,a) in &allowances {
            let target=targets.get(&role).ok_or(RoutingError::Invalid("missing fixed assignment"))?;
            if target.reference.role!=role || a.max_calls==0 || a.input_bytes==0 || a.output_bytes==0 {
                return Err(RoutingError::Invalid("invalid fixed allowance"));
            }
            references.insert(role,target.reference.clone());
        }
        Ok(FixedSpec { wire:FixedLease { id:Uuid::now_v7(),timeout_ms,allowances },decision_id,targets:references,count:count as u32 })
    }

    pub fn finish(mut self,report:Option<&FixedReport>) -> Result<(),RoutingError> {
        let Some(report)=report else { return Ok(()); };
        if !report.complete { return Ok(()); }
        if self.validate(report)? { self.budget.exhaust(); return Err(RoutingError::BudgetExhausted); }
        for call in &report.calls { self.reservations.get_mut(&call.role).unwrap().pop(); }
        for slot in self.reservations.into_values().flatten() { slot.release_unused(); }
        Ok(())
    }
    pub(super) fn validate(&self,report:&FixedReport) -> Result<bool,RoutingError> {
        if report.lease_id!=self.wire.id || report.calls.len()>1024 { return Err(RoutingError::Invalid("invalid fixed report")); }
        let mut ids=BTreeSet::new(); let mut counts=BTreeMap::<FixedRole,u32>::new(); let mut overrun=false;
        for call in &report.calls {
            let target=self.targets.get(&call.role).ok_or(RoutingError::Invalid("unplanned fixed role"))?;
            let a=&self.wire.allowances[&call.role];
            let count=counts.entry(call.role).or_default(); *count+=1;
            if !ids.insert(call.call_id) || call.lease_id!=self.wire.id || call.decision_id!=self.decision_id
                || call.provider_id!=target.provider_id || call.provider_revision!=target.provider_revision
                || call.profile_revision!=target.profile_revision || call.active_index_revision!=target.active_index_revision
                || call.desired_index_revision!=target.desired_index_revision
                || *count>a.max_calls || (matches!(call.status,CallStatus::Completed) && call.output_bytes.is_none()) {
                return Err(RoutingError::Invalid("fixed report differs from grant"));
            }
            overrun |= call.input_bytes>u64::from(a.input_bytes) || call.output_bytes.is_some_and(|n|n>u64::from(a.output_bytes));
        }
        Ok(overrun)
    }
}

pub struct PreparedFixedRequest { pub envelope:InferenceEnvelope,grant:FixedGrant }
impl PreparedFixedRequest {
    pub fn finish(self,report:Option<&FixedReport>) -> crate::Result<()> { self.grant.finish(report).map_err(Into::into) }
}

/// Pinned references come from the original actor's plan; never auto-select a
/// replacement embedding index. Revalidate edition/deployment restrictions for
/// each service and reserve against the same run ledger as LLM calls.
pub async fn prepare(state:&crate::AppState,context:&RoutingContext,plan:&ExecutionPlan,budget:&RunBudget,
    allowances:BTreeMap<FixedRole,FixedAllowance>,timeout:Duration
) -> crate::Result<PreparedFixedRequest> {
    if plan.schema_version!=SCHEMA_VERSION || plan.id!=context.run_id || plan.actor_user_id!=context.actor.user_id {
        return Err(RoutingError::Forbidden.into());
    }
    if !allowances.keys().any(|role|role.purpose()==context.purpose) { return Err(RoutingError::Invalid("missing fixed request purpose").into()); }
    let policy=store::load_policy(state,&context.actor,Some(plan.decision.policy_id)).await?;
    if policy.revision!=plan.decision.policy_revision { return Err(RoutingError::NeedsReplan.into()); }
    let mut constraints=plan.constraints.intersect(&policy.constraints).intersect(&store::deployment_constraints(state).await?);
    for &role in allowances.keys() {
        let mut ctx=context.clone(); ctx.purpose=role.purpose();
        constraints=constraints.intersect(&state.inference_policy.constraints(&state.pg,&ctx).await?);
    }
    budget.restrict(&constraints);
    let mut targets=BTreeMap::new();
    for &role in allowances.keys() {
        let reference=plan.fixed_assignments.get(&role).ok_or(RoutingError::Invalid("unpinned fixed service"))?;
        targets.insert(role,fixed::resolve(state,context,role,Some(reference),&constraints).await?);
    }
    if store::load_policy(state,&context.actor,Some(policy.id)).await?.revision!=policy.revision { return Err(RoutingError::NeedsReplan.into()); }
    let grant=FixedGrant::reserve(budget,plan.decision.id,&targets,allowances,timeout)?;
    let envelope=InferenceEnvelope { schema_version:SCHEMA_VERSION,decision_id:plan.decision.id,purpose:context.purpose,
        assignments:Default::default(),budget:None,fixed_assignments:targets,fixed_budget:Some(grant.wire.clone()) };
    Ok(PreparedFixedRequest { envelope,grant })
}

/// Extend a plan atomically, before execution or persistence. Existing pins are
/// revalidated, never replaced after provider/index changes. No key is stored.
pub async fn pin(state:&crate::AppState,context:&RoutingContext,plan:&mut ExecutionPlan,roles:&[FixedRole]) -> crate::Result<()> {
    if plan.schema_version!=SCHEMA_VERSION || plan.id!=context.run_id || plan.actor_user_id!=context.actor.user_id {
        return Err(RoutingError::Forbidden.into());
    }
    let policy=store::load_policy(state,&context.actor,Some(plan.decision.policy_id)).await?;
    if policy.revision!=plan.decision.policy_revision { return Err(RoutingError::NeedsReplan.into()); }
    let constraints=plan.constraints.intersect(&policy.constraints).intersect(&store::deployment_constraints(state).await?);
    let mut references=plan.fixed_assignments.clone();
    for &role in roles {
        let target=fixed::resolve(state,context,role,references.get(&role),&constraints).await?;
        references.insert(role,target.reference);
    }
    if store::load_policy(state,&context.actor,Some(policy.id)).await?.revision!=policy.revision { return Err(RoutingError::NeedsReplan.into()); }
    plan.fixed_assignments=references;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(budget:&RunBudget) -> FixedGrant {
        let reference=FixedReference { provider_id:Uuid::now_v7(),provider_revision:3,profile_revision:4,role:FixedRole::Embed,model:"embed".into(),active_index_revision:Some(5),desired_index_revision:None };
        let target=FixedTarget { schema_version:1,reference,base_url:"http://localhost/v1".into(),api_key:"secret".into(),embedding_dimension:Some(2),embedding_collection:Some("active".into()),ocr_pdf_mode:None };
        FixedGrant::reserve(budget,Uuid::now_v7(),&[(FixedRole::Embed,target)].into(),
            [(FixedRole::Embed,FixedAllowance { max_calls:2,input_bytes:100,output_bytes:200 })].into(),Duration::from_secs(1)).unwrap()
    }
    fn report(grant:&FixedGrant) -> FixedReport {
        let target=&grant.targets[&FixedRole::Embed];
        FixedReport { lease_id:grant.wire.id,complete:true,calls:vec![FixedCallRecord { call_id:Uuid::now_v7(),lease_id:grant.wire.id,
            decision_id:grant.decision_id,role:FixedRole::Embed,provider_id:target.provider_id,provider_revision:3,profile_revision:4,
            active_index_revision:Some(5),desired_index_revision:None,status:CallStatus::Completed,latency_ms:1,input_bytes:30,output_bytes:Some(40) }] }
    }
    #[test]
    fn fixed_usage_never_becomes_zero_tokens_or_free_money() {
        let budget=RunBudget::new(Default::default()); let grant=fixture(&budget); let report=report(&grant);
        grant.finish(Some(&report)).unwrap();
        assert_eq!(budget.snapshot().calls,1); assert_eq!(budget.snapshot().unknown_token_calls,1); assert_eq!(budget.snapshot().unknown_cost_calls,1);
        budget.restrict(&RoutingConstraints { max_total_tokens:Some(100),..Default::default() });
        assert!(budget.reserve(1,1,Some(0)).is_err());
    }
    #[test]
    fn fixed_caps_reject_atomically_and_share_llm_call_limit() {
        for limits in [RoutingConstraints { max_total_tokens:Some(100),..Default::default() },
            RoutingConstraints { max_output_tokens:Some(100),..Default::default() },
            RoutingConstraints { max_cost_microusd:Some(100),..Default::default() }] {
            let budget=RunBudget::new(limits); assert!(budget.reserve_unmetered_many(1).is_err()); assert_eq!(budget.snapshot().calls,0);
        }
        let budget=RunBudget::new(RoutingConstraints { max_calls:Some(2),..Default::default() });
        budget.reserve(1,1,None).unwrap(); assert!(budget.reserve_unmetered_many(2).is_err()); assert_eq!(budget.snapshot().calls,1);
    }
    #[test]
    fn fixed_invalid_missing_and_partial_reports_keep_reservation() {
        for mode in 0..6 {
            let budget=RunBudget::new(Default::default()); let grant=fixture(&budget); let mut report=report(&grant);
            match mode { 0=>report.complete=false,1=>report.calls[0].active_index_revision=Some(6),
                2=>report.calls[0].profile_revision=9,3=>report.calls[0].output_bytes=None,4=>report.calls[0].desired_index_revision=Some(5),_=>{} }
            let result=grant.finish(if mode==5 { None } else { Some(&report) });
            assert_eq!(result.is_err(),(1..=4).contains(&mode)); assert_eq!(budget.snapshot().calls,2);
        }
    }
    #[test]
    fn fixed_overrun_poison_and_complete_unused_release() {
        let budget=RunBudget::new(Default::default()); let grant=fixture(&budget); let mut report=report(&grant);
        report.calls[0].output_bytes=Some(201); assert!(grant.finish(Some(&report)).is_err()); assert!(budget.snapshot().exhausted);
        let budget=RunBudget::new(Default::default()); let grant=fixture(&budget);
        let report=FixedReport { lease_id:grant.wire.id,complete:true,calls:vec![] }; grant.finish(Some(&report)).unwrap();
        assert_eq!(budget.snapshot().calls,0); assert_eq!(budget.snapshot().unknown_token_calls,0);
    }
}
