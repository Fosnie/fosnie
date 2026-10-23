// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::super::{CallPurpose, RoutingError, RuleConditions, WorkloadRequirements};
use super::catalogue::VariantRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    Trivial,
    Routine,
    Standard,
    Complex,
}

/// Unknown is not trivial. Confidence is a bounded signal, not quality evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DifficultySignal {
    Unknown,
    Known {
        level: Difficulty,
        confidence_bp: u16,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DifficultyCondition {
    pub levels: BTreeSet<Difficulty>,
    pub min_confidence_bp: u16,
    pub include_unknown: bool,
}
impl Default for DifficultySignal {fn default()->Self{Self::Unknown}}

impl DifficultyCondition {
    fn matches(&self, signal: DifficultySignal) -> bool {
        match signal {
            DifficultySignal::Unknown => self.include_unknown,
            DifficultySignal::Known {
                level,
                confidence_bp,
            } => {
                confidence_bp <= 10_000
                    && confidence_bp >= self.min_confidence_bp
                    && self.levels.contains(&level)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PoolMembership {
    Selected { variant_ids: BTreeSet<Uuid> },
    AllAvailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assurance {
    Declared,
    Calibrated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityTarget {
    pub assurance: Assurance,
    /// Calibrated reference-relative quality difference; 200 = two pp.
    pub max_relative_loss_bp: u16,
    pub minimum_absolute_quality_bp: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReferencePolicy {
    Configured {
        variant: VariantRef,
        allow_uncalibrated: bool,
    },
    /// Per-workload selection uses evidence, never a price/name heuristic.
    Automatic {
        default_variant: Option<VariantRef>,
        allow_uncalibrated_default: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GroupAssignment {
    Pin { variant: VariantRef },
    Prefer { variant_id: Uuid },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskGroup {
    pub id: Uuid,
    pub label: String,
    pub priority: i32,
    pub conditions: RuleConditions,
    pub difficulty: Option<DifficultyCondition>,
    /// Every matched restriction intersects. Some(empty) deliberately denies.
    pub allowed_variants: Option<BTreeSet<Uuid>>,
    pub excluded_variants: BTreeSet<Uuid>,
    pub assignment: Option<GroupAssignment>,
}

/// Stored as a versioned extension of RoutingPolicy, never a second policy table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingProfileConfig {
    pub schema_version: u32,
    pub membership: PoolMembership,
    pub max_active_variants: usize,
    pub quality: QualityTarget,
    pub reference: ReferencePolicy,
    pub fallback_variant_ids: BTreeSet<Uuid>,
    pub groups: Vec<TaskGroup>,
    /// These are registered, immutable local asset identities, not executable paths.
    pub selector_sha256: Option<String>,
    pub verifier_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<super::recovery::Config>,
}

impl RoutingProfileConfig {
    pub fn validate(&self) -> Result<(), RoutingError> {
        if self
            .language_scope
            .as_ref()
            .is_some_and(|s| !matches!(s.as_str(), "en" | "ru"))
            || self
                .workload_scope
                .as_deref()
                .is_some_and(|s| s != "source_extract_qa_v1" && s != "mixed_semantic_v1")
            || self.recovery.as_ref().is_some_and(|c| {
                c.source_escalation
                    && self.workload_scope.as_deref() != Some("source_extract_qa_v1")
            })
            || self.schema_version != super::ROUTING_VNEXT_VERSION
            || self.max_active_variants == 0
            || self.max_active_variants > super::MAX_ACTIVE_VARIANTS
            || self.groups.len() > 100
            || self.fallback_variant_ids.len() > super::MAX_ACTIVE_VARIANTS
            || self.fallback_variant_ids.contains(&Uuid::nil())
            || self.quality.max_relative_loss_bp > 10_000
            || self
                .quality
                .minimum_absolute_quality_bp
                .is_some_and(|v| v > 10_000)
            || self
                .selector_sha256
                .as_ref()
                .is_some_and(|v| !super::sha256(v))
            || self
                .verifier_sha256
                .as_ref()
                .is_some_and(|v| !super::sha256(v))
        {
            return Err(RoutingError::Invalid("routing profile contract limits"));
        }
        if let PoolMembership::Selected { variant_ids } = &self.membership {
            if variant_ids.is_empty()
                || variant_ids.len() > self.max_active_variants
                || variant_ids.contains(&Uuid::nil())
            {
                return Err(RoutingError::Invalid("selected variant pool"));
            }
        }
        let valid_ref = |v: VariantRef| !v.id.is_nil() && v.revision > 0;
        match &self.reference {
            ReferencePolicy::Configured { variant, .. } if !valid_ref(*variant) => {
                return Err(RoutingError::Invalid("reference variant"));
            }
            ReferencePolicy::Automatic {
                default_variant: Some(v),
                ..
            } if !valid_ref(*v) => return Err(RoutingError::Invalid("default reference variant")),
            _ => {}
        }
        let mut ids = BTreeSet::new();
        let mut nodes = 0;
        for group in &self.groups {
            if group.id.is_nil()
                || !ids.insert(group.id)
                || group.label.trim().is_empty()
                || group.label.chars().count() > 120
                || group.allowed_variants.as_ref().is_some_and(|v| {
                    v.len() > super::MAX_ACTIVE_VARIANTS || v.contains(&Uuid::nil())
                })
                || group.excluded_variants.len() > super::MAX_ACTIVE_VARIANTS
                || group.excluded_variants.contains(&Uuid::nil())
                || group.difficulty.as_ref().is_some_and(|d| {
                    d.min_confidence_bp > 10_000 || (d.levels.is_empty() && !d.include_unknown)
                })
            {
                return Err(RoutingError::Invalid("task group contract limits"));
            }
            super::super::engine::validate_conditions(&group.conditions, 1, &mut nodes)?;
            match &group.assignment {
                Some(GroupAssignment::Pin { variant }) => {
                    if !valid_ref(*variant) {
                        return Err(RoutingError::Invalid("pinned variant"));
                    }
                    // Measured pins need a separate proven admission contract.
                    // Until it exists, never let a rule masquerade as calibration.
                    if self.quality.assurance == Assurance::Calibrated {
                        return Err(RoutingError::Invalid(
                            "hard pins require declared assurance",
                        ));
                    }
                }
                Some(GroupAssignment::Prefer { variant_id }) if variant_id.is_nil() => {
                    return Err(RoutingError::Invalid("preferred variant"));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> Result<String, RoutingError> {
        self.validate()?;
        super::digest("fosnie.routing.profile.v2", self)
    }

    /// D2 must invoke this alongside existing policy validation at save time.
    pub fn validate_policy_extension(
        &self,
        policy: &super::super::RoutingPolicy,
    ) -> Result<(), RoutingError> {
        self.validate()?;
        if self.recovery.as_ref().is_some_and(|r| {
            r.technical && !policy.technical_fallback
                || r.source_escalation && !policy.quality_escalation
        }) {
            return Err(RoutingError::Invalid(
                "profile recovery requires matching policy permission",
            ));
        }
        if policy.learned_ranking || policy.quality_escalation {
            return Err(RoutingError::Invalid("evaluation-based routing has been retired; save a routing profile"));
        }
        Ok(())
    }

    /// Call after legacy mandatory restrictions and before any quality scoring.
    /// This only narrows an authorised variant set; it cannot introduce a target.
    pub fn resolve_groups(
        &self,
        authorised_variants: &BTreeSet<Uuid>,
        req: &WorkloadRequirements,
        purpose: CallPurpose,
        difficulty: DifficultySignal,
    ) -> Result<GroupResolution, RoutingError> {
        self.resolve_groups_inner(authorised_variants, req, purpose, difficulty, None)
    }
    /// Runtime alias keys are server-derived after actor/mandatory filters. IDs
    /// remain distinct for restrictions and pins; capacity counts configurations.
    pub(crate) fn resolve_groups_with_aliases(
        &self,
        aliases: &std::collections::BTreeMap<Uuid, (Uuid, String)>,
        req: &WorkloadRequirements,
        purpose: CallPurpose,
        difficulty: DifficultySignal,
    ) -> Result<GroupResolution, RoutingError> {
        self.resolve_groups_inner(
            &aliases.keys().copied().collect(),
            req,
            purpose,
            difficulty,
            Some(aliases),
        )
    }
    fn resolve_groups_inner(
        &self,
        authorised_variants: &BTreeSet<Uuid>,
        req: &WorkloadRequirements,
        purpose: CallPurpose,
        difficulty: DifficultySignal,
        aliases: Option<&std::collections::BTreeMap<Uuid, (Uuid, String)>>,
    ) -> Result<GroupResolution, RoutingError> {
        self.validate()?;
        if matches!(difficulty, DifficultySignal::Known { confidence_bp, .. } if confidence_bp > 10_000)
        {
            return Err(RoutingError::Invalid("difficulty confidence"));
        }
        let mut allowed = match &self.membership {
            PoolMembership::AllAvailable => authorised_variants.clone(),
            PoolMembership::Selected { variant_ids } => authorised_variants
                .intersection(variant_ids)
                .copied()
                .collect(),
        };
        let mut fallback: BTreeSet<_> = authorised_variants
            .intersection(&self.fallback_variant_ids)
            .copied()
            .collect();
        let mut references = match &self.reference {
            ReferencePolicy::Configured { variant, .. } => {
                [variant.id].into_iter().collect::<BTreeSet<_>>()
            }
            ReferencePolicy::Automatic {
                default_variant, ..
            } => {
                let mut references = allowed.clone();
                if let Some(v) = default_variant {
                    references.insert(v.id);
                }
                references
            }
        };
        references = references
            .intersection(authorised_variants)
            .copied()
            .collect();
        let mut active = allowed.clone();
        active.extend(fallback.iter().copied());
        active.extend(references.iter().copied());
        // Enforce capacity across primary, fallback and references before task
        // narrowing. Separate pools cannot be used to evade the snapshot cap.
        let count = aliases.map_or(active.len(), |map| {
            active
                .iter()
                .filter_map(|id| map.get(id))
                .collect::<BTreeSet<_>>()
                .len()
        });
        if count > self.max_active_variants {
            return Err(RoutingError::Invalid("active variant capacity exceeded"));
        }
        let mut groups: Vec<_> = self
            .groups
            .iter()
            .filter(|g| {
                g.conditions.matches(req, purpose)
                    && g.difficulty.as_ref().is_none_or(|d| d.matches(difficulty))
            })
            .collect();
        groups.sort_by_key(|g| (std::cmp::Reverse(g.priority), g.id));
        let mut pinned: Option<(i32, VariantRef)> = None;
        let mut preferences = Vec::new();
        let mut matched = Vec::new();
        for group in groups {
            matched.push(group.id);
            if let Some(pool) = &group.allowed_variants {
                allowed = allowed.intersection(pool).copied().collect();
                fallback = fallback.intersection(pool).copied().collect();
                references = references.intersection(pool).copied().collect();
            }
            allowed.retain(|id| !group.excluded_variants.contains(id));
            fallback.retain(|id| !group.excluded_variants.contains(id));
            references.retain(|id| !group.excluded_variants.contains(id));
            match &group.assignment {
                Some(GroupAssignment::Pin { variant }) => match pinned {
                    Some((priority, other)) if priority == group.priority && other != *variant => {
                        return Err(RoutingError::Invalid(
                            "conflicting equal-priority hard pins",
                        ));
                    }
                    None => pinned = Some((group.priority, *variant)),
                    _ => {}
                },
                Some(GroupAssignment::Prefer { variant_id }) => {
                    if !preferences.contains(variant_id) {
                        preferences.push(*variant_id);
                    }
                }
                None => {}
            }
        }
        if pinned.is_some_and(|(_, v)| !allowed.contains(&v.id)) {
            return Err(RoutingError::NoEligibleModel(
                "hard pin excluded or unavailable",
            ));
        }
        preferences.retain(|id| allowed.contains(id));
        if allowed.is_empty() && fallback.is_empty() && references.is_empty() {
            return Err(RoutingError::NoEligibleModel(
                "task groups exclude the entire pool",
            ));
        }
        Ok(GroupResolution {
            allowed_variants: allowed,
            fallback_variants: fallback,
            reference_variants: references,
            pin: pinned.map(|(_, v)| v),
            preferences,
            matched_groups: matched,
        })
    }
}

#[derive(Debug, Clone)]
pub struct GroupResolution {
    pub allowed_variants: BTreeSet<Uuid>,
    pub fallback_variants: BTreeSet<Uuid>,
    pub reference_variants: BTreeSet<Uuid>,
    pub pin: Option<VariantRef>,
    pub preferences: Vec<Uuid>,
    pub matched_groups: Vec<Uuid>,
}
