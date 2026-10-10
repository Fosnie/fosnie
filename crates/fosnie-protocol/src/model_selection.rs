// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0

//! Absence means inheritance; Default explicitly resets a previous selection.
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingObjective {
    Economy,
    #[default]
    Balanced,
    Quality,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelSelection {
    #[default]
    Default,
    Manual { provider_id: Uuid },
    Auto {
        #[serde(default)]
        objective: RoutingObjective,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy_id: Option<Uuid>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingOptions {
    pub local_only: bool,
    pub effort: Option<String>,
}

/// A distinct opt-in frame: old servers cannot silently ignore selection fields
/// on chat.send. Clients discover support before sending this request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutedChatRequest {
    #[serde(default)] pub chat_id: Option<Uuid>,
    #[serde(default)] pub project_id: Option<Uuid>,
    #[serde(default)] pub agent_id: Option<Uuid>,
    pub content: String,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")] pub model_selection: Option<ModelSelection>,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")] pub options: Option<RoutingOptions>,
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

impl ModelSelection {
    pub fn from_request(selection: Option<Self>, legacy: Option<Uuid>) -> Result<Option<Self>, &'static str> {
        match (selection, legacy) {
            (None, None) => Ok(None),
            (None, Some(provider_id)) => Ok(Some(Self::Manual { provider_id })),
            (Some(selection), None) => Ok(Some(selection)),
            (Some(Self::Manual { provider_id }), Some(id)) if provider_id == id => Ok(Some(Self::Manual { provider_id })),
            (Some(_), Some(_)) => Err("selection conflicts with llm_provider_id"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routed_frame_is_strict_and_absence_inherits() {
        let absent: RoutedChatRequest = serde_json::from_value(serde_json::json!({"content":"hello"})).unwrap();
        assert!(absent.model_selection.is_none() && absent.options.is_none());
        for extra in [serde_json::json!({"model_selection":null}), serde_json::json!({"options":null}),
            serde_json::json!({"options":{"local_only":true,"unknown":true}}),serde_json::json!({"llm_provider_id":Uuid::nil()}),
            serde_json::json!({"attachment_ids":[]}),serde_json::json!({"model_selection":{"mode":"auto","objective":"unknown"}})] {
            let mut value=serde_json::json!({"content":"hello"}); value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            assert!(serde_json::from_value::<RoutedChatRequest>(value).is_err());
        }
        let frame=serde_json::json!({"type":"chat.routed.send","request":{"content":"hello","model_selection":{"mode":"auto","objective":"quality"},"options":{"local_only":true,"effort":"xhigh"}}});
        let parsed:crate::ClientFrame=serde_json::from_value(frame.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap()["request"]["model_selection"],frame["request"]["model_selection"]);
        assert!(serde_json::from_value::<crate::ClientFrame>(serde_json::json!({"type":"chat.send","content":"legacy","chat_id":null})).is_ok());
    }
    #[test]
    fn r03_absence_inherits_but_default_resets() {
        assert_eq!(ModelSelection::from_request(None, None), Ok(None));
        assert_eq!(ModelSelection::from_request(Some(ModelSelection::Default), None), Ok(Some(ModelSelection::Default)));
        assert_eq!(serde_json::from_str::<ModelSelection>(r#"{"mode":"auto"}"#).unwrap(), ModelSelection::Auto { objective: RoutingObjective::Balanced, policy_id: None });
    }
    #[test]
    fn r30_legacy_uuid_and_conflicting_intent() {
        let id = Uuid::from_u128(1);
        assert_eq!(ModelSelection::from_request(None, Some(id)), Ok(Some(ModelSelection::Manual { provider_id: id })));
        assert!(ModelSelection::from_request(Some(ModelSelection::Default), Some(id)).is_err());
        assert!(serde_json::from_str::<ModelSelection>(r#"{"mode":"manual"}"#).is_err());
        assert!(serde_json::from_str::<ModelSelection>(r#"{"mode":"auto","provider_id":"00000000-0000-0000-0000-000000000001"}"#).is_err());
    }
}
