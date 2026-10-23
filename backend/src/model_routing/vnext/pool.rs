// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::super::RoutingError;
use super::catalogue::{CandidateRef, VariantRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolSnapshot {
    pub schema_version: u32,
    pub policy_id: Uuid,
    pub policy_revision: i64,
    pub catalogue_revision: i64,
    /// Hash of authorised scope identity/revision, never a credential binding.
    pub authorisation_scope_sha256: String,
    pub profile_config_sha256: String,
    pub candidates: Vec<CandidateRef>,
    pub hash: String,
}

#[derive(Serialize)]
struct SnapshotIdentity<'a> {
    schema_version: u32,
    policy_id: Uuid,
    policy_revision: i64,
    catalogue_revision: i64,
    authorisation_scope_sha256: &'a str,
    profile_config_sha256: &'a str,
    candidates: &'a [CandidateRef],
}
impl PoolSnapshot {
    /// Inputs are already filtered by Core visibility, mandatory policy,
    /// locality and capabilities. A predictor never constructs this snapshot.
    pub fn freeze(
        policy_id: Uuid,
        policy_revision: i64,
        catalogue_revision: i64,
        authorisation_scope_sha256: String,
        profile_config_sha256: String,
        mut candidates: Vec<CandidateRef>,
        max_active_variants: usize,
    ) -> Result<Self, RoutingError> {
        if policy_id.is_nil()
            || policy_revision < 1
            || catalogue_revision < 1
            || !super::sha256(&authorisation_scope_sha256)
            || !super::sha256(&profile_config_sha256)
            || max_active_variants == 0
            || max_active_variants > super::MAX_ACTIVE_VARIANTS
            || candidates.len() > max_active_variants
        {
            return Err(RoutingError::Invalid("pool snapshot identity or capacity"));
        }
        if candidates.is_empty() {
            return Err(RoutingError::NoEligibleModel("no authorised variants"));
        }
        let mut ids = BTreeSet::new();
        let mut configs = BTreeSet::new();
        for candidate in &candidates {
            candidate.validate()?;
            if !ids.insert(candidate.variant.id)
                || !configs.insert((candidate.provider_id, candidate.behaviour_sha256.clone()))
            {
                return Err(RoutingError::Invalid("duplicate effective variant"));
            }
        }
        candidates.sort_by_key(|c| (c.provider_id, c.variant.id));
        let mut snapshot = Self {
            schema_version: super::ROUTING_VNEXT_VERSION,
            policy_id,
            policy_revision,
            catalogue_revision,
            authorisation_scope_sha256,
            profile_config_sha256,
            candidates,
            hash: String::new(),
        };
        snapshot.hash = snapshot.calculate_hash()?;
        Ok(snapshot)
    }

    fn calculate_hash(&self) -> Result<String, RoutingError> {
        super::digest(
            "fosnie.authorised.pool.v2",
            &SnapshotIdentity {
                schema_version: self.schema_version,
                policy_id: self.policy_id,
                policy_revision: self.policy_revision,
                catalogue_revision: self.catalogue_revision,
                authorisation_scope_sha256: &self.authorisation_scope_sha256,
                profile_config_sha256: &self.profile_config_sha256,
                candidates: &self.candidates,
            },
        )
    }

    pub fn validate(&self, max_active_variants: usize) -> Result<(), RoutingError> {
        if self.schema_version != super::ROUTING_VNEXT_VERSION {
            return Err(RoutingError::Invalid("pool schema version"));
        }
        let rebuilt = Self::freeze(
            self.policy_id,
            self.policy_revision,
            self.catalogue_revision,
            self.authorisation_scope_sha256.clone(),
            self.profile_config_sha256.clone(),
            self.candidates.clone(),
            max_active_variants,
        )?;
        if rebuilt.hash != self.hash || self.calculate_hash()? != self.hash {
            return Err(RoutingError::NeedsReplan);
        }
        Ok(())
    }

    pub fn require_variant(&self, reference: VariantRef) -> Result<&CandidateRef, RoutingError> {
        self.candidates
            .iter()
            .find(|c| c.variant == reference)
            .ok_or(RoutingError::NeedsReplan)
    }
}
