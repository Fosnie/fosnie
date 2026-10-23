// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Server-evaluated structural quality contracts, never a model-authored retry.
use super::*;
use serde::{Deserialize,Serialize};

#[derive(Debug,Clone,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum QualityContract { Nonempty, JsonObject {required_fields:Vec<String>} }
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize)]
#[serde(rename_all="snake_case")]
pub enum QualityFailure { EmptyAnswer, InvalidJson, MissingFields, IncompleteAnswer, NonSourceSpan }
#[derive(Debug,Clone,Serialize)]
pub(crate) struct QualityAttempt {
    pub failed_target:TargetReference,pub planned_target:TargetReference,pub failure:QualityFailure,
}
impl QualityContract {
    pub fn validate(&self)->Result<(),RoutingError> {
        if let Self::JsonObject{required_fields}=self {
            let keys:std::collections::BTreeSet<_>=required_fields.iter().collect();
            if required_fields.len()>32 || keys.len()!=required_fields.len()
                || required_fields.iter().any(|k|k.is_empty()||k.len()>128||k.chars().any(char::is_control)) {
                return Err(RoutingError::Invalid("invalid required JSON fields"));
            }
        }Ok(())
    }
    pub fn check(&self,answer:&str)->Result<(),QualityFailure> {
        if answer.trim().is_empty() {return Err(QualityFailure::EmptyAnswer);}
        if let Self::JsonObject{required_fields}=self {
            let value:serde_json::Value=serde_json::from_str(answer).map_err(|_|QualityFailure::InvalidJson)?;
            let object=value.as_object().ok_or(QualityFailure::InvalidJson)?;
            if required_fields.iter().any(|key|!object.contains_key(key)) {return Err(QualityFailure::MissingFields);}
        }Ok(())
    }
}
pub fn validate_policy(policy:&RoutingPolicy)->Result<(),RoutingError> {
    if policy.quality_escalation {
        let c=&policy.constraints;
        if c.max_calls.is_none_or(|v|v==0||v>8) || c.max_output_tokens.is_none_or(|v|v==0||v>8192)
            || c.max_total_tokens.is_none_or(|v|v==0||v>262144) || c.max_duration_ms.is_none_or(|v|v==0||v>120000) {
            return Err(RoutingError::Invalid("quality escalation requires bounded calls, output, total tokens and duration"));
        }
    }Ok(())
}
