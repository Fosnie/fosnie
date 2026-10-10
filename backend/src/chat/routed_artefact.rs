// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Request-owned draft export: deterministic rendering, consumed attempt before
//! side effects, no model naming call and no automatic resume/replay.
use std::{future::Future,time::Duration};
use axum::{extract::State,Json};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use uuid::Uuid;
use crate::{AppState,Result,auth::{AuthContext,keycloak::AuthUser},model_routing::{*,run::RoutedRun}};
pub const GATE:&str="routing.artefacts_enabled";
const MAX_BYTES:usize=4*1024*1024;
fn hash(bytes:&[u8])->String {Sha256::digest(bytes).iter().map(|b|format!("{b:02x}")).collect()}
async fn enabled(state:&AppState)->Result<bool> {
    Ok(crate::config::runtime::get(&state.pg,GATE).await?.is_some_and(|v|
        matches!(v.value_type,crate::config::runtime::ConfigValueType::Bool) && v.value=="true"))
}
pub async fn settings(State(state):State<AppState>,AuthUser(_):AuthUser)->Result<Json<Value>> {
    Ok(Json(json!({"schema_version":1,"enabled":enabled(&state).await?,"scope":"non_agentic_draft_export_v1",
        "formats":["md","docx","pdf"],"title":"deterministic","renderer_inference":false,"renderer_egress":false,
        "max_output_bytes":MAX_BYTES,"automatic_retry":false,"approval_resume":false,"voice":false,"durable":false})))
}
pub async fn set_enabled(State(state):State<AppState>,AuthUser(actor):AuthUser,
    Json(body):Json<crate::http::voice_routing::Enabled>)->Result<Json<Value>> {
    if actor.break_glass || actor.mfa_enroll_only {return Err(RoutingError::Forbidden.into());}
    state.rbac.require_permission(&state.pg,&actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    crate::config::runtime::set(&state.pg,GATE,if body.enabled {"true"} else {"false"},crate::config::runtime::ConfigValueType::Bool,
        "global",actor.user_id,actor.role.as_str()).await?;
    settings(State(state),AuthUser(actor)).await
}
pub(crate) struct Intent {state:AppState,actor:AuthContext,chat:Uuid,kind:&'static str,prompt:String,snapshot:Value}
impl Intent {
    pub(crate) fn capture<'a>(state:&'a AppState,actor:&'a AuthContext,chat:Uuid,prompt:&'a str)->std::pin::Pin<Box<dyn Future<Output=Result<Self>>+Send+'a>> {
        Box::pin(async move {
        let kind=super::artefact_kind(prompt);
        if !matches!(kind,"md"|"docx"|"pdf") {return Err(RoutingError::Incompatible("routed draft export supports Markdown, DOCX and PDF").into());}
        let snapshot=crate::voice::routed::capture(state,actor,chat).await?;
        let intent=Self {state:state.clone(),actor:actor.clone(),chat,kind,prompt:prompt.into(),snapshot};
        intent.check().await?;Ok(intent)
        })
    }
    pub(crate) fn matches(&self,prompt:&str)->bool {self.prompt==prompt}
    pub(crate) fn check(&self)->std::pin::Pin<Box<dyn Future<Output=Result<()>>+Send+'_>> {
        Box::pin(async move {
        let state=&self.state;let actor=&self.actor;
        if !enabled(state).await? {return Err(crate::AppError::Unavailable("routed artefact generation is disabled".into()));}
        if actor.break_glass || actor.mfa_enroll_only {return Err(RoutingError::Forbidden.into());}
        durable::Context::capture(&RoutingContext {actor:actor.clone(),chat_id:Some(self.chat),project_id:None,run_id:Uuid::nil(),purpose:CallPurpose::Primary})?.validate(&state.pg).await?;
        let scope=crate::voice::routed::capture(state,actor,self.chat).await?;
        if scope!=self.snapshot || scope["agent_id"].is_null() {return Err(RoutingError::Forbidden.into());}
        // Preserve the existing HITL boundary: agents with tools need their own
        // approval/resume adoption. This lane is the existing immediate drafter.
        if scope["agent"]["tools"].as_array().is_none_or(|tools|tools.iter().any(|t|t!="generate_artefact")) {
            return Err(RoutingError::Incompatible("tool agents require routed artefact approval adoption").into());
        }
        let adopted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM voice_routed_chats WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM api_routed_chats WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM routed_chat_branches WHERE chat_id=$1) OR EXISTS(SELECT 1 FROM routed_jobs WHERE context->>'chat_id'=$2)")
            .bind(self.chat).bind(self.chat.to_string()).fetch_one(&state.pg).await?;
        if adopted {return Err(RoutingError::Incompatible("this caller requires its own artefact adoption").into());}
        let tools=crate::tools::AuthorisedTools::build(&[],&["generate_artefact".into()],false,&Default::default());
        let overrides=crate::tools::load_overrides(&state.pg).await?;
        let project:Option<Uuid>=serde_json::from_value(scope["project"].clone()).map_err(|_|RoutingError::Forbidden)?;
        match crate::tools::authorize_native_call(state,actor,self.chat,&tools,&overrides,"generate_artefact",project).await {
            crate::tools::NativeDecision::Allowed(_)=>Ok(()),
            crate::tools::NativeDecision::Denied(e)=>Err(e),
            crate::tools::NativeDecision::Recoverable(_)=>Err(RoutingError::Forbidden.into()),
        }
        })
    }
    pub(crate) async fn guard<T>(&self,future:impl Future<Output=Result<T>>)->Result<T> {
        self.check().await?;tokio::pin!(future);
        let mut ticks=tokio::time::interval(Duration::from_millis(250));ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {tokio::select! {biased;
            _=ticks.tick()=>self.check().await?,
            result=&mut future=>{self.check().await?;return result;}
        }}
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rendered {schema_version:u8,kind:String,mime:String,bytes_base64:String,sha256:String}
fn mime(kind:&str)->&'static str {match kind {
    "md"=>"text/markdown","docx"=>"application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "pdf"=>"application/pdf",_=>unreachable!(),
}}
async fn render(state:&AppState,kind:&str,title:&str,body:&str)->Result<Vec<u8>> {
    let mut response=state.http.post(format!("{}/render-routed-artefact",state.boot.ml.base_url.trim_end_matches('/')))
        .timeout(Duration::from_secs(60)).json(&json!({"schema_version":1,"kind":kind,"title":title,"content":body})).send().await
        .map_err(|_|crate::AppError::Unavailable("routed renderer unavailable".into()))?;
    if !response.status().is_success() {return Err(crate::AppError::Unavailable("routed renderer rejected the draft".into()));}
    let mut bytes=Vec::new();
    while let Some(chunk)=response.chunk().await.map_err(|_|RoutingError::Invalid("renderer response"))? {
        if bytes.len().saturating_add(chunk.len())>6*1024*1024 {return Err(RoutingError::Invalid("renderer response too large").into());}
        bytes.extend_from_slice(&chunk);
    }
    let value:Rendered=serde_json::from_slice(&bytes).map_err(|_|RoutingError::Invalid("renderer contract"))?;
    let bytes=base64::engine::general_purpose::STANDARD.decode(value.bytes_base64).map_err(|_|RoutingError::Invalid("renderer bytes"))?;
    if value.schema_version!=1 || value.kind!=kind || value.mime!=mime(kind) || bytes.is_empty() || bytes.len()>MAX_BYTES || hash(&bytes)!=value.sha256
        || (kind=="docx" && !bytes.starts_with(b"PK")) || (kind=="pdf" && !bytes.starts_with(b"%PDF-"))
        || (kind=="md" && std::str::from_utf8(&bytes).is_err()) {
        return Err(RoutingError::Invalid("renderer output validation").into());
    }Ok(bytes)
}
/// Public to internal callers/tests, but usable only with the private intent
/// installed by authorized chat preparation. No caller can manufacture that seal.
pub fn publish<'a>(state:&'a AppState,run:&'a RoutedRun,message:Uuid,answer:&'a str)->std::pin::Pin<Box<dyn Future<Output=Result<Uuid>>+Send+'a>> {
    Box::pin(async move {
    let intent=run.artefact_intent().ok_or(RoutingError::Forbidden)?;
    run.guard(intent.guard(Box::pin(publish_inner(state,run,intent,message,answer)))).await
    })
}
async fn publish_inner(state:&AppState,run:&RoutedRun,intent:&Intent,message:Uuid,answer:&str)->Result<Uuid> {
    use tokio::io::AsyncWriteExt;
    let ctx=run.context(CallPurpose::Primary);
    super::routing::authorize(state,run).await?;
    if ctx.chat_id!=Some(intent.chat) || ctx.actor.user_id!=intent.actor.user_id {return Err(RoutingError::Forbidden.into());}
    if answer.trim().len()<40 || answer.len()>1024*1024 {return Err(RoutingError::Invalid("draft text length").into());}
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages WHERE id=$1 AND chat_id=$2 AND turn_id=$3 AND role='assistant' AND completed_at IS NOT NULL AND interrupted_at IS NULL)")
        .bind(message).bind(intent.chat).bind(ctx.run_id).fetch_one(&state.pg).await?;
    if !valid {return Err(RoutingError::Forbidden.into());}
    let saved:String=sqlx::query_scalar("SELECT content FROM messages WHERE id=$1").bind(message).fetch_one(&state.pg).await?;
    if super::strip_think(&saved).trim()!=super::strip_tool_leak(answer).trim() {return Err(RoutingError::Forbidden.into());}
    let (title,body)=super::extract_title_body(answer).unwrap_or_else(||(super::fallback_title(&intent.prompt),answer.trim().to_string()));
    let input=hash(json!({"title":title,"body":body,"kind":intent.kind,"message":message}).to_string().as_bytes());
    let id=Uuid::now_v7();let mut claim=state.pg.begin().await?;
    let won=sqlx::query("INSERT INTO routed_artefact_writes(turn_id,chat_id,message_id,actor_user_id,artefact_id,input_hash,status) VALUES($1,$2,$3,$4,$5,$6,'started') ON CONFLICT(turn_id) DO NOTHING")
        .bind(ctx.run_id).bind(intent.chat).bind(message).bind(ctx.actor.user_id).bind(id).bind(&input).execute(&mut *claim).await?.rows_affected();
    if won!=1 {
        claim.rollback().await?;
        let old:Option<Uuid>=sqlx::query_scalar("SELECT w.artefact_id FROM routed_artefact_writes w JOIN generated_artefacts g ON g.id=w.artefact_id WHERE w.turn_id=$1 AND w.chat_id=$2 AND w.message_id=$3 AND w.actor_user_id=$4 AND w.input_hash=$5 AND w.status='published'")
            .bind(ctx.run_id).bind(intent.chat).bind(message).bind(ctx.actor.user_id).bind(input).fetch_optional(&state.pg).await?;
        return old.ok_or_else(||crate::AppError::Conflict("artefact attempt already consumed; start a new turn".into()));
    }
    let mut event=crate::audit::AuditEvent::action("routing.artefact.started",ctx.actor.role.as_str());
    event.actor_user_id=ctx.actor.user_id;event.resource_type=Some("artefact".into());event.resource_id=Some(id);
    event.model_agent_traceability=Some(json!({"run_id":ctx.run_id,"chat_id":intent.chat,"decision_id":run.plan().decision.id,"selection":run.plan().selection,
        "kind":intent.kind,"renderer_inference":false}));
    crate::audit::append_with(&mut claim,&event).await?;claim.commit().await?;
    // No path supplied to ML; its temporary render cannot publish a Core file.
    let bytes=render(state,intent.kind,&title,&body).await?;
    run.validate_access().await?;
    let rel=format!("{}/{id}.{}",intent.chat,intent.kind);
    let path=crate::storage::resolve_file(&state.boot.storage.artefacts_dir,&rel);
    tokio::fs::create_dir_all(path.parent().ok_or(RoutingError::Invalid("artefact path"))?).await.map_err(anyhow::Error::from)?;
    let mut file=tokio::fs::OpenOptions::new().write(true).create_new(true).open(&path).await.map_err(anyhow::Error::from)?;
    file.write_all(&bytes).await.map_err(anyhow::Error::from)?;file.sync_all().await.map_err(anyhow::Error::from)?;drop(file);
    run.validate_access().await?;
    let mut publication=state.pg.begin().await?;run.fence(&mut publication).await?;
    sqlx::query("INSERT INTO generated_artefacts(id,chat_id,turn_id,message_id,kind,title,disk_path,mime,created_by) VALUES($1,$2,$3,$4,$5::artefact_kind,$6,$7,$8,$9)")
        .bind(id).bind(intent.chat).bind(ctx.run_id).bind(message).bind(intent.kind).bind(title).bind(rel).bind(mime(intent.kind)).bind(ctx.actor.user_id).execute(&mut *publication).await?;
    let n=sqlx::query("UPDATE routed_artefact_writes SET status='published',finished_at=now() WHERE turn_id=$1 AND artefact_id=$2 AND status='started'")
        .bind(ctx.run_id).bind(id).execute(&mut *publication).await?.rows_affected();
    if n!=1 {return Err(RoutingError::Forbidden.into());}
    event.action_type="routing.artefact.published".into();event.token_usage=Some(json!({"routing_usage":run.usage(),"renderer_calls":1,"output_bytes":bytes.len()}));
    crate::audit::append_with(&mut publication,&event).await?;publication.commit().await?;Ok(id)
}
