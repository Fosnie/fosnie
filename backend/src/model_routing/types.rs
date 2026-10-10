// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub use fosnie_protocol::{ModelSelection, RoutingObjective};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingStrategy { #[default] Rules, Classifier, Hybrid }

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RolloutMode { #[default] Off, Shadow, Active }

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locality { #[default] Unknown, Local, Cloud }

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus { #[default] Unknown, Declared, Verified }

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier { Economy, #[default] Balanced, Advanced }

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind { #[default] Unknown, CurrentFact, General, Editing, Extraction, Coding, Reasoning, Research }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPurpose {
    Primary, ToolLoop, Final, RouterClassify, UtilityTitle, UtilitySummary,
    RetrievalDecompose, RetrievalGrade, WebPlan, WebGrade, WebSynthesis,
    ResearchPlan, ResearchWrite, Document, Voice, Moderation, Verify, Embed, Rerank, Ocr, Stt, Tts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind { OpenaiChat, OpenaiResponses, Anthropic, Gemini }

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingConstraints {
    /// None is unrestricted; Some(empty) denies every provider.
    pub allowed_providers: Option<BTreeSet<Uuid>>,
    pub local_only: bool,
    pub max_calls: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub max_total_tokens: Option<u64>,
    pub max_duration_ms: Option<u64>,
    pub max_cost_microusd: Option<u64>,
}

fn tighter<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) }
}

impl RoutingConstraints {
    pub fn intersect(&self, other: &Self) -> Self {
        let allowed_providers = match (&self.allowed_providers, &other.allowed_providers) {
            (Some(a), Some(b)) => Some(a.intersection(b).copied().collect()),
            (a, b) => a.clone().or_else(|| b.clone()),
        };
        Self {
            allowed_providers, local_only: self.local_only || other.local_only,
            max_calls: tighter(self.max_calls, other.max_calls),
            max_output_tokens: tighter(self.max_output_tokens, other.max_output_tokens),
            max_total_tokens: tighter(self.max_total_tokens, other.max_total_tokens),
            max_duration_ms: tighter(self.max_duration_ms, other.max_duration_ms),
            max_cost_microusd: tighter(self.max_cost_microusd, other.max_cost_microusd),
        }
    }
    pub fn permits(&self, id: Uuid, locality: Locality) -> bool {
        self.allowed_providers.as_ref().is_none_or(|ids| ids.contains(&id))
            && (!self.local_only || locality == Locality::Local)
    }
    pub fn unrestricted(&self) -> bool { self == &Self::default() }
}

#[derive(Debug, Clone)]
pub struct RoutingContext {
    pub actor: crate::auth::AuthContext,
    pub chat_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub run_id: Uuid,
    pub purpose: CallPurpose,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelCapabilities {
    pub tools: EvidenceStatus,
    pub vision: EvidenceStatus,
    pub structured_output: EvidenceStatus,
    pub context_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub efforts: BTreeSet<String>,
    /// Optional operator allowlist for JSON output at an explicit API effort.
    /// None retains legacy compatibility; Some(empty) denies every JSON mode.
    /// A constrained profile never treats an unspecified provider default as a
    /// listed effort. This restriction is not a quality verification.
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub structured_output_efforts: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPricing {
    /// USD millionths per million tokens; integers avoid floating-point budget drift.
    pub input_microusd_per_million: Option<u64>,
    pub output_microusd_per_million: Option<u64>,
    pub source: Option<String>,
    pub checked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    pub provider_id: Uuid,
    pub provider_revision: i64,
    pub revision: i64,
    pub adapter: AdapterKind,
    pub model: String,
    #[serde(default)] pub locality: Locality,
    /// Set only by a deployment administrator's attestation/probe, never BYOK input.
    #[serde(default)] pub locality_trusted: bool,
    #[serde(default)] pub capabilities: ModelCapabilities,
    #[serde(default)] pub tier: ModelTier,
    #[serde(default)] pub task_tiers: BTreeMap<TaskKind, ModelTier>,
    /// Explicit operator exclusions for ranked/forced Auto executors and retries.
    /// Empty preserves existing behaviour; Manual and fixed assignments are separate.
    #[serde(default, skip_serializing_if="BTreeSet::is_empty")]
    pub task_exclusions: BTreeSet<TaskKind>,
    #[serde(default)] pub quality_status: EvidenceStatus,
    #[serde(default)] pub evaluation_samples: u32,
    #[serde(default)] pub evaluation_version: Option<String>,
    #[serde(default)] pub pricing: ModelPricing,
    #[serde(default)] pub latency_ms: Option<u64>,
    #[serde(default)] pub auto_enabled: bool,
    /// Model-specific preset for Auto; explicit request/rule effort wins.
    #[serde(default, skip_serializing_if="Option::is_none")] pub auto_effort: Option<String>,
    #[serde(default)] pub quality_evidence: Vec<super::learning::EvaluationEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkloadRequirements {
    pub task: TaskKind,
    pub minimum_tier: ModelTier,
    pub fresh_information: bool,
    pub tools: bool,
    pub vision: bool,
    pub structured_output: bool,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub language: Option<String>,
    pub effort: Option<String>,
    pub uncertain: bool,
}
impl Default for WorkloadRequirements {
    fn default()->Self {Self {task:TaskKind::Unknown,minimum_tier:ModelTier::Economy,fresh_information:false,
        tools:false,vision:false,structured_output:false,input_tokens:0,output_tokens:0,language:None,effort:None,uncertain:false}}
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuleConditions {
    pub tasks: BTreeSet<TaskKind>,
    pub purposes: BTreeSet<CallPurpose>,
    pub languages: BTreeSet<String>,
    pub min_input_tokens: Option<u32>,
    pub max_input_tokens: Option<u32>,
    pub tools: Option<bool>,
    pub vision: Option<bool>,
    pub uncertain: Option<bool>,
    pub fresh_information: Option<bool>,
    pub structured_output: Option<bool>,
    /// Local fields AND all children AND (any child, when the list is nonempty).
    /// Validation bounds depth and total nodes before matching.
    pub all: Vec<RuleConditions>,
    pub any: Vec<RuleConditions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleAction {
    Select { provider_id: Uuid, #[serde(default)] effort: Option<String> },
    Require { minimum_tier: ModelTier },
    /// Narrows main Auto executors, including technical fallback. Judge and
    /// explicit utility assignments retain their separate mandatory filters.
    RestrictPool { allowed_providers: BTreeSet<Uuid> },
    Classify,
}

#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(default,deny_unknown_fields)]
pub struct ClassifierConfig {
    pub native_schema:bool,
    /// Independent of the executor profile preset. None preserves the API default.
    pub effort:Option<String>,
    pub min_confidence:f32,
    pub max_input_tokens:u32,
    pub max_output_tokens:u32,
    pub local_timeout_ms:u64,
    pub cloud_timeout_ms:u64,
    /// Optional time left for the executor after the judge. Existing policies
    /// keep their previous timeout behaviour when this value is absent.
    #[serde(skip_serializing_if="Option::is_none")]
    pub answer_reserve_ms:Option<u64>,
}
impl Default for ClassifierConfig {
    fn default()->Self {Self {native_schema:false,effort:None,min_confidence:0.75,max_input_tokens:4096,max_output_tokens:512,local_timeout_ms:5000,cloud_timeout_ms:3000,answer_reserve_ms:None}}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingRule {
    pub id: Uuid,
    pub priority: i32,
    pub conditions: RuleConditions,
    pub action: RuleAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicy {
    pub id: Uuid,
    pub revision: i64,
    #[serde(default)] pub rollout: RolloutMode,
    #[serde(default)] pub strategy: RoutingStrategy,
    #[serde(default)] pub objective: RoutingObjective,
    #[serde(default)] pub constraints: RoutingConstraints,
    #[serde(default)] pub rules: Vec<RoutingRule>,
    pub default_provider_id: Option<Uuid>,
    pub classifier_provider_id: Option<Uuid>,
    #[serde(default)] pub utility_assignments: BTreeMap<CallPurpose, Uuid>,
    #[serde(default)] pub technical_fallback: bool,
    #[serde(default)] pub quality_escalation: bool,
    #[serde(default)] pub learned_ranking: bool,
    #[serde(default)] pub classifier: ClassifierConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetReference {
    pub provider_id: Uuid,
    pub provider_revision: i64,
    pub profile_revision: i64,
    pub model: String,
    pub effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingDecision {
    pub id: Uuid,
    pub schema_version: u32,
    pub policy_id: Uuid,
    pub policy_revision: i64,
    pub strategy: RoutingStrategy,
    pub proposed: Option<TargetReference>,
    pub actual: Option<TargetReference>,
    pub reason_codes: Vec<String>,
    pub excluded_counts: BTreeMap<String, u32>,
    pub estimated_cost_microusd: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlan {
    pub id: Uuid,
    pub schema_version: u32,
    pub actor_user_id: Option<Uuid>,
    pub selection: ModelSelection,
    pub decision: RoutingDecision,
    pub constraints: RoutingConstraints,
    pub assignments: BTreeMap<CallPurpose, TargetReference>,
    /// Durable references only; credentials are resolved again at dispatch.
    #[serde(default)] pub fixed_assignments: BTreeMap<super::fixed::FixedRole,super::fixed::FixedReference>,
}

#[derive(Debug, thiserror::Error)]
pub enum RoutingError {
    #[error("no eligible model: {0}")] NoEligibleModel(&'static str),
    #[error("model routing is forbidden")] Forbidden,
    #[error("provider is unavailable")] ProviderUnavailable,
    #[error("provider credentials are unavailable")] CredentialsUnavailable,
    #[error("incompatible model: {0}")] Incompatible(&'static str),
    #[error("routing budget exhausted")] BudgetExhausted,
    #[error("routing request cancelled")] Cancelled,
    #[error("routing plan must be rebuilt")] NeedsReplan,
    #[error("invalid routing configuration: {0}")] Invalid(&'static str),
}

impl From<RoutingError> for crate::AppError {
    fn from(error: RoutingError) -> Self {
        match error {
            RoutingError::Forbidden => Self::Forbidden(error.to_string()),
            RoutingError::Invalid(_) | RoutingError::Incompatible(_) => Self::Validation(error.to_string()),
            RoutingError::NeedsReplan => Self::Conflict(error.to_string()),
            RoutingError::BudgetExhausted => Self::TooManyRequests(error.to_string()),
            _ => Self::Unavailable(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn r11_r34_extensions_cannot_relax_restrictions() {
        let local = Uuid::from_u128(1);
        let cloud = Uuid::from_u128(2);
        let base = RoutingConstraints { local_only: true, allowed_providers: Some([local].into()), max_calls: Some(2), ..Default::default() };
        let extension = RoutingConstraints { allowed_providers: Some([local, cloud].into()), max_calls: Some(20), ..Default::default() };
        let effective = base.intersect(&extension);
        assert!(effective.permits(local, Locality::Local));
        assert!(!effective.permits(cloud, Locality::Local));
        assert!(!effective.permits(local, Locality::Cloud));
        assert!(!effective.permits(local, Locality::Unknown));
        assert_eq!(effective.max_calls, Some(2));
        assert_eq!(effective, extension.intersect(&base));
        assert_eq!(effective.intersect(&RoutingConstraints::default()), effective);
    }
    #[test]
    fn r37_empty_allowlist_is_not_unrestricted() {
        let denied = RoutingConstraints { allowed_providers: Some(BTreeSet::new()), ..Default::default() };
        assert!(!denied.intersect(&RoutingConstraints::default()).permits(Uuid::nil(), Locality::Local));
        assert!(!denied.unrestricted());
    }
}
