// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit fixed voice REST adoption. Live WS/telephony retain their own gate.
use std::{future::Future,sync::Arc,time::Duration};
use axum::{body::{Body,Bytes},extract::{Query,State},http::header,response::{IntoResponse,Response},Json};
use serde::Deserialize;
use serde_json::{json,Value};
use crate::{AppState,Result,auth::{AuthContext,keycloak::AuthUser},config::runtime::{self,ConfigValueType},
    model_routing::{RoutingContext,RoutingError,fixed::FixedRole,run::{AccessGuard,RoutedRun,MAX_AUDIO_BYTES,MAX_SPEECH_TEXT_BYTES}}};

const GATE:&str="routing.voice_rest_enabled";
pub async fn live_settings(State(state):State<AppState>,AuthUser(_):AuthUser)->Result<Json<Value>> {
    Ok(Json(crate::voice::routed::description(&state).await?))
}
pub async fn set_live_enabled(State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<Enabled>)->Result<Json<Value>> {
    if actor.mfa_enroll_only || actor.break_glass {return Err(RoutingError::Forbidden.into());}
    state.rbac.require_permission(&state.pg,&actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    runtime::set(&state.pg,crate::voice::routed::GATE,if body.enabled {"true"} else {"false"},ConfigValueType::Bool,"global",actor.user_id,actor.role.as_str()).await?;
    Ok(Json(crate::voice::routed::description(&state).await?))
}
async fn enabled(state:&AppState)->Result<bool> {
    Ok(runtime::get(&state.pg,GATE).await?.is_some_and(|v|matches!(v.value_type,ConfigValueType::Bool) && v.value=="true"))
}
async fn description(state:&AppState)->Result<Value> {
    Ok(json!({"schema_version":1,"enabled":enabled(state).await?,"scope":"fixed_voice_rest_v1",
        "transcribe":"/api/voice/routed/transcribe","speech":"/api/voice/routed/speech",
        "selection":"configured_fixed_profiles","max_audio_bytes":MAX_AUDIO_BYTES,"max_speech_text_bytes":MAX_SPEECH_TEXT_BYTES,
        "llm_selection":false,"live_voice":false,"telephony":false,"automatic_replay":false,
        "accounting":"calls_and_provider_bytes_tokens_and_cost_unknown"}))
}
pub async fn settings(State(state):State<AppState>,AuthUser(_):AuthUser)->Result<Json<Value>> {Ok(Json(description(&state).await?))}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enabled {pub enabled:bool}
pub async fn set_enabled(State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<Enabled>)->Result<Json<Value>> {
    if actor.mfa_enroll_only || actor.break_glass {return Err(RoutingError::Forbidden.into());}
    state.rbac.require_permission(&state.pg,&actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    runtime::set(&state.pg,GATE,if body.enabled {"true"} else {"false"},ConfigValueType::Bool,"global",actor.user_id,actor.role.as_str()).await?;
    Ok(Json(description(&state).await?))
}
#[derive(Clone)]
struct Access {state:AppState,actor:AuthContext}
#[async_trait::async_trait]
impl AccessGuard for Access {
    async fn check(&self)->Result<()> {
        let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND deactivated_at IS NULL AND role::text=$2)")
            .bind(self.actor.user_id).bind(self.actor.role.as_str()).fetch_one(&self.state.pg).await?;
        if !live || self.actor.mfa_enroll_only || self.actor.break_glass
            || !self.state.features.enabled_for(&self.state,&self.actor,"voice").await {return Err(RoutingError::Forbidden.into());}
        if !enabled(&self.state).await? {return Err(crate::AppError::Unavailable("routed voice REST is disabled".into()));}
        Ok(())
    }
}
impl Access {
    async fn guard<T>(&self,future:impl Future<Output=Result<T>>)->Result<T> {
        self.check().await?;tokio::pin!(future);
        let mut ticks=tokio::time::interval_at(tokio::time::Instant::now()+Duration::from_millis(250),Duration::from_millis(250));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {tokio::select! {biased;
            _=ticks.tick()=>self.check().await?,
            result=&mut future=>{self.check().await?;return result;}
        }}
    }
    async fn prepare(&self,role:FixedRole)->Result<RoutedRun> {
        self.check().await?;
        crate::cache::rate_limit_guard(&self.state.redis,&format!("voice:{}",self.actor.user_id.unwrap_or_default()),40,60).await?;
        let context=RoutingContext {actor:self.actor.clone(),run_id:uuid::Uuid::now_v7(),chat_id:None,project_id:None,purpose:role.purpose()};
        self.guard(async {RoutedRun::fixed_voice(&self.state,context,&[role],&tokio::sync::Notify::new()).await}).await?
            .with_access(Arc::new(self.clone()))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscribeQuery {pub mime:Option<String>,pub language:Option<String>}
pub async fn transcribe(State(state):State<AppState>,AuthUser(actor):AuthUser,Query(query):Query<TranscribeQuery>,body:Bytes)->Result<Json<Value>> {
    let access=Access {state,actor};let run=access.prepare(FixedRole::Stt).await?;
    let value=run.transcribe(&access.state,body.to_vec(),query.mime.as_deref().unwrap_or("application/octet-stream"),query.language.as_deref()).await?;
    access.check().await?;
    Ok(Json(json!({"text":value["text"],"fosnie_routing":{"schema_version":1,"run_id":run.plan().id,
        "decision_id":run.plan().decision.id,"fixed_assignment":run.plan().fixed_assignments.get(&FixedRole::Stt),"usage":run.usage()}})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpeakRequest {pub text:String,pub voice:Option<String>}
pub async fn speak(State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<SpeakRequest>)->Result<Response> {
    let access=Access {state,actor};let run=access.prepare(FixedRole::Tts).await?;
    let audio=run.speech(&access.state,&body.text,body.voice.as_deref()).await?;
    access.check().await?;
    // Binary responses expose correlation and conservative counters, never the
    // inference envelope or credentials. Full target references live in audit.
    Ok(([(header::CONTENT_TYPE.as_str(),audio.mime),
        ("x-fosnie-routing-run-id",run.plan().id.to_string()),
        ("x-fosnie-routing-calls",run.usage().calls.to_string()),
        ("x-fosnie-routing-accounting","tokens_and_cost_unknown".into())],Body::from(audio.bytes)).into_response())
}
