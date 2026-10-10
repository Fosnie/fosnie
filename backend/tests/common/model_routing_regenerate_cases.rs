// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{future::Future,pin::Pin,sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use axum::{extract::State,Json};
use serde_json::{json,Value};
use tokio::sync::{mpsc,Notify};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,keycloak::AuthUser},chat::regenerate,model_routing::*};

async fn source(state:&AppState,actor:&AuthContext,selection:ModelSelection)->anyhow::Result<(Uuid,Uuid)> {
    let chat=Uuid::now_v7();let answer=Uuid::now_v7();
    sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Original routed conversation')")
        .bind(chat).bind(actor.user_id).execute(&state.pg).await?;
    for (seq,role,text) in [(0,"user","Earlier context"),(1,"assistant","Earlier answer"),(2,"user","Repeat this question"),(3,"assistant","OLD ANSWER MUST STAY"),(4,"user","LATER TURN MUST NOT ENTER")] {
        sqlx::query("INSERT INTO messages(id,chat_id,role,sequence_number,content,completed_at,routing_selection,routing_decision) VALUES($1,$2,$3::message_role,$4,$5,now(),$6,$7)")
            .bind(if seq==3 {answer} else {Uuid::now_v7()}).bind(chat).bind(role).bind(seq).bind(text)
            .bind((role=="assistant").then(||json!(selection))).bind((role=="assistant").then(||json!({"id":Uuid::now_v7()}))).execute(&state.pg).await?;
    }Ok((chat,answer))
}
async fn snapshot(state:&AppState,chat:Uuid)->anyhow::Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(m) ORDER BY sequence_number) FROM messages m WHERE chat_id=$1").bind(chat).fetch_one(&state.pg).await?)
}
async fn attempt(state:&AppState,actor:&AuthContext,chat:Uuid,answer:Uuid)->anyhow::Result<(fosnie_backend::Result<Uuid>,Vec<Value>)> {
    let (tx,mut rx)=mpsc::channel(128);
    let job=tokio::spawn(regenerate::execute(state.clone(),actor.clone(),chat,answer,Uuid::now_v7(),tx,Arc::new(Notify::new())));
    let mut frames=vec![];while let Some(frame)=tokio::time::timeout(Duration::from_secs(10),rx.recv()).await? {frames.push(serde_json::to_value(frame)?);}
    Ok((job.await?,frames))
}
pub fn check<'a>(state:&'a AppState,actor:&'a AuthContext,admin:&'a AuthContext,provider:Uuid,mut policy:RoutingPolicy,
    calls:&'a Arc<AtomicUsize>,mode:&'a Arc<AtomicUsize>,entered:&'a Arc<Notify>)
    ->Pin<Box<dyn Future<Output=anyhow::Result<RoutingPolicy>>+Send+'a>> {
    Box::pin(async move {
        eprintln!("routing integration: non-destructive routed regeneration");
        use fosnie_backend::http::voice_routing::Enabled;
        let auto=ModelSelection::Auto {policy_id:Some(policy.id),objective:RoutingObjective::Economy};
        let (chat,answer)=source(state,actor,auto.clone()).await?;let original=snapshot(state,chat).await?;
        let before=calls.load(Ordering::SeqCst);
        assert!(attempt(state,actor,chat,answer).await?.0.is_err());assert_eq!(calls.load(Ordering::SeqCst),before);
        assert!(regenerate::set_enabled(State(state.clone()),AuthUser(actor.clone()),Json(Enabled {enabled:true})).await.is_err());
        let caps=regenerate::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:true})).await?.0;
        assert_eq!(caps["preserves_original"],true);
        policy=store::save_policy(state,admin,policy.id,store::SavePolicy {label:"Regeneration current policy".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone()}).await?;
        mode.store(20,Ordering::SeqCst);
        let (result,frames)=attempt(state,actor,chat,answer).await?;
        assert!(!frames.iter().any(|f|f["type"]=="chat.error"),"{frames:?}");
        let branch=result?;assert_ne!(branch,chat);assert_eq!(calls.load(Ordering::SeqCst)-before,2,"one fresh judge + answer");
        assert!(frames.iter().any(|f|f["type"]=="chat.completed"));assert_eq!(snapshot(state,chat).await?,original);
        let (intent,decision):(Value,Value)=sqlx::query_as("SELECT routing_selection,routing_decision FROM messages WHERE chat_id=$1 AND role='assistant' ORDER BY sequence_number DESC LIMIT 1")
            .bind(branch).fetch_one(&state.pg).await?;
        assert_eq!(intent,json!(auto));assert_eq!(decision["policy_revision"],json!(policy.revision));assert_ne!(decision["id"],original[3]["routing_decision"]["id"]);
        let history=snapshot(state,branch).await?;assert_eq!(history.as_array().unwrap().len(),4);
        assert_eq!(history[0]["content"],"Earlier context");assert_eq!(history[2]["content"],"Repeat this question");
        assert!(fosnie_backend::chat::regenerate_prepare(state,actor,chat,answer,None).await.is_err());
        assert!(fosnie_backend::voice::routed::require_legacy_chat(state,Some(branch)).await.is_err());
        for selection in [ModelSelection::Manual {provider_id:provider},ModelSelection::Default] {
            let (source,answer)=source(state,actor,selection.clone()).await?;
            let before=calls.load(Ordering::SeqCst);let (result,frames)=attempt(state,actor,source,answer).await?;let branch=result?;
            assert!(!frames.iter().any(|f|f["type"]=="chat.error"),"{frames:?}");assert_eq!(calls.load(Ordering::SeqCst)-before,1);
            let saved:Value=sqlx::query_scalar("SELECT routing_selection FROM messages WHERE chat_id=$1 AND role='assistant' ORDER BY sequence_number DESC LIMIT 1")
                .bind(branch).fetch_one(&state.pg).await?;assert_eq!(saved,json!(selection));
        }
        let (legacy,old)=source(state,actor,auto.clone()).await?;
        sqlx::query("UPDATE messages SET routing_selection=NULL WHERE id=$1").bind(old).execute(&state.pg).await?;
        let before=calls.load(Ordering::SeqCst);assert!(attempt(state,actor,legacy,old).await?.0.is_err());
        assert!(attempt(state,admin,chat,answer).await?.0.is_err());assert_eq!(calls.load(Ordering::SeqCst),before);
        for fault in 0..4 {
            mode.store(8,Ordering::SeqCst);let (tx,mut rx)=mpsc::channel(128);let cancel=Arc::new(Notify::new());
            let before=calls.load(Ordering::SeqCst);
            let job=tokio::spawn(regenerate::execute(state.clone(),actor.clone(),chat,answer,Uuid::now_v7(),tx,cancel.clone()));
            tokio::time::timeout(Duration::from_secs(5),entered.notified()).await?;
            match fault {
                0=>cancel.notify_one(),1=>rx.close(),
                2=>{let _=regenerate::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:false})).await?;},
                _=>{sqlx::query("UPDATE messages SET content='Changed concurrently' WHERE id=$1").bind(answer).execute(&state.pg).await?;}
            }
            assert!(tokio::time::timeout(Duration::from_secs(3),job).await??.is_err());assert_eq!(calls.load(Ordering::SeqCst)-before,1);
            if fault==2 {let _=regenerate::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:true})).await?;}
            if fault==3 {sqlx::query("UPDATE messages SET content='OLD ANSWER MUST STAY' WHERE id=$1").bind(answer).execute(&state.pg).await?;}
        }
        assert_eq!(snapshot(state,chat).await?,original);
        let _=regenerate::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:false})).await?;
        mode.store(0,Ordering::SeqCst);Ok(policy)
    })
}
