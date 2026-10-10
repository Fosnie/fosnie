// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use axum::{Json,extract::{State,Path},http::StatusCode};
use serde::Deserialize;
use serde_json::{json,Value};
use uuid::Uuid;
use crate::{AppState,Result,auth::{keycloak::AuthUser,permissions},groundedness::{jobs,durable}};

pub fn routes()->axum::Router<AppState> {
    use axum::routing::{get,post};
    axum::Router::new()
        .route("/api/routing/document-jobs",get(capabilities).put(set_enabled))
        .route("/api/document-verification-jobs",post(start))
        .route("/api/artefact-verification-jobs",post(start_artefact))
        .route("/api/document-verification-jobs/{id}",get(get_job))
        .route("/api/document-verification-jobs/{id}/cancel",post(cancel))
        .route("/api/document-verification-jobs/{id}/repair",post(repair))
}

pub async fn capabilities(State(state):State<AppState>,AuthUser(ctx):AuthUser)->Result<Json<Value>> {
    Ok(Json(json!({"schema_version":1,"enabled":jobs::enabled(&state).await? && state.features.enabled_for(&state,&ctx,"groundedness").await,
        "selection_modes":["default","manual","auto"],"target_types":["document","draft"],"delayed_repair":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtefactLaunch {pub artefact_id:Uuid,pub selection:crate::model_routing::ModelSelection}
pub async fn start_artefact(State(state):State<AppState>,AuthUser(ctx):AuthUser,Json(body):Json<ArtefactLaunch>)->Result<(StatusCode,Json<Value>)> {
    let id=jobs::launch_target(&state,&ctx,jobs::Launch {document_id:body.artefact_id,selection:body.selection,repair:false},
        crate::groundedness::routed_draft::TargetKind::Draft).await?;
    Ok((StatusCode::ACCEPTED,Json(json!({"job_id":id,"status_url":format!("/api/document-verification-jobs/{id}")}))))
}
pub async fn repair(State(state):State<AppState>,AuthUser(ctx):AuthUser,Path(id):Path<Uuid>)->Result<(StatusCode,Json<Value>)> {
    durable::enqueue_repair(&state,&ctx,id).await?;
    Ok((StatusCode::ACCEPTED,Json(json!({"job_id":id,"status_url":format!("/api/document-verification-jobs/{id}")}))))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetEnabled {pub enabled:bool}
pub async fn set_enabled(State(state):State<AppState>,AuthUser(ctx):AuthUser,Json(body):Json<SetEnabled>)->Result<Json<Value>> {
    if ctx.mfa_enroll_only {return Err(crate::model_routing::RoutingError::Forbidden.into());}
    state.rbac.require_permission(&state.pg,&ctx,permissions::PROVIDERS_MANAGE).await?;
    crate::config::runtime::set(&state.pg,jobs::ENABLED_KEY,if body.enabled {"true"} else {"false"},
        crate::config::runtime::ConfigValueType::Bool,"global",ctx.user_id,ctx.role.as_str()).await?;
    Ok(Json(json!({"enabled":body.enabled})))
}
pub async fn start(State(state):State<AppState>,AuthUser(ctx):AuthUser,Json(body):Json<jobs::Launch>)->Result<(StatusCode,Json<Value>)> {
    let id=jobs::launch(&state,&ctx,body).await?;
    Ok((StatusCode::ACCEPTED,Json(json!({"job_id":id,"status":"queued","status_url":format!("/api/document-verification-jobs/{id}")}))))
}
pub async fn get_job(State(state):State<AppState>,AuthUser(ctx):AuthUser,Path(id):Path<Uuid>)->Result<Json<jobs::Status>> {
    Ok(Json(jobs::status(&state,&ctx,id).await?))
}
pub async fn cancel(State(state):State<AppState>,AuthUser(ctx):AuthUser,Path(id):Path<Uuid>)->Result<Json<Value>> {
    // Cancellation remains available when access/entitlement/the rollout gate
    // was revoked. Ownership suffices to stop the caller's own spending.
    let own:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1 AND actor_user_id=$2 AND kind='verify_draft')")
        .bind(id).bind(ctx.user_id).fetch_one(&state.pg).await?;
    if !own {return Err(crate::model_routing::RoutingError::Forbidden.into());}
    durable::cancel(&state,&ctx,id).await?;
    let status:String=sqlx::query_scalar("SELECT status FROM routed_jobs WHERE id=$1 AND actor_user_id=$2")
        .bind(id).bind(ctx.user_id).fetch_one(&state.pg).await?;
    Ok(Json(json!({"job_id":id,"status":status})))
}
