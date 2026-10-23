// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit non-destructive regeneration. A branch is a new execution, never a
//! resumed tool checkpoint or a reconstructed budget from the previous answer.
use std::{future::Future,pin::Pin,sync::Arc,time::Duration};
use axum::{extract::State,Json};
use serde_json::{json,Value};
use tokio::sync::{mpsc,Notify};
use uuid::Uuid;
use crate::{AppState,Result,auth::{AuthContext,keycloak::AuthUser},model_routing::{*,run::{AccessGuard,RoutedRun}},ws::protocol::ServerFrame};

pub const GATE:&str="routing.regenerate_enabled";
async fn enabled(state:&AppState)->Result<bool> {
    Ok(crate::config::runtime::get(&state.pg,GATE).await?.is_some_and(|v|
        matches!(v.value_type,crate::config::runtime::ConfigValueType::Bool) && v.value=="true"))
}
pub async fn settings(State(state):State<AppState>,AuthUser(_):AuthUser)->Result<Json<Value>> {
    Ok(Json(json!({"schema_version":1,"enabled":enabled(&state).await?,"frame":"chat.routed.regenerate",
        "scope":"web_general_branch_v1","preserves_original":true,"selection":"saved_answer_intent",
        "auto":"reselect_current_policy","manual":"same_provider_current_profile","history_limit":256,
        "history_bytes_limit":1048576,"legacy_intent_inference":false,"edit":false,"attachments":false,
        "voice":false,"api":false,"desktop":true,"desktop_scope":"paired_remote_general_branch_v1","branch_continuation":false})))
}
pub async fn set_enabled(State(state):State<AppState>,AuthUser(actor):AuthUser,
    Json(body):Json<crate::http::voice_routing::Enabled>)->Result<Json<Value>> {
    if actor.break_glass || actor.mfa_enroll_only {return Err(RoutingError::Forbidden.into());}
    state.rbac.require_permission(&state.pg,&actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    crate::config::runtime::set(&state.pg,GATE,if body.enabled {"true"} else {"false"},crate::config::runtime::ConfigValueType::Bool,
        "global",actor.user_id,actor.role.as_str()).await?;
    settings(State(state),AuthUser(actor)).await
}
async fn principal(state:&AppState,actor:&AuthContext)->Result<()> {
    if !enabled(state).await? {return Err(crate::AppError::Unavailable("routed regeneration is disabled".into()));}
    if actor.break_glass || actor.mfa_enroll_only {return Err(RoutingError::Forbidden.into());}
    durable::Context::capture(&RoutingContext {actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::nil(),purpose:CallPurpose::Primary})?
        .validate(&state.pg).await?;Ok(())
}
pub(crate) async fn validate_run(state:&AppState,run:&RoutedRun)->Result<()> {
    let chat=run.context(CallPurpose::Primary).chat_id;
    let branch:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_chat_branches WHERE chat_id=$1)").bind(chat).fetch_one(&state.pg).await?;
    if branch && run.regeneration_chat()!=chat {return Err(RoutingError::Forbidden.into());}Ok(())
}
// Includes rows, citation metadata and explicit links; a concurrent edit to the
// selected prefix invalidates planning/output. Later turns do not enter the fork.
async fn fingerprint(state:&AppState,chat:Uuid,seq:i32)->Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('messages',(SELECT md5(COALESCE(jsonb_agg(to_jsonb(m) ORDER BY sequence_number)::text,'')) FROM messages m WHERE chat_id=$1 AND sequence_number<=$2),'citations',(SELECT md5(COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c.id)::text,'')) FROM citations c JOIN messages m ON m.id=c.message_id WHERE m.chat_id=$1 AND m.sequence_number<=$2),'web',(SELECT md5(COALESCE(jsonb_agg(to_jsonb(c) ORDER BY c.id)::text,'')) FROM web_citations c JOIN messages m ON m.id=c.message_id WHERE m.chat_id=$1 AND m.sequence_number<=$2),'links',(SELECT md5(COALESCE(jsonb_agg(to_jsonb(l) ORDER BY kb_id)::text,'')) FROM chat_kb_links l WHERE chat_id=$1))")
        .bind(chat).bind(seq).fetch_one(&state.pg).await?)
}
async fn supported_history(state:&AppState,chat:Uuid,seq:i32)->Result<()> {
    let (count,bytes):(i64,i64)=sqlx::query_as("SELECT count(*),COALESCE(sum(octet_length(content)),0)::bigint FROM messages WHERE chat_id=$1 AND sequence_number<=$2")
        .bind(chat).bind(seq).fetch_one(&state.pg).await?;
    if count>256 || bytes>1048576 {return Err(RoutingError::Incompatible("regeneration history exceeds the bounded branch limit").into());}
    let durable:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE context->>'chat_id'=$1)").bind(chat.to_string()).fetch_one(&state.pg).await?;
    if durable {return Err(RoutingError::Incompatible("durable output requires its original source witness").into());}
    let unsupported:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM voice_routed_chats WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM api_routed_chats WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM generated_artefacts WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM chat_attachments WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM messages WHERE chat_id=$1 AND sequence_number<=$2 AND role='assistant' AND (routing_selection IS NULL OR completed_at IS NULL OR interrupted_at IS NOT NULL))")
        .bind(chat).bind(seq).fetch_one(&state.pg).await?;
    if unsupported {return Err(RoutingError::Incompatible("history requires unsupported attachments, provenance or missing routed intent").into());}Ok(())
}
pub(crate) async fn cited_sources(state:&AppState,actor:&AuthContext,chat:Uuid,seq:i32)->Result<()> {
    let docs:Vec<(Option<Uuid>,Option<Uuid>)>=sqlx::query_as("SELECT DISTINCT c.doc_id,c.document_id FROM citations c JOIN messages m ON m.id=c.message_id WHERE m.chat_id=$1 AND m.sequence_number<=$2")
        .bind(chat).bind(seq).fetch_all(&state.pg).await?;
    for (kb_doc,document) in docs {
        if let Some(id)=kb_doc {
            let kb:Uuid=sqlx::query_scalar("SELECT kb_id FROM kb_documents WHERE id=$1").bind(id).fetch_one(&state.pg).await?;
            crate::kb::require_read(&state.pg,actor,kb).await?;
            if state.rbac.denied_kb_doc_ids(&state.pg,actor,&[kb]).await?.contains(&id) {return Err(RoutingError::Forbidden.into());}
        }
        if let Some(id)=document {
            let project=crate::documents::project_of(&state.pg,id).await?;
            state.rbac.require_project(&state.pg,actor,project,crate::auth::rbac::Permission::Read).await?;
            if state.rbac.document_access(&state.pg,actor,project,id).await?==crate::ext::DocAccess::Denied {return Err(RoutingError::Forbidden.into());}
        }
    }Ok(())
}
struct Scope {state:AppState,actor:AuthContext,surface:super::browser::Surface,source:Uuid,branch:Uuid,seq:i32,snapshot:Value,prefix:Value,tx:mpsc::Sender<ServerFrame>,intent:ModelSelection,options:Option<fosnie_protocol::RoutingOptions>,vnext:Option<fosnie_protocol::routing_vnext::RoutingIntentV2>}
#[async_trait::async_trait]
impl AccessGuard for Scope {
    fn regeneration_chat(&self)->Option<Uuid> {Some(self.branch)}
    fn desktop_device(&self)->Option<Uuid> {self.surface.device()}
    fn selection_intent(&self)->Option<ModelSelection> {Some(self.intent.clone())}
    fn routing_intent_vnext(&self)->Option<fosnie_protocol::routing_vnext::RoutingIntentV2>{self.vnext.clone()}
    fn routing_options(&self)->Option<fosnie_protocol::RoutingOptions> {self.options.clone()}
    async fn check(&self)->Result<()> {
        if self.tx.is_closed() {return Err(RoutingError::Cancelled.into());}
        principal(&self.state,&self.actor).await?;
        self.surface.check(&self.state,&self.actor).await?;
        supported_history(&self.state,self.source,self.seq).await?;
        cited_sources(&self.state,&self.actor,self.source,self.seq).await?;
        if self.surface.capture(&self.state,&self.actor,self.source).await?!=self.snapshot
            || self.surface.capture(&self.state,&self.actor,self.branch).await?!=self.snapshot
            || fingerprint(&self.state,self.source,self.seq).await?!=self.prefix {return Err(RoutingError::Forbidden.into());}
        Ok(())
    }
}
impl Scope {
    async fn guard<T>(&self,cancel:&Notify,future:impl Future<Output=Result<T>>)->Result<T> {
        self.check().await?;tokio::pin!(future);
        let mut ticks=tokio::time::interval(Duration::from_millis(250));ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {tokio::select! {biased;
            _=cancel.notified()=>return Err(RoutingError::Cancelled.into()),
            _=self.tx.closed()=>return Err(RoutingError::Cancelled.into()),
            _=ticks.tick()=>self.check().await?,
            result=&mut future=>{self.check().await?;return result;}
        }}
    }
}
/// Remote text callers only. No caller-supplied selection, content, target, context,
/// checkpoint or budget is accepted. Empty failed branches remain inspectable.
pub fn execute(state:AppState,actor:AuthContext,source:Uuid,answer:Uuid,turn:Uuid,tx:mpsc::Sender<ServerFrame>,cancel:Arc<Notify>)
    ->Pin<Box<dyn Future<Output=Result<Uuid>>+Send>> {
    execute_surface(state,actor,source,answer,turn,tx,cancel,super::browser::Surface::Web)
}
pub fn execute_surface(state:AppState,actor:AuthContext,source:Uuid,answer:Uuid,turn:Uuid,tx:mpsc::Sender<ServerFrame>,cancel:Arc<Notify>,surface:super::browser::Surface)
    ->Pin<Box<dyn Future<Output=Result<Uuid>>+Send>> {
    Box::pin(async move {
        principal(&state,&actor).await?;
        surface.check(&state,&actor).await?;
        let snapshot=surface.capture(&state,&actor,source).await?;
        let saved:Option<Value>=sqlx::query_scalar("SELECT source_scope FROM routed_chat_branches WHERE chat_id=$1").bind(source).fetch_optional(&state.pg).await?;
        if saved.is_some_and(|v|v!=snapshot) {return Err(RoutingError::Forbidden.into());}
        let row:Option<(i32,Value,Option<Value>)>=sqlx::query_as("SELECT sequence_number,routing_selection,routing_options FROM messages WHERE id=$1 AND chat_id=$2 AND role='assistant' AND routing_selection IS NOT NULL AND routing_decision IS NOT NULL AND completed_at IS NOT NULL AND interrupted_at IS NULL")
            .bind(answer).bind(source).fetch_optional(&state.pg).await?;
        let (seq,intent,options)=row.ok_or(RoutingError::Incompatible("select a completed routed answer with saved intent"))?;
        let vnext:Option<fosnie_protocol::routing_vnext::RoutingIntentV2>=if intent.get("schema_version").is_some(){Some(serde_json::from_value(intent.clone()).map_err(|_|RoutingError::Invalid("saved v2 intent"))?)}else{None};
        if let Some(v)=&vnext {v.require_support(&[1,2],&[1,2]).map_err(RoutingError::Invalid)?;}
        let selection:ModelSelection=if let Some(v)=&vnext {v.selection.clone()}else{serde_json::from_value(intent.clone()).map_err(|_|RoutingError::Invalid("saved selection"))?};
        let options:Option<fosnie_protocol::RoutingOptions>=options.map(serde_json::from_value).transpose().map_err(|_|RoutingError::Invalid("saved routing options"))?;
        let state=if let Some(options)=&options {super::browser::constrain(state,options)} else {state};
        supported_history(&state,source,seq).await?;
        cited_sources(&state,&actor,source,seq).await?;
        let prefix=fingerprint(&state,source,seq).await?;
        let (anchor,content):(i32,String)=sqlx::query_as("SELECT sequence_number,content FROM messages WHERE chat_id=$1 AND sequence_number<$2 AND role='user' ORDER BY sequence_number DESC LIMIT 1")
            .bind(source).bind(seq).fetch_one(&state.pg).await?;
        Box::pin(super::routing::workflow_scope(&state,&actor,source,&content)).await?;
        // The source is copied in a repeatable-read transaction. The guard checks
        // its fingerprint after commit before the classifier can see any text.
        let branch=Uuid::now_v7();let mut create=state.pg.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ").execute(&mut *create).await?;
        sqlx::query("INSERT INTO chats(id,owner_user_id,title,project_id,agent_id,origin,model_selection,routing_options) SELECT $2,owner_user_id,'Regenerated answer',project_id,agent_id,$5,$3,$4 FROM chats WHERE id=$1")
            .bind(source).bind(branch).bind(intent).bind(options.as_ref().map(|o|json!(o)).unwrap_or_else(||json!({}))).bind(surface.origin().as_str()).execute(&mut *create).await?;
        sqlx::query("INSERT INTO chat_kb_links(chat_id,kb_id,attached_by) SELECT $2,kb_id,attached_by FROM chat_kb_links WHERE chat_id=$1")
            .bind(source).bind(branch).execute(&mut *create).await?;
        let history:Vec<(Uuid,Value)>=sqlx::query_as("SELECT id,to_jsonb(m) FROM messages m WHERE chat_id=$1 AND sequence_number<$2 ORDER BY sequence_number")
            .bind(source).bind(anchor).fetch_all(&mut *create).await?;
        for (old,mut row) in history {
            let id=Uuid::now_v7();row["id"]=json!(id);row["chat_id"]=json!(branch);row["turn_id"]=Value::Null;
            sqlx::query("INSERT INTO messages SELECT (jsonb_populate_record(NULL::messages,$1)).*").bind(row).execute(&mut *create).await?;
            sqlx::query("INSERT INTO citations SELECT (jsonb_populate_record(NULL::citations,to_jsonb(c)||jsonb_build_object('id',gen_random_uuid(),'message_id',$2::uuid))).* FROM citations c WHERE message_id=$1")
                .bind(old).bind(id).execute(&mut *create).await?;
            sqlx::query("INSERT INTO web_citations SELECT (jsonb_populate_record(NULL::web_citations,to_jsonb(c)||jsonb_build_object('id',gen_random_uuid(),'message_id',$2::uuid,'turn_id',gen_random_uuid()))).* FROM web_citations c WHERE message_id=$1")
                .bind(old).bind(id).execute(&mut *create).await?;
        }
        sqlx::query("INSERT INTO routed_chat_branches(chat_id,source_chat_id,source_message_id,source_scope) VALUES($1,$2,$3,$4)")
            .bind(branch).bind(source).bind(answer).bind(&snapshot).execute(&mut *create).await?;
        let mut event=crate::audit::AuditEvent::action("chat.routing.regeneration_admitted",actor.role.as_str());event.actor_user_id=actor.user_id;
        event.resource_type=Some("chat".into());event.resource_id=Some(branch);
        event.model_agent_traceability=Some(json!({"turn_id":turn,"source_chat_id":source,"source_message_id":answer,"origin":surface.origin().as_str(),"device_id":surface.device(),"device_key_id":match surface {super::browser::Surface::Desktop(c)=>Some(c.key_id),_=>None}}));
        crate::audit::append_with(&mut create,&event).await?;
        create.commit().await?;
        let scope=Arc::new(Scope {state:state.clone(),actor:actor.clone(),surface,source,branch,seq,snapshot,prefix,tx:tx.clone(),intent:selection.clone(),options:options.clone(),vnext:vnext.clone()});
        scope.check().await?;
        // Announce before planning, so a failed attempt is still discoverable.
        scope.guard(&cancel,async {tokio::time::timeout(Duration::from_secs(5),tx.send(ServerFrame::ChatCreated {chat_id:branch})).await
            .map_err(|_|RoutingError::Cancelled)?.map_err(|_|RoutingError::Cancelled.into())}).await?;
        scope.guard(&cancel,async {tokio::time::timeout(Duration::from_secs(5),tx.send(ServerFrame::ChatRoutingStarted {turn_id:turn,chat_id:branch})).await
            .map_err(|_|RoutingError::Cancelled)?.map_err(|_|RoutingError::Cancelled.into())}).await?;
        let requirements=WorkloadRequirements {output_tokens:if options.is_some(){1024}else{4096},effort:options.as_ref().and_then(|o|o.effort.clone()),..Default::default()};
        let run=scope.guard(&cancel,async {match vnext {
            Some(v)=>super::routing::prepare_vnext(&state,&actor,branch,turn,&content,v,requirements,&[],&Notify::new()).await,
            None=>super::routing::prepare_selected(&state,&actor,branch,turn,&content,selection,requirements,&[],&Notify::new()).await}}).await?.with_access(scope.clone())?;
        let (out,mut rx)=mpsc::channel(64);
        let worker=tokio::spawn({let state=state.clone();let actor=actor.clone();let run=run.clone();let cancel=cancel.clone();async move {
            Box::pin(super::run_turn(&state,super::origin::TurnContext::new(&actor,surface.origin()).with_device(surface.device()).with_routing(&run),turn,Some(branch),None,None,content,
                vec![],vec![],false,None,None,None,None,&out,cancel)).await;
        }});
        let result=scope.guard(&Notify::new(),async {
            while let Some(frame)=rx.recv().await {
                scope.check().await?;
                tokio::time::timeout(Duration::from_secs(5),tx.send(frame)).await.map_err(|_|RoutingError::Cancelled)?
                    .map_err(|_|RoutingError::Cancelled)?;
            }Ok(())
        }).await;
        if result.is_err() {run.cancel();}
        drop(rx); // Wake blocked sends so the worker can finish its own cleanup.
        worker.await.map_err(|_|RoutingError::Cancelled)?;
        run.finish_children().await?;result?;scope.check().await?;Ok(branch)
    })
}
