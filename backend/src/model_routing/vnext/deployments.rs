// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::super::{ModelCapabilities, ModelPricing, ModelProfile, RoutingError};
use super::{catalogue::*, connections::*};
use crate::{auth::AuthContext, AppState, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelMetadata {
    pub model_version: Option<String>,
    pub local_digest: Option<String>,
    pub quantisation: Option<String>,
    pub template_sha256: Option<String>,
    pub runtime_config_sha256: Option<String>,
    pub generation_fields: BTreeSet<String>,
    pub max_reasoning_tokens: Option<u32>,
    pub max_temperature_milli: Option<u16>,
    pub max_top_p_milli: Option<u16>,
}
impl ModelMetadata {
    pub fn validate(&self) -> Result<()> {
        if self
            .model_version
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > 256)
            || self
                .quantisation
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 64)
            || [
                &self.local_digest,
                &self.template_sha256,
                &self.runtime_config_sha256,
            ]
            .into_iter()
            .any(|s| s.as_ref().is_some_and(|v| !super::sha256(v)))
            || self.generation_fields.iter().any(|v| {
                !matches!(
                    v.as_str(),
                    "max_reasoning_tokens" | "temperature_milli" | "top_p_milli" | "seed"
                )
            })
            || self.max_reasoning_tokens == Some(0)
            || self.max_temperature_milli.is_some_and(|v| v > 2000)
            || self.max_top_p_milli.is_some_and(|v| v == 0 || v > 1000)
        {
            return Err(RoutingError::Invalid("model metadata limits or generation fields").into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelTariffs {
    pub pricing: ModelPricing,
    pub cached_input_microusd_per_million: Option<u64>,
    pub reasoning_output_microusd_per_million: Option<u64>,
}
impl ModelTariffs {
    fn validate(&self) -> Result<()> {
        if self.pricing.source.as_ref().is_some_and(|v| v.len() > 2048)
            || self
                .pricing
                .checked_at
                .as_ref()
                .is_some_and(|v| v.len() > 64)
        {
            return Err(RoutingError::Invalid("pricing provenance limits").into());
        }
        Ok(())
    }
}
#[derive(sqlx::FromRow)]
pub(crate) struct DeploymentRow {
    pub id: Uuid,
    pub label: Option<String>,
    pub scope: String,
    pub scope_id: Option<Uuid>,
    pub revision: i64,
    pub provider_revision: i64,
    pub model_config_revision: i64,
    pub enabled: bool,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub reasoning_mode: Option<String>,
    pub is_default: bool,
    pub metadata: Value,
    pub tariffs: Value,
    pub connection_id: Option<Uuid>,
    pub auth_revision: Option<i64>,
    pub quota_group_id: Option<Uuid>,
    pub adapter: Option<String>,
    pub profile_revision: Option<i64>,
    pub profile: Option<Value>,
}
pub(crate) async fn rows(
    conn: &mut sqlx::PgConnection,
    user: Option<Uuid>,
) -> Result<Vec<DeploymentRow>> {
    Ok(sqlx::query_as("SELECT p.id,p.label,p.scope,p.scope_id,p.catalogue_entry_revision AS revision,p.routing_revision AS provider_revision,p.model_config_revision,p.enabled AND CASE WHEN p.connection_id IS NULL THEN true ELSE COALESCE(c.enabled,false) END AS enabled,p.model,p.base_url,p.reasoning_mode,p.is_default,p.model_metadata AS metadata,p.model_pricing AS tariffs,p.connection_id,c.auth_revision,c.quota_group_id,c.connection_adapter AS adapter,m.revision AS profile_revision,m.profile FROM provider_configs p LEFT JOIN provider_configs c ON c.id=p.connection_id AND c.role='connection' LEFT JOIN model_profiles m ON m.provider_id=p.id WHERE p.role='llm' AND (p.scope='deployment' OR (p.scope='user' AND p.scope_id=$1)) ORDER BY p.scope,p.label,p.id")
        .bind(user).fetch_all(conn).await?)
}
impl DeploymentRow {
    pub(crate) fn profile(&self) -> Result<ModelProfile> {
        let profile: ModelProfile = serde_json::from_value(self.profile.clone().ok_or(
            RoutingError::Incompatible("model capabilities need configuration"),
        )?)
        .map_err(|_| RoutingError::Invalid("stored model capabilities"))?;
        if profile.provider_id != self.id
            || self.profile_revision != Some(profile.revision)
            || self.model.as_ref() != Some(&profile.model)
        {
            return Err(RoutingError::NeedsReplan.into());
        }
        if self
            .adapter
            .as_deref()
            .is_some_and(|a| serde_json::to_value(profile.adapter).unwrap().as_str() != Some(a))
        {
            return Err(
                RoutingError::Incompatible("profile adapter differs from connection").into(),
            );
        }
        Ok(profile)
    }
    pub(crate) fn metadata(&self) -> Result<ModelMetadata> {
        let metadata: ModelMetadata = serde_json::from_value(self.metadata.clone())
            .map_err(|_| RoutingError::Invalid("stored model metadata"))?;
        metadata.validate()?;
        Ok(metadata)
    }
    pub(crate) fn basis(&self) -> Result<ModelDeployment> {
        let profile = self.profile()?;
        let metadata = self.metadata()?;
        let url = endpoint(
            self.base_url
                .as_deref()
                .ok_or(RoutingError::Invalid("model endpoint missing"))?,
        )?;
        let template = metadata.template_sha256.clone().unwrap_or(super::digest(
            "fosnie.default.request.template.v2",
            &(profile.adapter, "explicit-inference-envelope-v1"),
        )?);
        let runtime = super::digest(
            "fosnie.deployment.runtime.v2",
            &(
                profile.adapter,
                url,
                &self.reasoning_mode,
                &metadata.runtime_config_sha256,
            ),
        )?;
        let connection = match (self.connection_id, self.auth_revision) {
            (Some(id), Some(auth_revision)) => Some(ConnectionRef {
                id,
                auth_revision,
                quota_group_id: self.quota_group_id,
                scope: scope(&self.scope, self.scope_id)?,
            }),
            (None, None) => None,
            _ => return Err(RoutingError::NeedsReplan.into()),
        };
        let basis = ModelDeployment {
            provider_id: self.id,
            scope: scope(&self.scope, self.scope_id)?,
            provider_revision: self.provider_revision,
            model_config_revision: self.model_config_revision,
            connection,
            behaviour: BehaviourDescriptor {
                adapter: profile.adapter,
                model_id: profile.model,
                model_version: metadata.model_version,
                local_digest: metadata.local_digest,
                quantisation: metadata.quantisation,
                template_sha256: template,
                runtime_config_sha256: runtime,
                effort: EffortSetting::ProviderDefault,
                generation: GenerationConfig::default(),
            },
            capabilities: profile.capabilities,
            pricing: profile.pricing,
        };
        basis.validate()?;
        Ok(basis)
    }
    pub(crate) fn tariffs(&self) -> Result<ModelTariffs> {
        if self.tariffs == serde_json::json!({}) {
            return Ok(ModelTariffs {
                pricing: self.profile().map(|p| p.pricing).unwrap_or_default(),
                ..Default::default()
            });
        }
        let tariffs: ModelTariffs = serde_json::from_value(self.tariffs.clone())
            .map_err(|_| RoutingError::Invalid("stored model pricing"))?;
        tariffs.validate()?;
        Ok(tariffs)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeploymentEntry {
    pub id: Uuid,
    pub label: Option<String>,
    pub scope: CatalogueScope,
    pub revision: i64,
    pub provider_revision: i64,
    pub model_config_revision: i64,
    pub enabled: bool,
    pub model: Option<String>,
    pub reasoning_mode: Option<String>,
    pub is_default: bool,
    pub adapter: Option<String>,
    pub connection: Option<ConnectionRef>,
    pub profile_revision: Option<i64>,
    pub capabilities: Option<ModelCapabilities>,
    pub binding_current: bool,
    pub metadata: ModelMetadata,
    pub tariffs: ModelTariffs,
}
pub(crate) fn public(row: DeploymentRow) -> Result<DeploymentEntry> {
    let profile = row.profile().ok();
    let connection = match (row.connection_id, row.auth_revision) {
        (Some(id), Some(auth_revision)) => Some(ConnectionRef {
            id,
            auth_revision,
            quota_group_id: row.quota_group_id,
            scope: scope(&row.scope, row.scope_id)?,
        }),
        _ => None,
    };
    let metadata = row.metadata()?;
    let tariffs = row.tariffs()?;
    Ok(DeploymentEntry {
        id: row.id,
        label: row.label.clone(),
        scope: scope(&row.scope, row.scope_id)?,
        revision: row.revision,
        provider_revision: row.provider_revision,
        model_config_revision: row.model_config_revision,
        enabled: row.enabled,
        model: row.model.clone(),
        reasoning_mode: row.reasoning_mode.clone(),
        is_default: row.is_default,
        adapter: row.adapter.clone().or_else(|| {
            profile.as_ref().map(|p| {
                serde_json::to_value(p.adapter)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
        }),
        connection,
        profile_revision: row.profile_revision,
        binding_current: profile
            .as_ref()
            .is_some_and(|p| p.provider_revision == row.provider_revision),
        capabilities: profile.map(|p| p.capabilities),
        metadata,
        tariffs,
    })
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningMode {
    Auto,
    None,
    Toggle,
    Levels,
    Budget,
    AlwaysOn,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveDeployment {
    pub schema_version: u32,
    pub expected_revision: i64,
    pub scope: String,
    pub label: String,
    pub model: String,
    pub connection_id: Uuid,
    pub expected_connection_revision: i64,
    pub enabled: bool,
    pub reasoning_mode: Option<ReasoningMode>,
}
pub async fn save(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    body: SaveDeployment,
) -> Result<DeploymentEntry> {
    check_actor(state, actor).await?;
    version(body.schema_version, body.expected_revision)?;
    if id.is_nil()
        || body.connection_id.is_nil()
        || body.model.trim().is_empty()
        || body.model.len() > 256
    {
        return Err(RoutingError::Invalid("model deployment identity").into());
    }
    let owner = owner_for(state, actor, &body.scope).await?;
    let label = label(&body.label)?;
    let mode = match body.reasoning_mode {
        None | Some(ReasoningMode::Auto) => None,
        Some(v) => Some(
            serde_json::to_value(v)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned(),
        ),
    };
    let mut tx = state.pg.begin().await?;
    mutation_lock(&mut tx, owner).await?;
    let connection = row(&mut tx, body.connection_id, true)
        .await?
        .ok_or(RoutingError::ProviderUnavailable)?;
    if connection.scope != body.scope || connection.scope_id != owner {
        return Err(RoutingError::Forbidden.into());
    }
    if connection.revision != body.expected_connection_revision {
        return Err(RoutingError::NeedsReplan.into());
    }
    let old:Option<(String,Option<Uuid>,i64)>=sqlx::query_as("SELECT scope,scope_id,catalogue_entry_revision FROM provider_configs WHERE id=$1 AND role='llm' FOR UPDATE")
        .bind(id).fetch_optional(&mut *tx).await?;
    if let Some((kind, previous_owner, revision)) = old {
        if kind != body.scope || previous_owner != owner {
            return Err(RoutingError::Forbidden.into());
        }
        if revision != body.expected_revision {
            return Err(RoutingError::NeedsReplan.into());
        }
        sqlx::query("UPDATE provider_configs SET label=$2,model=$3,connection_id=$4,base_url=$5,enabled=$6,reasoning_mode=$7,updated_by=$8,updated_at=now() WHERE id=$1 AND role='llm'")
            .bind(id).bind(label).bind(body.model.trim()).bind(body.connection_id).bind(&connection.base_url).bind(body.enabled).bind(mode).bind(actor.user_id)
            .execute(&mut *tx).await.map_err(conflict)?;
    } else {
        if body.expected_revision != 0 {
            return Err(RoutingError::NeedsReplan.into());
        }
        let has_default:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM provider_configs WHERE role='llm' AND scope=$1 AND scope_id IS NOT DISTINCT FROM $2 AND is_default)")
            .bind(&body.scope).bind(owner).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO provider_configs(id,role,scope,scope_id,label,model,connection_id,base_url,enabled,reasoning_mode,is_default,updated_by) VALUES($1,'llm',$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(id).bind(&body.scope).bind(owner).bind(label).bind(body.model.trim()).bind(body.connection_id).bind(&connection.base_url).bind(body.enabled).bind(mode).bind(!has_default).bind(actor.user_id)
            .execute(&mut *tx).await.map_err(conflict)?;
    }
    // Shared/explicit connections no longer belong to one legacy editor.
    sqlx::query("UPDATE provider_configs SET legacy_owner_provider_id=NULL WHERE id=$1 AND legacy_owner_provider_id IS NOT NULL")
        .bind(body.connection_id).execute(&mut *tx).await?;
    let saved = rows(&mut tx, owner)
        .await?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or(RoutingError::ProviderUnavailable)?;
    audit(
        &mut tx,
        actor,
        "routing.deployment_saved",
        id,
        saved.revision,
    )
    .await?;
    let saved = public(saved)?;
    tx.commit().await?;
    Ok(saved)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveMetadata {
    pub schema_version: u32,
    pub expected_revision: i64,
    pub metadata: ModelMetadata,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveTariffs {
    pub schema_version: u32,
    pub expected_revision: i64,
    pub tariffs: ModelTariffs,
}
async fn edit_owner(state: &AppState, actor: &AuthContext, id: Uuid) -> Result<Option<Uuid>> {
    let known: Option<(String, Option<Uuid>)> =
        sqlx::query_as("SELECT scope,scope_id FROM provider_configs WHERE id=$1 AND role='llm'")
            .bind(id)
            .fetch_optional(&state.pg)
            .await?;
    let (kind, existing) = known.ok_or(RoutingError::ProviderUnavailable)?;
    let owner = owner_for(state, actor, &kind).await?;
    if existing != owner {
        return Err(RoutingError::Forbidden.into());
    }
    Ok(owner)
}
async fn edit(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    expected: i64,
    metadata: Option<ModelMetadata>,
    tariffs: Option<ModelTariffs>,
) -> Result<DeploymentEntry> {
    check_actor(state, actor).await?;
    version(2, expected)?;
    let owner = edit_owner(state, actor, id).await?;
    let mut tx = state.pg.begin().await?;
    mutation_lock(&mut tx, owner).await?;
    let changed=sqlx::query("UPDATE provider_configs SET model_metadata=COALESCE($3,model_metadata),model_pricing=COALESCE($4,model_pricing),updated_by=$5,updated_at=now() WHERE id=$1 AND role='llm' AND catalogue_entry_revision=$2")
        .bind(id).bind(expected).bind(metadata.map(|m|serde_json::to_value(m).unwrap())).bind(tariffs.map(|p|serde_json::to_value(p).unwrap())).bind(actor.user_id)
        .execute(&mut *tx).await.map_err(conflict)?;
    if changed.rows_affected() != 1 {
        return Err(RoutingError::NeedsReplan.into());
    }
    let saved = rows(&mut tx, owner)
        .await?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or(RoutingError::ProviderUnavailable)?;
    audit(
        &mut tx,
        actor,
        "routing.deployment_metadata_saved",
        id,
        saved.revision,
    )
    .await?;
    let saved = public(saved)?;
    tx.commit().await?;
    Ok(saved)
}
pub async fn save_metadata(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    body: SaveMetadata,
) -> Result<DeploymentEntry> {
    version(body.schema_version, body.expected_revision)?;
    body.metadata.validate()?;
    edit(
        state,
        actor,
        id,
        body.expected_revision,
        Some(body.metadata),
        None,
    )
    .await
}
pub async fn save_tariffs(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    body: SaveTariffs,
) -> Result<DeploymentEntry> {
    version(body.schema_version, body.expected_revision)?;
    body.tariffs.validate()?;
    edit(
        state,
        actor,
        id,
        body.expected_revision,
        None,
        Some(body.tariffs),
    )
    .await
}

pub async fn delete(state: &AppState, actor: &AuthContext, id: Uuid, expected: i64) -> Result<()> {
    check_actor(state, actor).await?;
    version(2, expected)?;
    let owner = edit_owner(state, actor, id).await?;
    let mut tx = state.pg.begin().await?;
    mutation_lock(&mut tx, owner).await?;
    let removed:Option<(bool,String)>=sqlx::query_as("DELETE FROM provider_configs WHERE id=$1 AND role='llm' AND catalogue_entry_revision=$2 RETURNING is_default,scope")
        .bind(id).bind(expected).fetch_optional(&mut *tx).await.map_err(conflict)?;
    let (default, kind) = removed.ok_or(RoutingError::NeedsReplan)?;
    if default {
        sqlx::query("UPDATE provider_configs SET is_default=true WHERE id=(SELECT id FROM provider_configs WHERE role='llm' AND scope=$1 AND scope_id IS NOT DISTINCT FROM $2 ORDER BY id LIMIT 1)")
            .bind(kind).bind(owner).execute(&mut *tx).await?;
    }
    audit(&mut tx, actor, "routing.deployment_deleted", id, expected).await?;
    tx.commit().await?;
    Ok(())
}
