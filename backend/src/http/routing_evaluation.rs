// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit manager-owned bounded text evaluation. No tools, sources or replay.
use std::{collections::BTreeMap,future::Future,time::Duration};
use axum::{Json,extract::State};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use uuid::Uuid;
use crate::{AppState,auth::{AuthContext,keycloak::AuthUser},model_routing::{*,run::RoutedRun,dispatch::StageRequest,transport::JsonOperation}};

#[derive(Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {pub role:String,pub content:String}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evaluation {
    pub evaluation_id:Uuid,
    pub policy_id:Uuid,
    pub expected_policy_revision:i64,
    pub selection:ModelSelection,
    pub messages:Vec<Message>,
    pub max_output_tokens:u32,
    pub max_cost_microusd:u64,
    pub max_duration_ms:u64,
    #[serde(default)] pub local_only:bool,
    #[serde(default)] pub requirements:Option<WorkloadRequirements>,
    #[serde(default)] pub response_schema:Option<Value>,
    #[serde(default)] pub temperature:Option<f32>,
}
struct Scope {state:AppState,actor:AuthContext,original:durable::Context,policy:(Uuid,i64)}
impl Scope {
    async fn check(&self)->crate::Result<()> {
        self.original.validate(&self.state.pg).await?;
        if self.actor.break_glass || self.actor.mfa_enroll_only {return Err(RoutingError::Forbidden.into());}
        self.state.rbac.require_permission(&self.state.pg,&self.actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
        let current=store::load_policy(&self.state,&self.actor,Some(self.policy.0)).await?;
        if current.revision!=self.policy.1 {return Err(RoutingError::NeedsReplan.into());} Ok(())
    }
    async fn guard<T>(&self,future:impl Future<Output=crate::Result<T>>)->crate::Result<T> {
        self.check().await?;tokio::pin!(future);
        let mut ticks=tokio::time::interval_at(tokio::time::Instant::now()+Duration::from_millis(250),Duration::from_millis(250));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {tokio::select! {biased;
            _=ticks.tick()=>self.check().await?,
            result=&mut future=>{self.check().await?;return result;}
        }}
    }
}
pub async fn evaluate(State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<Evaluation>)->crate::Result<Json<Value>> {
    if body.messages.is_empty() || body.messages.len()>64 || body.max_output_tokens==0 || body.max_output_tokens>2048
        || body.max_cost_microusd==0 || !(50..=30_000).contains(&body.max_duration_ms)
        || body.temperature.is_some_and(|t|!t.is_finite() || !(0.0..=2.0).contains(&t))
        || body.messages.iter().any(|m|!matches!(m.role.as_str(),"system"|"user"|"assistant") || m.content.len()>64_000) {
        return Err(RoutingError::Invalid("evaluation text, output, cost or duration bounds").into());
    }
    let messages=serde_json::to_value(&body.messages).map_err(|_|RoutingError::Invalid("evaluation messages"))?;
    let schema_extra=schema::overhead(body.response_schema.as_ref())?;
    let input=json!({"messages":messages,"tools":null}).to_string().len().checked_add(1024+schema_extra).ok_or(RoutingError::BudgetExhausted)?;
    if input>64_000 {return Err(RoutingError::Invalid("evaluation context exceeds bound").into());}
    let mut requirements=body.requirements.unwrap_or_default();
    requirements.input_tokens=requirements.input_tokens.max(input as u32);requirements.output_tokens=body.max_output_tokens;
    requirements.structured_output |= body.response_schema.is_some();
    if requirements.effort.as_ref().is_some_and(|s|s.is_empty() || s.len()>24 || s.trim()!=s)
        || requirements.language.as_ref().is_some_and(|s|s.len()>32) {
        return Err(RoutingError::Invalid("evaluation effort or language bounds").into());
    }
    if matches!(&body.selection,ModelSelection::Auto{policy_id,..} if *policy_id!=Some(body.policy_id)) {
        return Err(RoutingError::Invalid("evaluation must name its saved Auto policy").into());
    }
    let context=RoutingContext{actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Primary};
    let scope=Scope{state:state.clone(),actor:actor.clone(),original:durable::Context::capture(&context)?,policy:(body.policy_id,body.expected_policy_revision)};
    scope.check().await?;
    let limits=RoutingConstraints{local_only:body.local_only,max_calls:Some(2),max_total_tokens:Some(100_000),max_cost_microusd:Some(body.max_cost_microusd),
        max_duration_ms:Some(body.max_duration_ms),..Default::default()};
    let mut started=crate::audit::AuditEvent::action("routing.evaluation.started",actor.role.as_str());
    started.actor_user_id=actor.user_id;started.resource_type=Some("routing_evaluation".into());started.resource_id=Some(body.evaluation_id);
    started.model_agent_traceability=Some(json!({"run_id":context.run_id,"policy_id":body.policy_id,"policy_revision":body.expected_policy_revision,
        "limits":limits,"automatic_replay":false,"scope":"text_no_tools"}));
    crate::audit::append(&state.pg,&started).await?;
    let conversation=messages.to_string();
    let result=scope.guard(async {
        let run=RoutedRun::selected_capped(&state,context,body.selection,requirements.clone(),&conversation,scope.policy,limits).await?;
        let judge_usage=run.usage();let target=&run.plan().assignments[&CallPurpose::Primary];
        let profiles=store::current_profiles(store::catalog(&state,&actor).await?);
        let profile=profiles.iter().find(|p|p.provider_id==target.provider_id && p.provider_revision==target.provider_revision
            && p.revision==target.profile_revision && p.model==target.model).ok_or(RoutingError::NeedsReplan)?;
        if body.response_schema.is_some() && !schema::supported(profile.adapter) {
            return Err(RoutingError::Incompatible("native schema adapter is unsupported").into());
        }
        let req=WorkloadRequirements{effort:target.effort.clone(),..requirements};
        let sampling=crate::ml::Sampling{max_tokens:Some(body.max_output_tokens as i32),temperature:Some(body.temperature.unwrap_or(0.0)),
            response_schema:body.response_schema.clone(),..Default::default()};
        let stages:BTreeMap<_,_>=[(CallPurpose::Primary,StageRequest{requirements:req.clone(),max_calls:1})].into();
        let response=run.json(&state,CallPurpose::Primary,JsonOperation::ChatStep,
            json!({"messages":messages,"sampling":sampling}),&stages,Default::default(),&Default::default(),Duration::from_millis(body.max_duration_ms)).await?;
        let step:crate::ml::ChatStep=serde_json::from_value(response).map_err(|_|RoutingError::Invalid("evaluation response schema"))?;
        if step.finish_reason.as_deref()!=Some("stop") || !step.tool_calls.is_empty() || step.content.trim().is_empty() || step.content.len()>64_000 || !step.routing_report.as_ref().is_some_and(|r|r.complete && r.calls.len()==1
            && r.calls[0].purpose==CallPurpose::Primary && matches!(r.calls[0].status,lease::CallStatus::Completed)) {
            return Err(RoutingError::Invalid("evaluation requires one completed text-only accounted answer").into());
        }
        if let Some(schema)=&body.response_schema {
            schema::validate_output(schema,&step.content)?;
        }
        let call=&step.routing_report.as_ref().unwrap().calls[0];
        let profiles=store::current_profiles(store::catalog(&state,&actor).await?);
        let profile=profiles.iter().find(|p|p.provider_id==target.provider_id && p.provider_revision==target.provider_revision
            && p.revision==target.profile_revision && p.model==target.model).ok_or(RoutingError::NeedsReplan)?;
        let current=store::load_policy(&state,&actor,Some(body.policy_id)).await?;
        let effective=run.plan().constraints.intersect(&current.constraints).intersect(&store::deployment_constraints(&state).await?)
            .intersect(&state.inference_policy.constraints(&state.pg,&run.context(CallPurpose::Primary)).await?);
        if let Some(reason)=engine::target_incompatibility(profile,&req,&effective) {return Err(RoutingError::Incompatible(reason).into());}
        let mut decision=run.plan().decision.clone();decision.actual=Some(target.clone());
        Ok(json!({"ok":true,"mode":"live","evaluation_id":body.evaluation_id,"content":step.content,"decision":decision,
            "contract_version":"completed_text_v1","finish_reason":"stop","profile_sha256":learning::fingerprint(profile),
            "usage":run.usage(),"classifier_ledger":judge_usage,"actual_cost_microusd":null,"cost_basis":"conservative_profile_estimate",
            "stage_usage":[{"purpose":"primary","provider_id":call.provider_id,"input_tokens":call.usage.input_tokens,
                "output_tokens":call.usage.output_tokens,"total_tokens":call.usage.total_tokens,"reasoning_tokens":call.usage.reasoning_tokens,
                "cached_input_tokens":call.usage.cached_input_tokens,"latency_ms":call.latency_ms}],
            "automatic_replay":false,"quality_evaluated":false}))
    }).await;
    let mut finished=crate::audit::AuditEvent::action("routing.evaluation.finished",actor.role.as_str());
    finished.actor_user_id=actor.user_id;finished.resource_type=started.resource_type;finished.resource_id=Some(body.evaluation_id);
    finished.model_agent_traceability=started.model_agent_traceability.clone();
    let value=match result {
        Ok(value)=>{finished.token_usage=Some(json!({"ledger_snapshot":value["usage"],"accounting":"included_in_run"}));
            finished.model_agent_traceability=Some(json!({"run_id":started.model_agent_traceability.as_ref().and_then(|v|v.get("run_id")),
                "decision":value["decision"],"stage_usage":value["stage_usage"],"automatic_replay":false,"scope":"text_no_tools"}));value},
        Err(_error)=>{tracing::warn!("bounded evaluation failed or was revoked");finished.outcome=crate::audit::AuditOutcome::Failure;
            // Planning may have spent the judge allowance. An error never means free.
            finished.token_usage=Some(json!({"unknown":true,"max_reserved_cost_microusd":body.max_cost_microusd}));
            json!({"ok":false,"mode":"live","evaluation_id":body.evaluation_id,"error":"evaluation_failed_or_revoked",
                "usage":null,"actual_cost_microusd":null,"max_reserved_cost_microusd":body.max_cost_microusd,"automatic_replay":false})}
    };
    crate::audit::append(&state.pg,&finished).await?;Ok(Json(value))
}
