// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! One bounded metadata request. Never enables a model or certifies generation.
use super::super::{AdapterKind, CallPurpose, Locality, RoutingContext, RoutingError};
use super::connections::*;
use crate::{auth::AuthContext, AppError, AppState, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeSet, time::Duration};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRequest {
    pub schema_version: u32,
    pub expected_revision: i64,
}
#[derive(Debug, Serialize)]
pub struct DiscoveredModel {
    pub model_id: String,
    pub input_token_limit: Option<u32>,
    pub output_token_limit: Option<u32>,
    pub generation_supported: Option<bool>,
    pub enabled: bool,
    pub calibration_state: &'static str,
}
#[derive(Serialize)]
pub struct DiscoveryReport {
    pub schema_version: u32,
    pub connection_id: Uuid,
    pub connection_revision: i64,
    pub models: Vec<DiscoveredModel>,
    pub partial: bool,
    pub metadata_requests: u32,
    pub generation_calls: u32,
    pub credits_known: bool,
    pub capabilities_verified: bool,
}
fn unavailable() -> AppError {
    AppError::Unavailable("model catalogue metadata is unavailable".into())
}

pub async fn discover(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    body: DiscoveryRequest,
) -> Result<DiscoveryReport> {
    check_actor(state, actor).await?;
    version(body.schema_version, body.expected_revision)?;
    let known = list(state, actor)
        .await?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or(RoutingError::Forbidden)?;
    let owner = owner_for(state, actor, &known.scope).await?;
    if owner != known.scope_id || known.revision != body.expected_revision {
        return Err(RoutingError::NeedsReplan.into());
    }
    let context = RoutingContext {
        actor: actor.clone(),
        chat_id: None,
        project_id: None,
        run_id: Uuid::now_v7(),
        purpose: CallPurpose::RouterClassify,
    };
    let constraints = super::super::store::deployment_constraints(state)
        .await?
        .intersect(
            &state
                .inference_policy
                .constraints(&state.pg, &context)
                .await?,
        );
    let metadata: ConnectionMetadata = serde_json::from_value(known.metadata.clone())
        .map_err(|_| RoutingError::Invalid("connection metadata"))?;
    if constraints.local_only
        && (!metadata.locality_trusted || metadata.locality != Locality::Local)
    {
        return Err(RoutingError::Forbidden.into());
    }
    if let Some(allowed) = &constraints.allowed_providers {
        let visible:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM provider_configs WHERE role='llm' AND connection_id=$1 AND id=ANY($2))")
            .bind(id).bind(allowed.iter().copied().collect::<Vec<_>>()).fetch_one(&state.pg).await?;
        if !visible {
            return Err(RoutingError::Forbidden.into());
        }
    }
    let adapter: AdapterKind = serde_json::from_value(Value::String(known.adapter.clone().ok_or(
        RoutingError::Incompatible("configure the connection adapter before discovery"),
    )?))
    .map_err(|_| RoutingError::Invalid("connection adapter"))?;
    let provider = state
        .providers
        .resolve_connection_id(&state.pg, actor.user_id, id)
        .await?
        .ok_or(RoutingError::ProviderUnavailable)?;
    if provider.base_url != known.base_url || !provider.enabled {
        return Err(RoutingError::NeedsReplan.into());
    }
    let base = endpoint(
        provider
            .base_url
            .as_deref()
            .ok_or(RoutingError::Invalid("connection endpoint missing"))?,
    )?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|_| unavailable())?;
    let key = provider.api_key.unwrap_or_default();
    let mut url = reqwest::Url::parse(&format!("{base}/models")).map_err(|_| unavailable())?;
    match adapter {
        AdapterKind::Gemini => {
            url.query_pairs_mut().append_pair("pageSize", "512");
        }
        AdapterKind::Anthropic => {
            url.query_pairs_mut().append_pair("limit", "512");
        }
        _ => {}
    }
    let request = match adapter {
        AdapterKind::OpenaiChat | AdapterKind::OpenaiResponses => {
            let request = client.get(url.clone());
            if key.is_empty() {
                request
            } else {
                request.bearer_auth(&key)
            }
        }
        AdapterKind::Gemini => client.get(url.clone()).header("x-goog-api-key", &key),
        AdapterKind::Anthropic => client
            .get(url.clone())
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01"),
    };
    let mut response = request.send().await.map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > 262144 {
            return Err(RoutingError::Invalid("discovery metadata exceeds capacity").into());
        }
        bytes.extend_from_slice(&chunk);
    }
    // Vendor bodies are never relayed. Reject accidental secret reflection even
    // inside a seemingly valid catalogue before selecting any output fields.
    if !key.is_empty() && String::from_utf8_lossy(&bytes).contains(&key) {
        return Err(unavailable());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    let models = decode(adapter, &value)?;
    let partial = value
        .get("has_more")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .get("nextPageToken")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty());
    let mut conn = state.pg.acquire().await?;
    let current = row(&mut conn, id, false)
        .await?
        .ok_or(RoutingError::NeedsReplan)?;
    if current.revision != known.revision || !current.enabled {
        return Err(RoutingError::NeedsReplan.into());
    }
    check_actor(state, actor).await?;
    owner_for(state, actor, &known.scope).await?;
    Ok(DiscoveryReport {
        schema_version: 2,
        connection_id: id,
        connection_revision: known.revision,
        models,
        partial,
        metadata_requests: 1,
        generation_calls: 0,
        credits_known: false,
        capabilities_verified: false,
    })
}
pub fn decode(adapter: AdapterKind, value: &Value) -> Result<Vec<DiscoveredModel>> {
    let rows = value
        .get(if adapter == AdapterKind::Gemini {
            "models"
        } else {
            "data"
        })
        .and_then(Value::as_array)
        .ok_or_else(unavailable)?;
    if rows.len() > 512 {
        return Err(RoutingError::Invalid("discovery model capacity exceeded").into());
    }
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    for row in rows {
        let id = row
            .get(if adapter == AdapterKind::Gemini {
                "name"
            } else {
                "id"
            })
            .and_then(Value::as_str)
            .ok_or_else(unavailable)?;
        let id = if adapter == AdapterKind::Gemini {
            id.strip_prefix("models/").ok_or_else(unavailable)?
        } else {
            id
        };
        if id.is_empty()
            || id.len() > 256
            || id.chars().any(char::is_control)
            || !seen.insert(id.to_owned())
        {
            return Err(unavailable());
        }
        let limit = |name: &str| -> Result<Option<u32>> {
            match row.get(name) {
                None | Some(Value::Null) => Ok(None),
                Some(value) => value
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .filter(|v| *v > 0)
                    .map(Some)
                    .ok_or_else(unavailable),
            }
        };
        let generation_supported = if adapter == AdapterKind::Gemini {
            row.get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .map(|methods| {
                    methods
                        .iter()
                        .any(|m| m.as_str() == Some("generateContent"))
                })
        } else {
            None
        };
        models.push(DiscoveredModel {
            model_id: id.to_owned(),
            input_token_limit: limit("inputTokenLimit")?,
            output_token_limit: limit("outputTokenLimit")?,
            generation_supported,
            enabled: false,
            calibration_state: "unprofiled",
        });
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_never_activates_or_claims_quality_and_rejects_malformed_capacity() {
        let report=decode(AdapterKind::Gemini,&serde_json::json!({"models":[{"name":"models/fixture","supportedGenerationMethods":["generateContent"],"inputTokenLimit":32000}]})).unwrap();
        assert!(!report[0].enabled);
        assert_eq!(report[0].generation_supported, Some(true));
        assert_eq!(report[0].calibration_state, "unprofiled");
        assert!(decode(
            AdapterKind::OpenaiChat,
            &serde_json::json!({"data":[{"id":"same"},{"id":"same"}]})
        )
        .is_err());
        assert!(decode(
            AdapterKind::Gemini,
            &serde_json::json!({"models":[{"name":"models/x","outputTokenLimit":-1}]})
        )
        .is_err());
        assert!(decode(AdapterKind::OpenaiChat,&serde_json::json!({"data":(0..513).map(|n|serde_json::json!({"id":format!("model-{n}")})).collect::<Vec<_>>()})).is_err());
    }
}
