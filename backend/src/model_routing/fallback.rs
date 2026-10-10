// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Technical fallback eligibility. Quality cascades remain a separate feature.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(serde::Serialize,serde::Deserialize)]
#[serde(rename_all="snake_case")]
pub enum FailureKind { Connection, RateLimited, Unavailable, Unauthorized, InvalidRequest, Cancelled, Budget, MidStream, Timeout, QuotaExhausted, PaymentRequired }

#[derive(Clone)]
pub(super) struct RetryRequirements {
    pub requirements:WorkloadRequirements,
    pub allowed_providers:Option<std::collections::BTreeSet<uuid::Uuid>>,
}

/// Authored technical failure, kept typed until the API boundary. Generic 503
/// errors (revocation, reports, policy, deadline) cannot trigger a retry.
#[derive(Debug,thiserror::Error)]
#[error("routed inference transport failed")]
pub struct OpenFailure(pub super::failure::ProviderFailure);
impl OpenFailure {
    pub fn app(kind:FailureKind)->crate::AppError {crate::AppError::Other(anyhow::Error::new(Self(super::failure::ProviderFailure {schema_version:1,kind,before_output:true,retry_after_seconds:None})))}
    pub fn kind(error:&crate::AppError)->Option<FailureKind> {
        match error {crate::AppError::Other(error)=>error.downcast_ref::<Self>().filter(|e|e.0.before_output).map(|e|e.0.kind),_=>None}
    }
    pub(super) fn status(status:u16)->Option<FailureKind> {
        match status {429=>Some(FailureKind::RateLimited),500|502|503|504=>Some(FailureKind::Unavailable),_=>None}
    }
}

#[derive(Debug,Clone,serde::Serialize)]
pub(crate) struct Attempt {
    pub retry_index:u8,
    pub failed_target:TargetReference,
    pub planned_target:TargetReference,
    pub failure:FailureKind,
}

#[derive(Default)]
pub struct AttemptState {
    pub retries: u8,
    pub emitted_output: bool,
    pub executed_tool: bool,
}

impl AttemptState {
    pub fn can_retry(&self, policy: &RoutingPolicy, selection: &ModelSelection, failure: FailureKind) -> bool {
        policy.technical_fallback && policy.rollout == RolloutMode::Active
            && matches!(selection, ModelSelection::Auto { .. })
            && self.retries == 0 && !self.emitted_output && !self.executed_tool
            && matches!(failure, FailureKind::Connection | FailureKind::RateLimited | FailureKind::Unavailable | FailureKind::Timeout | FailureKind::QuotaExhausted | FailureKind::PaymentRequired)
    }
    pub fn reserve_retry(&mut self, policy: &RoutingPolicy, selection: &ModelSelection, failure: FailureKind) -> Result<(), RoutingError> {
        if !self.can_retry(policy,selection,failure) { return Err(RoutingError::ProviderUnavailable); }
        self.retries += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn r22_no_retry_after_output_side_effects_or_auth_errors() {
        let policy: RoutingPolicy = serde_json::from_value(serde_json::json!({"id":uuid::Uuid::nil(),"revision":1,"rollout":"active","technical_fallback":true,"default_provider_id":null,"classifier_provider_id":null})).unwrap();
        let selection = ModelSelection::Auto { objective:RoutingObjective::Balanced,policy_id:None };
        for failure in [FailureKind::Unauthorized,FailureKind::InvalidRequest,FailureKind::Cancelled,FailureKind::Budget,FailureKind::MidStream] {
            assert!(!AttemptState::default().can_retry(&policy,&selection,failure));
        }
        assert!(!AttemptState { emitted_output:true,..Default::default() }.can_retry(&policy,&selection,FailureKind::Connection));
        assert!(!AttemptState { executed_tool:true,..Default::default() }.can_retry(&policy,&selection,FailureKind::Connection));
        let mut state = AttemptState::default();
        state.reserve_retry(&policy,&selection,FailureKind::Unavailable).unwrap();
        assert!(state.reserve_retry(&policy,&selection,FailureKind::Unavailable).is_err());
        assert!(!AttemptState::default().can_retry(&policy,&ModelSelection::Manual { provider_id:uuid::Uuid::nil() },FailureKind::Connection));
    }
}
