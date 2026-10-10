// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{future::Future,pin::Pin,sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use axum::{extract::State,Json};
use base64::Engine;
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use tokio::sync::{mpsc,Notify};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,keycloak::AuthUser},chat::{routed_artefact as artefact,origin::TurnContext},model_routing::{*,run::RoutedRun}};
const DRAFT:&str="# Draft\n\nThis is the exact drafted answer, long enough to be exported as a useful document.";
const PROMPT:&str="Write a downloadable memo as Markdown";
async fn chat(state:&AppState,actor:&AuthContext,agent:Uuid)->anyhow::Result<Uuid> {
    let id=Uuid::now_v7();sqlx::query("INSERT INTO chats(id,owner_user_id,agent_id,title) VALUES($1,$2,$3,'Draft')")
        .bind(id).bind(actor.user_id).bind(agent).execute(&state.pg).await?;Ok(id)
}
async fn prepare(state:&AppState,actor:&AuthContext,chat:Uuid,turn:Uuid,selection:ModelSelection)->fosnie_backend::Result<RoutedRun> {
    Box::pin(fosnie_backend::chat::routing::prepare_selected(state,actor,chat,turn,PROMPT,selection,WorkloadRequirements {output_tokens:1024,..Default::default()},&[],&Notify::new())).await
}
async fn execute(state:&AppState,actor:&AuthContext,chat:Uuid,run:&RoutedRun)->anyhow::Result<Vec<Value>> {
    let (tx,mut rx)=mpsc::channel(128);let id=run.context(CallPurpose::Primary).run_id;
    let task=tokio::spawn({let state=state.clone();let actor=actor.clone();let run=run.clone();async move {
        Box::pin(fosnie_backend::chat::run_turn(&state,TurnContext::web(&actor).with_routing(&run),id,Some(chat),None,None,PROMPT.into(),
            vec![],vec![],false,None,None,None,None,&tx,Arc::new(Notify::new()))).await;
    }});
    let mut frames=vec![];while let Some(frame)=tokio::time::timeout(Duration::from_secs(12),rx.recv()).await? {frames.push(serde_json::to_value(frame)?);}
    task.await?;Ok(frames)
}
pub fn check<'a>(original:&'a AppState,actor:&'a AuthContext,admin:&'a AuthContext,provider:Uuid,policy:Uuid)->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
        eprintln!("routing integration: deterministic draft export and consumed write attempts");
        use fosnie_backend::http::voice_routing::Enabled;
        let calls=Arc::new(AtomicUsize::new(0));let renders=Arc::new(AtomicUsize::new(0));let mode=Arc::new(AtomicUsize::new(0));let entered=Arc::new(Notify::new());
        let app=axum::Router::new().route("/generate",axum::routing::post({
            let calls=calls.clone();move |Json(body):Json<Value>| {let calls=calls.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);assert_eq!(body["overrides"]["routing"]["purpose"],"final");
                format!("{}\n{}\n",json!({"type":"token","delta":DRAFT}),json!({"type":"done","finish_reason":"stop","routing_report":super::routing_mock_report(&body)}))
            }}
        })).route("/chat-step",axum::routing::post({
            let calls=calls.clone();move |Json(body):Json<Value>| {let calls=calls.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);assert_eq!(body["overrides"]["routing"]["purpose"],"router_classify","no hidden title call");
                Json(json!({"content":json!({"task":"general","minimum_tier":"economy","fresh_information":false,"tools":false,"vision":false,"structured_output":false,"confidence":0.95,"uncertain":false}).to_string(),"routing_report":super::routing_mock_report(&body)}))
            }}
        })).route("/render-routed-artefact",axum::routing::post({
            let renders=renders.clone();let mode=mode.clone();let entered=entered.clone();
            move |Json(body):Json<Value>| {let renders=renders.clone();let mode=mode.clone();let entered=entered.clone();async move {
                renders.fetch_add(1,Ordering::SeqCst);assert!(body.get("out_path").is_none() && body.get("overrides").is_none());
                // A one-word heading is deliberately not a usable title. The
                // deterministic fallback uses the prompt, never a naming model.
                assert_eq!(body["title"],PROMPT);assert!(body["content"].as_str().unwrap().contains("exact drafted answer"));
                let scenario=mode.load(Ordering::SeqCst);
                if scenario==1 {entered.notify_one();tokio::time::sleep(Duration::from_secs(3)).await;}
                let data=DRAFT.as_bytes();
                Json(json!({"schema_version":1,"kind":"md","mime":"text/markdown","bytes_base64":base64::engine::general_purpose::STANDARD.encode(data),
                    "sha256":if scenario==2 {"wrong".into()} else {Sha256::digest(data).iter().map(|b|format!("{b:02x}")).collect::<String>()}}))
            }}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
        let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
        let root=tempfile::tempdir()?;let mut state=original.clone();let mut boot=(*state.boot).clone();
        boot.ml.base_url=format!("http://{address}");boot.storage.artefacts_dir=root.path().to_string_lossy().into();state.boot=Arc::new(boot);
        let agent=Uuid::now_v7();sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'Routed drafter','Draft the requested document.','{}',$2,ARRAY['general'])")
            .bind(agent).bind(actor.user_id).execute(&state.pg).await?;
        let source=chat(&state,actor,agent).await?;
        let manual=ModelSelection::Manual {provider_id:provider};
        assert!(prepare(&state,actor,source,Uuid::now_v7(),manual.clone()).await.is_err());assert_eq!(calls.load(Ordering::SeqCst),0);
        assert!(artefact::set_enabled(State(state.clone()),AuthUser(actor.clone()),Json(Enabled {enabled:true})).await.is_err());
        let _=artefact::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:true})).await?;
        for selection in [manual.clone(),ModelSelection::Auto {policy_id:Some(policy),objective:RoutingObjective::Economy}] {
            let source=chat(&state,actor,agent).await?;let before=calls.load(Ordering::SeqCst);let count=renders.load(Ordering::SeqCst);
            let auto=matches!(selection,ModelSelection::Auto {..});
            let run=prepare(&state,actor,source,Uuid::now_v7(),selection).await?;
            let frames=execute(&state,actor,source,&run).await?;assert!(frames.iter().any(|f|f["type"]=="chat.completed"),"{frames:?}");
            assert_eq!(calls.load(Ordering::SeqCst)-before,if auto {2} else {1});assert_eq!(renders.load(Ordering::SeqCst),count+1);
            assert_eq!(run.usage().calls,if auto {2} else {1},"rendering consumes time, not an inference grant");
            let (id,message,path):(Uuid,Uuid,String)=sqlx::query_as("SELECT id,message_id,disk_path FROM generated_artefacts WHERE chat_id=$1").bind(source).fetch_one(&state.pg).await?;
            assert_eq!(tokio::fs::read_to_string(root.path().join(path)).await?,DRAFT);
            assert_eq!(artefact::publish(&state,&run,message,DRAFT).await?,id);
            assert!(artefact::publish(&state,&run,message,"Changed body that is long enough to pass the size guard.").await.is_err());
            assert_eq!(renders.load(Ordering::SeqCst),count+1,"repeat never re-renders");
        }
        // Interrupted renderer: reservation stays consumed after cancellation.
        mode.store(1,Ordering::SeqCst);let source=chat(&state,actor,agent).await?;let turn=Uuid::now_v7();
        let run=prepare(&state,actor,source,turn,manual.clone()).await?;
        let task=tokio::spawn({let state=state.clone();let actor=actor.clone();let run=run.clone();async move {execute(&state,&actor,source,&run).await}});
        tokio::time::timeout(Duration::from_secs(6),entered.notified()).await?;
        let claimed_message:Uuid=sqlx::query_scalar("SELECT message_id FROM routed_artefact_writes WHERE turn_id=$1 AND status='started'").bind(turn).fetch_one(&state.pg).await?;
        let render_count=renders.load(Ordering::SeqCst);
        assert!(artefact::publish(&state,&run,claimed_message,DRAFT).await.is_err(),"concurrent contender cannot render twice");
        assert_eq!(renders.load(Ordering::SeqCst),render_count);
        let _=artefact::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:false})).await?;
        let frames=tokio::time::timeout(Duration::from_secs(4),task).await???;
        assert!(frames.iter().any(|f|f["type"]=="chat.error"));assert!(!frames.iter().any(|f|f["type"]=="chat.completed"));
        let published:i64=sqlx::query_scalar("SELECT count(*) FROM generated_artefacts WHERE chat_id=$1").bind(source).fetch_one(&state.pg).await?;assert_eq!(published,0);
        let message:Uuid=sqlx::query_scalar("SELECT message_id FROM routed_artefact_writes WHERE turn_id=$1 AND status='started'").bind(turn).fetch_one(&state.pg).await?;
        let _=artefact::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:true})).await?;
        let retry=prepare(&state,actor,source,turn,manual.clone()).await?;let count=renders.load(Ordering::SeqCst);
        assert!(artefact::publish(&state,&retry,message,DRAFT).await.is_err());assert_eq!(renders.load(Ordering::SeqCst),count);
        // Bad render report never publishes a pointer or silently uses legacy.
        mode.store(2,Ordering::SeqCst);let source=chat(&state,actor,agent).await?;
        let run=prepare(&state,actor,source,Uuid::now_v7(),manual.clone()).await?;let frames=execute(&state,actor,source,&run).await?;
        assert!(frames.iter().any(|f|f["type"]=="chat.error"));
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM generated_artefacts WHERE chat_id=$1").bind(source).fetch_one(&state.pg).await?;assert_eq!(count,0);
        // Existing approval semantics are not bypassed for tool-enabled agents.
        sqlx::query("INSERT INTO agent_tools(agent_id,tool_name) VALUES($1,'web_search')").bind(agent).execute(&state.pg).await?;
        let before=calls.load(Ordering::SeqCst);assert!(prepare(&state,actor,source,Uuid::now_v7(),manual).await.is_err());assert_eq!(calls.load(Ordering::SeqCst),before);
        let _=artefact::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:false})).await?;
        server.abort();Ok(())
    })
}
