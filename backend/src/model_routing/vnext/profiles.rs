// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Profile management extends the existing policy row and CAS revision.
use super::super::{RoutingError, RoutingPolicy, store};
use super::{
    catalogue::{CatalogueScope, VariantRef},
    connections::check_actor,
    profile::*,
    variants,
};
use crate::{AppState, Result, auth::AuthContext};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Serialize)]
pub struct ProfileEntry {
    pub schema_version: u32,
    pub id: Uuid,
    pub label: String,
    pub scope: String,
    pub revision: i64,
    pub is_default: bool,
    pub policy: RoutingPolicy,
    pub config: RoutingProfileConfig,
    pub calibration_state: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveProfile {
    pub schema_version: u32,
    pub label: String,
    pub scope: String,
    pub expected_revision: i64,
    pub is_default: bool,
    pub policy: RoutingPolicy,
    pub config: RoutingProfileConfig,
}
pub async fn list(state: &AppState, actor: &AuthContext) -> Result<Vec<ProfileEntry>> {
    check_actor(state, actor).await?;
    let rows:Vec<(Uuid,String,String,i64,bool,serde_json::Value,serde_json::Value)>=sqlx::query_as("SELECT id,label,scope,revision,is_default,policy,profile_config FROM routing_policies WHERE profile_config IS NOT NULL AND (scope='deployment' OR (scope='user' AND scope_id=$1)) ORDER BY scope,label,id")
        .bind(actor.user_id).fetch_all(&state.pg).await?;

    rows.into_iter()
        .map(|(id, label, scope, revision, is_default, policy, config)| {
            let policy: RoutingPolicy = serde_json::from_value(policy)
                .map_err(|_| RoutingError::Invalid("stored profile policy"))?;
            let config: RoutingProfileConfig = serde_json::from_value(config)
                .map_err(|_| RoutingError::Invalid("stored profile config"))?;
            config.validate_policy_extension(&policy)?;
            if policy.id != id || policy.revision != revision {
                return Err(RoutingError::NeedsReplan.into());
            }
            let calibration_state="configured".to_owned();
            Ok(ProfileEntry {
                schema_version: 2,
                id,
                label,
                scope,
                revision,
                is_default,
                policy,
                config,
                calibration_state,
            })
        })
        .collect()
}
pub async fn load(state: &AppState, actor: &AuthContext, id: Uuid) -> Result<RoutingProfileConfig> {
    list(state, actor)
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .map(|p| p.config)
        .ok_or_else(|| RoutingError::Invalid("v2 profile required").into())
}
pub(crate) async fn validate_references(
    state: &AppState,
    actor: &AuthContext,
    owner: Option<Uuid>,
    config: &RoutingProfileConfig,
) -> Result<()> {
    let catalogue = variants::catalogue(state, actor).await?;
    let visible: BTreeMap<_, _> = catalogue
        .variants
        .iter()
        .filter(|e| {
            e.configuration_current
                && e.binding_current
                && (owner.is_some() || e.variant.scope == CatalogueScope::Deployment)
        })
        .map(|e| (e.variant.id, &e.variant))
        .collect();
    let mut ids = BTreeSet::new();
    let mut exact = Vec::new();
    if let PoolMembership::Selected { variant_ids } = &config.membership {
        ids.extend(variant_ids);
    }
    ids.extend(&config.fallback_variant_ids);
    match &config.reference {
        ReferencePolicy::Configured { variant, .. } => exact.push(*variant),
        ReferencePolicy::Automatic {
            default_variant, ..
        } => exact.extend(*default_variant),
    }
    for g in &config.groups {
        ids.extend(g.allowed_variants.iter().flatten());
        ids.extend(&g.excluded_variants);
        match &g.assignment {
            Some(GroupAssignment::Pin { variant }) => exact.push(*variant),
            Some(GroupAssignment::Prefer { variant_id }) => {
                ids.insert(*variant_id);
            }
            None => {}
        }
    }
    if ids.iter().any(|id| !visible.contains_key(id)) {
        return Err(RoutingError::Forbidden.into());
    }
    if exact.iter().any(|v| {
        !visible
            .get(&v.id)
            .is_some_and(|saved| saved.revision == v.revision)
    }) {
        return Err(RoutingError::NeedsReplan.into());
    }
    Ok(())
}
pub async fn save(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    mut body: SaveProfile,
) -> Result<ProfileEntry> {
    check_actor(state, actor).await?;
    body.config.selector_sha256=None;
    body.config.verifier_sha256=None;
    body.config.workload_scope=None;
    body.config.quality.assurance=Assurance::Declared;
    body.policy.learned_ranking=false;
    body.policy.quality_escalation=false;
    if let Some(r)=&mut body.config.recovery {r.source_escalation=false;}
    if body.schema_version != 2 {
        return Err(RoutingError::Invalid("profile schema").into());
    }
    let mut legacy = store::SavePolicy {
        label: body.label,
        scope: body.scope,
        expected_revision: body.expected_revision,
        is_default: body.is_default,
        policy: body.policy,
    };
    store::save_policy_extended(state, actor, id, &mut legacy, Some(&body.config)).await?;
    list(state, actor)
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| RoutingError::NeedsReplan.into())
}

/// Portable drafts contain no connection, credential, quality artifact or receipt.
/// IDs are source-local slots: an import must explicitly map every referenced slot.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileExport {
    pub schema_version: u32,
    pub label: String,
    pub policy: RoutingPolicy,
    pub config: RoutingProfileConfig,
}
pub async fn export(state: &AppState, actor: &AuthContext, id: Uuid) -> Result<ProfileExport> {
    let e = list(state, actor)
        .await?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or(RoutingError::Forbidden)?;
    let mut config = e.config;
    config.selector_sha256 = None;
    config.verifier_sha256 = None;
    Ok(ProfileExport {
        schema_version: 2,
        label: e.label,
        policy: e.policy,
        config,
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportProfile {
    pub schema_version: u32,
    pub scope: String,
    pub expected_revision: i64,
    pub draft: ProfileExport,
    pub variant_mapping: BTreeMap<Uuid, VariantRef>,
    pub provider_mapping: BTreeMap<Uuid, Uuid>,
}
pub async fn import(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    mut b: ImportProfile,
) -> Result<ProfileEntry> {
    if b.schema_version != 2 || b.draft.schema_version != 2 {
        return Err(RoutingError::Invalid("profile import schema").into());
    }
    let map = |v: Uuid| {
        b.variant_mapping
            .get(&v)
            .copied()
            .ok_or(RoutingError::Invalid("explicit variant mapping required"))
    };
    let set = |ids: &BTreeSet<Uuid>| {
        ids.iter()
            .map(|v| map(*v).map(|r| r.id))
            .collect::<std::result::Result<BTreeSet<_>, _>>()
    };
    let c = &mut b.draft.config;
    if let PoolMembership::Selected { variant_ids } = &mut c.membership {
        *variant_ids = set(variant_ids)?;
    }
    c.fallback_variant_ids = set(&c.fallback_variant_ids)?;
    match &mut c.reference {
        ReferencePolicy::Configured { variant, .. } => *variant = map(variant.id)?,
        ReferencePolicy::Automatic {
            default_variant, ..
        } => {
            if let Some(v) = default_variant {
                *v = map(v.id)?;
            }
        }
    }
    for g in &mut c.groups {
        g.id = Uuid::now_v7();
        g.allowed_variants = g.allowed_variants.as_ref().map(set).transpose()?;
        g.excluded_variants = set(&g.excluded_variants)?;
        match &mut g.assignment {
            Some(GroupAssignment::Pin { variant }) => *variant = map(variant.id)?,
            Some(GroupAssignment::Prefer { variant_id }) => *variant_id = map(*variant_id)?.id,
            None => {}
        }
    }
    c.selector_sha256 = None;
    c.verifier_sha256 = None;
    let p = &mut b.draft.policy;
    p.id = id;
    p.revision = 1;
    let provider = |v: Uuid| {
        b.provider_mapping
            .get(&v)
            .copied()
            .ok_or(RoutingError::Invalid("explicit provider mapping required"))
    };
    p.default_provider_id = p.default_provider_id.map(provider).transpose()?;
    p.classifier_provider_id = p.classifier_provider_id.map(provider).transpose()?;
    for v in p.utility_assignments.values_mut() {
        *v = provider(*v)?;
    }
    if let Some(ids) = &mut p.constraints.allowed_providers {
        *ids = ids
            .iter()
            .map(|v| provider(*v))
            .collect::<std::result::Result<_, _>>()?;
    }
    for r in &mut p.rules {
        r.id = Uuid::now_v7();
        match &mut r.action {
            super::super::RuleAction::Select { provider_id, .. } => {
                *provider_id = provider(*provider_id)?
            }
            super::super::RuleAction::RestrictPool { allowed_providers } => {
                *allowed_providers = allowed_providers
                    .iter()
                    .map(|v| provider(*v))
                    .collect::<std::result::Result<_, _>>()?
            }
            _ => {}
        }
    }
    save(
        state,
        actor,
        id,
        SaveProfile {
            schema_version: 2,
            label: b.draft.label,
            scope: b.scope,
            expected_revision: b.expected_revision,
            is_default: false,
            policy: b.draft.policy,
            config: b.draft.config,
        },
    )
    .await
}
