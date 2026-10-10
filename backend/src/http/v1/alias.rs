// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Collision-safe opt-in namespace, separate from actual downstream models.
use axum::{Json,extract::State};
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use crate::{AppState,Result,AppError,auth::keycloak::AuthUser,model_routing::{RoutingObjective,RoutingError}};

#[derive(Clone,Serialize)]
pub struct Reference {pub name:String,pub revision:i64}
#[derive(sqlx::FromRow)]
struct Stored {name:String,enabled:bool,objective:String,revision:i64,updated_at:time::OffsetDateTime,collision:bool}
impl Stored {
    fn objective(&self)->Result<RoutingObjective> {match self.objective.as_str() {
        "economy"=>Ok(RoutingObjective::Economy),"balanced"=>Ok(RoutingObjective::Balanced),"quality"=>Ok(RoutingObjective::Quality),
        _=>Err(RoutingError::Invalid("stored API alias objective").into()),
    }}
}
async fn load(state:&AppState)->Result<Stored> {
    Ok(sqlx::query_as("SELECT a.name,a.enabled,a.objective,a.revision,a.updated_at,EXISTS(SELECT 1 FROM provider_configs p WHERE p.role='llm' AND COALESCE(p.label,p.model,p.id::text)=a.name) AS collision FROM routing_api_alias a WHERE a.id=1")
        .fetch_one(&state.pg).await?)
}
pub fn provider_error(error:sqlx::Error)->AppError {
    if error.as_database_error().is_some_and(|e|e.constraint()==Some("routing_api_alias_name_collision")) {
        AppError::Conflict("this name collides with the enabled API routing alias; choose another name or disable the alias first".into())
    } else {AppError::Database(error)}
}
fn valid_name(name:&str)->bool {
    !name.is_empty() && name.len()<=128 && !name.starts_with(super::models::AGENT_PREFIX)
        && name.bytes().next().is_some_and(|c|c.is_ascii_alphanumeric())
        && name.bytes().all(|c|c.is_ascii_alphanumeric() || b"._/-".contains(&c))
}
pub async fn describe(state:&AppState)->Result<Value> {
    let alias=load(state).await?;
    let status=if alias.collision {"conflict"} else if !alias.enabled {"disabled"}
        else if !super::routed::enabled(state).await? {"routing_disabled"} else {"ready"};
    Ok(json!({"schema_version":1,"name":alias.name,"enabled":alias.enabled,"revision":alias.revision,
        "objective":alias.objective()?,"status":status,"agent_field":"fosnie_agent_id"}))
}
pub async fn settings(State(state):State<AppState>,AuthUser(actor):AuthUser)->Result<Json<Value>> {
    state.rbac.require_permission(&state.pg,&actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    Ok(Json(describe(&state).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {pub name:String,pub enabled:bool,pub objective:RoutingObjective,pub expected_revision:i64}
pub async fn save(State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<Save>)->Result<Json<Value>> {
    if actor.break_glass || actor.mfa_enroll_only || actor.role==crate::auth::PlatformRole::SuperAdmin {return Err(RoutingError::Forbidden.into());}
    let context=crate::model_routing::RoutingContext {actor:actor.clone(),chat_id:None,project_id:None,run_id:uuid::Uuid::now_v7(),purpose:crate::model_routing::CallPurpose::Primary};
    crate::model_routing::durable::Context::capture(&context)?.validate(&state.pg).await?;
    state.rbac.require_permission(&state.pg,&actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    if !valid_name(&body.name) || body.expected_revision<1 {return Err(RoutingError::Invalid("API alias name or revision").into());}
    let objective=match body.objective {RoutingObjective::Economy=>"economy",RoutingObjective::Balanced=>"balanced",RoutingObjective::Quality=>"quality"};
    let mut tx=state.pg.begin().await?;
    let revision:Option<i64>=sqlx::query_scalar("UPDATE routing_api_alias SET name=$1,enabled=$2,objective=$3,revision=nextval('routing_revision_generation'),updated_by=$4,updated_at=now() WHERE id=1 AND revision=$5 RETURNING revision")
        .bind(&body.name).bind(body.enabled).bind(objective).bind(actor.user_id).bind(body.expected_revision)
        .fetch_optional(&mut *tx).await.map_err(provider_error)?;
    let revision=revision.ok_or(RoutingError::NeedsReplan)?;
    let mut event=crate::audit::AuditEvent::action("routing.api_alias_changed",actor.role.as_str());event.actor_user_id=actor.user_id;
    event.resource_type=Some("routing_api_alias".into());event.payload=Some(json!({"name":body.name,"enabled":body.enabled,"objective":objective,"revision":revision}));
    crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;
    Ok(Json(describe(&state).await?))
}
/// Disabled/conflicting names retain legacy label semantics. An enabled alias
/// with its execution gate off fails explicitly rather than entering legacy.
pub async fn resolve(state:&AppState,name:&str)->Result<Option<(Reference,RoutingObjective)>> {
    let alias=load(state).await?;
    if alias.name!=name || !alias.enabled || alias.collision {return Ok(None);}
    if !super::routed::enabled(state).await? {return Err(AppError::Unavailable("public API model routing is disabled".into()));}
    Ok(Some((Reference {name:alias.name.clone(),revision:alias.revision},alias.objective()?)))
}
pub(super) async fn check(state:&AppState,reference:&Reference)->Result<()> {
    match resolve(state,&reference.name).await? {
        Some((current,_)) if current.revision==reference.revision=>Ok(()),
        _=>Err(AppError::Conflict("API routing alias changed or is unavailable; request was not replayed".into())),
    }
}
pub(super) async fn listed(state:&AppState)->Result<Option<Value>> {
    let alias=load(state).await?;
    if !alias.enabled || alias.collision || !super::routed::enabled(state).await? {return Ok(None);}
    Ok(Some(json!({"id":alias.name,"object":"model","created":alias.updated_at.unix_timestamp(),"owned_by":"fosnie-routing",
        "fosnie_routing":{"schema_version":1,"virtual_alias":true,"objective":alias.objective()?,"agent_field":"fosnie_agent_id"}})))
}
pub(super) fn metadata(value:&mut Value,reference:Option<&Reference>) {
    if let Some(reference)=reference {value["requested_model"]=json!(reference.name);value["alias_revision"]=json!(reference.revision);}
}
