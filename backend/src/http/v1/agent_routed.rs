// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit selection on agent/<uuid>, with a persistent source witness and
//! one request at a time per conversation. No request-owned client tools.
use std::sync::Arc;
use axum::{Json,http::HeaderMap,response::{IntoResponse,Response}};
use serde_json::{Value,json};
use tokio::sync::{Notify,mpsc,oneshot};
use uuid::Uuid;
use crate::{AppState,auth::AuthContext,ml,model_routing::{CallPurpose,WorkloadRequirements,RoutingError,
    run::{RoutedRun,AccessGuard}},ws::protocol::ServerFrame};
use super::{agent,routed::Access,error::ApiError,chat::{self,ChatCompletionRequest,Emit},sse::{self,StreamState,TurnGuard}};

pub(super) async fn lock_chat(state:&AppState,chat:Uuid)->Result<sqlx::Transaction<'static,sqlx::Postgres>,ApiError> {
    let mut tx=state.pg.begin().await?;
    let locked:bool=sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("api-chat:{chat}")).fetch_one(&mut *tx).await?;
    if !locked {return Err(crate::AppError::Conflict("another API turn is running in this conversation".into()).into());}
    Ok(tx)
}

struct Scope {access:Access,chat:Uuid,agent:Uuid,snapshot:Value}
impl Scope {
    async fn capture(state:&AppState,actor:&AuthContext,chat:Uuid,agent_id:Uuid)->crate::Result<Value> {
        let row:Option<(Option<Uuid>,Option<Uuid>,String)>=sqlx::query_as("SELECT project_id,agent_id,mode FROM chats WHERE id=$1 AND owner_user_id=$2 AND origin='api' AND archived_at IS NULL")
            .bind(chat).bind(actor.user_id).fetch_optional(&state.pg).await?;
        let (project,agent,mode)=row.ok_or(RoutingError::Forbidden)?;
        if agent!=Some(agent_id) || mode!="general" {return Err(RoutingError::Incompatible("API continuation requires the original agent and general mode").into());}
        let config=crate::workflows::agents::capture(&mut *state.pg.acquire().await?,agent_id).await?;
        if config.is_null() || (!actor.is_admin() && !config["created_by"].is_null() && config["created_by"]!=json!(actor.user_id)) {
            return Err(RoutingError::Forbidden.into());
        }
        if !config["modes"].as_array().is_some_and(|v|v.iter().any(|v|v=="general")) {
            return Err(RoutingError::Incompatible("API agent must support general mode").into());
        }
        if let Some(project)=project {
            state.rbac.require_project(&state.pg,actor,project,crate::auth::rbac::Permission::Read).await?;
        }
        // Require every configured agent source, not merely the surviving
        // subset returned by an ACL-filtered allowlist.
        let configured:Vec<Uuid>=serde_json::from_value(config["kb_ids"].clone()).map_err(|_|RoutingError::Forbidden)?;
        for id in &configured {crate::kb::require_read(&state.pg,actor,*id).await?;}
        let mut libraries=crate::kb::retrieval_allowlist(&state.pg,actor,chat,project,Some(agent_id)).await?;
        libraries.sort();libraries.dedup();
        for id in &libraries {crate::kb::require_read(&state.pg,actor,*id).await?;crate::kb::brief(&state.pg,*id).await?;}
        let mut denied=state.rbac.denied_kb_doc_ids(&state.pg,actor,&libraries).await?;denied.sort();denied.dedup();
        Ok(json!({"agent":config,"project":project,"libraries":libraries,"denied":denied}))
    }
    async fn check_api(&self)->Result<(),ApiError> {
        self.access.check().await?;
        if Self::capture(&self.access.state,&self.access.actor,self.chat,self.agent).await?!=self.snapshot {
            return Err(crate::AppError::Conflict("agent or source access changed; start a new conversation".into()).into());
        }
        Ok(())
    }
    async fn guard<T>(&self,future:impl std::future::Future<Output=Result<T,ApiError>>)->Result<T,ApiError> {
        self.check_api().await?;tokio::pin!(future);
        let mut ticks=tokio::time::interval(std::time::Duration::from_millis(250));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {tokio::select! {biased;
            _=ticks.tick()=>self.check_api().await?,
            result=&mut future=>{self.check_api().await?;return result;}
        }}
    }
}
#[async_trait::async_trait]
impl AccessGuard for Scope {
    async fn check(&self)->crate::Result<()> {
        self.check_api().await.map_err(|e|crate::AppError::Forbidden(e.message))
    }
    fn unattended(&self)->bool {true}
}

fn validate(req:&ChatCompletionRequest)->Result<String,ApiError> {
    // Server-owned history and agent sampling are deliberate contracts. Do not
    // pretend to honor request system prompts or silently discard sampling.
    if req.messages.len()!=1 || req.messages[0].role!="user" || !req.extra.is_empty()
        || req.temperature.is_some() || req.top_p.is_some() || req.max_tokens.is_some() || req.max_completion_tokens.is_some()
        || req.frequency_penalty.is_some() || req.presence_penalty.is_some() || req.stop.is_some() || req.tool_choice.is_some()
        || req.messages[0].name.is_some() || req.messages[0].tool_call_id.is_some() {
        return Err(ApiError::bad_request("routed agents accept one user text message; history and sampling belong to the agent"));
    }
    match &req.messages[0].content {
        Some(Value::String(text)) if !text.trim().is_empty()=>Ok(text.clone()),
        _=>Err(ApiError::bad_request("routed agents require a nonempty text string")),
    }
}

pub async fn run(state:&AppState,actor:&AuthContext,key:Uuid,req:ChatCompletionRequest,agent_id:Uuid,headers:&HeaderMap)->Result<Response,ApiError> {
    run_in(state,actor,key,req,agent_id,headers,None).await
}
pub async fn run_alias(state:&AppState,actor:&AuthContext,key:Uuid,req:ChatCompletionRequest,agent_id:Uuid,headers:&HeaderMap,alias:super::alias::Reference)->Result<Response,ApiError> {
    run_in(state,actor,key,req,agent_id,headers,Some(alias)).await
}
async fn run_in(state:&AppState,actor:&AuthContext,key:Uuid,req:ChatCompletionRequest,agent_id:Uuid,headers:&HeaderMap,alias:Option<super::alias::Reference>)->Result<Response,ApiError> {
    let content=validate(&req)?;
    let response_model=alias.as_ref().map(|a|a.name.clone()).unwrap_or_else(||req.model.clone());
    let access=Access {state:state.clone(),actor:actor.clone(),key,alias};access.check().await?;
    let uid=super::models::user_id(actor)?;
    agent::require_visible_agent(state,actor,uid,agent_id).await?;
    let chat=match agent::continued_chat(headers)? {
        Some(chat)=>{agent::verify_api_chat(state,uid,chat).await?;chat},
        None=>agent::create_chat(state,uid,agent_id,&content).await?,
    };
    let lease=lock_chat(state,chat).await?;
    let snapshot=Scope::capture(state,actor,chat,agent_id).await?;
    let saved:Option<(Uuid,Value)>=sqlx::query_as("SELECT agent_id,source_scope FROM api_routed_chats WHERE chat_id=$1")
        .bind(chat).fetch_optional(&state.pg).await?;
    if let Some((id,prior))=saved {
        if id!=agent_id || prior!=snapshot {return Err(crate::AppError::Conflict("agent or sources changed; start a new conversation".into()).into());}
    } else {
        let history:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages WHERE chat_id=$1)").bind(chat).fetch_one(&state.pg).await?;
        if history {return Err(ApiError::bad_request("start a new conversation to adopt routing; legacy history has no source witness"));}
        sqlx::query("INSERT INTO api_routed_chats(chat_id,agent_id,source_scope) VALUES($1,$2,$3)")
            .bind(chat).bind(agent_id).bind(&snapshot).execute(&state.pg).await?;
    }
    let scope=Arc::new(Scope {access,chat,agent:agent_id,snapshot});
    let cancel=Arc::new(Notify::new());let turn=Uuid::now_v7();
    let selection=req.model_selection.clone().ok_or_else(||ApiError::bad_request("model_selection is required"))?;
    crate::chat::routing::workflow_scope(state,actor,chat,&content).await?;
    let output=scope.snapshot["agent"]["params"]["max_tokens"].as_u64().unwrap_or(4096).clamp(1,4096) as u32;
    let run=scope.guard(async {
        scope.check_api().await?;
        let run=crate::chat::routing::prepare_selected(state,actor,chat,turn,&content,selection.clone(),
            WorkloadRequirements {output_tokens:output,..Default::default()},&[],&cancel).await?;
        scope.check_api().await?;
        Ok(run)
    }).await?.with_access(scope.clone())?;
    sqlx::query("UPDATE chats SET model_selection=$2 WHERE id=$1").bind(chat).bind(json!(selection)).execute(&state.pg).await?;
    let (tx,rx)=mpsc::channel(256);let (completed,done)=oneshot::channel();
    {
        let state=state.clone();let actor=actor.clone();let run=run.clone();let scope=scope.clone();let cancel=cancel.clone();
        tokio::spawn(async move {
            // Retain the conversation lock through inference, child accounting
            // and cleanup even if the HTTP response has already been dropped.
            let _lease=lease;
            crate::chat::run_turn(&state,crate::chat::origin::TurnContext::web(&actor).with_routing(&run),turn,Some(chat),None,
                Some(agent_id),content,vec![],vec![],true,None,None,None,None,&tx,cancel).await;
            drop(tx);
            let result=finish(&state,&scope,&run).await;
            run.cancel();
            let _=completed.send(result);
        });
    }
    let source=Source {rx,done:Some(done),scope,run,cancel:cancel.clone(),ended:false,citations:vec![]};
    let id=chat::completion_id();let created=chat::now_unix();
    let mut stream=StreamState::new(TurnGuard::new(cancel,None),source,id.clone(),response_model.clone(),created);
    let mut response=if req.stream {sse::stream_response(stream)}
        else {let aggregate=sse::collect(&mut stream).await?;Json(chat::completion(&id,&response_model,created,&aggregate)).into_response()};
    response.headers_mut().insert(chat::CHAT_ID_HEADER,chat.to_string().parse().map_err(|_|ApiError::internal("invalid chat id"))?);
    Ok(response)
}

fn metadata(run:&RoutedRun,completed:bool)->Value {
    let mut decision=run.plan().decision.clone();
    if completed {decision.actual=run.plan().assignments.get(&CallPurpose::Final).cloned();}
    json!({"schema_version":1,"run_id":run.plan().id,"selection":run.plan().selection,"decision":decision,"usage":run.usage()})
}
async fn finish(state:&AppState,scope:&Scope,run:&RoutedRun)->Result<(ml::Usage,Value),ApiError> {
    let result=async {
        run.finish_children().await?;
        scope.check_api().await?;
        let row:Option<(Option<i32>,Option<i32>)>=sqlx::query_as("SELECT prompt_tokens,completion_tokens FROM messages WHERE chat_id=$1 AND turn_id=$2 AND role='assistant' AND completed_at IS NOT NULL AND interrupted_at IS NULL ORDER BY created_at DESC LIMIT 1")
            .bind(scope.chat).bind(run.plan().id).fetch_optional(&state.pg).await?;
        let (prompt_tokens,completion_tokens)=row.ok_or_else(||ApiError::unavailable("agent turn did not complete"))?;
        if !prompt_tokens.zip(completion_tokens).is_some_and(|(p,c)|p>=0 && c>=0 && p.checked_add(c).is_some()) {
            return Err(ApiError::unavailable("agent completion token usage is unavailable"));
        }
        Ok::<_,ApiError>(ml::Usage {prompt_tokens,completion_tokens,reasoning_tokens:None})
    }.await;
    let mut event=crate::audit::AuditEvent::action("api.agent.routed",scope.access.actor.role.as_str());
    event.actor_user_id=scope.access.actor.user_id;event.resource_type=Some("api_completion".into());event.resource_id=Some(run.plan().id);
    if result.is_err() {event.outcome=crate::audit::AuditOutcome::Failure;}
    let mut routing=metadata(run,result.is_ok());
    super::alias::metadata(&mut routing,scope.access.alias.as_ref());
    routing["agent_id"]=json!(scope.agent);
    event.model_agent_traceability=Some(json!({"routing":routing,"agent_id":scope.agent,"chat_id":scope.chat,
        "api_key_id":scope.access.key,"status":if result.is_ok() {"completed"} else {"failed"}}));
    event.token_usage=Some(json!({"routing_usage":run.usage(),"completion":result.as_ref().ok()}));
    crate::audit::append(&state.pg,&event).await?;
    Ok((result?,routing))
}

struct Source {rx:mpsc::Receiver<ServerFrame>,done:Option<oneshot::Receiver<Result<(ml::Usage,Value),ApiError>>>,
    scope:Arc<Scope>,run:RoutedRun,cancel:Arc<Notify>,ended:bool,citations:Vec<Value>}
impl Drop for Source {fn drop(&mut self) {if !self.ended {self.run.cancel();self.cancel.notify_one();}}}
impl Source {
    async fn terminal(&mut self)->Emit {
        let result=match self.done.take() {
            Some(done)=>done.await.unwrap_or_else(|_|Err(ApiError::unavailable("agent worker stopped"))),
            None=>Err(ApiError::unavailable("missing agent outcome")),
        };
        self.ended=true;
        match result {
            Ok((usage,routing))=>match self.scope.check_api().await {
                Ok(())=>Emit::Final {finish_reason:"stop".into(),usage,citations:sse::citations_value(&self.citations),routing:Some(routing)},
                Err(error)=>Emit::Failed(error),
            },
            Err(error)=>Emit::Failed(error),
        }
    }
}
impl sse::EmitSource for Source {
    async fn next(&mut self)->Option<Emit> {
        if self.ended {return None;}
        loop {
            if let Err(error)=self.scope.check_api().await {self.run.cancel();self.cancel.notify_one();self.ended=true;return Some(Emit::Failed(error));}
            let frame=self.rx.recv().await;
            if let Err(error)=self.scope.check_api().await {self.run.cancel();self.cancel.notify_one();self.ended=true;return Some(Emit::Failed(error));}
            match frame {
                Some(ServerFrame::ChatToken {delta,..})=>return Some(Emit::Delta {content:delta,reasoning:String::new()}),
                Some(ServerFrame::ChatReasoning {delta,..})=>return Some(Emit::Delta {content:String::new(),reasoning:delta}),
                Some(ServerFrame::ChatCitations {citations,..})=>self.citations=citations.into_iter().filter_map(|v|serde_json::to_value(v).ok()).collect(),
                Some(ServerFrame::ChatCompleted {..})|Some(ServerFrame::ChatInterrupted {..})|Some(ServerFrame::ChatError {..})|None=>return Some(self.terminal().await),
                Some(ServerFrame::AgentApproval {..})=>{self.run.cancel();self.cancel.notify_one();},
                _=>{},
            }
        }
    }
}
