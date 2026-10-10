// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use crate::{AppState, AppError, Result, auth::{AuthContext, permissions}};
use super::*;

#[derive(Serialize, sqlx::FromRow)]
pub struct CatalogEntry {
    pub id: Uuid,
    pub label: Option<String>,
    pub scope: String,
    pub enabled: bool,
    pub model: Option<String>,
    pub provider_revision: i64,
    pub profile_revision: Option<i64>,
    pub profile: Option<Value>,
}

pub async fn byok_enabled(state: &AppState) -> Result<bool> {
    Ok(crate::config::runtime::get(&state.pg, "providers.user_byok_enabled").await?
        .map_or(state.boot.providers.user_byok_enabled, |entry| entry.value == "true"))
}

pub async fn catalog(state: &AppState, ctx: &AuthContext) -> Result<Vec<CatalogEntry>> {
    let user = if byok_enabled(state).await? { ctx.user_id } else { None };
    Ok(sqlx::query_as::<_, CatalogEntry>("SELECT p.id, p.label, p.scope, p.enabled, p.model, p.routing_revision AS provider_revision, m.revision AS profile_revision, m.profile FROM provider_configs p LEFT JOIN model_profiles m ON m.provider_id=p.id WHERE p.role='llm' AND (p.scope='deployment' OR (p.scope='user' AND p.scope_id=$1)) ORDER BY p.scope, p.label, p.id")
        .bind(user).fetch_all(&state.pg).await?)
}

pub fn current_profiles(catalog: Vec<CatalogEntry>) -> Vec<ModelProfile> {
    catalog.into_iter().filter_map(|entry| {
        if !entry.enabled { return None; }
        let profile: ModelProfile = serde_json::from_value(entry.profile?).ok()?;
        (profile.provider_id == entry.id && profile.provider_revision == entry.provider_revision
            && Some(profile.revision) == entry.profile_revision && Some(&profile.model) == entry.model.as_ref()).then_some(profile)
    }).collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveProfile {
    pub expected_revision: i64,
    pub profile: ModelProfile,
}

pub async fn save_profile(state: &AppState, ctx: &AuthContext, id: Uuid, mut body: SaveProfile) -> Result<ModelProfile> {
    if body.expected_revision < 0 || body.profile.provider_id != id { return Err(RoutingError::Invalid("profile identity or revision").into()); }
    let admin = state.rbac.has_permission(&state.pg, ctx, permissions::PROVIDERS_MANAGE).await?;
    let mut tx = state.pg.begin().await?;
    let row: Option<(String, Option<Uuid>, i64, Option<String>)> = sqlx::query_as("SELECT scope, scope_id, routing_revision, model FROM provider_configs WHERE id=$1 AND role='llm' FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?;
    let (scope, owner, revision, model) = row.ok_or(RoutingError::ProviderUnavailable)?;
    if scope == "deployment" {
        if !admin { return Err(RoutingError::Forbidden.into()); }
    } else if owner.is_none() || owner != ctx.user_id || !byok_enabled(state).await? {
        return Err(RoutingError::Forbidden.into());
    }
    if body.profile.locality_trusted && !admin { return Err(RoutingError::Forbidden.into()); }
    if body.profile.provider_revision != revision || model.as_ref() != Some(&body.profile.model) { return Err(RoutingError::NeedsReplan.into()); }
    let profile = &mut body.profile;
    if !profile.quality_evidence.is_empty() && !admin {return Err(RoutingError::Forbidden.into());}
    engine::validate_auto_effort(profile)?;
    super::learning::validate(profile)?;
    if profile.model.len() > 256 || profile.capabilities.efforts.len() > 16
        || profile.capabilities.efforts.iter().any(|s| !engine::valid_effort(s))
        || profile.evaluation_version.as_ref().is_some_and(|s| s.len() > 128)
        || profile.pricing.source.as_ref().is_some_and(|s| s.len() > 2048)
        || profile.pricing.checked_at.as_ref().is_some_and(|s| s.len() > 64) {
        return Err(RoutingError::Invalid("profile field limits").into());
    }
    if profile.quality_status == EvidenceStatus::Verified && (profile.evaluation_samples == 0 || profile.evaluation_version.is_none()) {
        return Err(RoutingError::Invalid("verified quality requires evaluation provenance").into());
    }
    profile.revision = sqlx::query_scalar("SELECT nextval('routing_revision_generation')").fetch_one(&mut *tx).await?;
    let json = serde_json::to_value(&*profile).map_err(|_| RoutingError::Invalid("profile"))?;
    let saved: Option<i64> = sqlx::query_scalar("INSERT INTO model_profiles (provider_id, provider_revision, revision, profile, updated_by) SELECT $1,$2,$6,$3,$4 WHERE $5=0 ON CONFLICT (provider_id) DO NOTHING RETURNING revision")
        .bind(id).bind(revision).bind(&json).bind(ctx.user_id).bind(body.expected_revision).bind(profile.revision).fetch_optional(&mut *tx).await?;
    if saved.is_none() {
        let updated = sqlx::query("UPDATE model_profiles SET provider_revision=$2, revision=$6, profile=$3, updated_by=$4, updated_at=now() WHERE provider_id=$1 AND revision=$5")
            .bind(id).bind(revision).bind(json).bind(ctx.user_id).bind(body.expected_revision).bind(profile.revision).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 { return Err(RoutingError::NeedsReplan.into()); }
    }
    tx.commit().await?;
    audit(state, ctx, "routing.profile_updated", id, profile.revision).await;
    Ok(body.profile)
}

#[derive(Serialize, sqlx::FromRow)]
pub struct PolicyEntry {
    pub id: Uuid,
    pub label: String,
    pub scope: String,
    pub revision: i64,
    pub is_default: bool,
    pub policy: Value,
}

pub async fn policies(state: &AppState, ctx: &AuthContext) -> Result<Vec<PolicyEntry>> {
    Ok(sqlx::query_as::<_, PolicyEntry>("SELECT id,label,scope,revision,is_default,policy FROM routing_policies WHERE scope='deployment' OR (scope='user' AND scope_id=$1) ORDER BY scope,label,id")
        .bind(ctx.user_id).fetch_all(&state.pg).await?)
}

pub async fn delete_profile(state: &AppState, ctx: &AuthContext, id: Uuid, expected_revision: i64) -> Result<()> {
    let mut tx = state.pg.begin().await?;
    let row: Option<(String, Option<Uuid>)> = sqlx::query_as("SELECT scope,scope_id FROM provider_configs WHERE id=$1 AND role='llm' FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?;
    let (scope, owner) = row.ok_or(RoutingError::ProviderUnavailable)?;
    if scope == "deployment" { state.rbac.require_permission(&state.pg, ctx, permissions::PROVIDERS_MANAGE).await?; }
    else if owner.is_none() || owner != ctx.user_id || !byok_enabled(state).await? { return Err(RoutingError::Forbidden.into()); }
    let deleted = sqlx::query("DELETE FROM model_profiles WHERE provider_id=$1 AND revision=$2")
        .bind(id).bind(expected_revision).execute(&mut *tx).await?;
    if deleted.rows_affected() != 1 { return Err(RoutingError::NeedsReplan.into()); }
    tx.commit().await?;
    audit(state, ctx, "routing.profile_deleted", id, expected_revision).await;
    Ok(())
}

pub async fn load_policy(state: &AppState, ctx: &AuthContext, id: Option<Uuid>) -> Result<RoutingPolicy> {
    let entries = policies(state, ctx).await?;
    let entry = if let Some(id) = id { entries.into_iter().find(|p| p.id == id) }
        else { entries.into_iter().filter(|p| p.is_default).max_by_key(|p| p.scope == "user") }
        .ok_or_else(|| AppError::NotFound("routing policy is unavailable".into()))?;
    let policy: RoutingPolicy = serde_json::from_value(entry.policy).map_err(|_| RoutingError::Invalid("stored policy"))?;
    if policy.revision != entry.revision || policy.id != entry.id { return Err(RoutingError::NeedsReplan.into()); }
    Ok(policy)
}

pub async fn deployment_constraints(state: &AppState) -> Result<RoutingConstraints> {
    let value: Option<Value> = sqlx::query_scalar("SELECT policy FROM routing_policies WHERE scope='deployment' AND is_default")
        .fetch_optional(&state.pg).await?;
    value.map(|v| serde_json::from_value::<RoutingPolicy>(v).map(|p| p.constraints)
        .map_err(|_| RoutingError::Invalid("deployment policy").into())).transpose().map(|v| v.unwrap_or_default())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavePolicy {
    pub label: String,
    pub scope: String,
    pub expected_revision: i64,
    pub is_default: bool,
    pub policy: RoutingPolicy,
}

pub async fn save_policy(state: &AppState, ctx: &AuthContext, id: Uuid, mut body: SavePolicy) -> Result<RoutingPolicy> {
    if body.policy.id != id || body.expected_revision < 0 || body.label.trim().is_empty() || body.label.chars().count() > 120 {
        return Err(RoutingError::Invalid("policy identity, label or revision").into());
    }
    let owner = match body.scope.as_str() {
        "deployment" => { state.rbac.require_permission(&state.pg, ctx, permissions::PROVIDERS_MANAGE).await?; None }
        "user" => Some(ctx.user_id.ok_or(RoutingError::Forbidden)?),
        _ => return Err(RoutingError::Invalid("policy scope").into()),
    };
    body.policy.revision = body.expected_revision.checked_add(1).ok_or(RoutingError::Invalid("revision overflow"))?;
    engine::validate_policy(&body.policy)?;
    let candidates = catalog(state, ctx).await?;
    let visible: std::collections::BTreeSet<_> = candidates.into_iter().filter(|p| p.enabled && (owner.is_some() || p.scope == "deployment")).map(|p| p.id).collect();
    let mut refs = Vec::new();
    refs.extend(body.policy.default_provider_id);
    refs.extend(body.policy.classifier_provider_id);
    refs.extend(body.policy.utility_assignments.values().copied());
    refs.extend(body.policy.rules.iter().filter_map(|r| if let RuleAction::Select { provider_id, .. } = r.action { Some(provider_id) } else { None }));
    for rule in &body.policy.rules {
        if let RuleAction::RestrictPool{allowed_providers}=&rule.action {refs.extend(allowed_providers.iter().copied());}
    }
    if refs.iter().any(|id| !visible.contains(id)) { return Err(RoutingError::Forbidden.into()); }
    if let Some(ids)=&body.policy.constraints.allowed_providers {
        let mut permitted=visible;
        permitted.extend(fixed::catalog(state,ctx).await?.into_iter()
            .filter(|p|p.enabled && (owner.is_some() || p.scope=="deployment")).map(|p|p.id));
        if ids.iter().any(|id|!permitted.contains(id)) { return Err(RoutingError::Forbidden.into()); }
    }
    let mut tx = state.pg.begin().await?;
    let existing: Option<(String, Option<Uuid>, i64)> = sqlx::query_as("SELECT scope,scope_id,revision FROM routing_policies WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?;
    if let Some((scope, scope_id, revision)) = &existing {
        if scope != &body.scope || *scope_id != owner { return Err(RoutingError::Forbidden.into()); }
        if *revision != body.expected_revision { return Err(RoutingError::NeedsReplan.into()); }
    } else if body.expected_revision != 0 { return Err(RoutingError::NeedsReplan.into()); }
    body.policy.revision = sqlx::query_scalar("SELECT nextval('routing_revision_generation')").fetch_one(&mut *tx).await?;
    let value = serde_json::to_value(&body.policy).map_err(|_| RoutingError::Invalid("policy"))?;
    let result = if existing.is_some() {
        sqlx::query("UPDATE routing_policies SET label=$2,revision=$3,is_default=$4,policy=$5,updated_by=$6,updated_at=now() WHERE id=$1")
            .bind(id).bind(body.label.trim()).bind(body.policy.revision).bind(body.is_default).bind(value).bind(ctx.user_id).execute(&mut *tx).await
    } else {
        sqlx::query("INSERT INTO routing_policies (id,label,scope,scope_id,revision,is_default,policy,updated_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)")
            .bind(id).bind(body.label.trim()).bind(body.scope).bind(owner).bind(body.policy.revision).bind(body.is_default).bind(value).bind(ctx.user_id).execute(&mut *tx).await
    };
    match result {
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => return Err(AppError::Conflict("a default policy or policy identity already exists".into())),
        other => { other?; }
    }
    tx.commit().await?;
    audit(state, ctx, "routing.policy_updated", id, body.policy.revision).await;
    Ok(body.policy)
}

pub async fn delete_policy(state: &AppState, ctx: &AuthContext, id: Uuid, expected_revision: i64) -> Result<()> {
    let mut tx = state.pg.begin().await?;
    let row: Option<(String, Option<Uuid>, i64)> = sqlx::query_as("SELECT scope,scope_id,revision FROM routing_policies WHERE id=$1 FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?;
    let (scope, owner, revision) = row.ok_or_else(|| AppError::NotFound("policy unavailable".into()))?;
    if scope == "deployment" { state.rbac.require_permission(&state.pg, ctx, permissions::PROVIDERS_MANAGE).await?; }
    else if owner.is_none() || owner != ctx.user_id { return Err(RoutingError::Forbidden.into()); }
    if revision != expected_revision { return Err(RoutingError::NeedsReplan.into()); }
    sqlx::query("DELETE FROM routing_policies WHERE id=$1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    audit(state, ctx, "routing.policy_deleted", id, revision).await;
    Ok(())
}

pub(super) async fn audit(state: &AppState, ctx: &AuthContext, action: &str, id: Uuid, revision: i64) {
    let mut event = crate::audit::AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.payload = Some(serde_json::json!({"id":id,"revision":revision}));
    if let Err(error) = crate::audit::append(&state.pg, &event).await { tracing::error!(%error, "routing configuration audit failed"); }
}
