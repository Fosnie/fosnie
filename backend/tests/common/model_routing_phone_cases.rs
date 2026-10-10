// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{future::Future,pin::Pin,sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use axum::{extract::{State,Path},Json};
use serde_json::{json,Value};
use tokio::sync::Notify;
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,keycloak::AuthUser},model_routing::{ModelSelection,RoutingObjective},
    telephony::{self,CallDetails,routing},voice::telephony::{Wire,pace::Outbound}};

async fn utterance(voice:&routing::CallVoice) {
    let loud:Vec<u8>=(0..640).flat_map(|_|4000i16.to_le_bytes()).collect();
    for _ in 0..8 {voice.on_pcm(loud.clone()).await;}
    for _ in 0..45 {voice.on_pcm(vec![0;1280]).await;}
}
async fn logged(state:&AppState,actor:&AuthContext,line:Uuid,agent:Uuid)->anyhow::Result<(CallDetails,Uuid)> {
    let details=CallDetails {owner_user_id:actor.user_id.unwrap(),agent_id:agent,phone_number_id:line,call_sid:Uuid::now_v7().to_string(),
        from:"+447700900001".into(),to:"+441315550123".into(),record_calls:false,
        opening:telephony::notice::opening(None,None,false),routing:routing::selection(state,line).await?};
    let id=telephony::log::open(&state.pg,&details,"audiosocket").await?;Ok((details,id))
}
async fn start(state:&AppState,actor:&AuthContext,details:&CallDetails,call:Uuid)->anyhow::Result<Option<telephony::StartedCall>> {
    let state=state.clone();let actor=actor.clone();let details=details.clone();
    Ok(tokio::spawn(async move {telephony::start_session(&state,&actor,&details,Some(call),Wire::Pcm16,false).await}).await?)
}
pub fn check<'a>(state:&'a AppState,actor:&'a AuthContext,admin:&'a AuthContext,policy:Uuid,calls:&'a Arc<AtomicUsize>,mode:&'a Arc<AtomicUsize>,entered:&'a Arc<Notify>)
    ->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
        eprintln!("routing integration: authenticated telephone caller");
        let pool=&state.pg;let line=Uuid::now_v7();let agent=Uuid::now_v7();
        sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'Phone routed','Answer the caller from permitted sources.','{}',$2,ARRAY['general'])")
            .bind(agent).bind(actor.user_id).execute(pool).await?;
        sqlx::query("INSERT INTO phone_numbers(id,e164,provider,owner_user_id,agent_id,enabled) VALUES($1,'+441315550123','audiosocket',$2,$3,true)")
            .bind(line).bind(actor.user_id).bind(agent).execute(pool).await?;
        let prior:Option<(String,String)>=sqlx::query_as("SELECT value,value_type FROM config_settings WHERE key='telephony.provider'").fetch_optional(pool).await?;
        sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES('telephony.provider','audiosocket','string') ON CONFLICT(key) DO UPDATE SET value='audiosocket',value_type='string'").execute(pool).await?;
        let skills:Vec<Uuid>=sqlx::query_scalar("UPDATE skills SET enabled=false WHERE enabled AND is_default RETURNING id").fetch_all(pool).await?;
        let selection=ModelSelection::Auto {objective:RoutingObjective::Economy,policy_id:Some(policy)};
        assert!(routing::selection(state,line).await?.is_none());
        assert!(routing::set(State(state.clone()),AuthUser(actor.clone()),Path(line),Json(routing::Update {model_selection:json!(selection)})).await.is_err());
        let _=routing::set(State(state.clone()),AuthUser(admin.clone()),Path(line),Json(routing::Update {model_selection:json!(selection)})).await?;
        let (details,call)=logged(state,actor,line,agent).await?;
        let before=calls.load(Ordering::SeqCst);mode.store(17,Ordering::SeqCst);
        let mut old=details.clone();old.routing=None;
        assert!(start(state,actor,&old,call).await?.is_none(),"old ticket cannot enter routed line");
        assert!(start(state,admin,&details,call).await?.is_none(),"line owner's principal cannot be replaced");
        let session=start(state,actor,&details,call).await?.ok_or_else(||anyhow::anyhow!("phone start refused"))?;
        let telephony::StartedCall {voice,sink,out_rx:mut rx,control_rx:_control,pacer_task,ending,..}=session;
        let audio=Arc::new(AtomicUsize::new(0));let heard=audio.clone();
        let writer=tokio::spawn(async move {while let Some(item)=rx.recv().await {if let Outbound::Frame(bytes)=item {
            assert_eq!(bytes.len(),320);heard.fetch_add(1,Ordering::SeqCst);
        }}});
        assert!(matches!(voice.as_ref(),routing::CallVoice::Routed(_)));
        assert_eq!(voice.capture_rate(),16000);
        let chat=voice.chat_id().unwrap();
        assert!(start(state,actor,&details,call).await?.is_none(),"single authenticated call cannot replay");
        utterance(&voice).await;assert_eq!(calls.load(Ordering::SeqCst),before,"caller is deaf until notice completes");
        assert!(voice.announce(details.opening.clone()).await);
        assert!(!voice.announce(details.opening.clone()).await,"notice is never automatically replayed");
        assert_eq!(calls.load(Ordering::SeqCst),before+1,"opening is a fixed-only budgeted call, no judge");
        utterance(&voice).await;
        let usage:Value=tokio::time::timeout(Duration::from_secs(12),async {
            loop {
                let usage:Option<Value>=sqlx::query_scalar("SELECT token_usage FROM audit_events WHERE action_type='voice.turn.routed' AND model_agent_traceability->>'call_id'=$1")
                    .bind(call.to_string()).fetch_optional(pool).await?;
                if let Some(usage)=usage {return Ok::<_,anyhow::Error>(usage);}
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await??;
        assert_eq!(usage["routing_usage"]["calls"],4);assert_eq!(calls.load(Ordering::SeqCst),before+5);
        let origin:String=sqlx::query_scalar("SELECT origin FROM chats WHERE id=$1").bind(chat).fetch_one(pool).await?;assert_eq!(origin,"phone");
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM messages WHERE chat_id=$1 AND role='assistant' AND completed_at IS NOT NULL AND interrupted_at IS NULL")
            .bind(chat).fetch_one(pool).await?;assert_eq!(count,1);
        assert!(fosnie_backend::voice::routed::require_legacy_chat(state,Some(chat)).await.is_err());
        assert!(fosnie_backend::chat::regenerate_prepare(state,actor,chat,Uuid::now_v7(),None).await.is_err());
        // A line disabled during synthesis stops the live call and suppresses audio.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let delivered=audio.load(Ordering::SeqCst);mode.store(18,Ordering::SeqCst);utterance(&voice).await;
        tokio::time::timeout(Duration::from_secs(5),entered.notified()).await?;
        sqlx::query("UPDATE phone_numbers SET enabled=false WHERE id=$1").bind(line).execute(pool).await?;
        tokio::time::timeout(Duration::from_secs(3),ending.ended()).await?;
        voice.shutdown().await;assert_eq!(audio.load(Ordering::SeqCst),delivered);
        drop(voice);drop(sink);pacer_task.abort();writer.abort();
        sqlx::query("UPDATE phone_numbers SET enabled=true WHERE id=$1").bind(line).execute(pool).await?;
        let (stale,call)=logged(state,actor,line,agent).await?;let before=calls.load(Ordering::SeqCst);
        let _=routing::set(State(state.clone()),AuthUser(admin.clone()),Path(line),Json(routing::Update {model_selection:json!(selection)})).await?;
        assert!(start(state,actor,&stale,call).await?.is_none(),"same model with new revision still invalidates ticket");
        assert_eq!(calls.load(Ordering::SeqCst),before);
        let (details,call)=logged(state,actor,line,agent).await?;
        sqlx::query("INSERT INTO agent_tools(agent_id,tool_name) VALUES($1,'transfer_call')").bind(agent).execute(pool).await?;
        assert!(start(state,actor,&details,call).await?.is_none(),"unsupported phone writes cannot enter legacy dispatcher");
        sqlx::query("DELETE FROM agent_tools WHERE agent_id=$1").bind(agent).execute(pool).await?;
        let _=routing::set(State(state.clone()),AuthUser(admin.clone()),Path(line),Json(routing::Update {model_selection:Value::Null})).await?;
        assert!(start(state,actor,&details,call).await?.is_none(),"disabled routing never downgrades an in-flight ticket");
        for id in skills {sqlx::query("UPDATE skills SET enabled=true WHERE id=$1").bind(id).execute(pool).await?;}
        if let Some((value,kind))=prior {sqlx::query("UPDATE config_settings SET value=$1,value_type=$2 WHERE key='telephony.provider'").bind(value).bind(kind).execute(pool).await?;}
        else {sqlx::query("DELETE FROM config_settings WHERE key='telephony.provider'").execute(pool).await?;}
        mode.store(0,Ordering::SeqCst);Ok(())
    })
}
