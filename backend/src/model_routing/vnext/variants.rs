// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::super::RoutingError;
use super::{catalogue::*, connections::*, deployments};
use crate::{
    auth::{permissions, AuthContext},
    AppState, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct StoredVariant {
    id: Uuid,
    provider_id: Uuid,
    scope: String,
    scope_id: Option<Uuid>,
    revision: i64,
    model_config_revision: i64,
    behaviour_sha256: String,
    variant: Value,
}
impl StoredVariant {
    fn value(self) -> Result<ModelVariant> {
        let value: ModelVariant = serde_json::from_value(self.variant)
            .map_err(|_| RoutingError::Invalid("stored model variant"))?;
        if value.id != self.id
            || value.provider_id != self.provider_id
            || value.scope != scope(&self.scope, self.scope_id)?
            || value.revision != self.revision
            || value.model_config_revision != self.model_config_revision
            || value.behaviour_sha256 != self.behaviour_sha256
            || value.behaviour.fingerprint()? != self.behaviour_sha256
        {
            return Err(RoutingError::NeedsReplan.into());
        }
        Ok(value)
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct VariantEntry {
    pub variant: ModelVariant,
    pub configuration_current: bool,
    pub binding_current: bool,
    pub calibration_state: &'static str,
    pub behaviour_state: String,
}
#[derive(Serialize)]
pub struct Catalogue {
    pub schema_version: u32,
    pub catalogue_revision: i64,
    pub deployments: Vec<deployments::DeploymentEntry>,
    pub variants: Vec<VariantEntry>,
    pub max_active_variants: usize,
    pub selection_versions: Vec<u32>,
    pub profile_versions: Vec<u32>,
}

pub async fn catalogue(state: &AppState, actor: &AuthContext) -> Result<Catalogue> {
    check_actor(state, actor).await?;
    let user = if super::super::store::byok_enabled(state).await? {
        actor.user_id
    } else {
        None
    };
    let mut tx = state.pg.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *tx)
        .await?;
    let deployments = deployments::rows(&mut tx, user).await?;
    let by_id: BTreeMap<_, _> = deployments.iter().map(|r| (r.id, r)).collect();
    let records:Vec<StoredVariant>=sqlx::query_as("SELECT v.id,v.provider_id,v.scope,v.scope_id,v.revision,v.model_config_revision,v.behaviour_sha256,v.variant FROM model_variants v JOIN provider_configs p ON p.id=v.provider_id WHERE (v.scope='deployment' OR (v.scope='user' AND v.scope_id=$1)) AND (p.scope='deployment' OR (p.scope='user' AND p.scope_id=$2)) ORDER BY v.scope,v.id")
        .bind(actor.user_id).bind(user).fetch_all(&mut *tx).await?;
    let mut variants = Vec::new();
    for record in records {
        let variant = record.value()?;
        let Some(deployment) = by_id.get(&variant.provider_id) else {
            continue;
        };
        let configuration_current = deployment
            .basis()
            .is_ok_and(|d| variant.validate(&d).is_ok());
        let binding_current = deployment.enabled
            && deployment
                .profile()
                .is_ok_and(|p| p.provider_revision == deployment.provider_revision);
        variants.push(VariantEntry {
            behaviour_state: if configuration_current && binding_current {"configured"} else {"stale"}.into(),
            variant,
            configuration_current,
            binding_current,
            calibration_state: "configured",
        });
    }
    let catalogue_revision:i64=sqlx::query_scalar("SELECT COALESCE(max(revision),1) FROM routing_catalogue_generations WHERE scope_key='00000000-0000-0000-0000-000000000000'::uuid OR scope_key=$1")
        .bind(actor.user_id).fetch_one(&mut *tx).await?;
    let deployments = deployments
        .into_iter()
        .map(deployments::public)
        .collect::<Result<Vec<_>>>()?;
    tx.commit().await?;
    Ok(Catalogue {
        schema_version: 2,
        catalogue_revision,
        deployments,
        variants,
        max_active_variants: super::MAX_ACTIVE_VARIANTS,
        selection_versions: vec![1,2],
        profile_versions: vec![1,2],
    })
}

pub(crate) fn validate_adapter_generation(b:&BehaviourDescriptor)->Result<()> {
    let g=&b.generation;
    let model=b.model_id.strip_prefix("models/").unwrap_or(&b.model_id);
    let gemma_levels=matches!(model,"gemma-4-26b-a4b-it"|"gemma-4-31b-it");
    let gemini_levels=model.strip_prefix("gemini-").and_then(|s|s.split(['.','-']).next())
        .and_then(|s|s.parse::<u32>().ok()).is_some_and(|n|n>=3);
    if matches!(b.adapter,super::super::AdapterKind::Gemini) && gemma_levels
        && b.effort.value().is_some_and(|v|!matches!(v,"minimal"|"high")) {
        return Err(RoutingError::Incompatible("hosted Gemma supports only minimal/high effort or provider default").into());
    }
    if g.seed.is_some() && !matches!(b.adapter,super::super::AdapterKind::OpenaiChat|super::super::AdapterKind::Gemini) {
        return Err(RoutingError::Incompatible("adapter has no seed mapping").into());
    }
    if let Some(n)=g.max_reasoning_tokens {
        match b.adapter {
            super::super::AdapterKind::Anthropic if b.effort.value().is_none() && n>=1024 && g.max_output_tokens.is_some_and(|o|o>n)=>{},
            super::super::AdapterKind::Gemini if b.effort.value().is_none() && !gemini_levels && !gemma_levels=>{},
            _=>return Err(RoutingError::Incompatible("reasoning budget conflicts with adapter or effort").into()),
        }
    }
    Ok(())
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveVariant {
    pub schema_version: u32,
    pub expected_revision: i64,
    pub expected_model_config_revision: i64,
    pub scope: String,
    pub provider_id: Uuid,
    pub label: String,
    pub effort: EffortSetting,
    pub generation: GenerationConfig,
}
async fn variant_owner(state: &AppState, actor: &AuthContext, kind: &str) -> Result<Option<Uuid>> {
    match kind {
        "user" => Ok(Some(actor.user_id.ok_or(RoutingError::Forbidden)?)),
        "deployment" => {
            state
                .rbac
                .require_permission(&state.pg, actor, permissions::PROVIDERS_MANAGE)
                .await?;
            Ok(None)
        }
        _ => Err(RoutingError::Invalid("variant scope").into()),
    }
}
pub(crate) fn check_generation(
    metadata: &deployments::ModelMetadata,
    generation: &GenerationConfig,
) -> Result<()> {
    for (field, configured) in [
        (
            "max_reasoning_tokens",
            generation.max_reasoning_tokens.is_some(),
        ),
        ("temperature_milli", generation.temperature_milli.is_some()),
        ("top_p_milli", generation.top_p_milli.is_some()),
        ("seed", generation.seed.is_some()),
    ] {
        if configured && !metadata.generation_fields.contains(field) {
            return Err(
                RoutingError::Incompatible("generation setting is not declared supported").into(),
            );
        }
    }
    if generation
        .max_reasoning_tokens
        .is_some_and(|n| metadata.max_reasoning_tokens.is_none_or(|max| n > max))
    {
        return Err(
            RoutingError::Incompatible("reasoning token limit exceeds model capabilities").into(),
        );
    }
    if generation
        .temperature_milli
        .is_some_and(|v| metadata.max_temperature_milli.is_none_or(|max| v > max))
        || generation
            .top_p_milli
            .is_some_and(|v| metadata.max_top_p_milli.is_none_or(|max| v > max))
    {
        return Err(
            RoutingError::Incompatible("sampling setting exceeds declared model limits").into(),
        );
    }
    Ok(())
}
pub async fn save(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    body: SaveVariant,
) -> Result<ModelVariant> {
    check_actor(state, actor).await?;
    version(body.schema_version, body.expected_revision)?;
    let owner = variant_owner(state, actor, &body.scope).await?;
    let label = label(&body.label)?.to_owned();
    if id.is_nil() || body.provider_id.is_nil() || body.expected_model_config_revision < 1 {
        return Err(RoutingError::Invalid("variant identity").into());
    }
    let user = if super::super::store::byok_enabled(state).await? {
        actor.user_id
    } else {
        None
    };
    let mut tx = state.pg.begin().await?;
    mutation_lock(&mut tx, owner).await?;
    let visible = deployments::rows(&mut tx, user)
        .await?
        .into_iter()
        .find(|r| r.id == body.provider_id)
        .ok_or(RoutingError::Forbidden)?;
    // Keep the source stable while validating and saving; dispatch still has
    // its independent current binding/revision checks.
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT model_config_revision FROM provider_configs WHERE id=$1 AND role='llm' FOR SHARE",
    )
    .bind(body.provider_id)
    .fetch_optional(&mut *tx)
    .await?;
    if current != Some(body.expected_model_config_revision)
        || visible.model_config_revision != body.expected_model_config_revision
    {
        return Err(RoutingError::NeedsReplan.into());
    }
    let deployment = deployments::rows(&mut tx, user)
        .await?
        .into_iter()
        .find(|r| r.id == body.provider_id)
        .ok_or(RoutingError::Forbidden)?;
    let basis = deployment.basis()?;
    check_generation(&deployment.metadata()?, &body.generation)?;
    let mut behaviour = basis.behaviour.clone();
    behaviour.effort = body.effort;
    behaviour.generation = body.generation;
    let revision: i64 = sqlx::query_scalar("SELECT nextval('routing_revision_generation')")
        .fetch_one(&mut *tx)
        .await?;
    let variant = ModelVariant {
        id,
        revision,
        scope: scope(&body.scope, owner)?,
        provider_id: body.provider_id,
        model_config_revision: body.expected_model_config_revision,
        label,
        behaviour_sha256: behaviour.fingerprint()?,
        behaviour,
    };
    variant.validate(&basis)?;
    validate_adapter_generation(&variant.behaviour)?;
    let old: Option<(String, Option<Uuid>, Uuid, i64)> = sqlx::query_as(
        "SELECT scope,scope_id,provider_id,revision FROM model_variants WHERE id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((kind, previous_owner, provider, previous_revision)) = old {
        if kind != body.scope || previous_owner != owner || provider != body.provider_id {
            return Err(RoutingError::Forbidden.into());
        }
        if previous_revision != body.expected_revision {
            return Err(RoutingError::NeedsReplan.into());
        }
        sqlx::query("UPDATE model_variants SET revision=$2,model_config_revision=$3,behaviour_sha256=$4,variant=$5,updated_by=$6,updated_at=now() WHERE id=$1")
            .bind(id).bind(revision).bind(variant.model_config_revision).bind(&variant.behaviour_sha256).bind(serde_json::to_value(&variant).unwrap()).bind(actor.user_id)
            .execute(&mut *tx).await.map_err(conflict)?;
    } else {
        if body.expected_revision != 0 {
            return Err(RoutingError::NeedsReplan.into());
        }
        sqlx::query("INSERT INTO model_variants(id,provider_id,scope,scope_id,revision,model_config_revision,behaviour_sha256,variant,updated_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(id).bind(body.provider_id).bind(&body.scope).bind(owner).bind(revision).bind(variant.model_config_revision).bind(&variant.behaviour_sha256)
            .bind(serde_json::to_value(&variant).unwrap()).bind(actor.user_id).execute(&mut *tx).await.map_err(conflict)?;
    }
    audit(&mut tx, actor, "routing.variant_saved", id, revision).await?;
    tx.commit().await?;
    Ok(variant)
}
pub async fn delete(state: &AppState, actor: &AuthContext, id: Uuid, expected: i64) -> Result<()> {
    check_actor(state, actor).await?;
    version(2, expected)?;
    let known = catalogue(state, actor)
        .await?
        .variants
        .into_iter()
        .find(|v| v.variant.id == id)
        .ok_or(RoutingError::Forbidden)?
        .variant;
    let (kind, owner) = match known.scope {
        CatalogueScope::Deployment => ("deployment", None),
        CatalogueScope::User { owner_id } => ("user", Some(owner_id)),
    };
    if variant_owner(state, actor, kind).await? != owner {
        return Err(RoutingError::Forbidden.into());
    }
    let mut tx = state.pg.begin().await?;
    mutation_lock(&mut tx, owner).await?;
    let result = sqlx::query("DELETE FROM model_variants WHERE id=$1 AND revision=$2")
        .bind(id)
        .bind(expected)
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() != 1 {
        return Err(RoutingError::NeedsReplan.into());
    }
    audit(&mut tx, actor, "routing.variant_deleted", id, expected).await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantExport {
    pub schema_version: u32,
    pub label: String,
    pub model_hint: String,
    pub adapter_hint: super::super::AdapterKind,
    pub prior_behaviour_sha256: String,
    pub effort: EffortSetting,
    pub generation: GenerationConfig,
}
pub async fn export(state: &AppState, actor: &AuthContext, id: Uuid) -> Result<VariantExport> {
    let variant = catalogue(state, actor)
        .await?
        .variants
        .into_iter()
        .find(|v| v.variant.id == id)
        .ok_or(RoutingError::Forbidden)?
        .variant;
    Ok(VariantExport {
        schema_version: 2,
        label: variant.label,
        model_hint: variant.behaviour.model_id,
        adapter_hint: variant.behaviour.adapter,
        prior_behaviour_sha256: variant.behaviour_sha256,
        effort: variant.behaviour.effort,
        generation: variant.behaviour.generation,
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportVariant {
    pub schema_version: u32,
    pub scope: String,
    pub provider_id: Uuid,
    pub expected_model_config_revision: i64,
    pub package: VariantExport,
}
pub async fn import(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    body: ImportVariant,
) -> Result<ModelVariant> {
    check_actor(state, actor).await?;
    version(body.schema_version, 0)?;
    version(body.package.schema_version, 0)?;
    if !super::sha256(&body.package.prior_behaviour_sha256) {
        return Err(RoutingError::Invalid("variant import provenance").into());
    }
    let user = if super::super::store::byok_enabled(state).await? {
        actor.user_id
    } else {
        None
    };
    let mut conn = state.pg.acquire().await?;
    let deployment = deployments::rows(&mut conn, user)
        .await?
        .into_iter()
        .find(|r| r.id == body.provider_id)
        .ok_or(RoutingError::Forbidden)?;
    let basis = deployment.basis()?;
    if basis.behaviour.adapter != body.package.adapter_hint
        || basis.behaviour.model_id != body.package.model_hint
    {
        return Err(
            RoutingError::Incompatible("map the import to a compatible configured model").into(),
        );
    }
    drop(conn);
    // This is a fresh server-derived configuration, never a transplanted prior,
    // review, calibration binding or provider UUID.
    save(
        state,
        actor,
        id,
        SaveVariant {
            schema_version: 2,
            expected_revision: 0,
            expected_model_config_revision: body.expected_model_config_revision,
            scope: body.scope,
            provider_id: body.provider_id,
            label: body.package.label,
            effort: body.package.effort,
            generation: body.package.generation,
        },
    )
    .await
}
