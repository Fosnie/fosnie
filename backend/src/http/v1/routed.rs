// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit, stateless text completions on the shared execution plan/ledger.
//! Explicit selection and admitted virtual aliases share this execution path.
use std::{collections::VecDeque, future::Future, sync::Arc, time::Duration};
use axum::{Json, extract::State, response::{IntoResponse, Response}};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Notify;
use uuid::Uuid;
use crate::{AppState, auth::{AuthContext, api_key::ApiKeyAuth, keycloak::AuthUser},
    config::runtime::{self, ConfigValueType}, ml,
    model_routing::{CallPurpose, RoutingContext, WorkloadRequirements, run::{RoutedRun, RoutedStream}}};
use super::{chat::{ChatCompletionRequest, Emit, completion, completion_id, now_unix},
    error::ApiError, sse::{self, StreamState, TurnGuard}};
use crate::model_routing::quality::{QualityContract,QualityFailure};

const GATE: &str = "routing.public_api_enabled";
pub(super) async fn enabled(state: &AppState) -> crate::Result<bool> {
    Ok(runtime::get(&state.pg, GATE).await?
        .is_some_and(|v| matches!(v.value_type, ConfigValueType::Bool) && v.value == "true"))
}
async fn description(state: &AppState) -> crate::Result<Value> {
    Ok(json!({"schema_version":1,"enabled":enabled(state).await?,"scope":"stateless_text_v1",
        "selection_modes":["default","manual","auto"],"request_field":"model_selection",
        "model_field":"omit_or_empty","default_max_output_tokens":1024,
        "agent_selection":true,"agent_scope":"general_text_read_tools_v1","agent_sampling":"configured_agent",
        "agent_messages":"one_user_text","agent_continuation":"explicit_selection_original_agent_and_sources",
        "client_tools":false,"virtual_alias":super::alias::listed(state).await?.map(|a|a["id"].clone()),
        "alias_configuration":super::alias::describe(state).await?,"automatic_replay":false,
        "technical_fallback":{"scope":"stateless_text_before_first_event","max_retries":1,"requires_active_auto_policy":true},
        "quality_contract":{"scope":"stateless_nonstreaming_text","kinds":["nonempty","json_object"],
            "requires_active_auto_policy":true,"shared_retry_limit":1}}))
}
pub async fn settings(State(state): State<AppState>, AuthUser(_): AuthUser) -> crate::Result<Json<Value>> {
    Ok(Json(description(&state).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enabled { pub enabled: bool }
pub async fn set_enabled(State(state): State<AppState>, AuthUser(actor): AuthUser, Json(body): Json<Enabled>) -> crate::Result<Json<Value>> {
    if actor.mfa_enroll_only || actor.break_glass {
        return Err(crate::model_routing::RoutingError::Forbidden.into());
    }
    state.rbac.require_permission(&state.pg, &actor, crate::auth::permissions::PROVIDERS_MANAGE).await?;
    runtime::set(&state.pg, GATE, if body.enabled {"true"} else {"false"}, ConfigValueType::Bool,
        "global", actor.user_id, actor.role.as_str()).await?;
    Ok(Json(description(&state).await?))
}
pub async fn capabilities(State(state): State<AppState>, ApiKeyAuth(actor, key): ApiKeyAuth) -> Result<Json<Value>, ApiError> {
    super::require_enabled_for(&state, &actor).await?;
    super::rate_limit(&state, key).await?;
    Ok(Json(description(&state).await?))
}

fn validate(req: &ChatCompletionRequest) -> Result<i32, ApiError> {
    if !req.model.is_empty() {
        return Err(ApiError::bad_request("omit 'model' when using model_selection; named models and agent selection cannot be combined with this extension"));
    }
    if !req.extra.is_empty() || req.stop.is_some() || req.tool_choice.is_some()
        || req.messages.iter().any(|m| m.name.is_some() || m.tool_call_id.is_some()
            || !matches!(m.role.as_str(), "system" | "developer" | "user" | "assistant")
            || match &m.content {
                Some(Value::String(_)) => false,
                Some(Value::Array(parts)) => parts.iter().any(|p| p.get("type").and_then(Value::as_str) != Some("text")
                    || p.get("text").and_then(Value::as_str).is_none()),
                _ => true,
            }) {
        return Err(ApiError::bad_request("routed completions support text messages and sampling only; unsupported fields cannot be ignored"));
    }
    if req.max_tokens.zip(req.max_completion_tokens).is_some_and(|(a,b)| a != b) {
        return Err(ApiError::bad_request("max_tokens and max_completion_tokens disagree"));
    }
    let output = req.max_tokens.or(req.max_completion_tokens).unwrap_or(1024);
    if output <= 0 || req.temperature.is_some_and(|v| !v.is_finite() || !(0.0..=2.0).contains(&v))
        || req.top_p.is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
        || [req.frequency_penalty, req.presence_penalty].iter().flatten().any(|v| !v.is_finite() || !(-2.0..=2.0).contains(v)) {
        return Err(ApiError::bad_request("invalid sampling or output limit"));
    }
    Ok(output)
}

/// Recheck the original principal and credential while planning, opening the
/// upstream response and consuming it. The guarded future is pinned once: a
/// timer tick never restarts a classifier or generation request.
#[derive(Clone)]
pub(super) struct Access { pub state: AppState, pub actor: AuthContext, pub key: Uuid, pub alias:Option<super::alias::Reference> }
impl Access {
    pub(super) async fn check(&self) -> Result<(), ApiError> {
        let live: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_keys k JOIN users u ON u.id=k.user_id WHERE k.id=$1 AND k.user_id=$2 AND k.kind='api' AND k.revoked_at IS NULL AND (k.expires_at IS NULL OR k.expires_at>now()) AND u.deactivated_at IS NULL AND u.role::text=$3)")
            .bind(self.key).bind(self.actor.user_id).bind(self.actor.role.as_str()).fetch_one(&self.state.pg).await?;
        if !live || self.actor.mfa_enroll_only || self.actor.break_glass {
            return Err(ApiError::forbidden("API credential or principal is no longer valid"));
        }
        super::require_enabled_globally(&self.state).await?;
        super::require_enabled_for(&self.state, &self.actor).await?;
        if !enabled(&self.state).await? {
            return Err(ApiError::unavailable("public API model routing is disabled"));
        }
        if let Some(alias)=&self.alias {super::alias::check(&self.state,alias).await?;}
        Ok(())
    }
    pub(super) async fn guard<T>(&self, future: impl Future<Output=Result<T, ApiError>>) -> Result<T, ApiError> {
        self.check().await?;
        tokio::pin!(future);
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_millis(250), Duration::from_millis(250));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! { biased;
                _ = ticks.tick() => self.check().await?,
                result = &mut future => { self.check().await?; return result; }
            }
        }
    }
}

pub async fn run(state: &AppState, actor: &AuthContext, key: Uuid, req: ChatCompletionRequest) -> Result<Response, ApiError> {
    run_in(state,actor,key,req,None).await
}
pub async fn run_alias(state:&AppState,actor:&AuthContext,key:Uuid,req:ChatCompletionRequest,alias:super::alias::Reference)->Result<Response,ApiError> {
    run_in(state,actor,key,req,Some(alias)).await
}
async fn run_in(state:&AppState,actor:&AuthContext,key:Uuid,mut req:ChatCompletionRequest,alias:Option<super::alias::Reference>)->Result<Response,ApiError> {
    let contract:Option<QualityContract>=req.extra.remove("quality_contract").map(serde_json::from_value).transpose()
        .map_err(|_|ApiError::bad_request("invalid quality_contract"))?;
    if let Some(contract)=&contract {
        if req.stream {return Err(ApiError::bad_request("quality_contract requires stream=false"));}
        contract.validate().map_err(crate::AppError::from)?;
    }
    let output = validate(&req)?;
    let response_model=alias.as_ref().map(|a|a.name.clone());
    let access = Access { state:state.clone(), actor:actor.clone(), key,alias };
    let messages = req.ml_messages();
    let conversation = serde_json::to_string(&messages).map_err(|_| ApiError::bad_request("invalid messages"))?;
    let input = u32::try_from(json!({"messages":messages,"tools":null}).to_string().len().saturating_add(1024))
        .map_err(|_| ApiError::bad_request("messages too large"))?;
    let context = RoutingContext { actor:actor.clone(), chat_id:None, project_id:None, run_id:Uuid::now_v7(), purpose:CallPurpose::Primary };
    let cancel = Arc::new(Notify::new());
    let selected = req.model_selection.clone().ok_or_else(|| ApiError::bad_request("model_selection is required"))?;
    if contract.is_some() {
        let crate::model_routing::ModelSelection::Auto{policy_id,..}=&selected else {
            return Err(ApiError::bad_request("quality_contract requires Auto selection"));
        };
        access.guard(async {
            let policy=crate::model_routing::store::load_policy(state,actor,*policy_id).await?;
            if !policy.quality_escalation || policy.rollout!=crate::model_routing::RolloutMode::Active {
                return Err(ApiError::bad_request("quality_contract requires an active policy with quality escalation enabled"));
            }
            crate::model_routing::engine::validate_policy(&policy).map_err(crate::AppError::from)?;Ok(())
        }).await?;
    }
    let run = access.guard(async {
        RoutedRun::selected(state, context, selected, WorkloadRequirements {input_tokens:input, output_tokens:output as u32,
            structured_output:matches!(contract,Some(QualityContract::JsonObject{..})),
            ..Default::default()}, &conversation, &[], &cancel).await.map_err(ApiError::from)
    }).await?;
    let mut source = Source {access, run, stream:None, first:None, buffered:VecDeque::new(), quality_failure:None, ended:false, recorded:false};
    let mut sampling = req.sampling(); sampling.max_tokens = Some(output);
    if let Err(error)=source.open_checked(state,&messages,&sampling,contract.as_ref()).await {
        source.finish("failed",None).await?;return Err(error);
    }
    let model=response_model.unwrap_or_else(||source.run.plan().assignments[&CallPurpose::Final].model.clone());
    let id = completion_id(); let created = now_unix();
    let mut stream = StreamState::new(TurnGuard::new(cancel, None), source, id.clone(), model.clone(), created);
    if req.stream { Ok(sse::stream_response(stream)) }
    else { let aggregate = sse::collect(&mut stream).await?; Ok(Json(completion(&id, &model, created, &aggregate)).into_response()) }
}

struct Source {access:Access, run:RoutedRun, stream:Option<RoutedStream>, first:Option<Value>, buffered:VecDeque<Value>,
    quality_failure:Option<QualityFailure>, ended:bool, recorded:bool}
impl Source {
    async fn open_checked(&mut self,state:&AppState,messages:&[ml::Message],sampling:&ml::Sampling,contract:Option<&QualityContract>)->Result<(),ApiError> {
        let Some(contract)=contract else {return self.open(state,messages,sampling).await;};
        self.run.require_quality_escalation(state).await?;
        loop {
            self.open(state,messages,sampling).await?;
            let mut answer=String::new();let mut bytes=0usize;
            loop {
                let event=self.access.guard(async {
                    if let Some(first)=self.first.take() {Ok(Some(first))}
                    else {self.stream.as_mut().expect("opened routed stream").recv().await.map_err(ApiError::from)}
                }).await?.ok_or_else(||ApiError::unavailable("routed stream ended before completion"))?;
                bytes=bytes.saturating_add(event.to_string().len());
                if bytes>1_048_576 || self.buffered.len()>=4096 {return Err(ApiError::unavailable("quality validation buffer exceeded"));}
                let finished=match translate(&event,Value::Null)? {
                    Emit::Delta{content,..}=>{answer.push_str(&content);false},
                    Emit::Final{finish_reason,..}=>{
                        if finish_reason!="stop" {return Err(ApiError::unavailable("quality contract requires a complete, unfiltered answer"));}
                        true
                    },
                    Emit::Failed(error)=>return Err(error),
                };
                self.buffered.push_back(event);
                if finished {break;}
            }
            // A completed lease must be released before changing the executor.
            self.stream.take();
            match contract.check(&answer) {
                Ok(())=>return Ok(()),
                Err(failure)=>{
                    self.quality_failure=Some(failure);self.buffered.clear();
                    let access=self.access.clone();
                    if !access.guard(async {self.run.escalate_stateless(state,failure).await.map_err(ApiError::from)}).await? {
                        return Err(ApiError::unavailable("answer failed quality contract; retry allowance exhausted"));
                    }
                }
            }
        }
    }
    async fn open(&mut self,state:&AppState,messages:&[ml::Message],sampling:&ml::Sampling)->Result<(),ApiError> {
        let access=self.access.clone();
        loop {
            // Preserve typed inference failures inside the access guard. An
            // expired key/alias/gate is an outer error and can never be retried.
            let result=access.guard(async {Ok(async {
                let mut stream=self.run.generate(state,ml::GenerateRequest {messages:messages.to_vec(),sampling:sampling.clone(),
                    model:None,tools:None,overrides:Default::default()}).await?;
                let first=stream.recv().await?.ok_or_else(||crate::AppError::Unavailable("routed stream ended before completion".into()))?;
                Ok::<_,crate::AppError>((stream,first))
            }.await)}).await?;
            match result {
                Ok((stream,first))=>{self.stream=Some(stream);self.first=Some(first);return Ok(());}
                Err(error)=>{
                    if let Some(kind)=crate::model_routing::fallback::OpenFailure::kind(&error) {
                        let retry=access.guard(async {self.run.retry_stateless(state,kind).await.map_err(ApiError::from)}).await?;
                        if retry {continue;}
                    }
                    return Err(error.into());
                }
            }
        }
    }
    fn metadata(&self, completed: bool) -> Value {
        let mut decision = self.run.plan().decision.clone();
        // A proposal is not evidence that a model completed an inference.
        if completed { decision.actual = self.run.plan().assignments.get(&CallPurpose::Final).cloned(); }
        let mut value=json!({"schema_version":1,"run_id":self.run.plan().id,"selection":self.run.plan().selection,
            "decision":decision,"usage":self.run.usage(),"fallback":self.run.fallback_attempt(),
            "quality_escalation":self.run.quality_attempt(),"quality_rejection":self.quality_failure});
        super::alias::metadata(&mut value,self.access.alias.as_ref());value
    }
    async fn finish(&mut self, status:&str, usage:Option<&ml::Usage>) -> Result<(),ApiError> {
        self.ended = true;
        self.first.take();
        self.buffered.clear();
        self.stream.take();
        if status != "completed" { self.run.cancel(); }
        if !self.recorded {
            let event = self.audit_event(status, usage);
            // A failed audit cannot turn a completion into an unmetered success.
            crate::audit::append(&self.access.state.pg, &event).await?;
            self.recorded = true;
        }
        Ok(())
    }
    fn audit_event(&self, status:&str, usage:Option<&ml::Usage>) -> crate::audit::AuditEvent {
        let mut event = crate::audit::AuditEvent::action("api.completion.routed", self.access.actor.role.as_str());
        event.actor_user_id = self.access.actor.user_id;
        event.resource_type = Some("api_completion".into());
        if status != "completed" { event.outcome = crate::audit::AuditOutcome::Failure; }
        event.resource_id = Some(self.run.plan().id);
        event.model_agent_traceability = Some(json!({"routing":self.metadata(status=="completed"),"status":status,"api_key_id":self.access.key}));
        event.token_usage = Some(json!({"prompt_tokens":usage.and_then(|u|u.prompt_tokens),"completion_tokens":usage.and_then(|u|u.completion_tokens),
            "routing_usage":self.run.usage()}));
        event
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        // Drop the lease first so an interrupted attempt retains its reservation.
        self.stream.take();
        self.run.cancel();
        if !self.recorded {
            let event = self.audit_event("cancelled", None);
            let pool = self.access.state.pg.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    if let Err(error) = crate::audit::append(&pool, &event).await {
                        tracing::error!(%error, "failed to record cancelled routed API completion");
                    }
                });
            }
        }
    }
}
impl sse::EmitSource for Source {
    async fn next(&mut self) -> Option<Emit> {
        if self.ended { return None; }
        let result = self.access.guard(async {
            if let Some(event)=self.buffered.pop_front() {Ok(Some(event))}
            else if let Some(first)=self.first.take() {Ok(Some(first))}
            else {self.stream.as_mut().expect("opened routed stream").recv().await.map_err(ApiError::from)}
        }).await;
        let result = result.and_then(|event| event.ok_or_else(|| ApiError::unavailable("routed stream ended before completion")))
            .and_then(|event| translate(&event, self.metadata(true)));
        match result {
            Ok(emit @ Emit::Delta {..}) => Some(emit),
            Ok(emit @ Emit::Final {..}) => {
                let usage = match &emit {Emit::Final {usage,..}=>usage, _=>unreachable!()};
                match self.finish("completed", Some(usage)).await {Ok(())=>Some(emit),Err(error)=>Some(Emit::Failed(error))}
            }
            Ok(Emit::Failed(error)) | Err(error) => {
                let error = self.finish("failed", None).await.err().unwrap_or(error);
                Some(Emit::Failed(error))
            }
        }
    }
}

fn translate(event:&Value, routing:Value) -> Result<Emit,ApiError> {
    match event.get("type").and_then(Value::as_str) {
        Some("token" | "reasoning") => {
            let delta = event.get("delta").and_then(Value::as_str).ok_or_else(|| ApiError::unavailable("invalid routed delta"))?.to_owned();
            Ok(if event["type"]=="token" {Emit::Delta {content:delta,reasoning:String::new()}}
                else {Emit::Delta {content:String::new(),reasoning:delta}})
        }
        Some("done") => {
            // Only the lease-validated report is authoritative for usage. Never
            // fabricate zero tokens from missing/contradictory provider totals.
            let calls = event["routing_report"]["calls"].as_array().filter(|c| c.len()==1)
                .ok_or_else(|| ApiError::unavailable("completion requires one accounted model call"))?;
            let call = &calls[0];
            if call["purpose"]!="final" || call["status"]!="completed" {
                return Err(ApiError::unavailable("model call did not complete"));
            }
            let count = |key:&str| call["usage"][key].as_u64().and_then(|v| i32::try_from(v).ok());
            let usage = ml::Usage {prompt_tokens:count("input_tokens"),completion_tokens:count("output_tokens"),reasoning_tokens:count("reasoning_tokens")};
            if usage.prompt_tokens.is_none() || usage.completion_tokens.is_none()
                || usage.prompt_tokens.unwrap().checked_add(usage.completion_tokens.unwrap()).is_none() {
                return Err(ApiError::unavailable("completion token usage is unavailable"));
            }
            let reason = event["finish_reason"].as_str().filter(|s| matches!(*s,"stop"|"length"|"content_filter"))
                .ok_or_else(|| ApiError::unavailable("unsupported routed finish reason"))?;
            Ok(Emit::Final {finish_reason:reason.into(),usage,citations:None,routing:Some(routing)})
        }
        _ => Err(ApiError::unavailable("unexpected routed event; stateless completions cannot execute tools")),
    }
}
