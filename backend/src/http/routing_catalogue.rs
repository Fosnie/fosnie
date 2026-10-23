// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use crate::{
    auth::keycloak::AuthUser,
    model_routing::vnext::{connections, deployments, discovery, variants},
    AppState, Result,
};
use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;
pub async fn availability(State(state):State<AppState>,AuthUser(actor):AuthUser)->Result<Json<serde_json::Value>> {
    use crate::model_routing::{self as routing,CallPurpose};
    let catalogue=variants::catalogue(&state,&actor).await?;
    let profiles=routing::store::current_profiles(routing::store::catalog(&state,&actor).await?);
    let mut models=Vec::new();
    for d in &catalogue.deployments {
        let failure=if let Some(p)=profiles.iter().find(|p|p.provider_id==d.id) {
            routing::health::blocked(&state,&routing::TargetReference{provider_id:d.id,provider_revision:p.provider_revision,profile_revision:p.revision,model:p.model.clone(),effort:None,variant:None},CallPurpose::Primary).await?
        }else{None};
        models.push(serde_json::json!({"provider_id":d.id,"state":if failure.is_some(){"reported_unavailable"}else{"availability_unconfirmed"},"failure":failure.map(|f|f.public()),"credits":"unknown"}));
    }
    Ok(Json(serde_json::json!({"schema_version":2,"models":models,"generation_calls":0})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub expected_revision: i64,
}
pub async fn project_binding(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(id):Path<Uuid>)->Result<Json<crate::model_routing::vnext::bindings::ProjectBinding>> {
    Ok(Json(crate::model_routing::vnext::bindings::project(&state,&actor,id).await?))
}
pub async fn save_project_binding(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(id):Path<Uuid>,Json(body):Json<crate::model_routing::vnext::bindings::SaveProjectBinding>)->Result<Json<crate::model_routing::vnext::bindings::ProjectBinding>> {
    Ok(Json(crate::model_routing::vnext::bindings::save_project(&state,&actor,id,body).await?))
}
pub async fn profiles(State(state):State<AppState>,AuthUser(actor):AuthUser)->Result<Json<Vec<crate::model_routing::vnext::profiles::ProfileEntry>>> {
    Ok(Json(crate::model_routing::vnext::profiles::list(&state,&actor).await?))
}
pub async fn save_routing_profile(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(id):Path<Uuid>,
    Json(body):Json<crate::model_routing::vnext::profiles::SaveProfile>)->Result<Json<crate::model_routing::vnext::profiles::ProfileEntry>> {
    Ok(Json(crate::model_routing::vnext::profiles::save(&state,&actor,id,body).await?))
}
pub async fn delete_routing_profile(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(id):Path<Uuid>,Query(body):Query<Revision>)->Result<Json<serde_json::Value>> {
    crate::model_routing::vnext::connections::check_actor(&state,&actor).await?;
    crate::model_routing::store::delete_policy_supported(&state,&actor,id,body.expected_revision,2).await?;
    Ok(Json(serde_json::json!({"ok":true})))
}
pub async fn export_routing_profile(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(id):Path<Uuid>)->Result<Json<crate::model_routing::vnext::profiles::ProfileExport>> {
    Ok(Json(crate::model_routing::vnext::profiles::export(&state,&actor,id).await?))
}
pub async fn import_routing_profile(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(id):Path<Uuid>,
    Json(body):Json<crate::model_routing::vnext::profiles::ImportProfile>)->Result<Json<crate::model_routing::vnext::profiles::ProfileEntry>> {
    Ok(Json(crate::model_routing::vnext::profiles::import(&state,&actor,id,body).await?))
}
pub async fn catalogue(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
) -> Result<Json<variants::Catalogue>> {
    Ok(Json(variants::catalogue(&state, &actor).await?))
}
pub async fn connections(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
) -> Result<Json<Vec<connections::ConnectionEntry>>> {
    Ok(Json(connections::list(&state, &actor).await?))
}
pub async fn save_connection(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<connections::SaveConnection>,
) -> Result<Json<connections::ConnectionEntry>> {
    Ok(Json(connections::save(&state, &actor, id, body).await?))
}
pub async fn delete_connection(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
    Query(body): Query<Revision>,
) -> Result<Json<serde_json::Value>> {
    connections::delete(&state, &actor, id, body.expected_revision).await?;
    Ok(Json(serde_json::json!({"ok":true})))
}
pub async fn discovery(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<discovery::DiscoveryRequest>,
) -> Result<Json<discovery::DiscoveryReport>> {
    Ok(Json(discovery::discover(&state, &actor, id, body).await?))
}
pub async fn save_deployment(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<deployments::SaveDeployment>,
) -> Result<Json<deployments::DeploymentEntry>> {
    Ok(Json(deployments::save(&state, &actor, id, body).await?))
}
pub async fn delete_deployment(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
    Query(body): Query<Revision>,
) -> Result<Json<serde_json::Value>> {
    deployments::delete(&state, &actor, id, body.expected_revision).await?;
    Ok(Json(serde_json::json!({"ok":true})))
}
pub async fn save_metadata(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<deployments::SaveMetadata>,
) -> Result<Json<deployments::DeploymentEntry>> {
    Ok(Json(
        deployments::save_metadata(&state, &actor, id, body).await?,
    ))
}
pub async fn save_tariffs(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<deployments::SaveTariffs>,
) -> Result<Json<deployments::DeploymentEntry>> {
    Ok(Json(
        deployments::save_tariffs(&state, &actor, id, body).await?,
    ))
}
pub async fn save_variant(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<variants::SaveVariant>,
) -> Result<Json<super::super::model_routing::vnext::catalogue::ModelVariant>> {
    Ok(Json(variants::save(&state, &actor, id, body).await?))
}
pub async fn delete_variant(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
    Query(body): Query<Revision>,
) -> Result<Json<serde_json::Value>> {
    variants::delete(&state, &actor, id, body.expected_revision).await?;
    Ok(Json(serde_json::json!({"ok":true})))
}
pub async fn export_variant(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<variants::VariantExport>> {
    Ok(Json(variants::export(&state, &actor, id).await?))
}
pub async fn import_variant(
    State(state): State<AppState>,
    AuthUser(actor): AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<variants::ImportVariant>,
) -> Result<Json<super::super::model_routing::vnext::catalogue::ModelVariant>> {
    Ok(Json(variants::import(&state, &actor, id, body).await?))
}
