// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0

//! Pure routing primitives: no inference, database, secrets or model-name heuristics.
use std::collections::BTreeMap;
use uuid::Uuid;
use super::*;

#[derive(Debug, Clone, Default)]
pub struct RuleMatch {
    pub target: Option<Uuid>,
    pub effort: Option<String>,
    pub classify: bool,
    pub matched: Vec<Uuid>,
    pub allowed_providers:Option<std::collections::BTreeSet<Uuid>>,
}

impl RuleConditions {
    fn matches(&self, task: &WorkloadRequirements, purpose: CallPurpose) -> bool {
        (self.tasks.is_empty() || self.tasks.contains(&task.task))
            && (self.purposes.is_empty() || self.purposes.contains(&purpose))
            && (self.languages.is_empty() || task.language.as_ref().is_some_and(|l| self.languages.iter().any(|code|code.eq_ignore_ascii_case(l))))
            && self.min_input_tokens.is_none_or(|v| task.input_tokens >= v)
            && self.max_input_tokens.is_none_or(|v| task.input_tokens <= v)
            && self.tools.is_none_or(|v| task.tools == v)
            && self.vision.is_none_or(|v| task.vision == v)
            && self.uncertain.is_none_or(|v| task.uncertain == v)
            && self.fresh_information.is_none_or(|v| task.fresh_information == v)
            && self.structured_output.is_none_or(|v| task.structured_output == v)
            && self.all.iter().all(|c|c.matches(task,purpose))
            && (self.any.is_empty() || self.any.iter().any(|c|c.matches(task,purpose)))
    }
}

pub fn validate_policy(policy: &RoutingPolicy) -> Result<(), RoutingError> {
    if policy.revision < 1 || policy.rules.len() > 100 || policy.utility_assignments.len() > 24 {
        return Err(RoutingError::Invalid("policy exceeds limits"));
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut nodes=0;
    super::classifier::validate_config(&policy.classifier)?;
    super::quality::validate_policy(policy)?;
    if policy.constraints.allowed_providers.as_ref().is_some_and(|p|p.len()>1024) {
        return Err(RoutingError::Invalid("provider pool exceeds limits"));
    }
    for rule in &policy.rules {
        if !ids.insert(rule.id) { return Err(RoutingError::Invalid("duplicate rule id")); }
        validate_conditions(&rule.conditions,1,&mut nodes)?;
        match &rule.action {
            RuleAction::Select{effort:Some(effort),..} if effort.is_empty() || effort.len()>24 || effort.trim()!=effort =>
                return Err(RoutingError::Invalid("invalid rule effort")),
            RuleAction::RestrictPool{allowed_providers} if allowed_providers.len()>256 =>
                return Err(RoutingError::Invalid("rule pool exceeds limits")),
            _=>{}
        }
        if matches!(rule.action, RuleAction::Classify) && policy.strategy == RoutingStrategy::Rules {
            return Err(RoutingError::Invalid("rules strategy cannot call classifier"));
        }
    }
    if policy.strategy != RoutingStrategy::Rules && policy.classifier_provider_id.is_none() {
        return Err(RoutingError::Invalid("classifier assignment is required"));
    }
    if policy.utility_assignments.contains_key(&CallPurpose::RouterClassify) {
        return Err(RoutingError::Invalid("classifier has one explicit assignment"));
    }
    if policy.utility_assignments.keys().any(|p| matches!(p, CallPurpose::Primary | CallPurpose::ToolLoop | CallPurpose::Final | CallPurpose::Embed | CallPurpose::Rerank | CallPurpose::Ocr | CallPurpose::Stt | CallPurpose::Tts | CallPurpose::Verify)) {
        return Err(RoutingError::Invalid("utility assignments cannot replace the primary turn or fixed capabilities"));
    }
    Ok(())
}

fn validate_conditions(c:&RuleConditions,depth:usize,nodes:&mut usize)->Result<(),RoutingError> {
    *nodes+=1;
    if depth>4 || *nodes>512 || c.all.len()+c.any.len()>16 || c.languages.len()>32
        || c.languages.iter().any(|v|v.is_empty() || v.len()>32 || !v.split('-').all(|part|!part.is_empty()&&part.bytes().all(|b|b.is_ascii_alphanumeric())))
        || matches!((c.min_input_tokens,c.max_input_tokens),(Some(a),Some(b)) if a>b) {
        return Err(RoutingError::Invalid("invalid rule condition bounds"));
    }
    for child in c.all.iter().chain(&c.any) {validate_conditions(child,depth+1,nodes)?;}
    Ok(())
}

pub fn apply_rules(policy: &RoutingPolicy, requirements: &mut WorkloadRequirements, purpose: CallPurpose) -> Result<RuleMatch, RoutingError> {
    validate_policy(policy)?;
    let mut rules: Vec<_> = policy.rules.iter().collect();
    rules.sort_by_key(|r| (std::cmp::Reverse(r.priority), r.id));
    let mut result = RuleMatch::default();
    for rule in rules {
        if !rule.conditions.matches(requirements, purpose) { continue; }
        result.matched.push(rule.id);
        match &rule.action {
            RuleAction::Require { minimum_tier } => requirements.minimum_tier = requirements.minimum_tier.max(*minimum_tier),
            RuleAction::RestrictPool{allowed_providers}=>result.allowed_providers=Some(match &result.allowed_providers {
                Some(pool)=>pool.intersection(allowed_providers).copied().collect(),None=>allowed_providers.clone(),
            }),
            RuleAction::Select { provider_id, effort } => {
                result.target = Some(*provider_id);
                result.effort = effort.clone();
                break;
            }
            RuleAction::Classify => { result.classify = true; break; }
        }
    }
    Ok(result)
}

pub fn estimate_cost(profile: &ModelProfile, req: &WorkloadRequirements) -> Option<u64> {
    let input = u128::from(profile.pricing.input_microusd_per_million?) * u128::from(req.input_tokens);
    let output = u128::from(profile.pricing.output_microusd_per_million?) * u128::from(req.output_tokens);
    u64::try_from((input + output).div_ceil(1_000_000)).ok()
}

fn tier(profile: &ModelProfile, task: TaskKind) -> ModelTier {
    profile.task_tiers.get(&task).copied().unwrap_or(profile.tier)
}

pub fn incompatibility(profile: &ModelProfile, req: &WorkloadRequirements, constraints: &RoutingConstraints) -> Option<&'static str> {
    if !profile.auto_enabled { return Some("auto_disabled"); }
    if profile.task_exclusions.contains(&req.task) { return Some("task_excluded"); }
    if profile.quality_status == EvidenceStatus::Unknown || tier(profile, req.task) < req.minimum_tier { return Some("quality_evidence"); }
    target_incompatibility(profile, &auto_requirements(profile,req), constraints)
}

/// Resolve per candidate, never turn a preset into a pool-wide hard constraint.
pub fn auto_requirements(profile:&ModelProfile,req:&WorkloadRequirements)->WorkloadRequirements {
    WorkloadRequirements {effort:req.effort.clone().or_else(||profile.auto_effort.clone()),..req.clone()}
}
pub(super) fn valid_effort(value:&str)->bool {
    !value.is_empty() && value.len()<=24 && value.trim()==value
}
pub fn validate_auto_effort(profile:&ModelProfile)->Result<(),RoutingError> {
    if profile.capabilities.structured_output_efforts.as_ref().is_some_and(|efforts|
        efforts.len()>16 || efforts.iter().any(|v|!valid_effort(v) || !profile.capabilities.efforts.contains(v))) {
        return Err(RoutingError::Invalid("JSON output efforts must be declared supported API values"));
    }
    if profile.auto_effort.as_ref().is_some_and(|v|!valid_effort(v) || !profile.capabilities.efforts.contains(v)) {
        return Err(RoutingError::Invalid("Auto effort must be a declared supported API value"));
    }
    Ok(())
}

/// Shared JSON compatibility check for executors, helpers and the independent
/// classifier. Never change effort to evade an operator restriction.
pub fn structured_output_incompatibility(profile:&ModelProfile, effort:Option<&str>)->Option<&'static str> {
    if profile.capabilities.structured_output == EvidenceStatus::Unknown { return Some("structured_output_unknown"); }
    if profile.capabilities.structured_output_efforts.as_ref().is_some_and(|allowed|
        effort.is_none_or(|value|!allowed.contains(value))) { return Some("structured_output_effort"); }
    None
}

/// Capability/policy checks shared by explicit and default targets; manual
/// selection does not pretend that a missing quality evaluation is a rating.
pub fn target_incompatibility(profile: &ModelProfile, req: &WorkloadRequirements, constraints: &RoutingConstraints) -> Option<&'static str> {
    let locality = if profile.locality_trusted { profile.locality } else { Locality::Unknown };
    if !constraints.permits(profile.provider_id, locality) { return Some("policy"); }
    let caps = &profile.capabilities;
    if req.tools && caps.tools == EvidenceStatus::Unknown { return Some("tools_unknown"); }
    if req.vision && caps.vision == EvidenceStatus::Unknown { return Some("vision_unknown"); }
    if req.structured_output {
        if let Some(reason)=structured_output_incompatibility(profile,req.effort.as_deref()) {return Some(reason);}
    }
    if caps.context_tokens.is_none_or(|n| u64::from(n) < u64::from(req.input_tokens) + u64::from(req.output_tokens)) { return Some("context"); }
    if caps.max_output_tokens.is_none_or(|n| req.output_tokens > n) { return Some("output_limit"); }
    if req.effort.as_ref().is_some_and(|effort| !caps.efforts.contains(effort)) { return Some("effort"); }
    if constraints.max_output_tokens.is_some_and(|max| req.output_tokens > max) { return Some("budget_output"); }
    if constraints.max_total_tokens.is_some_and(|max| u64::from(req.input_tokens) + u64::from(req.output_tokens) > max) { return Some("budget_tokens"); }
    if constraints.max_calls == Some(0) || constraints.max_duration_ms == Some(0) { return Some("budget_work"); }
    if let Some(max) = constraints.max_cost_microusd {
        if estimate_cost(profile, req).is_none_or(|cost| cost > max) { return Some("budget_cost"); }
    }
    None
}

/// Candidates must already be visible and revision-current. A forced/default
/// target passes exactly the same hard filters as ranked candidates.
/// Skipping inference cannot resolve an unknown task against exclusions.
/// Refuse that default rather than bypassing its declared suitability limits.
pub(super) fn classifier_free_default(profiles:&[ModelProfile],requirements:&WorkloadRequirements,policy:&RoutingPolicy,constraints:&RoutingConstraints)->Result<TargetReference,RoutingError> {
    let target=checked_default(profiles,requirements,policy,constraints)?;
    let profile=profiles.iter().find(|p|p.provider_id==target.provider_id).ok_or(RoutingError::ProviderUnavailable)?;
    if (requirements.task==TaskKind::Unknown && !profile.task_exclusions.is_empty())
        || profile.task_exclusions.contains(&requirements.task) {
        return Err(RoutingError::NoEligibleModel("default workload restrictions require classification but the judge cannot fit"));
    }
    Ok(target)
}

pub fn checked_default(profiles:&[ModelProfile],requirements:&WorkloadRequirements,policy:&RoutingPolicy,constraints:&RoutingConstraints)->Result<TargetReference,RoutingError> {
    let id=policy.default_provider_id.ok_or(RoutingError::NoEligibleModel("default is required"))?;
    let profile=profiles.iter().find(|p|p.provider_id==id).ok_or(RoutingError::ProviderUnavailable)?;
    let requirements=auto_requirements(profile,requirements);
    if let Some(reason)=target_incompatibility(profile,&requirements,&constraints.intersect(&policy.constraints)) {
        return Err(RoutingError::Incompatible(reason));
    }
    Ok(TargetReference {provider_id:id,provider_revision:profile.provider_revision,profile_revision:profile.revision,
        model:profile.model.clone(),effort:requirements.effort.clone()})
}

pub fn select(
    profiles: &[ModelProfile], requirements: &WorkloadRequirements,
    policy: &RoutingPolicy, constraints: &RoutingConstraints,
    objective: RoutingObjective, rule: &RuleMatch, use_default: bool,
) -> Result<RoutingDecision, RoutingError> {
    select_for(CallPurpose::Primary,profiles,requirements,policy,constraints,objective,rule,use_default)
}
pub fn select_for(purpose:CallPurpose,profiles:&[ModelProfile],requirements:&WorkloadRequirements,
    policy:&RoutingPolicy,constraints:&RoutingConstraints,objective:RoutingObjective,rule:&RuleMatch,use_default:bool)
    ->Result<RoutingDecision,RoutingError> {
    let constraints = constraints.intersect(&policy.constraints).intersect(&RoutingConstraints {
        allowed_providers:rule.allowed_providers.clone(),..Default::default()
    });
    let mut req = requirements.clone();
    if let Some(effort) = &rule.effort {
        if req.effort.as_ref().is_some_and(|old| old != effort) { return Err(RoutingError::Incompatible("rule conflicts with requested effort")); }
        req.effort = Some(effort.clone());
    }
    let mut excluded = BTreeMap::new();
    let mut candidates = Vec::new();
    for profile in profiles {
        if let Some(reason) = incompatibility(profile, &req, &constraints) {
            *excluded.entry(reason.to_string()).or_default() += 1;
        } else { candidates.push(profile); }
    }
    let forced = rule.target.or_else(|| (use_default || req.uncertain).then_some(policy.default_provider_id).flatten());
    if (use_default || req.uncertain) && forced.is_none() { return Err(RoutingError::NoEligibleModel("an allowed default is required")); }
    let observations:Option<Vec<_>>=if policy.learned_ranking && forced.is_none() {
        candidates.iter().map(|p|super::learning::nearest(p,&auto_requirements(p,&req),purpose).map(|(e,score)|(p.provider_id,e,score))).collect()
    } else {None};
    // Comparing different cohorts or evaluators would reward dataset selection,
    // not model quality. Incomplete/incomparable evidence leaves the old policy.
    let evidence=observations.filter(|rows|rows.first().is_some_and(|(_,first,_)|rows.iter().all(|(_,e,_)|
        super::learning::comparable(e,first))))
        .map(|rows|rows.into_iter().map(|(id,e,score)|(id,(e.id,score))).collect::<BTreeMap<_,_>>());
    let learned=evidence.as_ref();
    if let Some(scores)=learned {
        candidates.retain(|p|{
            if scores[&p.provider_id].1>=8000 {true}
            else {*excluded.entry("measured_quality_floor".into()).or_default()+=1;false}
        });
    }
    let chosen = if let Some(id) = forced {
        candidates.into_iter().find(|p| p.provider_id == id)
    } else {
        candidates.sort_by_key(|profile| {
            let cost = estimate_cost(profile, &auto_requirements(profile,&req)).unwrap_or(u64::MAX);
            let quality = tier(profile, req.task) as u64;
            let latency = profile.latency_ms.unwrap_or(u64::MAX);
            let rank = match objective {
                RoutingObjective::Economy => (cost, latency, quality),
                RoutingObjective::Balanced => (quality, cost, latency),
                RoutingObjective::Quality => (u64::MAX - quality, cost, latency),
            };
            let measured=if objective==RoutingObjective::Quality {learned.map_or(0,|scores|10000-scores[&profile.provider_id].1)} else {0};
            (measured,rank, profile.provider_id)
        });
        candidates.first().copied()
    }.ok_or(RoutingError::NoEligibleModel("capability, policy or budget filters excluded the target"))?;
    let mut reasons = vec![if rule.target.is_some() { "explicit_rule" } else if use_default || req.uncertain { "insufficient_evidence" } else { "ranked_eligible_pool" }.into()];
    if rule.allowed_providers.is_some() {reasons.push("rule_pool".into());}
    if let Some(scores)=learned {reasons.push(format!("learned_evidence:{}",scores[&chosen.provider_id].0));}
    else if policy.learned_ranking && forced.is_none() {reasons.push("learned_insufficient_evidence".into());}
    let chosen_req=auto_requirements(chosen,&req);
    if req.effort.is_none() && chosen.auto_effort.is_some() {reasons.push("model_effort_preset".into());}
    if chosen.quality_status == EvidenceStatus::Declared { reasons.push("operator_declared_quality".into()); }
    if estimate_cost(chosen, &req).is_none() { reasons.push("price_unknown".into()); }
    Ok(RoutingDecision { id: Uuid::now_v7(), schema_version: SCHEMA_VERSION,
        policy_id: policy.id, policy_revision: policy.revision, strategy: policy.strategy,
        proposed: Some(TargetReference { provider_id: chosen.provider_id, provider_revision: chosen.provider_revision,
            profile_revision: chosen.revision, model: chosen.model.clone(), effort: chosen_req.effort.clone() }),
        actual: None, reason_codes: reasons, excluded_counts: excluded,
        estimated_cost_microusd: estimate_cost(chosen, &chosen_req) })
}
