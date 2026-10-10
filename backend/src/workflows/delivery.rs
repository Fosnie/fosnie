// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Captured recipients, guarded output reads and transactional link delivery.
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use uuid::Uuid;
use crate::{AppState,Result,auth::AuthContext,model_routing::RoutingError};
use super::sources::Scope;

const NOTICE:&str="Workflow completed — open the output.";
#[derive(Serialize,Deserialize)]
struct Destination {group:Uuid,recipients:Vec<Uuid>}
pub(super) fn target(cfg:&Value)->Result<Option<Uuid>> {
    serde_json::from_value(cfg.get("deliver_group_chat_id").cloned().unwrap_or(Value::Null))
        .map_err(|_|RoutingError::Invalid("invalid workflow delivery group").into())
}
pub(super) async fn capture(conn:&mut sqlx::PgConnection,cfg:&Value)->Result<Value> {
    let Some(group)=target(cfg)? else {return Ok(Value::Null);};
    let recipients:Vec<Uuid>=sqlx::query_scalar("SELECT user_id FROM group_chat_members WHERE group_chat_id=$1 ORDER BY user_id LIMIT 129")
        .bind(group).fetch_all(conn).await?;
    // Capture an invalid audience too: execution fails this firing without
    // rolling back dispatch of unrelated workflows in the same event batch.
    Ok(json!(Destination {group,recipients}))
}
fn saved(input:&Value)->Result<Option<Destination>> {
    let Some(group)=target(&input["config"])? else {return Ok(None);};
    let dest:Destination=serde_json::from_value(input["delivery"].clone()).map_err(|_|RoutingError::Invalid("missing queued delivery snapshot"))?;
    if dest.group!=group || dest.recipients.is_empty() || dest.recipients.len()>128
        || dest.recipients.windows(2).any(|w|w[0]>=w[1]) {return Err(RoutingError::Forbidden.into());}
    Ok(Some(dest))
}
pub(super) async fn check(state:&AppState,actor:&AuthContext,input:&Value)->Result<()> {
    let Some(dest)=saved(input)? else {return Ok(());};
    if !dest.recipients.contains(&actor.user_id.ok_or(RoutingError::Forbidden)?)
        || !crate::features::enabled_for(state,actor,"messaging").await {return Err(RoutingError::Forbidden.into());}
    let current:Vec<Uuid>=sqlx::query_scalar("SELECT user_id FROM group_chat_members WHERE group_chat_id=$1 ORDER BY user_id LIMIT 129")
        .bind(dest.group).fetch_all(&state.pg).await?;
    if current!=dest.recipients {return Err(RoutingError::NeedsReplan.into());} Ok(())
}
pub(super) async fn protect(tx:&mut sqlx::Transaction<'_,sqlx::Postgres>,input:&Value,scope:&Scope,run:Uuid,chat:Uuid)->Result<()> {
    if let Some(dest)=saved(input)? {
        sqlx::query("INSERT INTO workflow_output_scopes(chat_id,run_id,group_chat_id,recipients,source_scope) VALUES($1,$2,$3,$4,$5)")
            .bind(chat).bind(run).bind(dest.group).bind(dest.recipients).bind(json!(scope)).execute(&mut **tx).await?;
    } Ok(())
}
pub(super) async fn check_readers(state:&AppState,input:&Value,scope:&Scope)->Result<()> {
    if let Some(dest)=saved(input)? {for user in &dest.recipients {
        let recipient=crate::auth::load_context(&state.pg,*user).await?;
        if !crate::features::enabled_for(state,&recipient,"messaging").await {return Err(RoutingError::Forbidden.into());}
        scope.authorize_reader(state,&recipient).await?;
    }} Ok(())
}
pub(super) struct Published {group:Uuid,recipients:Vec<Uuid>,chat:Uuid,pub message:Uuid,seq:i32,created:String}
impl Published {
    pub fn notify(self,state:&AppState) {
        for user in self.recipients {state.hub.send_to_user(user,crate::ws::protocol::ServerFrame::GroupMessage {
            chat_id:self.group,id:self.message,seq:self.seq,sender_user_id:None,message_type:"system".into(),
            content:NOTICE.into(),created_at:self.created.clone(),attachments:None,shared_resources:Some(json!({"chat_id":self.chat})),
        });}
    }
}
pub(super) async fn publish(state:&AppState,actor:&AuthContext,input:&Value,scope:&Scope,chat:Uuid,tx:&mut sqlx::Transaction<'_,sqlx::Postgres>)->Result<Option<Published>> {
    let Some(dest)=saved(input)? else {return Ok(None);};
    check(state,actor,input).await?;
    check_readers(state,input,scope).await?;
    // Match the normal message path's lock order (sequence lock before member
    // rows). The parent lock also blocks FK inserts of new members until commit.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))").bind(dest.group.to_string()).execute(&mut **tx).await?;
    let group:Option<Uuid>=sqlx::query_scalar("SELECT id FROM group_chats WHERE id=$1 FOR UPDATE").bind(dest.group).fetch_optional(&mut **tx).await?;
    if group.is_none() {return Err(RoutingError::Forbidden.into());}
    let current:Vec<Uuid>=sqlx::query_scalar("SELECT user_id FROM group_chat_members WHERE group_chat_id=$1 ORDER BY user_id LIMIT 129 FOR SHARE")
        .bind(dest.group).fetch_all(&mut **tx).await?;
    if current!=dest.recipients {return Err(RoutingError::NeedsReplan.into());}
    sqlx::query("INSERT INTO chat_shares(chat_id,group_chat_id,shared_by) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(chat).bind(dest.group).bind(actor.user_id).execute(&mut **tx).await?;
    let (message,seq,created)=crate::http::messaging::stage_workflow_link(state,tx,dest.group,chat,NOTICE).await?;
    let n=sqlx::query("UPDATE workflow_output_scopes SET message_id=$2,delivered_at=now() WHERE chat_id=$1 AND delivered_at IS NULL")
        .bind(chat).bind(message).execute(&mut **tx).await?.rows_affected();
    if n!=1 {return Err(RoutingError::Cancelled.into());}
    Ok(Some(Published {group:dest.group,recipients:dest.recipients,chat,message,seq,created}))
}
/// Run before ordinary project/share read grants. False means an ordinary chat.
pub(crate) async fn authorize_read(state:&AppState,actor:&AuthContext,chat:Uuid)->Result<bool> {
    let row:Option<(Uuid,Vec<Uuid>,Value,bool,Uuid)>=sqlx::query_as("SELECT group_chat_id,recipients,source_scope,delivered_at IS NOT NULL,run_id FROM workflow_output_scopes WHERE chat_id=$1")
        .bind(chat).fetch_optional(&state.pg).await?;
    let Some((group,recipients,scope,delivered,run))=row else {return Ok(false);};
    let uid=actor.user_id.ok_or(RoutingError::Forbidden)?;
    if !delivered || !recipients.contains(&uid) {return Err(RoutingError::Forbidden.into());}
    let current=crate::auth::load_context(&state.pg,uid).await?;
    if !crate::features::enabled_for(state,&current,"messaging").await {return Err(RoutingError::Forbidden.into());}
    let member:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM chat_shares s JOIN group_chat_members m ON m.group_chat_id=s.group_chat_id JOIN chats c ON c.id=s.chat_id WHERE s.chat_id=$1 AND s.group_chat_id=$2 AND m.user_id=$3 AND c.archived_at IS NULL)")
        .bind(chat).bind(group).bind(uid).fetch_one(&state.pg).await?;
    if !member {return Err(RoutingError::Forbidden.into());}
    // Delivery grants access to this workflow result, not later private turns
    // added by its owner using a different source scope or denial set.
    let extended:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM messages WHERE chat_id=$1 AND role IN ('user','assistant') AND turn_id IS DISTINCT FROM $2)")
        .bind(chat).bind(run).fetch_one(&state.pg).await?;
    if extended {return Err(RoutingError::Forbidden.into());}
    let scope:Scope=serde_json::from_value(scope).map_err(|_|RoutingError::Forbidden)?;
    scope.authorize_reader(state,&current).await?;Ok(true)
}
pub(crate) async fn check_share_target(state:&AppState,chat:Uuid,group:Uuid)->Result<()> {
    let original:Option<Uuid>=sqlx::query_scalar("SELECT group_chat_id FROM workflow_output_scopes WHERE chat_id=$1").bind(chat).fetch_optional(&state.pg).await?;
    if original.is_some_and(|id|id!=group) {return Err(RoutingError::Forbidden.into());} Ok(())
}
