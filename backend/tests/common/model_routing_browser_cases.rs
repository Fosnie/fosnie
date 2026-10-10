// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{future::Future,pin::Pin,sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use axum::{extract::{Path,State},Json};
use serde_json::{json,Value};
use tokio::sync::{mpsc,Notify};
use uuid::Uuid;
use fosnie_protocol::{RoutedChatRequest,RoutingOptions};
use fosnie_backend::{AppState,auth::{AuthContext,PlatformRole,keycloak::AuthUser},chat::browser,config::BootConfig,state::AppStateBuilder,model_routing::*};

fn request(chat:Option<Uuid>,selection:Option<ModelSelection>,options:Option<RoutingOptions>)->RoutedChatRequest {
    RoutedChatRequest {chat_id:chat,project_id:None,agent_id:None,content:"Browser routing question".into(),model_selection:selection,options}
}
async fn attempt(state:&AppState,actor:&AuthContext,request:RoutedChatRequest)->anyhow::Result<(fosnie_backend::Result<Uuid>,Vec<Value>)> {
    let (tx,mut rx)=mpsc::channel(128);
    let job=tokio::spawn(browser::execute(state.clone(),actor.clone(),request,Uuid::now_v7(),tx,Arc::new(Notify::new())));
    let mut frames=vec![];while let Some(frame)=tokio::time::timeout(Duration::from_secs(10),rx.recv()).await? {frames.push(serde_json::to_value(frame)?);}
    Ok((job.await?,frames))
}
async fn empty(state:&AppState,actor:&AuthContext)->anyhow::Result<Uuid> {
    let chat=Uuid::now_v7();sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Existing browser chat')")
        .bind(chat).bind(actor.user_id).execute(&state.pg).await?;Ok(chat)
}
pub fn check<'a>(pool:&'a sqlx::PgPool,admin:&'a AuthContext)->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
        eprintln!("routing integration: browser composer, saved intent and actual metadata");
        let calls=Arc::new(AtomicUsize::new(0));let judges=Arc::new(AtomicUsize::new(0));let titles=Arc::new(AtomicUsize::new(0));
        let mode=Arc::new(AtomicUsize::new(0));let entered=Arc::new(Notify::new());
        let app=axum::Router::new().route("/chat-step",axum::routing::post({
            let calls=calls.clone();let judges=judges.clone();let titles=titles.clone();let mode=mode.clone();let entered=entered.clone();
            move |Json(body):Json<Value>| {let calls=calls.clone();let judges=judges.clone();let titles=titles.clone();let mode=mode.clone();let entered=entered.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);
                let purpose=body["overrides"]["routing"]["purpose"].as_str().unwrap();
                let content=if purpose=="utility_title" {titles.fetch_add(1,Ordering::SeqCst);"Browser routing title".to_string()}
                else {
                    assert_eq!(purpose,"router_classify");judges.fetch_add(1,Ordering::SeqCst);
                    if mode.load(Ordering::SeqCst)==1 {entered.notify_one();tokio::time::sleep(Duration::from_secs(4)).await;}
                    json!({"task":"general","minimum_tier":"balanced","fresh_information":false,"tools":false,"vision":false,
                        "structured_output":false,"confidence":0.95,"uncertain":false}).to_string()
                };
                Json(json!({"content":content,"routing_report":super::routing_mock_report(&body)}))
            }}
        })).route("/generate",axum::routing::post({
            let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();
            move |Json(body):Json<Value>| {let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);
                let routing=&body["overrides"]["routing"];assert_eq!(routing["purpose"],"final");
                if mode.load(Ordering::SeqCst)==2 {entered.notify_one();tokio::time::sleep(Duration::from_secs(4)).await;}
                let target=&routing["assignments"]["final"];
                format!("{}\n{}\n",json!({"type":"token","delta":"Browser answer"}),json!({"type":"done","model":target["model"],
                    "finish_reason":"stop","usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)}))
            }}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
        let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
        let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{address}");
        let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot)).build();
        let uid=Uuid::now_v7();sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Browser routing','user')")
            .bind(uid).bind(format!("{uid}@routing.test")).execute(pool).await?;
        let actor=AuthContext {user_id:Some(uid),role:PlatformRole::User,email:None,display_name:None,break_glass:false,mfa_enroll_only:false};
        let local=Uuid::now_v7();let cloud=Uuid::now_v7();
        for (id,model,locality,cost) in [(local,"browser-local","local",0),(cloud,"browser-cloud","cloud",1000)] {
            sqlx::query("INSERT INTO provider_configs(id,role,scope,model,label,base_url) VALUES($1,'llm','deployment',$2,$2,'http://browser.test/v1')")
                .bind(id).bind(model).execute(pool).await?;
            let profile:ModelProfile=serde_json::from_value(json!({"provider_id":id,"provider_revision":1,"revision":1,"model":model,"adapter":"openai_chat",
                "locality":locality,"locality_trusted":true,"auto_enabled":true,"quality_status":"declared","tier":"balanced",
                "pricing":{"input_microusd_per_million":cost,"output_microusd_per_million":cost},
                "capabilities":{"context_tokens":32000,"max_output_tokens":8192,"structured_output":"declared","efforts":["high"]}}))?;
            store::save_profile(&state,admin,id,store::SaveProfile {expected_revision:0,profile}).await?;
        }
        let (policy_id,expected_revision)=sqlx::query_as::<_,(Uuid,i64)>("SELECT id,revision FROM routing_policies WHERE scope='deployment' AND is_default")
            .fetch_optional(pool).await?.unwrap_or_else(||(Uuid::now_v7(),0));
        let policy:RoutingPolicy=serde_json::from_value(json!({"id":policy_id,"revision":1,"strategy":"classifier","rollout":"active",
            "default_provider_id":cloud,"classifier_provider_id":local,"constraints":{"allowed_providers":[local,cloud],"max_calls":12,"max_duration_ms":15000}}))?;
        let mut policy=store::save_policy(&state,admin,policy_id,store::SavePolicy {label:"Browser policy".into(),scope:"deployment".into(),expected_revision,is_default:true,policy}).await?;
        let auto=ModelSelection::Auto {objective:RoutingObjective::Economy,policy_id:None};
        assert!(attempt(&state,&actor,request(None,Some(auto.clone()),None)).await?.0.is_err());assert_eq!(calls.load(Ordering::SeqCst),0);
        use fosnie_backend::http::voice_routing::Enabled;
        assert!(browser::set_enabled(State(state.clone()),AuthUser(actor.clone()),Json(Enabled {enabled:true})).await.is_err());
        let _=browser::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:true})).await?;
        let (result,frames)=attempt(&state,&actor,request(None,Some(auto.clone()),None)).await?;let chat=result?;
        assert!(!frames.iter().any(|f|f["type"]=="chat.error"),"{frames:?}");assert!(frames.iter().any(|f|f["type"]=="chat.completed"));
        assert_eq!(judges.load(Ordering::SeqCst),1);assert_eq!(titles.load(Ordering::SeqCst),1);
        let saved=browser::saved(State(state.clone()),AuthUser(actor.clone()),Path(chat)).await?.0;
        assert_eq!(saved["selection"],json!(auto));assert_eq!(saved["adopted"],true);
        let messages=fosnie_backend::http::chats::list_messages(State(state.clone()),AuthUser(actor.clone()),Path(chat)).await?.0;
        let messages=serde_json::to_value(messages)?;let metadata=&messages.as_array().unwrap().last().unwrap()["routing"];
        assert_eq!(metadata["actual"]["model"],"browser-local");assert_eq!(metadata["selection_mode"],"auto");
        assert!(!metadata.to_string().contains("provider_id") && !metadata.to_string().contains("api_key"));
        let inherited=attempt(&state,&actor,request(Some(chat),None,None)).await?;inherited.0?;
        assert!(!inherited.1.iter().any(|f|f["type"]=="chat.error"),"{:?}",inherited.1);assert_eq!(judges.load(Ordering::SeqCst),2);
        let options=RoutingOptions {local_only:true,effort:Some("high".into())};
        let before=judges.load(Ordering::SeqCst);
        let changed=attempt(&state,&actor,request(Some(chat),Some(ModelSelection::Manual {provider_id:local}),Some(options.clone()))).await?;
        changed.0?;assert!(!changed.1.iter().any(|f|f["type"]=="chat.error"),"{:?}",changed.1);assert_eq!(judges.load(Ordering::SeqCst),before);
        let answer:Uuid=sqlx::query_scalar("SELECT id FROM messages WHERE chat_id=$1 AND role='assistant' ORDER BY sequence_number DESC LIMIT 1").bind(chat).fetch_one(pool).await?;
        let metadata=browser::message_metadata(&state,&actor,&[answer]).await?;
        assert_eq!(metadata[&answer]["actual"]["effort"],"high");assert_eq!(metadata[&answer]["local_only"],true);
        // Local-only is sticky when omitted and cannot escape via explicit Default.
        let before=calls.load(Ordering::SeqCst);
        assert!(attempt(&state,&actor,request(Some(chat),Some(ModelSelection::Default),None)).await?.0.is_err());assert_eq!(calls.load(Ordering::SeqCst),before);
        let reset=attempt(&state,&actor,request(Some(chat),Some(ModelSelection::Default),Some(RoutingOptions::default()))).await?;reset.0?;
        assert!(!reset.1.iter().any(|f|f["type"]=="chat.error"),"{:?}",reset.1);
        assert!(fosnie_backend::model_routing::admission::require_legacy_chat(&state,Some(chat)).await.is_err());
        // One eligible candidate with no classify rule bypasses the judge.
        let before=judges.load(Ordering::SeqCst);let single=empty(&state,&actor).await?;
        let single_result=attempt(&state,&actor,request(Some(single),Some(auto.clone()),Some(RoutingOptions {local_only:true,effort:None}))).await?;single_result.0?;
        assert!(!single_result.1.iter().any(|f|f["type"]=="chat.error"),"{:?}",single_result.1);assert_eq!(judges.load(Ordering::SeqCst),before);
        let (answer,intent,decision):(Uuid,Value,Value)=sqlx::query_as("SELECT id,routing_selection,routing_decision FROM messages WHERE chat_id=$1 AND role='assistant'").bind(single).fetch_one(pool).await?;
        assert_eq!(intent,json!(auto));assert!(decision["reason_codes"].as_array().unwrap().contains(&json!("single_eligible_model")));
        // Regeneration carries answer options into both branch and new message.
        let _=fosnie_backend::chat::regenerate::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:true})).await?;
        let (tx,mut rx)=mpsc::channel(128);let job=tokio::spawn(fosnie_backend::chat::regenerate::execute(state.clone(),actor.clone(),single,answer,Uuid::now_v7(),tx,Arc::new(Notify::new())));
        let mut frames=vec![];while let Some(f)=tokio::time::timeout(Duration::from_secs(10),rx.recv()).await? {frames.push(serde_json::to_value(f)?);}
        let branch=job.await??;assert!(!frames.iter().any(|f|f["type"]=="chat.error"),"{frames:?}");
        let branch_options:Value=sqlx::query_scalar("SELECT routing_options FROM chats WHERE id=$1").bind(branch).fetch_one(pool).await?;assert_eq!(branch_options["local_only"],true);
        assert_eq!(browser::saved(State(state.clone()),AuthUser(actor.clone()),Path(branch)).await?.0["supported"],false);
        // Shadow displays the default dispatch, never the proposed local target.
        policy.rollout=RolloutMode::Shadow;policy.strategy=RoutingStrategy::Rules;
        policy.rules=vec![RoutingRule {id:Uuid::now_v7(),priority:10,conditions:Default::default(),action:RuleAction::Select {provider_id:local,effort:None}}];
        policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Browser shadow".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy}).await?;
        let shadow=empty(&state,&actor).await?;attempt(&state,&actor,request(Some(shadow),Some(auto.clone()),None)).await?.0?;
        let answer:Uuid=sqlx::query_scalar("SELECT id FROM messages WHERE chat_id=$1 AND role='assistant'").bind(shadow).fetch_one(pool).await?;
        assert_eq!(browser::message_metadata(&state,&actor,&[answer]).await?[&answer]["actual"]["model"],"browser-cloud");
        // Hide retired/private targets without rewriting the historical decision.
        sqlx::query("UPDATE provider_configs SET scope='user',scope_id=$2 WHERE id=$1").bind(cloud).bind(admin.user_id).execute(pool).await?;
        let hidden=browser::message_metadata(&state,&actor,&[answer]).await?;assert!(hidden[&answer]["actual"].is_null());assert_eq!(hidden[&answer]["actual_unavailable"],true);
        assert!(browser::message_metadata(&state,admin,&[answer]).await?.is_empty(),"another owner receives no browser metadata");
        sqlx::query("UPDATE provider_configs SET scope='deployment',scope_id=NULL WHERE id=$1").bind(cloud).execute(pool).await?;
        policy.rollout=RolloutMode::Active;policy.strategy=RoutingStrategy::Classifier;policy.rules.clear();
        let _=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Browser current".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy}).await?;
        // Wrong owner/role/effort fail before inference. Gate, role and cancellation
        // changes during planning/output suppress all late frames.
        let before=calls.load(Ordering::SeqCst);
        assert!(attempt(&state,admin,request(Some(chat),Some(auto.clone()),None)).await?.0.is_err());
        assert!(attempt(&state,&actor,request(Some(chat),Some(ModelSelection::Manual {provider_id:local}),Some(RoutingOptions {local_only:false,effort:Some("invented".into())}))).await?.0.is_err());
        let mut stale=actor.clone();stale.role=PlatformRole::PowerUser;assert!(attempt(&state,&stale,request(None,Some(auto.clone()),None)).await?.0.is_err());
        assert_eq!(calls.load(Ordering::SeqCst),before);
        for fault in 0..5 {
            let chat=empty(&state,&actor).await?;mode.store(if fault<3 {1} else {2},Ordering::SeqCst);
            let (tx,mut rx)=mpsc::channel(128);let cancel=Arc::new(Notify::new());let before=calls.load(Ordering::SeqCst);
            let selection=if fault<3 {auto.clone()} else {ModelSelection::Manual {provider_id:local}};
            let job=tokio::spawn(browser::execute(state.clone(),actor.clone(),request(Some(chat),Some(selection),None),Uuid::now_v7(),tx,cancel.clone()));
            tokio::time::timeout(Duration::from_secs(5),entered.notified()).await?;
            if fault==0 {
                assert!(attempt(&state,&actor,request(Some(chat),Some(ModelSelection::Default),None)).await?.0.is_err(),"lease prevents concurrent selection changes");
                cancel.notify_one();
            } else if fault==1 || fault==3 {let _=browser::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:false})).await?;}
            else if fault==2 {sqlx::query("UPDATE users SET role='power_user' WHERE id=$1").bind(uid).execute(pool).await?;}
            else {rx.close();}
            assert!(tokio::time::timeout(Duration::from_secs(3),job).await??.is_err());assert_eq!(calls.load(Ordering::SeqCst)-before,1);
            while let Ok(frame)=rx.try_recv() {let f=serde_json::to_value(frame)?;assert_ne!(f["type"],"chat.token");assert_ne!(f["type"],"chat.completed");}
            if fault==1 || fault==3 {let _=browser::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:true})).await?;}
            if fault==2 {sqlx::query("UPDATE users SET role='user' WHERE id=$1").bind(uid).execute(pool).await?;}
        }
        mode.store(0,Ordering::SeqCst);let _=browser::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:false})).await?;
        let _=fosnie_backend::chat::regenerate::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(Enabled {enabled:false})).await?;
        server.abort();Ok(())
    })
}
