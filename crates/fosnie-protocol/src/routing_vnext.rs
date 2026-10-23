// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0

//! Explicit v2 intent. The v1 frames and ModelSelection remain byte-compatible.
//! D2 advertises support explicitly per adopted surface; old frames stay strict.
use crate::{ModelSelection, RoutingOptions};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const ROUTING_VNEXT_VERSION: u32 = 2;
pub const MAX_ACTIVE_VARIANTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantSelection {
    pub id: Uuid,
    pub expected_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingIntentV2 {
    pub schema_version: u32,
    pub selection: ModelSelection,
    /// Required even when a policy is selected by an existing policy_id.
    /// An old server must never execute a different interpretation of it.
    pub required_profile_version: u32,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub variant: Option<VariantSelection>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub options: Option<RoutingOptions>,
}

/// Absence inherits the saved binding; Default explicitly resets it. A turn
/// override only changes the saved binding when save_binding is requested.
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutedChatRequestV2 {
    pub content:String,
    pub chat_id:Option<Uuid>,
    pub project_id:Option<Uuid>,
    pub agent_id:Option<Uuid>,
    #[serde(default,deserialize_with="present",skip_serializing_if="Option::is_none")]
    pub routing_intent:Option<RoutingIntentV2>,
    /// Independent per-turn option override is allowed when intent is inherited.
    #[serde(default,deserialize_with="present",skip_serializing_if="Option::is_none")]
    pub options:Option<RoutingOptions>,
    #[serde(default)] pub save_binding:bool,
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

impl RoutingIntentV2 {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != ROUTING_VNEXT_VERSION
            || !matches!(self.required_profile_version, 1 | ROUTING_VNEXT_VERSION)
        {
            return Err("unsupported routing contract version");
        }
        if let Some(variant) = &self.variant {
            if !matches!(self.selection, ModelSelection::Manual { .. })
                || variant.id.is_nil()
                || variant.expected_revision < 1
            {
                return Err("a variant requires an explicit manual model and current revision");
            }
        }
        if self.required_profile_version != 1
            && !matches!(self.selection, ModelSelection::Auto { .. })
        {
            return Err("profile version applies to Auto selection");
        }
        Ok(())
    }

    /// Both contract and profile semantics must be supported before dispatch.
    pub fn require_support(
        &self,
        selection_versions: &[u32],
        profile_versions: &[u32],
    ) -> Result<(), &'static str> {
        self.validate()?;
        if !selection_versions.contains(&self.schema_version)
            || !profile_versions.contains(&self.required_profile_version)
        {
            return Err("routing feature is not supported by this server");
        }
        Ok(())
    }

    /// Downgrade is a caller decision. It is safe only for unchanged v1 intent;
    /// returning an error preserves a draft instead of silently discarding it.
    pub fn legacy(&self) -> Result<(ModelSelection, Option<RoutingOptions>), &'static str> {
        self.validate()?;
        if self.variant.is_some() || self.required_profile_version != 1 {
            return Err("routing intent cannot be represented by v1");
        }
        Ok((self.selection.clone(), self.options.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RoutingObjective;
    #[test]
    fn v2_chat_absence_inherits_and_null_is_rejected() {
        let absent:RoutedChatRequestV2=serde_json::from_value(serde_json::json!({"content":"continue"})).unwrap();
        assert!(absent.routing_intent.is_none());assert!(!absent.save_binding);
        assert!(serde_json::from_value::<RoutedChatRequestV2>(serde_json::json!({"content":"continue","routing_intent":null})).is_err());
        assert!(serde_json::from_value::<crate::ClientFrame>(serde_json::json!({"type":"chat.routed.send","request":{"content":"hi","routing_intent":{"schema_version":2}}})).is_err());
        let frame=serde_json::json!({"type":"chat.routed.v2.send","request":{"chat_id":null,"project_id":null,"agent_id":null,"content":"hi","routing_intent":{"schema_version":2,"required_profile_version":1,"selection":{"mode":"default"}},"save_binding":true}});
        let parsed:crate::ClientFrame=serde_json::from_value(frame.clone()).unwrap();assert_eq!(serde_json::to_value(parsed).unwrap(),frame);
    }

    #[test]
    fn v2_intent_requires_support_and_preserves_v1_authority() {
        let policy = Uuid::from_u128(9);
        let intent = RoutingIntentV2 {
            schema_version: 2,
            required_profile_version: 2,
            selection: ModelSelection::Auto {
                objective: RoutingObjective::Balanced,
                policy_id: Some(policy),
            },
            variant: None,
            options: None,
        };
        assert!(intent.require_support(&[1], &[1]).is_err());
        assert!(intent.require_support(&[1, 2], &[1]).is_err());
        intent.require_support(&[1, 2], &[1, 2]).unwrap();
        assert!(intent.legacy().is_err());
        let wire = serde_json::to_value(&intent).unwrap();
        assert_eq!(wire["selection"]["policy_id"], policy.to_string());
        assert!(serde_json::from_value::<ModelSelection>(wire).is_err());
        let mut legacy = intent.clone();
        legacy.required_profile_version = 1;
        assert_eq!(legacy.legacy().unwrap().0, intent.selection);
    }

    #[test]
    fn manual_variants_cannot_downgrade_or_become_auto() {
        let mut intent = RoutingIntentV2 {
            schema_version: 2,
            required_profile_version: 1,
            selection: ModelSelection::Manual {
                provider_id: Uuid::from_u128(1),
            },
            variant: Some(VariantSelection {
                id: Uuid::from_u128(2),
                expected_revision: 3,
            }),
            options: None,
        };
        intent.validate().unwrap();
        assert!(intent.legacy().is_err());
        intent.selection = ModelSelection::Auto {
            objective: RoutingObjective::Economy,
            policy_id: None,
        };
        assert!(intent.validate().is_err());
        intent.variant = None;
        intent.schema_version = 3;
        assert!(intent.validate().is_err());
    }

    #[test]
    fn v2_schema_rejects_competing_ids_and_unknown_fields() {
        let base = serde_json::json!({"schema_version":2,"required_profile_version":1,"selection":{"mode":"default"}});
        let valid: RoutingIntentV2 = serde_json::from_value(base.clone()).unwrap();
        valid.validate().unwrap();
        for key in ["profile_id", "api_key", "connection_url"] {
            let mut value = base.clone();
            value[key] = serde_json::json!("ignored");
            assert!(serde_json::from_value::<RoutingIntentV2>(value).is_err());
        }
        for key in ["variant", "options"] {
            let mut value = base.clone();
            value[key] = serde_json::Value::Null;
            assert!(serde_json::from_value::<RoutingIntentV2>(value).is_err());
        }
    }
}
