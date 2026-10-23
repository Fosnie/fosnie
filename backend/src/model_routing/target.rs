// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0

//! Strict, opt-in target resolution. Legacy calls retain their own compatibility
//! path; routed calls never assemble an endpoint from process defaults.
use serde::Serialize;
use uuid::Uuid;
use super::*;

/// Only this transient object contains a decrypted credential. Never persist it.
#[derive(Clone, Serialize)]
pub struct InferenceTarget {
    pub schema_version: u32,
    pub provider_id: Uuid,
    pub provider_revision: i64,
    pub profile_revision: i64,
    pub adapter: AdapterKind,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub effort: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub generation: Option<vnext::catalogue::GenerationConfig>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub variant: Option<vnext::execution::VariantBinding>,
}

impl std::fmt::Debug for InferenceTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceTarget").field("provider_id", &self.provider_id)
            .field("provider_revision", &self.provider_revision).finish_non_exhaustive()
    }
}

impl InferenceTarget {
    pub fn reference(&self) -> TargetReference {
        TargetReference { provider_id: self.provider_id, provider_revision: self.provider_revision,
            profile_revision: self.profile_revision, model: self.model.clone(), effort: self.effort.clone(), variant:self.variant.clone() }
    }
}

#[derive(Serialize)]
pub struct InferenceEnvelope {
    pub schema_version: u32,
    pub decision_id: Uuid,
    pub purpose: CallPurpose,
    pub assignments: std::collections::BTreeMap<CallPurpose, InferenceTarget>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub budget: Option<super::lease::BudgetLease>,
    #[serde(skip_serializing_if="std::collections::BTreeMap::is_empty")]
    pub fixed_assignments: std::collections::BTreeMap<super::fixed::FixedRole,super::fixed::FixedTarget>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub fixed_budget: Option<super::fixed_lease::FixedLease>,
}

impl InferenceEnvelope {
    pub fn apply(&self, mut overrides: crate::ml::ProviderOverrides) -> crate::Result<crate::ml::ProviderOverrides> {
        if !self.assignments.contains_key(&self.purpose) && !self.fixed_assignments.keys().any(|role|role.purpose()==self.purpose) {
            return Err(RoutingError::Invalid("missing inference assignment").into());
        }
        overrides.insert("routing".into(), serde_json::to_value(self)
            .map_err(|_| RoutingError::Invalid("inference envelope"))?);
        Ok(overrides)
    }
}

#[derive(sqlx::FromRow)]
struct TargetMetadata {
    scope: String,
    scope_id: Option<Uuid>,
    enabled: bool,
    base_url: Option<String>,
    routing_revision: i64,
    profile_revision: Option<i64>,
    profile: Option<serde_json::Value>,
}

/// All checks that do not require credentials run before the injected registry
/// decrypts anything. Only explicitly routed execution calls this function.
pub async fn resolve_target(
    state: &crate::AppState,
    context: &RoutingContext,
    reference: &TargetReference,
    constraints: &RoutingConstraints,
) -> crate::Result<InferenceTarget> {
    let row = sqlx::query_as::<_, TargetMetadata>(
        "SELECT p.scope, p.scope_id, p.enabled AND CASE WHEN p.connection_id IS NULL THEN true ELSE COALESCE(c.enabled,false) END AS enabled, p.base_url, p.routing_revision, m.revision AS profile_revision, m.profile FROM provider_configs p LEFT JOIN provider_configs c ON c.id=p.connection_id AND c.role='connection' LEFT JOIN model_profiles m ON m.provider_id=p.id WHERE p.id=$1 AND p.role='llm'")
        .bind(reference.provider_id).fetch_optional(&state.pg).await?
        .ok_or(RoutingError::ProviderUnavailable)?;
    if row.scope != "deployment" {
        if row.scope != "user" || row.scope_id.is_none() || row.scope_id != context.actor.user_id {
            return Err(RoutingError::Forbidden.into());
        }
        let byok = crate::config::runtime::get(&state.pg, "providers.user_byok_enabled").await?
            .map_or(state.boot.providers.user_byok_enabled, |entry| entry.value == "true");
        if !byok { return Err(RoutingError::Forbidden.into()); }
    }
    if !row.enabled { return Err(RoutingError::ProviderUnavailable.into()); }
    let profile: ModelProfile = serde_json::from_value(row.profile.ok_or(RoutingError::NeedsReplan)?)
        .map_err(|_| RoutingError::Invalid("stored model profile"))?;
    if profile.provider_id != reference.provider_id || profile.provider_revision != row.routing_revision
        || reference.provider_revision != row.routing_revision || row.profile_revision != Some(reference.profile_revision)
        || profile.revision != reference.profile_revision || profile.model != reference.model {
        return Err(RoutingError::NeedsReplan.into());
    }
    let extension = state.inference_policy.constraints(&state.pg, context).await?;
    let effective = constraints.intersect(&extension);
    let locality = if profile.locality_trusted { profile.locality } else { Locality::Unknown };
    if !effective.permits(reference.provider_id, locality) { return Err(RoutingError::Forbidden.into()); }
    if reference.effort.as_ref().is_some_and(|effort| !profile.capabilities.efforts.contains(effort)) {
        return Err(RoutingError::Incompatible("unsupported reasoning effort").into());
    }
    if let Some(failure)=super::health::blocked(state,reference,context.purpose).await? {return Err(failure.app());}
    let variant=vnext::execution::current_variant(state,&context.actor,reference).await?;
    let provider = state.providers.resolve_llm_id(&state.pg, context.actor.user_id, reference.provider_id).await?
        .ok_or(RoutingError::ProviderUnavailable)?;
    let base_url = provider.base_url.filter(|v| !v.trim().is_empty())
        .ok_or(RoutingError::Invalid("model endpoint is required"))?;
    let model = provider.model.filter(|v| !v.trim().is_empty())
        .ok_or(RoutingError::Invalid("model name is required"))?;
    if row.base_url.as_deref().map(|v| v.trim_end_matches('/')) != Some(base_url.trim_end_matches('/')) {
        return Err(RoutingError::NeedsReplan.into());
    }
    if !provider.enabled || model != reference.model { return Err(RoutingError::NeedsReplan.into()); }
    // Recheck the revision after decrypt to detect edits during resolution.
    let current: Option<(i64,i64)> = sqlx::query_as("SELECT p.routing_revision, m.revision FROM provider_configs p JOIN model_profiles m ON m.provider_id=p.id WHERE p.id=$1")
        .bind(reference.provider_id).fetch_optional(&state.pg).await?;
    if current != Some((reference.provider_revision,reference.profile_revision)) { return Err(RoutingError::NeedsReplan.into()); }
    let target=InferenceTarget { schema_version: if reference.variant.is_some(){2}else{1},
        generation:variant.as_ref().map(|v|v.behaviour.generation.clone()),variant:reference.variant.clone(), provider_id: reference.provider_id,
        provider_revision: reference.provider_revision, profile_revision: reference.profile_revision, adapter: profile.adapter,
        base_url, model, api_key: provider.api_key.unwrap_or_default(), effort: reference.effort.clone() };
    vnext::execution::current_variant(state,&context.actor,reference).await?;
    super::health::register(state,&target,context.purpose).await?;
    Ok(target)
}
