// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Shared project defaults use deployment references. Each actor resolves them
//! independently with their current permissions; no captured principal or budget.
use super::super::{ModelSelection, RoutingError, store};
use super::{catalogue::CatalogueScope, connections::check_actor, variants};
use crate::{
    AppState, Result,
    auth::{AuthContext, rbac::Permission},
};
use fosnie_protocol::routing_vnext::RoutingIntentV2;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Serialize)]
pub struct ProjectBinding {
    pub schema_version: u32,
    pub project_id: Uuid,
    pub revision: i64,
    pub routing_intent: Option<RoutingIntentV2>,
}
pub async fn project(state: &AppState, actor: &AuthContext, id: Uuid) -> Result<ProjectBinding> {
    check_actor(state, actor).await?;
    state
        .rbac
        .require_project(&state.pg, actor, id, Permission::Read)
        .await?;
    let (revision, value): (i64, Option<serde_json::Value>) =
        sqlx::query_as("SELECT routing_revision,routing_intent FROM projects WHERE id=$1")
            .bind(id)
            .fetch_one(&state.pg)
            .await?;
    let intent: Option<RoutingIntentV2> = value
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| RoutingError::Invalid("stored project routing intent"))?;
    if let Some(v) = &intent {
        v.require_support(&[1, 2], &[1, 2])
            .map_err(RoutingError::Invalid)?;
    }
    Ok(ProjectBinding {
        schema_version: 2,
        project_id: id,
        revision,
        routing_intent: intent,
    })
}
fn present<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<RoutingIntentV2>, D::Error> {
    RoutingIntentV2::deserialize(d).map(Some)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveProjectBinding {
    pub schema_version: u32,
    pub expected_revision: i64,
    #[serde(default, deserialize_with = "present")]
    pub routing_intent: Option<RoutingIntentV2>,
    #[serde(default)]
    pub clear: bool,
}
pub async fn save_project(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    body: SaveProjectBinding,
) -> Result<ProjectBinding> {
    check_actor(state, actor).await?;
    state
        .rbac
        .require_project(&state.pg, actor, id, Permission::Write)
        .await?;
    if body.schema_version != 2
        || body.expected_revision < 1
        || body.clear == body.routing_intent.is_some()
    {
        return Err(RoutingError::Invalid("project binding revision or clear action").into());
    }
    if let Some(intent) = &body.routing_intent {
        intent
            .require_support(&[1, 2], &[1, 2])
            .map_err(RoutingError::Invalid)?;
        let catalogue = variants::catalogue(state, actor).await?;
        match intent.selection {
            ModelSelection::Manual { provider_id } => {
                if !catalogue.deployments.iter().any(|d| {
                    d.id == provider_id
                        && d.scope == CatalogueScope::Deployment
                        && d.enabled
                        && d.binding_current
                }) {
                    return Err(RoutingError::Forbidden.into());
                }
                if let Some(v) = &intent.variant {
                    if !catalogue.variants.iter().any(|e| {
                        e.variant.id == v.id
                            && e.variant.revision == v.expected_revision
                            && e.variant.provider_id == provider_id
                            && e.variant.scope == CatalogueScope::Deployment
                            && e.configuration_current
                            && e.binding_current
                    }) {
                        return Err(RoutingError::NeedsReplan.into());
                    }
                }
            }
            ModelSelection::Auto {
                policy_id: Some(policy_id),
                ..
            } => {
                let policies = store::policies(state, actor).await?;
                if !policies.iter().any(|p| {
                    p.id == policy_id
                        && p.scope == "deployment"
                        && p.required_profile_version as u32 == intent.required_profile_version
                }) {
                    return Err(RoutingError::Forbidden.into());
                }
            }
            _ => {}
        }
    }
    let mut tx = state.pg.begin().await?;
    let revision: i64 = sqlx::query_scalar("SELECT nextval('routing_revision_generation')")
        .fetch_one(&mut *tx)
        .await?;
    let changed=sqlx::query("UPDATE projects SET routing_intent=$2,routing_revision=$3 WHERE id=$1 AND routing_revision=$4")
        .bind(id).bind(body.routing_intent.as_ref().map(|v|serde_json::to_value(v).unwrap())).bind(revision).bind(body.expected_revision).execute(&mut *tx).await?;
    if changed.rows_affected() != 1 {
        return Err(RoutingError::NeedsReplan.into());
    }
    super::connections::audit(
        &mut tx,
        actor,
        "routing.project_binding_saved",
        id,
        revision,
    )
    .await?;
    tx.commit().await?;
    project(state, actor, id).await
}
