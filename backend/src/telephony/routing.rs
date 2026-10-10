// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit line opt-in and a witness minted only by authenticated call starters.
use std::{future::Future,pin::Pin};
use axum::{extract::{Path,State},Json};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use uuid::Uuid;
use crate::{AppState,Result,auth::{AuthContext,keycloak::AuthUser,permissions},model_routing::{ModelSelection,RoutingError}};

#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {pub revision:Uuid,pub model_selection:ModelSelection}
pub async fn selection(state:&AppState,line:Uuid)->Result<Option<Selection>> {
    let row:Option<(Uuid,Value)>=sqlx::query_as("SELECT revision,model_selection FROM phone_routing WHERE phone_number_id=$1")
        .bind(line).fetch_optional(&state.pg).await?;
    row.map(|(revision,value)|Ok(Selection {revision,model_selection:serde_json::from_value(value).map_err(|_|RoutingError::Invalid("invalid phone selection"))?})).transpose()
}
async fn permission(state:&AppState,actor:&AuthContext)->Result<()> {
    if actor.break_glass || actor.mfa_enroll_only {return Err(RoutingError::Forbidden.into());}
    state.rbac.require_permission(&state.pg,actor,permissions::TELEPHONY_MANAGE).await?;
    state.rbac.require_permission(&state.pg,actor,permissions::PROVIDERS_MANAGE).await?;Ok(())
}
pub async fn get(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(line):Path<Uuid>)->Result<Json<Value>> {
    permission(&state,&actor).await?;
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM phone_numbers WHERE id=$1)").bind(line).fetch_one(&state.pg).await?;
    if !exists {return Err(RoutingError::Forbidden.into());}
    Ok(Json(json!({"schema_version":1,"scope":"phone_pcm_batch_v1","routing":selection(&state,line).await?,
        "selection_modes":["default","manual","auto"],"phone_write_tools":false,"realtime_stt":false,"automatic_replay":false})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {pub model_selection:Value}
pub async fn set(State(state):State<AppState>,AuthUser(actor):AuthUser,Path(line):Path<Uuid>,Json(body):Json<Update>)->Result<Json<Value>> {
    permission(&state,&actor).await?;
    let selected=if body.model_selection.is_null() {None} else {Some(Selection {revision:Uuid::now_v7(),
        model_selection:serde_json::from_value(body.model_selection).map_err(|_|RoutingError::Invalid("invalid phone selection"))?})};
    let mut tx=state.pg.begin().await?;
    let exists:Option<Uuid>=sqlx::query_scalar("SELECT id FROM phone_numbers WHERE id=$1 FOR UPDATE").bind(line).fetch_optional(&mut *tx).await?;
    if exists.is_none() {return Err(RoutingError::Forbidden.into());}
    if let Some(selected)=&selected {
        sqlx::query("INSERT INTO phone_routing(phone_number_id,revision,model_selection) VALUES($1,$2,$3) ON CONFLICT(phone_number_id) DO UPDATE SET revision=EXCLUDED.revision,model_selection=EXCLUDED.model_selection")
            .bind(line).bind(selected.revision).bind(json!(selected.model_selection)).execute(&mut *tx).await?;
    } else {sqlx::query("DELETE FROM phone_routing WHERE phone_number_id=$1").bind(line).execute(&mut *tx).await?;}
    let mut event=crate::audit::AuditEvent::action("telephony.routing.updated",actor.role.as_str());
    event.actor_user_id=actor.user_id;event.resource_type=Some("phone_number".into());event.resource_id=Some(line);event.payload=Some(json!({"routing":selected}));
    crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;
    Ok(Json(json!({"schema_version":1,"routing":selected})))
}

pub(crate) struct Binding {pub call:Uuid,pub details:super::CallDetails,pub ending:super::Ending}
impl Binding {
    pub(crate) fn capture<'a>(&'a self,state:&'a AppState,actor:&'a AuthContext,chat:Option<Uuid>)->Pin<Box<dyn Future<Output=Result<Value>>+Send+'a>> {
        Box::pin(async move {
            if actor.user_id!=Some(self.details.owner_user_id) || actor.break_glass || actor.mfa_enroll_only {return Err(RoutingError::Forbidden.into());}
            let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND role::text=$2 AND deactivated_at IS NULL)")
                .bind(actor.user_id).bind(actor.role.as_str()).fetch_one(&state.pg).await?;
            if !live || !state.features.enabled_for(state,actor,"voice").await || !state.features.enabled_for(state,actor,"voice_live").await
                || !crate::features::enabled_for_user(state,None,"telephony").await {return Err(RoutingError::Forbidden.into());}
            let cfg=super::TelephonyResolved::load(&state.pg,state.message_key,&state.boot.telephony,&state.boot.server.public_url).await;
            if !cfg.has_provider() {return Err(RoutingError::Forbidden.into());}
            let selected=selection(state,self.details.phone_number_id).await?;
            if selected.is_none() || selected!=self.details.routing {return Err(RoutingError::NeedsReplan.into());}
            let line:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(p) FROM phone_numbers p WHERE id=$1 AND enabled AND owner_user_id=$2 AND agent_id=$3 AND e164=$4")
                .bind(self.details.phone_number_id).bind(actor.user_id).bind(self.details.agent_id).bind(&self.details.to).fetch_optional(&state.pg).await?;
            let line=line.ok_or(RoutingError::Forbidden)?;
            if line["provider"]!=cfg.provider {return Err(RoutingError::Forbidden.into());}
            if line["record_calls"]!=json!(self.details.record_calls) || super::notice::opening(line["greeting"].as_str(),line["notice"].as_str(),self.details.record_calls)!=self.details.opening {
                return Err(RoutingError::NeedsReplan.into());
            }
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM calls WHERE id=$1 AND phone_number_id=$2 AND owner_user_id=$3 AND agent_id=$4 AND provider_call_id=$5 AND to_e164=$6 AND from_e164=$7 AND provider=$8 AND ended_at IS NULL AND outcome='in_progress' AND chat_id IS NOT DISTINCT FROM $9)")
                .bind(self.call).bind(self.details.phone_number_id).bind(actor.user_id).bind(self.details.agent_id).bind(&self.details.call_sid)
                .bind(&self.details.to).bind(&self.details.from).bind(line["provider"].as_str()).bind(chat).fetch_one(&state.pg).await?;
            if !valid {return Err(RoutingError::Forbidden.into());}
            let agent=crate::workflows::agents::capture(&mut *state.pg.acquire().await?,self.details.agent_id).await?;
            if agent.is_null() || (!actor.is_admin() && !agent["created_by"].is_null() && agent["created_by"]!=json!(actor.user_id))
                || !agent["modes"].as_array().is_some_and(|v|v.iter().any(|v|v=="general")) {return Err(RoutingError::Forbidden.into());}
            // No write/phone tools, skills or project attachment may widen an anonymous caller.
            if agent["tools"].as_array().is_none_or(|v|v.iter().any(|v|v!="web_search")) || agent["skills"].as_array().is_none_or(|v|!v.is_empty()) {
                return Err(RoutingError::Incompatible("routed phone supports library answers and web_search only").into());
            }
            let mut libraries:Vec<Uuid>=serde_json::from_value(agent["kb_ids"].clone()).map_err(|_|RoutingError::Forbidden)?;
            libraries.sort();libraries.dedup();
            for id in &libraries {crate::kb::require_read(&state.pg,actor,*id).await?;crate::kb::brief(&state.pg,*id).await?;}
            if let Some(chat)=chat {
                let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chats WHERE id=$1 AND owner_user_id=$2 AND agent_id=$3 AND project_id IS NULL AND origin='phone' AND mode='general' AND archived_at IS NULL AND NOT EXISTS(SELECT 1 FROM chat_workspace WHERE chat_id=$1))")
                    .bind(chat).bind(actor.user_id).bind(self.details.agent_id).fetch_one(&state.pg).await?;
                if !valid {return Err(RoutingError::Forbidden.into());}
                let mut effective=crate::kb::retrieval_allowlist(&state.pg,actor,chat,None,Some(self.details.agent_id)).await?;effective.sort();effective.dedup();
                if effective!=libraries {return Err(RoutingError::Forbidden.into());}
            }
            let mut denied=state.rbac.denied_kb_doc_ids(&state.pg,actor,&libraries).await?;denied.sort();denied.dedup();
            Ok(json!({"project":null,"call_id":self.call,"line":line,"routing":selected,"agent":agent,"libraries":libraries,"denied":denied}))
        })
    }
}

/// Shared transport facade; caller authentication remains outside either session.
pub enum CallVoice {Legacy(std::sync::Arc<crate::voice::Session>),Routed(std::sync::Arc<crate::voice::routed::Session>)}
impl CallVoice {
    pub fn capture_rate(&self)->u32 {match self {Self::Legacy(s)=>s.capture_rate(),Self::Routed(_)=>16000}}
    pub fn chat_id(&self)->Option<Uuid> {match self {Self::Legacy(s)=>s.chat_id(),Self::Routed(s)=>Some(s.chat_id())}}
    pub fn hold_for_notice(&self) {if let Self::Legacy(s)=self {s.hold_for_notice();}}
    pub async fn announce(&self,text:String)->bool {match self {Self::Legacy(s)=>s.announce(text).await,Self::Routed(s)=>s.announce_phone(text).await}}
    pub async fn on_pcm(&self,pcm:Vec<u8>) {match self {Self::Legacy(s)=>s.on_pcm(pcm).await,Self::Routed(s)=>{let _=s.on_pcm(pcm).await;}}}
    pub async fn shutdown(&self) {match self {Self::Legacy(s)=>s.shutdown().await,Self::Routed(s)=>s.shutdown().await}}
}
