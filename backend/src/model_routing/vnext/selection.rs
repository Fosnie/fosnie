// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! User assignments and declared model roles. No benchmark admission is required.
use super::{catalogue::ModelVariant, profile::*};
use crate::model_routing::*;
use std::collections::BTreeMap;

pub fn choose(
    config: &RoutingProfileConfig,
    groups: &GroupResolution,
    candidates: &BTreeMap<uuid::Uuid, (&ModelVariant, &ModelProfile)>,
    req: &WorkloadRequirements,
    objective: RoutingObjective,
    legacy_pin: Option<super::catalogue::VariantRef>,
) -> Result<(super::catalogue::VariantRef, &'static str), RoutingError> {
    if let (Some(a), Some(b)) = (groups.pin, legacy_pin) {
        if a != b { return Err(RoutingError::Invalid("conflicting model assignments")); }
    }
    let exact = |v: super::catalogue::VariantRef| {
        candidates.get(&v.id).filter(|(m,_)| m.revision == v.revision)
            .map(|_| v).ok_or(RoutingError::NeedsReplan)
    };
    if let Some(pin) = groups.pin.or(legacy_pin) {
        if !groups.allowed_variants.contains(&pin.id) {
            return Err(RoutingError::NoEligibleModel("assigned variant excluded or unavailable"));
        }
        return Ok((exact(pin)?, "task_assignment"));
    }
    let default = match config.reference {
        ReferencePolicy::Configured {variant,..} => Some(variant),
        ReferencePolicy::Automatic {default_variant,..} => default_variant,
    };
    if req.uncertain {
        if let Some(v) = default.filter(|v| groups.reference_variants.contains(&v.id)) {
            return Ok((exact(v)?, "uncertain_task_default"));
        }
        return Err(RoutingError::NoEligibleModel("uncertain task needs an available default model"));
    }
    // A user preference is an explicit choice, including a premium model for a
    // routine task or a small model for a complex task. Capabilities still apply.
    if let Some((v,_)) = groups.preferences.iter().find_map(|id| candidates.get(id)) {
        return Ok((super::catalogue::VariantRef {id:v.id,revision:v.revision}, "task_preference"));
    }
    let mut ranked: Vec<_> = groups.allowed_variants.iter().filter_map(|id| candidates.get(id))
        .filter(|(_,p)| p.auto_enabled && p.task_tiers.get(&req.task).copied().unwrap_or(p.tier) >= req.minimum_tier)
        .collect();
    ranked.sort_by_key(|(v,p)| {
        let tier = p.task_tiers.get(&req.task).copied().unwrap_or(p.tier) as u64;
        let cost = engine::estimate_cost(p,req).unwrap_or(u64::MAX);
        let latency = p.latency_ms.unwrap_or(u64::MAX);
        let rank = match objective {
            RoutingObjective::Economy => (cost,tier,latency),
            RoutingObjective::Balanced => (tier,cost,latency),
            RoutingObjective::Quality => (u64::MAX-tier,cost,latency),
        };
        (rank, default.is_none_or(|d| d.id != v.id), v.id)
    });
    if let Some((v,_)) = ranked.first() {
        return Ok((super::catalogue::VariantRef {id:v.id,revision:v.revision}, "configured_model_roles"));
    }
    if let Some(v) = default.filter(|v| groups.reference_variants.contains(&v.id)) {
        return Ok((exact(v)?, "no_matching_role_default"));
    }
    Err(RoutingError::NoEligibleModel("no model matches the configured routing roles"))
}
