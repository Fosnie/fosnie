// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Capability-discovered remote text entry. Selection is intent, never a client plan.
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use axum::{extract::{Path,State},Json};
use serde_json::{json,Value};
use tokio::sync::{mpsc,Notify};
use uuid::Uuid;
use fosnie_protocol::{RoutedChatRequest,RoutingOptions};
use crate::{AppState,Result,auth::{AuthContext,keycloak::AuthUser},model_routing::{*,run::{AccessGuard,RoutedRun}},ws::protocol::ServerFrame};
pub(crate) const OUTPUT_TOKENS:u32=1024;
pub const GATE:&str="routing.browser_chat_enabled";
/// Authentication-owned surface, absent from the client request/wire schema.
#[derive(Clone,Copy)]
pub enum Surface {Web,Desktop(crate::auth::device::DeviceCredential)}
impl Surface {
    pub fn authenticated(origin:super::origin::ChatOrigin,device:Option<Uuid>,key:Option<Uuid>)->Result<Self> {
        match (origin,device,key) {
            (super::origin::ChatOrigin::Web,None,None)=>Ok(Self::Web),
            (super::origin::ChatOrigin::Desktop,Some(device_id),Some(key_id))=>Ok(Self::Desktop(crate::auth::device::DeviceCredential {device_id,key_id})),
            _=>Err(RoutingError::Incompatible("remote routing requires an original session or device credential; reconnect the desktop to obtain a fresh ticket").into()),
        }
    }
    pub(crate) fn origin(self)->super::origin::ChatOrigin {match self {Self::Web=>super::origin::ChatOrigin::Web,Self::Desktop(_)=>super::origin::ChatOrigin::Desktop}}
    pub(crate) fn device(self)->Option<Uuid> {match self {Self::Desktop(c)=>Some(c.device_id),Self::Web=>None}}
    pub(crate) async fn check(self,state:&AppState,actor:&AuthContext)->Result<()> {if let Self::Desktop(c)=self {c.validate(state,actor).await?;}Ok(())}
    pub(crate) fn capture<'a>(self,state:&'a AppState,actor:&'a AuthContext,chat:Uuid)->Pin<Box<dyn Future<Output=Result<Value>>+Send+'a>> {
        crate::voice::routed::capture_text(state,actor,chat,self.origin())
    }
}
pub async fn enabled(state:&AppState)->Result<bool> {
    Ok(crate::config::runtime::get(&state.pg,GATE).await?.is_some_and(|v|
        matches!(v.value_type,crate::config::runtime::ConfigValueType::Bool) && v.value=="true"))
}
pub async fn settings(State(state):State<AppState>,AuthUser(_):AuthUser)->Result<Json<Value>> {
    Ok(Json(json!({"schema_version":1,"enabled":enabled(&state).await?,"frame":"chat.routed.send",
        "selection_modes":["default","manual","auto"],"selection_versions":[1,2],"profile_versions":[1,2],"v2_frame":"chat.routed.v2.send","scope":"owner_web_general_v1",
        "local_only":true,"effort":true,"attachments":false,"desktop":true,"desktop_scope":"paired_remote_general_v1",
        "workspace":false,"desktop_credential":"original_key_ticket_v1","legacy_fallback":false,
        "output_tokens":1024,"local_only_web":"inference_only; source access still requires tool permission"})))
}
pub async fn set_enabled(State(state):State<AppState>,AuthUser(actor):AuthUser,
    Json(body):Json<crate::http::voice_routing::Enabled>)->Result<Json<Value>> {
    if actor.break_glass || actor.mfa_enroll_only {return Err(RoutingError::Forbidden.into());}
    state.rbac.require_permission(&state.pg,&actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    crate::config::runtime::set(&state.pg,GATE,if body.enabled {"true"} else {"false"},crate::config::runtime::ConfigValueType::Bool,
        "global",actor.user_id,actor.role.as_str()).await?;
    settings(State(state),AuthUser(actor)).await
}
pub async fn saved(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(chat):Path<Uuid>)->Result<Json<Value>> {
    saved_for(&state,&actor,chat,super::origin::ChatOrigin::Web).await.map(Json)
}
pub async fn saved_surface(State(state):State<AppState>,AuthUser(actor):AuthUser,crate::auth::device::MaybeDevice(device):crate::auth::device::MaybeDevice,Path(chat):Path<Uuid>)->Result<Json<Value>> {
    saved_for(&state,&actor,chat,super::origin::ChatOrigin::from_device(device)).await.map(Json)
}
async fn saved_for(state:&AppState,actor:&AuthContext,chat:Uuid,surface:super::origin::ChatOrigin)->Result<Value> {
    let row:Option<(Value,Value,String,String)>=sqlx::query_as("SELECT model_selection,routing_options,origin,mode FROM chats WHERE id=$1 AND owner_user_id=$2 AND archived_at IS NULL")
        .bind(chat).bind(actor.user_id).fetch_optional(&state.pg).await?;
    let (selection,options,origin,mode)=row.ok_or(RoutingError::Forbidden)?;
    let adopted=crate::model_routing::admission::chat_requires_routing(&state,chat).await?;
    let unsupported:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM voice_routed_chats WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM api_routed_chats WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM routed_chat_branches WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM chat_workspace WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM chat_attachments WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM routed_jobs WHERE context->>'chat_id'=$1::text)")
        .bind(chat).fetch_one(&state.pg).await?;
    Ok(json!({"schema_version":1,"selection":selection,"options":options,"adopted":adopted,
        "supported":origin==surface.as_str() && mode=="general" && !unsupported}))
}
async fn principal(state:&AppState,actor:&AuthContext)->Result<()> {
    if !enabled(state).await? {return Err(crate::AppError::Unavailable("browser model routing is disabled".into()));}
    crate::model_routing::durable::Context::capture(&RoutingContext {actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::nil(),purpose:CallPurpose::Primary})?
        .validate(&state.pg).await
}
pub(crate) async fn validate_run(state:&AppState,run:&RoutedRun)->Result<()> {
    let chat=run.context(CallPurpose::Primary).chat_id;
    let adopted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM browser_routed_chats WHERE chat_id=$1)").bind(chat).fetch_one(&state.pg).await?;
    if adopted && run.browser_chat()!=chat {return Err(RoutingError::Forbidden.into());}Ok(())
}
struct Narrow {parent:Arc<dyn InferencePolicy>,options:RoutingOptions}
#[async_trait::async_trait]
impl InferencePolicy for Narrow {
    fn single_candidate_fast_path(&self)->bool {true}
    async fn constraints(&self,pool:&sqlx::PgPool,context:&RoutingContext)->Result<RoutingConstraints> {
        Ok(self.parent.constraints(pool,context).await?.intersect(&RoutingConstraints {
            local_only:self.options.local_only,..Default::default()}))
    }
}
pub(crate) fn constrain(mut state:AppState,options:&RoutingOptions)->AppState {
    state.inference_policy=Arc::new(Narrow {parent:state.inference_policy.clone(),options:options.clone()});state
}
struct Scope {state:AppState,actor:AuthContext,chat:Uuid,surface:Surface,snapshot:Value,intent:ModelSelection,options:RoutingOptions,tx:mpsc::Sender<ServerFrame>,plain_text_retry:bool,vnext:Option<fosnie_protocol::routing_vnext::RoutingIntentV2>}
#[async_trait::async_trait]
impl AccessGuard for Scope {
    fn browser_chat(&self)->Option<Uuid> {Some(self.chat)}
    fn plain_text_retry(&self)->bool {self.plain_text_retry}
    fn desktop_device(&self)->Option<Uuid> {self.surface.device()}
    fn selection_intent(&self)->Option<ModelSelection> {Some(self.intent.clone())}
    fn routing_intent_vnext(&self)->Option<fosnie_protocol::routing_vnext::RoutingIntentV2>{self.vnext.clone()}
    fn routing_options(&self)->Option<RoutingOptions> {Some(self.options.clone())}
    async fn check(&self)->Result<()> {
        if self.tx.is_closed() {return Err(RoutingError::Cancelled.into());}
        principal(&self.state,&self.actor).await?;
        self.surface.check(&self.state,&self.actor).await?;
        if self.surface.capture(&self.state,&self.actor,self.chat).await?!=self.snapshot {return Err(RoutingError::Forbidden.into());}
        super::regenerate::cited_sources(&self.state,&self.actor,self.chat,i32::MAX).await?;
        Ok(())
    }
}
impl Scope {
    async fn guard<T>(&self,cancel:&Notify,future:impl Future<Output=Result<T>>)->Result<T> {
        self.check().await?;tokio::pin!(future);
        let mut ticks=tokio::time::interval(Duration::from_millis(250));ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {tokio::select! {biased;
            _=cancel.notified()=>return Err(RoutingError::Cancelled.into()),
            _=ticks.tick()=>self.check().await?,
            result=&mut future=>{self.check().await?;return result;}
        }}
    }
}
pub fn execute(state:AppState,actor:AuthContext,request:RoutedChatRequest,turn:Uuid,tx:mpsc::Sender<ServerFrame>,cancel:Arc<Notify>)
    ->Pin<Box<dyn Future<Output=Result<Uuid>>+Send>> {
    execute_surface(state,actor,request,turn,tx,cancel,Surface::Web)
}
pub fn execute_surface(state:AppState,actor:AuthContext,request:RoutedChatRequest,turn:Uuid,tx:mpsc::Sender<ServerFrame>,cancel:Arc<Notify>,surface:Surface)
    ->Pin<Box<dyn Future<Output=Result<Uuid>>+Send>> {
    execute_surface_in(state,actor,request,turn,tx,cancel,surface,1,None,true)
}
pub fn execute_vnext(state:AppState,actor:AuthContext,request:fosnie_protocol::routing_vnext::RoutedChatRequestV2,turn:Uuid,tx:mpsc::Sender<ServerFrame>,cancel:Arc<Notify>,surface:Surface)
    ->Pin<Box<dyn Future<Output=Result<Uuid>>+Send>> {
    let old=RoutedChatRequest{content:request.content,chat_id:request.chat_id,project_id:request.project_id,agent_id:request.agent_id,model_selection:None,options:request.options};
    execute_surface_in(state,actor,old,turn,tx,cancel,surface,2,request.routing_intent,request.save_binding)
}
fn execute_surface_in(state:AppState,actor:AuthContext,request:RoutedChatRequest,turn:Uuid,tx:mpsc::Sender<ServerFrame>,cancel:Arc<Notify>,surface:Surface,
    version:u32,mut vnext:Option<fosnie_protocol::routing_vnext::RoutingIntentV2>,save_binding:bool)->Pin<Box<dyn Future<Output=Result<Uuid>>+Send>> {
    Box::pin(async move {
        if vnext.is_some() && request.options.is_some() {return Err(RoutingError::Invalid("provide options in the explicit intent or in an inherited request, not both").into());}
        principal(&state,&actor).await?;
        surface.check(&state,&actor).await?;
        if request.content.trim().is_empty() || request.content.len()>262144 {return Err(RoutingError::Invalid("empty or oversized routed message").into());}
        let explicit_vnext=vnext.is_some();
        let created=request.chat_id.is_none();let chat=request.chat_id.unwrap_or_else(Uuid::now_v7);
        // The lease spans planning, output relay and worker cleanup. Row locks
        // below are committed first so title tasks can update the conversation.
        let mut lease=state.pg.begin().await?;
        let locked:bool=sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))").bind(format!("browser-chat:{chat}"))
            .fetch_one(&mut *lease).await?;
        if !locked {return Err(crate::AppError::Conflict("another routed turn owns this conversation".into()));}
        let mut adopt=state.pg.begin().await?;
        if created {
            if let Some(project)=request.project_id {state.rbac.require_project(&state.pg,&actor,project,crate::auth::rbac::Permission::Read).await?;}
            if let Some(agent)=request.agent_id {
                let cfg=crate::workflows::agents::capture(&mut *adopt,agent).await?;
                if cfg.is_null() || (!actor.is_admin() && !cfg["created_by"].is_null() && cfg["created_by"]!=json!(actor.user_id))
                    || !cfg["modes"].as_array().is_some_and(|m|m.iter().any(|m|m=="general")) {return Err(RoutingError::Forbidden.into());}
            }
            sqlx::query("INSERT INTO chats(id,owner_user_id,title,project_id,agent_id,origin) VALUES($1,$2,'New chat',$3,$4,$5)")
                .bind(chat).bind(actor.user_id).bind(request.project_id).bind(request.agent_id).bind(surface.origin().as_str()).execute(&mut *adopt).await?;
        }
        let row:Option<(Value,Value,Option<Uuid>,Option<Uuid>,String,String)>=sqlx::query_as("SELECT model_selection,routing_options,project_id,agent_id,origin,mode FROM chats WHERE id=$1 AND owner_user_id=$2 AND archived_at IS NULL FOR UPDATE")
            .bind(chat).bind(actor.user_id).fetch_optional(&mut *adopt).await?;
        let (saved,options,project,agent,origin,mode)=row.ok_or(RoutingError::Forbidden)?;
        if origin!=surface.origin().as_str() || mode!="general" || request.project_id.is_some_and(|p|Some(p)!=project) || request.agent_id.is_some_and(|a|Some(a)!=agent) {
            return Err(RoutingError::Incompatible("routed remote chat requires its original surface and general scope").into());
        }
        let unsupported:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM voice_routed_chats WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM api_routed_chats WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM routed_chat_branches WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM chat_workspace WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM chat_attachments WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM routed_jobs WHERE context->>'chat_id'=$1::text) OR EXISTS(SELECT 1 FROM messages WHERE chat_id=$1 AND role='assistant' AND completed_at IS NULL AND interrupted_at IS NULL)")
            .bind(chat).fetch_one(&mut *adopt).await?;
        if unsupported {return Err(RoutingError::Incompatible("conversation provenance or pending turn is not supported by browser routing").into());}
        if version==2 && vnext.is_none() && created {
            if let Some(id)=project {vnext=crate::model_routing::vnext::bindings::project(&state,&actor,id).await?.routing_intent;}
        }
        if version==2 && vnext.is_none() {
            vnext=Some(if saved.get("schema_version").is_some(){serde_json::from_value(saved.clone()).map_err(|_|RoutingError::Invalid("saved v2 intent"))?}
                else {fosnie_protocol::routing_vnext::RoutingIntentV2{schema_version:2,required_profile_version:1,
                    selection:serde_json::from_value(saved.clone()).map_err(|_|RoutingError::Invalid("saved selection"))?,variant:None,options:None}});
        }
        if let Some(v)=&vnext {v.require_support(&[1,2],&[1,2]).map_err(RoutingError::Invalid)?;}
        let intent=if let Some(v)=&vnext {v.selection.clone()} else {match request.model_selection {Some(intent)=>intent,None=>serde_json::from_value(saved).map_err(|_|RoutingError::Invalid("saved selection"))?}};
        let options=match request.options.or_else(||vnext.as_ref().and_then(|v|v.options.clone())) {Some(options)=>options,None=>serde_json::from_value(options).map_err(|_|RoutingError::Invalid("saved routing options"))?};
        if options.effort.as_ref().is_some_and(|e|e.trim().is_empty() || e.len()>24) {return Err(RoutingError::Invalid("invalid effort").into());}
        if let Some(v)=&mut vnext {v.options=Some(options.clone());}
        if save_binding || (created && version==2 && !explicit_vnext && vnext.is_some()) {let saved_intent=vnext.as_ref().map(|v|json!(v)).unwrap_or_else(||json!(intent));
        sqlx::query("UPDATE chats SET model_selection=$2,routing_options=$3 WHERE id=$1").bind(chat).bind(saved_intent).bind(json!(options)).execute(&mut *adopt).await?;}
        sqlx::query("INSERT INTO browser_routed_chats(chat_id) VALUES($1) ON CONFLICT DO NOTHING").bind(chat).execute(&mut *adopt).await?;
        let mut event=crate::audit::AuditEvent::action("chat.routing.admitted",actor.role.as_str());event.actor_user_id=actor.user_id;
        event.resource_type=Some("chat".into());event.resource_id=Some(chat);
        event.model_agent_traceability=Some(json!({"turn_id":turn,"origin":surface.origin().as_str(),"device_id":surface.device(),"device_key_id":match surface {Surface::Desktop(c)=>Some(c.key_id),_=>None}}));
        crate::audit::append_with(&mut adopt,&event).await?;
        adopt.commit().await?;
        if created {tokio::time::timeout(Duration::from_secs(5),tx.send(ServerFrame::ChatCreated {chat_id:chat})).await
            .map_err(|_|RoutingError::Cancelled)?.map_err(|_|RoutingError::Cancelled)?;}
        let state=constrain(state,&options);
        let snapshot=surface.capture(&state,&actor,chat).await?;
        let agent_id=snapshot["agent_id"].as_str().and_then(|s|s.parse::<Uuid>().ok());
        let plain_text_retry=super::load_agent(&state.pg,agent_id).await?.tools.iter().all(|t|t=="generate_artefact")
            && snapshot["libraries"].as_array().is_some_and(|v|v.is_empty()) && !super::wants_artefact(&request.content);
        if let Some(v)=&mut vnext {v.options=Some(options.clone());}
        let scope=Arc::new(Scope {state:state.clone(),actor:actor.clone(),chat,surface,snapshot,intent:intent.clone(),options:options.clone(),tx:tx.clone(),plain_text_retry,vnext:vnext.clone()});
        scope.guard(&cancel,async {tokio::time::timeout(Duration::from_secs(5),tx.send(ServerFrame::ChatRoutingStarted {turn_id:turn,chat_id:chat})).await
            .map_err(|_|RoutingError::Cancelled)?.map_err(|_|RoutingError::Cancelled.into())}).await?;
        let requirements=WorkloadRequirements {output_tokens:OUTPUT_TOKENS,effort:options.effort.clone(),..Default::default()};
        let planned=scope.guard(&cancel,async {match vnext {
            Some(mut v)=>{v.options=Some(options.clone());super::routing::prepare_vnext(&state,&actor,chat,turn,&request.content,v,requirements,&[],&cancel).await},
            None=>super::routing::prepare_selected(&state,&actor,chat,turn,&request.content,intent,requirements,&[],&cancel).await}}).await;
        let run=match planned {
            Ok(run)=>run.with_access(scope.clone())?,
            Err(error)=>{
                // Retain an authorised submitted question even when planning
                // failed before the normal turn worker created its message rows.
                if scope.check().await.is_ok() {
                    let sequence:i32=sqlx::query_scalar("SELECT COALESCE(MAX(sequence_number),0)+1 FROM messages WHERE chat_id=$1").bind(chat).fetch_one(&state.pg).await?;
                    let failure=crate::model_routing::failure::public(&error);
                    sqlx::query("INSERT INTO messages(id,chat_id,role,sequence_number,content,turn_id,routing_selection,routing_options,routing_failure,interrupted_at) VALUES($1,$2,'user',$3,$4,$5,$6,$7,NULL,NULL),($8,$2,'assistant',$3+1,'',$5,$6,$7,$9,now())")
                        .bind(Uuid::now_v7()).bind(chat).bind(sequence).bind(&request.content).bind(turn)
                        .bind(serde_json::to_value(scope.vnext.as_ref().map(|v|json!(v)).unwrap_or_else(||json!(scope.intent))).map_err(|_|RoutingError::Invalid("selection serialization"))?)
                        .bind(serde_json::to_value(&scope.options).map_err(|_|RoutingError::Invalid("options serialization"))?)
                        .bind(Uuid::now_v7()).bind(failure).execute(&state.pg).await?;
                    crate::model_routing::failure::notify_chat(&state,turn,Some(chat),&error,&tx,false).await;
                }
                return Err(error);
            }
        };
        let (out,mut rx)=mpsc::channel(32);
        let worker=tokio::spawn({let state=state.clone();let actor=actor.clone();let run=run.clone();let cancel=cancel.clone();async move {
            Box::pin(super::run_turn(&state,super::origin::TurnContext::new(&actor,surface.origin()).with_device(surface.device()).with_routing(&run),turn,Some(chat),None,None,request.content,
                vec![],vec![],false,None,None,None,None,&out,cancel)).await;
        }});
        let result=scope.guard(&Notify::new(),async {
            while let Some(frame)=rx.recv().await {
                scope.check().await?;
                tokio::time::timeout(Duration::from_secs(5),tx.send(frame)).await.map_err(|_|RoutingError::Cancelled)?.map_err(|_|RoutingError::Cancelled)?;
            }Ok(())
        }).await;
        if result.is_err() {run.cancel();}drop(rx);
        worker.await.map_err(|_|RoutingError::Cancelled)?;
        run.finish_children().await?;
        result?;scope.check().await?;lease.commit().await?;Ok(chat)
    })
}

/// Only expose persisted actual dispatch, never a shadow proposal, provider
/// credentials/labels, exclusions or another owner's private catalog.
pub async fn message_metadata(state:&AppState,actor:&AuthContext,ids:&[Uuid])->Result<std::collections::HashMap<Uuid,Value>> {
    let visible:std::collections::HashSet<_>=store::catalog(state,actor).await?.into_iter().map(|p|p.id).collect();
    let rows:Vec<(Uuid,Option<Value>,Option<Value>,Option<Value>,bool,bool,Option<Value>,Option<Value>)>=sqlx::query_as("SELECT id,routing_selection,routing_decision,routing_options,completed_at IS NOT NULL,interrupted_at IS NOT NULL,routing_failure,routing_attempts FROM messages WHERE id=ANY($1) AND chat_id IN(SELECT id FROM chats WHERE owner_user_id=$2 AND archived_at IS NULL)")
        .bind(ids).bind(actor.user_id).fetch_all(&state.pg).await?;
    let mut metadata=std::collections::HashMap::new();
    for (id,intent,decision,options,completed,interrupted,failure,attempts) in rows {
        let Some(intent)=intent.and_then(|v|if v.get("schema_version").is_some(){serde_json::from_value::<fosnie_protocol::routing_vnext::RoutingIntentV2>(v).ok().map(|v|v.selection)}else{serde_json::from_value::<ModelSelection>(v).ok()}) else {continue;};
        let decision=decision.and_then(|v|serde_json::from_value::<RoutingDecision>(v).ok());
        let actual=decision.as_ref().and_then(|d|d.actual.as_ref());
        let readable=actual.is_some_and(|a|visible.contains(&a.provider_id));
        metadata.insert(id,json!({"schema_version":1,"selection_mode":match intent {ModelSelection::Auto {..}=>"auto",ModelSelection::Manual {..}=>"manual",_=>"default"},
            "actual":actual.filter(|_|readable).map(|a|json!({"model":a.model,"effort":a.effort})),"actual_unavailable":actual.is_some() && !readable,
            "local_only":options.as_ref().is_some_and(|o|o["local_only"]==true),
            "reason_codes":decision.as_ref().map(|d|d.reason_codes.clone()).unwrap_or_default(),
            "failure":failure,"attempts":attempts.and_then(|v|v.as_array().cloned()).map(|rows|rows.into_iter().map(|a|{
                let readable=a["failed_target"]["provider_id"].as_str().and_then(|s|s.parse::<Uuid>().ok()).is_some_and(|id|visible.contains(&id));
                json!({"failure":a["failure"],"model":if readable {a["failed_target"]["model"].clone()}else{Value::Null}})
            }).collect::<Vec<_>>()),
            "status":if interrupted {"interrupted"} else if completed {"completed"} else {"planning_or_streaming"}}));
    }Ok(metadata)
}
