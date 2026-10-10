// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use axum::{Json,extract::{State,Path,Query},http::StatusCode};
use serde::Deserialize;
use serde_json::{Value,json};
use uuid::Uuid;
use crate::{AppState,Result,auth::{keycloak::AuthUser,permissions},research::jobs};
pub fn routes()->axum::Router<AppState> {
    use axum::routing::{get,post};
    axum::Router::new().route("/api/routing/research-jobs",get(capabilities).put(set_enabled))
        .route("/api/research-jobs/scope",get(scope))
        .route("/api/research-jobs",get(history).post(start)).route("/api/research-jobs/{id}",get(status))
        .route("/api/research-launches/{id}",get(receipt))
        .route("/api/research-jobs/{id}/cancel",post(cancel))
        .route("/api/research-jobs/{id}/answers",post(answers))
}
/// Metadata only: includes project knowledge without invoking legacy triage or
/// disclosing file paths/content. Launch revalidates the complete source inventory.
pub async fn scope(State(state):State<AppState>,AuthUser(actor):AuthUser)->Result<Json<Value>> {
    if actor.mfa_enroll_only || actor.user_id.is_none() {return Err(crate::model_routing::RoutingError::Forbidden.into());}
    let entries=crate::kb::dr_scope(&state.pg,&actor).await?;
    Ok(Json(json!(entries.into_iter().map(|entry|json!({"id":entry.id,"name":entry.name,
        "kind":entry.kind,"doc_count":entry.doc_count})).collect::<Vec<_>>())))
}
pub async fn capabilities(State(state):State<AppState>,AuthUser(_):AuthUser)->Result<Json<Value>> {
    Ok(Json(json!({"schema_version":1,"enabled":jobs::enabled(&state).await?,"selection_modes":["default","manual","auto"],
        "sources":["files","web","hybrid"],"web_enabled":crate::integrations::is_enabled(&state.pg,crate::integrations::ConnectorKind::WebSearch).await?,
        "templates":["exploration","formal","literature","freeform"],"custom_templates":true,"maximum_template_sections":8,
        "maximum_documents":4,"maximum_web_pages":4,"clarification":true,"verification":true,"deepening":"existing_sources_v1",
        "history":true,"idempotent_launch":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetEnabled {pub enabled:bool}
pub async fn set_enabled(State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<SetEnabled>)->Result<Json<Value>> {
    if actor.mfa_enroll_only {return Err(crate::model_routing::RoutingError::Forbidden.into());}
    state.rbac.require_permission(&state.pg,&actor,permissions::PROVIDERS_MANAGE).await?;
    crate::config::runtime::set(&state.pg,jobs::ENABLED_KEY,if body.enabled {"true"} else {"false"},
        crate::config::runtime::ConfigValueType::Bool,"global",actor.user_id,actor.role.as_str()).await?;
    Ok(Json(json!({"enabled":body.enabled})))
}
pub async fn start(State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<jobs::Launch>)->Result<(StatusCode,Json<jobs::Started>)> {
    Ok((StatusCode::ACCEPTED,Json(jobs::launch(&state,&actor,body).await?)))
}
pub async fn status(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(id):Path<Uuid>)->Result<Json<Value>> {
    Ok(Json(jobs::status(&state,&actor,id).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {before:Option<Uuid>}
pub async fn history(State(state):State<AppState>,AuthUser(actor):AuthUser,Query(query):Query<HistoryQuery>)->Result<Json<Value>> {
    Ok(Json(crate::research::history::list(&state,&actor,query.before).await?))
}
pub async fn receipt(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(id):Path<Uuid>)->Result<Json<Value>> {
    Ok(Json(crate::research::history::lookup(&state,&actor,id).await?))
}
pub async fn answers(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(id):Path<Uuid>,Json(body):Json<jobs::Answers>)->Result<Json<Value>> {
    jobs::resume(&state,&actor,id,body).await?;
    Ok(Json(json!({"job_id":id,"accepted":true})))
}
pub async fn cancel(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(id):Path<Uuid>)->Result<Json<Value>> {
    jobs::cancel(&state,&actor,id).await?;
    let status:String=sqlx::query_scalar("SELECT status FROM routed_jobs WHERE id=$1 AND actor_user_id=$2 AND kind='deep_research'")
        .bind(id).bind(actor.user_id).fetch_one(&state.pg).await?;
    Ok(Json(json!({"job_id":id,"status":status})))
}
