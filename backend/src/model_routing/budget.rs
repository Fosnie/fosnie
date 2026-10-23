// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use super::{RoutingConstraints, RoutingError};

/// A shared run ledger. Concurrent helpers reserve BEFORE dispatch. Failed or
/// cancelled attempts keep their reservation until actual usage is known.
#[derive(Clone)]
pub struct RunBudget(Arc<Inner>);
struct Inner { limits: RwLock<RoutingConstraints>, started: Instant, usage: Mutex<Usage> }
#[derive(Default, Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub calls: u32, pub tokens: u64, pub cost_microusd: u64,
    /// `cost_microusd` is only the known subtotal when this is nonzero.
    pub unknown_cost_calls: u32,
    /// Fixed services have no proven token bound; bytes are a separate unit.
    pub unknown_token_calls: u32,
    pub exhausted: bool,
}
pub struct Reservation { budget: RunBudget, tokens: u64, cost: u64, unknown_cost: bool, unknown_tokens: bool }

impl RunBudget {
    pub fn new(limits: RoutingConstraints) -> Self {
        Self(Arc::new(Inner { limits:RwLock::new(limits), started: Instant::now(), usage: Mutex::new(Usage::default()) }))
    }
    pub fn snapshot(&self) -> Usage { *self.0.usage.lock().unwrap_or_else(|e| e.into_inner()) }
    pub(super) fn limits(&self) -> RoutingConstraints {
        self.0.limits.read().unwrap_or_else(|e|e.into_inner()).clone()
    }
    pub(super) fn exclusive(&self) -> bool { Arc::strong_count(&self.0)==1 }
    /// Restore a trusted durable ledger with only its remaining original time.
    /// Callers must load usage and the absolute deadline from their owned store.
    pub fn restore(mut limits:RoutingConstraints,usage:Usage,remaining_ms:u64)->Result<Self,RoutingError> {
        if remaining_ms==0 || usage.exhausted {return Err(RoutingError::BudgetExhausted);}
        if limits.max_calls.is_some_and(|n|usage.calls>n) || limits.max_total_tokens.is_some_and(|n|usage.tokens>n)
            || limits.max_cost_microusd.is_some_and(|n|usage.cost_microusd>n) {return Err(RoutingError::BudgetExhausted);}
        limits.max_duration_ms=Some(limits.max_duration_ms.unwrap_or(remaining_ms).min(remaining_ms));
        let budget=Self::new(limits);
        *budget.0.usage.lock().unwrap_or_else(|e|e.into_inner())=usage;
        Ok(budget)
    }
    pub(super) fn exhaust(&self) {
        self.0.usage.lock().unwrap_or_else(|e|e.into_inner()).exhausted=true;
    }
    /// Revalidated deployment/edition restrictions can only tighten a live run.
    pub fn restrict(&self, constraints: &RoutingConstraints) {
        let mut limits = self.0.limits.write().unwrap_or_else(|e|e.into_inner());
        *limits = limits.intersect(constraints);
    }
    pub fn remaining(&self, ceiling: std::time::Duration) -> std::time::Duration {
        self.0.limits.read().unwrap_or_else(|e|e.into_inner()).max_duration_ms.map(|ms| std::time::Duration::from_millis(ms).saturating_sub(self.0.started.elapsed()).min(ceiling)).unwrap_or(ceiling)
    }
    /// This query never reserves/refunds work or relaxes any shared limit.
    /// Planning is serial; final reservation remains the atomic authority.
    pub(super) fn can_spare_calls(&self, count:u32) -> bool {
        let limits=self.0.limits.read().unwrap_or_else(|e|e.into_inner());
        let usage=self.0.usage.lock().unwrap_or_else(|e|e.into_inner());
        !usage.exhausted && usage.calls.checked_add(count).is_some_and(|needed|limits.max_calls.is_none_or(|max|needed<=max))
    }
    /// Bound one phase while leaving captured wall-clock room for its successor.
    /// No deadline is extended; a later restriction may only reduce this room.
    pub(super) fn phase_remaining(&self, ceiling:std::time::Duration,reserve:std::time::Duration)->std::time::Duration {
        self.remaining(ceiling.saturating_add(reserve)).saturating_sub(reserve).min(ceiling)
    }
    pub fn check_deadline(&self) -> Result<(), RoutingError> {
        if self.0.limits.read().unwrap_or_else(|e|e.into_inner()).max_duration_ms.is_some_and(|ms| self.0.started.elapsed().as_millis() >= u128::from(ms)) {
            return Err(RoutingError::BudgetExhausted);
        }
        Ok(())
    }
    pub fn reserve(&self, input: u64, output: u32, cost_upper_bound: Option<u64>) -> Result<Reservation, RoutingError> {
        Ok(self.reserve_many(&[(input, output, cost_upper_bound)])?.remove(0))
    }
    /// Reserve a pipeline atomically: a rejected batch charges no phantom calls.
    pub fn reserve_many(&self, requests: &[(u64, u32, Option<u64>)]) -> Result<Vec<Reservation>, RoutingError> {
        self.reserve_mixed(requests,0)
    }
    /// One transaction for LLM estimates followed by unmetered fixed attempts.
    /// Validation of either lane cannot charge the other lane partially.
    pub(super) fn reserve_mixed(&self,requests:&[(u64,u32,Option<u64>)],fixed_calls:u32) -> Result<Vec<Reservation>,RoutingError> {
        self.check_deadline()?;
        let limits = self.0.limits.read().unwrap_or_else(|e|e.into_inner());
        let mut usage = self.0.usage.lock().unwrap_or_else(|e| e.into_inner());
        if usage.exhausted || (limits.max_cost_microusd.is_some() && usage.unknown_cost_calls > 0)
            || ((limits.max_total_tokens.is_some() || limits.max_output_tokens.is_some()) && usage.unknown_token_calls > 0) {
            return Err(RoutingError::BudgetExhausted);
        }
        if fixed_calls>1024 || (fixed_calls>0 && (limits.max_total_tokens.is_some()
            || limits.max_output_tokens.is_some() || limits.max_cost_microusd.is_some())) {
            return Err(RoutingError::BudgetExhausted);
        }
        let mut next = *usage;
        let mut reservations = Vec::with_capacity(requests.len());
        for &(input, output, cost_upper_bound) in requests {
            if limits.max_output_tokens.is_some_and(|max| output > max)
                || (limits.max_cost_microusd.is_some() && cost_upper_bound.is_none()) {
                return Err(RoutingError::BudgetExhausted);
            }
            let tokens = input.checked_add(u64::from(output)).ok_or(RoutingError::BudgetExhausted)?;
            let cost = cost_upper_bound.unwrap_or(0);
            next.calls = next.calls.checked_add(1).ok_or(RoutingError::BudgetExhausted)?;
            next.tokens = next.tokens.checked_add(tokens).ok_or(RoutingError::BudgetExhausted)?;
            next.cost_microusd = next.cost_microusd.checked_add(cost).ok_or(RoutingError::BudgetExhausted)?;
            next.unknown_cost_calls = next.unknown_cost_calls.checked_add(u32::from(cost_upper_bound.is_none())).ok_or(RoutingError::BudgetExhausted)?;
            reservations.push(Reservation { budget:self.clone(), tokens, cost, unknown_cost:cost_upper_bound.is_none(), unknown_tokens:false });
        }
        next.calls=next.calls.checked_add(fixed_calls).ok_or(RoutingError::BudgetExhausted)?;
        next.unknown_token_calls=next.unknown_token_calls.checked_add(fixed_calls).ok_or(RoutingError::BudgetExhausted)?;
        next.unknown_cost_calls=next.unknown_cost_calls.checked_add(fixed_calls).ok_or(RoutingError::BudgetExhausted)?;
        reservations.extend((0..fixed_calls).map(|_|Reservation { budget:self.clone(),tokens:0,cost:0,unknown_cost:true,unknown_tokens:true }));
        if limits.max_calls.is_some_and(|max| next.calls > max)
            || limits.max_total_tokens.is_some_and(|max| next.tokens > max)
            || limits.max_cost_microusd.is_some_and(|max| next.cost_microusd > max) {
            return Err(RoutingError::BudgetExhausted);
        }
        *usage = next;
        Ok(reservations)
    }

    /// Reserve fixed service attempts without inventing token or monetary prices.
    /// Such work is allowed only with call/deadline/byte limits, never a hard
    /// token/money cap. Mixed requests share this same atomic run ledger.
    pub(super) fn reserve_unmetered_many(&self, count:u32) -> Result<Vec<Reservation>,RoutingError> {
        if count==0 { return Err(RoutingError::BudgetExhausted); }
        self.reserve_mixed(&[],count)
    }
}

impl Reservation {
    /// Only a validated, complete ML lease report can prove a slot was unused.
    pub(super) fn release_unused(self) {
        let mut usage = self.budget.0.usage.lock().unwrap_or_else(|e| e.into_inner());
        usage.calls -= 1;
        usage.tokens -= self.tokens;
        usage.cost_microusd -= self.cost;
        usage.unknown_cost_calls -= u32::from(self.unknown_cost);
        usage.unknown_token_calls -= u32::from(self.unknown_tokens);
    }
    /// Unknown usage keeps the conservative reservation. This is intentionally
    /// consumed once; reasoning is already inside the provider's output total.
    pub fn finish(self, actual_tokens: Option<u64>, actual_cost: Option<u64>) -> Result<(), RoutingError> {
        let tokens = actual_tokens.unwrap_or(self.tokens);
        let cost = actual_cost.unwrap_or(self.cost);
        let limits = self.budget.0.limits.read().unwrap_or_else(|e|e.into_inner());
        let mut usage = self.budget.0.usage.lock().unwrap_or_else(|e| e.into_inner());
        usage.tokens = usage.tokens.saturating_sub(self.tokens).saturating_add(tokens);
        usage.cost_microusd = usage.cost_microusd.saturating_sub(self.cost).saturating_add(cost);
        if self.unknown_cost && actual_cost.is_some() { usage.unknown_cost_calls -= 1; }
        if self.unknown_tokens && actual_tokens.is_some() { usage.unknown_token_calls -= 1; }
        if tokens > self.tokens || (limits.max_cost_microusd.is_some() && cost > self.cost) {
            usage.exhausted = true;
            return Err(RoutingError::BudgetExhausted);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restored_ledger_retains_spend_and_rejects_larger_usage_or_expired_time() {
        let limits=RoutingConstraints{max_calls:Some(2),max_total_tokens:Some(100),max_cost_microusd:Some(20),max_duration_ms:Some(1000),..Default::default()};
        let usage=Usage{calls:1,tokens:80,cost_microusd:15,..Default::default()};
        let b=RunBudget::restore(limits.clone(),usage,200).unwrap();
        assert!(b.remaining(std::time::Duration::MAX)<=std::time::Duration::from_millis(200));
        assert!(b.reserve(20,1,Some(5)).is_err());assert_eq!(b.snapshot().calls,1);
        b.reserve(20,0,Some(5)).unwrap();assert_eq!(b.snapshot().calls,2);assert!(b.reserve(0,0,Some(0)).is_err());
        assert!(RunBudget::restore(limits.clone(),Usage{calls:3,..usage},200).is_err());
        assert!(RunBudget::restore(limits.clone(),Usage{tokens:101,..usage},200).is_err());
        assert!(RunBudget::restore(limits.clone(),Usage{cost_microusd:21,..usage},200).is_err());
        assert!(RunBudget::restore(limits,usage,0).is_err());
    }
    #[test]
    fn classifier_preserves_last_call_without_charging_a_phantom_reservation() {
        let budget=RunBudget::new(RoutingConstraints {max_calls:Some(1),..Default::default()});
        assert!(!budget.can_spare_calls(2));assert!(budget.can_spare_calls(1));
        assert_eq!(budget.snapshot().calls,0);
        budget.reserve(10,20,None).unwrap();
        assert!(!budget.can_spare_calls(1));assert_eq!(budget.snapshot().calls,1);
        let budget=RunBudget::new(RoutingConstraints {max_calls:Some(3),..Default::default()});
        budget.reserve(10,20,None).unwrap();assert!(budget.can_spare_calls(2));
        budget.restrict(&RoutingConstraints {max_calls:Some(2),..Default::default()});
        assert!(!budget.can_spare_calls(2));
    }
    #[test]
    fn classifier_time_window_leaves_answer_room_without_extending_deadline() {
        use std::time::Duration;
        let budget=RunBudget::new(RoutingConstraints {max_duration_ms:Some(1000),..Default::default()});
        let window=budget.phase_remaining(Duration::from_millis(700),Duration::from_millis(600));
        assert!(window<=Duration::from_millis(400));assert!(window>Duration::ZERO);
        budget.restrict(&RoutingConstraints {max_duration_ms:Some(500),..Default::default()});
        assert_eq!(budget.phase_remaining(Duration::from_millis(700),Duration::from_millis(600)),Duration::ZERO);
        let unbounded=RunBudget::new(Default::default());
        assert_eq!(unbounded.phase_remaining(Duration::from_millis(700),Duration::from_secs(30)),Duration::from_millis(700));
    }
    #[test]
    fn r20_r21_parallel_reservations_cannot_overspend() {
        let budget = RunBudget::new(RoutingConstraints { max_calls: Some(1), max_total_tokens: Some(100), ..Default::default() });
        let mut threads = Vec::new();
        for _ in 0..8 { let budget = budget.clone(); threads.push(std::thread::spawn(move || budget.reserve(20, 80, None).is_ok())); }
        assert_eq!(threads.into_iter().filter_map(|h| h.join().ok()).filter(|v| *v).count(), 1);
        assert_eq!(budget.snapshot().tokens, 100, "dropped/unknown attempts stay charged");
    }
    #[test]
    fn r20_unknown_cost_blocks_money_cap_and_known_usage_releases_only_unused_tokens() {
        let budget = RunBudget::new(RoutingConstraints { max_cost_microusd: Some(10), ..Default::default() });
        assert!(budget.reserve(1, 2, None).is_err());
        budget.reserve(10, 90, Some(10)).unwrap().finish(Some(40), Some(4)).unwrap();
        assert_eq!(budget.snapshot().tokens, 40);
        assert_eq!(budget.snapshot().cost_microusd, 4);
        assert_eq!(budget.snapshot().calls, 1);
    }
    #[test]
    fn r21_zero_deadline_denies_before_dispatch() {
        assert!(RunBudget::new(RoutingConstraints { max_duration_ms: Some(0), ..Default::default() }).reserve(0, 1, None).is_err());
    }
    #[test]
    fn r26_batch_rejection_is_atomic_and_unknown_price_is_explicit() {
        let budget = RunBudget::new(RoutingConstraints { max_calls:Some(1), ..Default::default() });
        assert!(budget.reserve_many(&[(10,20,None),(10,20,None)]).is_err());
        assert_eq!(budget.snapshot().calls,0);
        budget.reserve(10,20,None).unwrap().finish(None,None).unwrap();
        assert_eq!(budget.snapshot().unknown_cost_calls,1);
    }
    #[test]
    fn r26_new_money_cap_cannot_treat_previous_unknown_spend_as_zero() {
        let budget=RunBudget::new(Default::default());
        budget.reserve(10,20,None).unwrap().finish(None,None).unwrap();
        budget.restrict(&RoutingConstraints { max_cost_microusd:Some(100),..Default::default() });
        assert!(budget.reserve(1,1,Some(0)).is_err());
        budget.restrict(&RoutingConstraints::default());
        assert!(budget.reserve(1,1,Some(0)).is_err(),"restrictions cannot be relaxed mid-run");
    }
}
