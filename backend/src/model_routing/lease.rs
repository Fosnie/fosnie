// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! A bounded grant for one ML request, shared by all of its concurrent helpers.
//! Lost/error responses keep the full reservation; only a complete validated
//! report releases unused slots. A grant contains no prices or credentials.
use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use super::{*, budget::{RunBudget, Reservation}};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowance { pub max_calls: u32, pub input_tokens: u32, pub output_tokens: u32 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetLease {
    pub id: Uuid,
    pub timeout_ms: u64,
    pub allowances: BTreeMap<CallPurpose, Allowance>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallUsage {
    pub input_tokens: Option<u64>, pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>, pub cached_input_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    #[serde(default)] pub raw_usage: serde_json::Value,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum CallStatus { Completed, Failed, Cancelled }
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallRecord {
    pub call_id: Uuid, pub lease_id: Uuid, pub decision_id: Uuid,
    pub provider_id: Uuid, pub provider_revision: i64, pub profile_revision: i64,
    pub purpose: CallPurpose, pub status: CallStatus, pub latency_ms: u64,
    pub usage: CallUsage,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseReport {
    pub lease_id: Uuid, pub complete: bool, pub calls: Vec<CallRecord>,
}

pub struct GrantedLease {
    pub wire: BudgetLease,
    budget: RunBudget,
    decision_id: Uuid,
    targets: BTreeMap<CallPurpose, TargetReference>,
    reservations: BTreeMap<CallPurpose, Vec<Reservation>>,
}

pub(super) struct LeaseSpec {
    pub wire:BudgetLease,decision_id:Uuid,targets:BTreeMap<CallPurpose,TargetReference>,
    pub requests:Vec<(u64,u32,Option<u64>)>,
}
impl LeaseSpec {
    pub fn bind(self,budget:&RunBudget,reserved:Vec<Reservation>) -> GrantedLease {
        assert_eq!(reserved.len(),self.requests.len());
        let mut reserved=reserved.into_iter();
        let reservations=self.wire.allowances.iter().map(|(&purpose,a)| (purpose,reserved.by_ref().take(a.max_calls as usize).collect())).collect();
        GrantedLease { wire:self.wire,budget:budget.clone(),decision_id:self.decision_id,targets:self.targets,reservations }
    }
}

impl GrantedLease {
    pub fn reserve(
        budget: &RunBudget, decision_id: Uuid,
        assignments: &BTreeMap<CallPurpose, InferenceTarget>, profiles: &[ModelProfile],
        allowances: BTreeMap<CallPurpose, Allowance>, timeout: std::time::Duration,
    ) -> Result<Self, RoutingError> {
        let spec=Self::spec(budget,decision_id,assignments,profiles,allowances,timeout)?;
        let reserved=budget.reserve_many(&spec.requests)?;
        Ok(spec.bind(budget,reserved))
    }
    pub(super) fn spec(budget:&RunBudget,decision_id:Uuid,assignments:&BTreeMap<CallPurpose,InferenceTarget>,
        profiles:&[ModelProfile],allowances:BTreeMap<CallPurpose,Allowance>,timeout:std::time::Duration
    ) -> Result<LeaseSpec,RoutingError> {
        let count: u64 = allowances.values().map(|a| u64::from(a.max_calls)).sum();
        if count == 0 || count > 1024 || allowances.len() > 24 {
            return Err(RoutingError::Invalid("lease must grant 1 to 1024 calls"));
        }
        let timeout_ms = budget.remaining(timeout).as_millis().min(86_400_000) as u64;
        if timeout_ms == 0 { return Err(RoutingError::BudgetExhausted); }
        let mut requests = Vec::new();
        let mut targets = BTreeMap::new();
        for (&purpose, allowance) in &allowances {
            if allowance.max_calls == 0 || allowance.output_tokens == 0 {
                return Err(RoutingError::Invalid("empty purpose allowance"));
            }
            let target = assignments.get(&purpose).ok_or(RoutingError::Invalid("missing lease assignment"))?;
            let profile = profiles.iter().find(|p| p.provider_id == target.provider_id
                && p.provider_revision == target.provider_revision && p.revision == target.profile_revision
                && p.model == target.model).ok_or(RoutingError::NeedsReplan)?;
            let workload = WorkloadRequirements { input_tokens:allowance.input_tokens,
                output_tokens:allowance.output_tokens, effort:target.effort.clone(), ..Default::default() };
            if let Some(reason) = engine::target_incompatibility(profile, &workload, &RoutingConstraints::default()) {
                return Err(RoutingError::Incompatible(reason));
            }
            for _ in 0..allowance.max_calls {
                requests.push((u64::from(allowance.input_tokens), allowance.output_tokens, engine::estimate_cost(profile,&workload)));
            }
            targets.insert(purpose,target.reference());
        }
        Ok(LeaseSpec { wire:BudgetLease { id:Uuid::now_v7(),timeout_ms,allowances },decision_id,targets,requests })
    }

    /// Missing, partial or malformed reports never refund a reservation. Actual
    /// token totals are checked independently of raw provider metadata/prices.
    pub fn finish(mut self, report: Option<&LeaseReport>) -> Result<(), RoutingError> {
        let Some(report) = report else { return Ok(()); };
        if !report.complete { return Ok(()); }
        let overrun=self.validate(report)?;
        for call in &report.calls {
            let reservation = self.reservations.get_mut(&call.purpose).unwrap().pop().unwrap();
            reservation.finish(call.usage.total_tokens,None)?;
        }
        if overrun { self.budget.exhaust(); return Err(RoutingError::BudgetExhausted); }
        for reservation in self.reservations.into_values().flatten() { reservation.release_unused(); }
        Ok(())
    }

    /// Non-mutating validation lets a mixed grant check every report before any refund.
    pub(super) fn validate(&self,report:&LeaseReport) -> Result<bool,RoutingError> {
        let mut ids = BTreeSet::new();
        let mut counts = BTreeMap::<CallPurpose,u32>::new();
        let mut overrun=false;
        if report.lease_id != self.wire.id || report.calls.len() > 1024 {
            return Err(RoutingError::Invalid("invalid lease report"));
        }
        // Validate the whole report before allowing any refund.
        for call in &report.calls {
            let target = self.targets.get(&call.purpose).ok_or(RoutingError::Invalid("unplanned inference purpose"))?;
            let count = counts.entry(call.purpose).or_default(); *count += 1;
            if !ids.insert(call.call_id) || call.lease_id != self.wire.id || call.decision_id != self.decision_id
                || call.provider_id != target.provider_id || call.provider_revision != target.provider_revision
                || call.profile_revision != target.profile_revision || *count > self.wire.allowances[&call.purpose].max_calls {
                return Err(RoutingError::Invalid("inference report differs from grant"));
            }
            let usage = &call.usage;
            let total = usage.input_tokens.zip(usage.output_tokens).and_then(|(a,b)| a.checked_add(b));
            if (usage.input_tokens.is_some() && usage.output_tokens.is_some() && total.is_none())
                || total != usage.total_tokens || usage.reasoning_tokens.zip(usage.output_tokens).is_some_and(|(a,b)| a>b)
                || usage.cached_input_tokens.zip(usage.input_tokens).is_some_and(|(a,b)| a>b) {
                return Err(RoutingError::Invalid("inconsistent inference usage"));
            }
            let allowance=&self.wire.allowances[&call.purpose];
            overrun |= usage.input_tokens.is_some_and(|n|n>u64::from(allowance.input_tokens))
                || usage.output_tokens.is_some_and(|n|n>u64::from(allowance.output_tokens));
        }
        Ok(overrun)
    }
}
