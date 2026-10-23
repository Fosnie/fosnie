// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::super::{AdapterKind, ModelCapabilities, ModelPricing, RoutingError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CatalogueScope {
    Deployment,
    User { owner_id: Uuid },
}
impl CatalogueScope {
    fn valid(&self) -> bool {
        !matches!(self, Self::User { owner_id } if owner_id.is_nil())
    }
}

/// Metadata only. The existing provider credential subsystem resolves secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionRef {
    pub id: Uuid,
    pub auth_revision: i64,
    pub quota_group_id: Option<Uuid>,
    pub scope: CatalogueScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDeployment {
    pub provider_id: Uuid,
    pub scope: CatalogueScope,
    /// Dispatch revision includes auth/binding changes; behaviour has its own identity.
    pub provider_revision: i64,
    pub model_config_revision: i64,
    pub connection: Option<ConnectionRef>,
    /// Current server-resolved behaviour base. Variant effort/generation may
    /// differ; model/runtime identity must match it exactly.
    pub behaviour: BehaviourDescriptor,
    pub capabilities: ModelCapabilities,
    pub pricing: ModelPricing,
}
impl ModelDeployment {
    pub fn validate(&self) -> Result<(), RoutingError> {
        if self.provider_id.is_nil()
            || self.provider_revision < 1
            || self.model_config_revision < 1
            || !self.scope.valid()
            || self.connection.as_ref().is_some_and(|c| {
                c.id.is_nil()
                    || c.auth_revision < 1
                    || c.scope != self.scope
                    || c.quota_group_id.is_some_and(|id| id.is_nil())
            })
        {
            return Err(RoutingError::Invalid(
                "deployment identity or connection scope",
            ));
        }
        self.behaviour.validate()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EffortSetting {
    #[default]
    ProviderDefault,
    Explicit {
        value: String,
    },
}
impl EffortSetting {
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::ProviderDefault => None,
            Self::Explicit { value } => Some(value),
        }
    }
}

/// Effective, adapter-normalised settings; unknown defaults are not invented.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GenerationConfig {
    pub max_output_tokens: Option<u32>,
    pub max_reasoning_tokens: Option<u32>,
    pub temperature_milli: Option<u16>,
    pub top_p_milli: Option<u16>,
    pub seed: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviourDescriptor {
    pub adapter: AdapterKind,
    pub model_id: String,
    pub model_version: Option<String>,
    pub local_digest: Option<String>,
    pub quantisation: Option<String>,
    pub template_sha256: String,
    /// Adapter/template/defaults provenance, never a hash of a credential.
    /// Unknown endpoints need their own measured descriptor, not name matching.
    pub runtime_config_sha256: String,
    pub effort: EffortSetting,
    pub generation: GenerationConfig,
}

impl BehaviourDescriptor {
    pub fn validate(&self) -> Result<(), RoutingError> {
        if self.model_id.trim().is_empty()
            || self.model_id.len() > 256
            || !super::sha256(&self.template_sha256)
            || !super::sha256(&self.runtime_config_sha256)
            || self
                .local_digest
                .as_ref()
                .is_some_and(|v| !super::sha256(v))
            || self
                .model_version
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 256)
            || self
                .quantisation
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 64)
            || self
                .effort
                .value()
                .is_some_and(|v| !super::super::engine::valid_effort(v))
            || self.generation.max_output_tokens == Some(0)
            || self.generation.max_reasoning_tokens == Some(0)
            || self.generation.temperature_milli.is_some_and(|v| v > 2000)
            || self
                .generation
                .top_p_milli
                .is_some_and(|v| v == 0 || v > 1000)
        {
            return Err(RoutingError::Invalid("variant behaviour descriptor"));
        }
        Ok(())
    }
    pub fn fingerprint(&self) -> Result<String, RoutingError> {
        self.validate()?;
        super::digest("fosnie.variant.behaviour.v2", self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelVariant {
    pub id: Uuid,
    pub revision: i64,
    /// Personal variants may configure visible shared models without owning
    /// their credentials. They cannot expose somebody else's private model.
    pub scope: CatalogueScope,
    pub provider_id: Uuid,
    pub model_config_revision: i64,
    pub label: String,
    pub behaviour: BehaviourDescriptor,
    pub behaviour_sha256: String,
}

impl ModelVariant {
    pub fn validate(&self, deployment: &ModelDeployment) -> Result<(), RoutingError> {
        deployment.validate()?;
        if !self.scope.valid()
            || match (&self.scope, &deployment.scope) {
                (CatalogueScope::Deployment, CatalogueScope::User { .. }) => true,
                (CatalogueScope::User { owner_id: a }, CatalogueScope::User { owner_id: b }) => {
                    a != b
                }
                _ => false,
            }
        {
            return Err(RoutingError::Forbidden);
        }
        if self.id.is_nil()
            || self.revision < 1
            || self.label.trim().is_empty()
            || self.label.chars().count() > 120
        {
            return Err(RoutingError::Invalid("variant identity or label"));
        }
        if self.provider_id != deployment.provider_id
            || self.model_config_revision != deployment.model_config_revision
        {
            return Err(RoutingError::NeedsReplan);
        }
        let base = &deployment.behaviour;
        base.validate()?;
        if self.behaviour.adapter != base.adapter
            || self.behaviour.model_id != base.model_id
            || self.behaviour.model_version != base.model_version
            || self.behaviour.local_digest != base.local_digest
            || self.behaviour.quantisation != base.quantisation
            || self.behaviour.template_sha256 != base.template_sha256
            || self.behaviour.runtime_config_sha256 != base.runtime_config_sha256
        {
            return Err(RoutingError::NeedsReplan);
        }
        if self.behaviour.fingerprint()? != self.behaviour_sha256 {
            return Err(RoutingError::Invalid("variant behaviour fingerprint"));
        }
        let caps = &deployment.capabilities;
        if self
            .behaviour
            .effort
            .value()
            .is_some_and(|v| !caps.efforts.contains(v))
            || self
                .behaviour
                .generation
                .max_output_tokens
                .is_some_and(|v| caps.max_output_tokens.is_none_or(|max| v > max))
        {
            return Err(RoutingError::Incompatible(
                "variant settings exceed model capabilities",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantRef {
    pub id: Uuid,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRef {
    pub provider_id: Uuid,
    pub provider_revision: i64,
    pub profile_revision: i64,
    pub model_config_revision: i64,
    pub variant: VariantRef,
    pub effort: EffortSetting,
    pub behaviour_sha256: String,
    pub connection: Option<ConnectionRef>,
}

impl CandidateRef {
    pub fn validate(&self) -> Result<(), RoutingError> {
        if self.provider_id.is_nil()
            || self.provider_revision < 1
            || self.profile_revision < 1
            || self.model_config_revision < 1
            || self.variant.id.is_nil()
            || self.variant.revision < 1
            || !super::sha256(&self.behaviour_sha256)
            || self
                .effort
                .value()
                .is_some_and(|v| !super::super::engine::valid_effort(v))
            || self.connection.as_ref().is_some_and(|c| {
                c.id.is_nil()
                    || c.auth_revision < 1
                    || matches!(c.scope, CatalogueScope::User { owner_id } if owner_id.is_nil())
                    || c.quota_group_id.is_some_and(|id| id.is_nil())
            })
        {
            return Err(RoutingError::Invalid("candidate reference"));
        }
        Ok(())
    }
}
