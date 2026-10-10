// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{future::Future,pin::Pin,sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}},time::Duration};
use axum::{body::{Body,Bytes,to_bytes},extract::{Query,State},http::HeaderMap,response::{IntoResponse,Response},Json};
use serde_json::{json,Value};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,keycloak::AuthUser},config::BootConfig,state::AppStateBuilder,
    http::voice_routing as http,model_routing::{*,fixed::FixedRole,run::RoutedRun}};

struct Features(Arc<AtomicBool>);
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Features {
    async fn enabled_for_user(&self,_:&AppState,_:Option<Uuid>,feature:&str)->bool {matches!(feature,"voice"|"voice_live"|"telephony") && self.0.load(Ordering::SeqCst)}
}
struct Deny;
#[async_trait::async_trait]
impl InferencePolicy for Deny {
    async fn constraints(&self,_:&sqlx::PgPool,_:&RoutingContext)->fosnie_backend::Result<RoutingConstraints> {
        Ok(RoutingConstraints {allowed_providers:Some(Default::default()),..Default::default()})
    }
}
struct AudioConstraint(RoutingConstraints);
#[async_trait::async_trait]
impl InferencePolicy for AudioConstraint {
    async fn constraints(&self,_:&sqlx::PgPool,context:&RoutingContext)->fosnie_backend::Result<RoutingConstraints> {
        Ok(if context.purpose==CallPurpose::Stt {self.0.clone()} else {RoutingConstraints::default()})
    }
}
async fn gate(pool:&sqlx::PgPool,on:bool)->anyhow::Result<()> {
    sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES('routing.voice_rest_enabled',$1,'bool') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,value_type='bool'")
        .bind(if on {"true"} else {"false"}).execute(pool).await?;Ok(())
}
async fn request(state:AppState,actor:AuthContext,speech:bool)->anyhow::Result<fosnie_backend::Result<Response>> {
    Ok(tokio::spawn(async move {
        if speech {http::speak(State(state),AuthUser(actor),Json(http::SpeakRequest {text:"Synthetic read aloud".into(),voice:Some("test".into())})).await}
        else {http::transcribe(State(state),AuthUser(actor),Query(http::TranscribeQuery {mime:Some("audio/wav".into()),language:Some("en-GB".into())}),Bytes::from_static(b"audio")).await.map(IntoResponse::into_response)}
    }).await?)
}
fn report(routing:&Value,role:&str,mode:usize)->Value {
    let target=&routing["fixed_assignments"][role];
    let mut report=json!({"lease_id":routing["fixed_budget"]["id"],"complete":true,"calls":[{
        "call_id":Uuid::now_v7(),"lease_id":routing["fixed_budget"]["id"],"decision_id":routing["decision_id"],"role":role,
        "provider_id":target["provider_id"],"provider_revision":target["provider_revision"],"profile_revision":target["profile_revision"],
        "active_index_revision":null,"desired_index_revision":null,"status":"completed","latency_ms":1,"input_bytes":1000,"output_bytes":if role=="tts" {5} else {100}}]});
    match mode {
        1=>report["calls"]=json!([]),2=>report["calls"][0]["status"]=json!("failed"),
        3=>report["calls"][0]["provider_id"]=json!(Uuid::now_v7()),
        5=>report["calls"][0]["output_bytes"]=json!(1),
        7=>report["calls"][0]["input_bytes"]=json!(u32::MAX),_=>(),
    };report
}
async fn receipt(pool:&sqlx::PgPool,routing:&Value) {
    // Auto creates a selection decision within the original run; decision ID
    // is not run ID. Correlate the exact dispatch lease across both phases.
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type='routing.audio.started' AND model_agent_traceability->>'lease_id'=$1")
        .bind(routing["fixed_budget"]["id"].as_str()).fetch_one(pool).await.unwrap();
    assert_eq!(count,1,"audio requires its exact durable receipt before dispatch");
    assert!(routing["assignments"].as_object().unwrap().is_empty(),"no chat model required for dictation/read aloud");
}
fn context(actor:&AuthContext)->RoutingContext {RoutingContext {actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Stt}}
pub fn check<'a>(pool:&'a sqlx::PgPool,admin:&'a AuthContext,provider:Uuid)->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
        let calls=Arc::new(AtomicUsize::new(0));let mode=Arc::new(AtomicUsize::new(0));let entered=Arc::new(tokio::sync::Notify::new());
        let app=axum::Router::new().route("/transcribe",axum::routing::post({
            let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();let pool=pool.clone();
            move |headers:HeaderMap,Query(query):Query<std::collections::HashMap<String,String>>,body:Bytes| {
                let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();let pool=pool.clone();async move {
                    if mode.load(Ordering::SeqCst)>=9 {assert!(body.starts_with(b"RIFF"));} else {assert_eq!(&body[..],b"audio");}
                    assert_eq!(headers["content-type"],"audio/wav");
                    if let Some(lang)=query.get("language") {assert_eq!(lang,"en-GB");}
                    let overrides:Value=serde_json::from_str(headers["x-fosnie-overrides"].to_str().unwrap()).unwrap();
                    let routing=&overrides["routing"];receipt(&pool,routing).await;
                    calls.fetch_add(1,Ordering::SeqCst);let mode=mode.load(Ordering::SeqCst);
                    if matches!(mode,6|10) {entered.notify_one();tokio::time::sleep(Duration::from_secs(3)).await;}
                    let mut value=json!({"text":"Synthetic transcript","fixed_routing_report":report(routing,"stt",mode)});
                    if mode==4 {value.as_object_mut().unwrap().remove("fixed_routing_report");}
                    Json(value)
                }
            }
        })).route("/speech",axum::routing::post({
            let calls=calls.clone();let mode=mode.clone();let pool=pool.clone();let entered=entered.clone();
            move |Json(body):Json<Value>| {let calls=calls.clone();let mode=mode.clone();let pool=pool.clone();let entered=entered.clone();async move {
                if mode.load(Ordering::SeqCst)<17 {assert_eq!(body["text"],"Synthetic read aloud");}
                else {assert!(!body["text"].as_str().unwrap().is_empty());}
                if mode.load(Ordering::SeqCst)<9 {assert_eq!(body["voice"],"test");}
                let routing=&body["overrides"]["routing"];receipt(&pool,routing).await;calls.fetch_add(1,Ordering::SeqCst);
                let mode=mode.load(Ordering::SeqCst);
                let pcm=mode>=12;
                if pcm {assert_eq!(body["response_format"],"wav");} else {assert!(body.get("response_format").is_none());}
                let bytes=if pcm && mode!=14 {fosnie_backend::voice::stt_stream::pcm_to_wav(&vec![0u8;2400],if mode==13 {16000} else {24000})} else {b"audio".to_vec()};
                let mut receipt=report(routing,"tts",mode);
                if pcm {receipt["calls"][0]["output_bytes"]=json!(bytes.len());}
                if mode==15 {receipt["calls"]=json!([]);}
                let mut response=([("content-type",if pcm && mode!=14 {"audio/wav"} else {"audio/mpeg"})],Body::from(bytes)).into_response();
                if matches!(mode,11|16|18) {entered.notify_one();tokio::time::sleep(Duration::from_secs(3)).await;}
                if mode!=4 {response.headers_mut().insert("x-fosnie-fixed-routing-report",receipt.to_string().parse().unwrap());}
                response
            }}
        })).route("/chat-step",axum::routing::post({
            let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();
            move |Json(body):Json<Value>| {let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);assert_eq!(body["overrides"]["routing"]["purpose"],"router_classify");
                if mode.load(Ordering::SeqCst)==8 {entered.notify_one();tokio::time::sleep(Duration::from_secs(3)).await;}
                Json(json!({"content":json!({"task":"general","minimum_tier":"economy","fresh_information":false,
                    "tools":false,"vision":false,"structured_output":false,"confidence":0.95,"uncertain":false}).to_string(),
                    "routing_report":super::routing_mock_report(&body)}))
            }}
        })).route("/generate",axum::routing::post({
            let calls=calls.clone();let mode=mode.clone();
            move |Json(body):Json<Value>| {let calls=calls.clone();let mode=mode.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);
                if mode.load(Ordering::SeqCst)==20 {
                    let messages=body["messages"].to_string();
                    assert!(messages.contains("Earlier context") && messages.contains("Earlier answer") && messages.contains("Repeat this question"));
                    assert!(!messages.contains("OLD ANSWER MUST STAY") && !messages.contains("LATER TURN MUST NOT ENTER"));
                }
                let token=if mode.load(Ordering::SeqCst)>=9 {format!("{}\n",json!({"type":"token","delta":"Synthetic read aloud"}))} else {String::new()};
                format!("{token}{}\n",json!({"type":"done","text":"Synthetic read aloud","finish_reason":"stop","routing_report":super::routing_mock_report(&body)}))
            }}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
        let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
        let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{address}");
        let features=Arc::new(AtomicBool::new(true));
        let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
            .with_features(Arc::new(Features(features.clone()))).build();
        let uid=Uuid::now_v7();sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Voice routing','user')")
            .bind(uid).bind(format!("{uid}@routing.test")).execute(pool).await?;
        let actor=AuthContext {user_id:Some(uid),role:fosnie_backend::auth::PlatformRole::User,email:None,display_name:None,break_glass:false,mfa_enroll_only:false};
        let mut policy=store::load_policy(&state,admin,None).await?;
        policy.default_provider_id=None;policy.rollout=RolloutMode::Off;policy.strategy=RoutingStrategy::Rules;policy.rules.clear();
        policy.constraints=RoutingConstraints {max_calls:Some(2),max_duration_ms:Some(15_000),..Default::default()};
        let mut policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Fixed voice".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy}).await?;
        assert!(request(state.clone(),actor.clone(),false).await?.is_err());assert_eq!(calls.load(Ordering::SeqCst),0);
        assert!(http::set_enabled(State(state.clone()),AuthUser(actor.clone()),Json(http::Enabled {enabled:true})).await.is_err());
        let settings=http::set_enabled(State(state.clone()),AuthUser(admin.clone()),Json(http::Enabled {enabled:true})).await?.0;
        assert_eq!(settings["enabled"],true);assert_eq!(settings["live_voice"],false);assert_eq!(settings["telephony"],false);
        let response=request(state.clone(),actor.clone(),false).await??;
        let value:Value=serde_json::from_slice(&to_bytes(response.into_body(),1_000_000).await?)?;
        assert_eq!(value["text"],"Synthetic transcript");assert_eq!(value["fosnie_routing"]["usage"]["calls"],1);
        assert_eq!(value["fosnie_routing"]["usage"]["unknown_token_calls"],1);
        assert_eq!(value["fosnie_routing"]["usage"]["unknown_cost_calls"],1);
        let response=request(state.clone(),actor.clone(),true).await??;
        assert_eq!(response.headers()["content-type"],"audio/mpeg");assert_eq!(response.headers()["x-fosnie-routing-calls"],"1");
        assert_eq!(&to_bytes(response.into_body(),100).await?[..],b"audio");
        super::voice_pcm_cases::check(&state,&actor,&calls,&mode,&entered).await?;
        mode.store(0,Ordering::SeqCst);
        // Strict reports cannot refund an attempted request or fake success.
        for fault in [1,2,3,4,5,7] {
            mode.store(fault,Ordering::SeqCst);
            for speech in [false,true] {assert!(request(state.clone(),actor.clone(),speech).await?.is_err(),"fault {fault} speech {speech}");}
        }
        mode.store(0,Ordering::SeqCst);
        let run=Box::pin(RoutedRun::fixed_voice(&state,context(&actor),&[FixedRole::Stt,FixedRole::Tts],&tokio::sync::Notify::new())).await?;
        Box::pin(run.transcribe(&state,b"audio".to_vec(),"audio/wav",None)).await?;
        Box::pin(run.speech(&state,"Synthetic read aloud",Some("test"))).await?;
        assert_eq!(run.usage().calls,2);let before=calls.load(Ordering::SeqCst);
        assert!(Box::pin(run.transcribe(&state,b"audio".to_vec(),"audio/wav",None)).await.is_err());assert_eq!(calls.load(Ordering::SeqCst),before);
        let run=Box::pin(RoutedRun::fixed_voice(&state,context(&actor),&[FixedRole::Stt],&tokio::sync::Notify::new())).await?;
        assert!(Box::pin(run.transcribe(&state,vec![],"audio/wav",None)).await.is_err());
        assert!(Box::pin(run.transcribe(&state,b"audio".to_vec(),"audio/wav",Some("bad language"))).await.is_err());
        assert!(Box::pin(run.speech(&state,"Synthetic read aloud",Some("test"))).await.is_err(),"unpinned TTS cannot dispatch");
        assert_eq!(calls.load(Ordering::SeqCst),before);
        let mut denied=state.clone();denied.inference_policy=Arc::new(Deny);
        assert!(request(denied,actor.clone(),false).await?.is_err());
        features.store(false,Ordering::SeqCst);assert!(request(state.clone(),actor.clone(),false).await?.is_err());features.store(true,Ordering::SeqCst);
        assert_eq!(calls.load(Ordering::SeqCst),before);
        // Idle revocation drops the in-flight transport before it can publish.
        mode.store(6,Ordering::SeqCst);
        let pending=tokio::spawn({let state=state.clone();let actor=actor.clone();async move {request(state,actor,false).await}});
        tokio::time::timeout(Duration::from_secs(4),entered.notified()).await?;gate(pool,false).await?;
        assert!(tokio::time::timeout(Duration::from_secs(2),pending).await???.is_err());gate(pool,true).await?;
        let pending=tokio::spawn({let state=state.clone();let actor=actor.clone();async move {request(state,actor,false).await}});
        tokio::time::timeout(Duration::from_secs(4),entered.notified()).await?;
        sqlx::query("UPDATE users SET deactivated_at=now() WHERE id=$1").bind(uid).execute(pool).await?;
        assert!(tokio::time::timeout(Duration::from_secs(2),pending).await???.is_err());
        sqlx::query("UPDATE users SET deactivated_at=NULL WHERE id=$1").bind(uid).execute(pool).await?;
        // Cancellation leaves a durable unmatched start and conservative spend.
        let run=Box::pin(RoutedRun::fixed_voice(&state,context(&actor),&[FixedRole::Stt],&tokio::sync::Notify::new())).await?;
        let pending=tokio::spawn({let state=state.clone();let run=run.clone();async move {run.transcribe(&state,b"audio".to_vec(),"audio/wav",None).await}});
        tokio::time::timeout(Duration::from_secs(4),entered.notified()).await?;run.cancel();
        assert!(tokio::time::timeout(Duration::from_secs(2),pending).await??.is_err());assert_eq!(run.usage().calls,1);
        let unmatched:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events s WHERE s.action_type='routing.audio.started' AND s.model_agent_traceability->>'run_id'=$1 AND NOT EXISTS(SELECT 1 FROM audit_events f WHERE f.action_type='routing.audio.finished' AND f.resource_id=s.resource_id)")
            .bind(run.plan().id.to_string()).fetch_one(pool).await?;assert_eq!(unmatched,1);
        mode.store(0,Ordering::SeqCst);
        // One owned STT -> classifier -> final -> TTS ledger, including the
        // history-authorizing chat helper. No fresh budget at transcript handoff.
        policy.default_provider_id=Some(provider);policy.classifier_provider_id=Some(provider);
        policy.rollout=RolloutMode::Active;policy.strategy=RoutingStrategy::Classifier;policy.constraints.max_calls=Some(4);
        policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Voice conversation".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone()}).await?;
        let chat=Uuid::now_v7();sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Voice conversation')").bind(chat).bind(uid).execute(pool).await?;
        for selection in [ModelSelection::Default,ModelSelection::Manual {provider_id:provider},ModelSelection::Auto {objective:RoutingObjective::Economy,policy_id:Some(policy.id)}] {
            let mut context=context(&actor);context.chat_id=Some(chat);let id=context.run_id;
            let expected_calls=if matches!(selection,ModelSelection::Auto {..}) {4} else {3};
            let before=calls.load(Ordering::SeqCst);
            let run=Box::pin(RoutedRun::pending_voice(&state,context,selection.clone(),&[FixedRole::Stt,FixedRole::Tts],&tokio::sync::Notify::new())).await?;
            assert_eq!(calls.load(Ordering::SeqCst),before,"pending phase never calls the judge");
            let pins=run.plan().fixed_assignments.clone();
            Box::pin(run.transcribe(&state,b"audio".to_vec(),"audio/wav",None)).await?;
            let run=Box::pin(fosnie_backend::chat::routing::prepare_voice(&state,run,"Synthetic transcript",
                WorkloadRequirements {output_tokens:64,..Default::default()},&[],&tokio::sync::Notify::new())).await?;
            assert_eq!(run.plan().id,id);assert_eq!(run.plan().fixed_assignments,pins);
            assert_eq!(run.context(CallPurpose::Primary).chat_id,Some(chat));
            assert_eq!(run.context(CallPurpose::Primary).actor.user_id,Some(uid));
            let mut stream=Box::pin(run.generate(&state,fosnie_backend::ml::GenerateRequest {messages:vec![json!({"role":"user","content":"Synthetic transcript"})],
                sampling:fosnie_backend::ml::Sampling {max_tokens:Some(64),..Default::default()},model:None,tools:None,overrides:Default::default()})).await?;
            assert_eq!(stream.recv().await?.unwrap()["type"],"done");drop(stream);
            Box::pin(run.speech(&state,"Synthetic read aloud",Some("test"))).await?;
            assert_eq!(run.usage().calls,expected_calls);assert_eq!(run.usage().unknown_token_calls,2);
            assert_eq!(run.usage().tokens,u64::from(expected_calls-2)*140);
            assert_eq!(run.usage().unknown_cost_calls,expected_calls,"fixture LLM pricing is unknown too");
            assert_eq!(calls.load(Ordering::SeqCst)-before,expected_calls as usize);
            assert!(Box::pin(run.select_voice(&state,WorkloadRequirements::default(),"again",&[],&tokio::sync::Notify::new())).await.is_err(),"cannot select twice");
        }
        let run=Box::pin(RoutedRun::pending_voice(&state,context(&actor),ModelSelection::Default,&[FixedRole::Stt,FixedRole::Tts],&tokio::sync::Notify::new())).await?;
        let shared=run.clone();let before=calls.load(Ordering::SeqCst);
        assert!(Box::pin(run.select_voice(&state,WorkloadRequirements::default(),"fork",&[],&tokio::sync::Notify::new())).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst),before);drop(shared);
        // Cancellation during classification retains STT plus the judge's
        // reservation in its durable start receipt; no generation follows.
        let run=Box::pin(RoutedRun::pending_voice(&state,context(&actor),ModelSelection::Auto {objective:RoutingObjective::Economy,policy_id:Some(policy.id)},
            &[FixedRole::Stt,FixedRole::Tts],&tokio::sync::Notify::new())).await?;
        let id=run.plan().id;Box::pin(run.transcribe(&state,b"audio".to_vec(),"audio/wav",None)).await?;
        mode.store(8,Ordering::SeqCst);let cancel=Arc::new(tokio::sync::Notify::new());
        let pending=tokio::spawn({let state=state.clone();let cancel=cancel.clone();async move {
            run.select_voice(&state,WorkloadRequirements {output_tokens:64,..Default::default()},"Synthetic transcript",&[],&cancel).await
        }});
        tokio::time::timeout(Duration::from_secs(4),entered.notified()).await?;cancel.notify_one();
        assert!(tokio::time::timeout(Duration::from_secs(2),pending).await??.is_err());mode.store(0,Ordering::SeqCst);
        let receipt:Value=sqlx::query_scalar("SELECT token_usage FROM audit_events WHERE action_type='routing.classifier.started' AND model_agent_traceability->>'run_id'=$1")
            .bind(id.to_string()).fetch_one(pool).await?;
        assert_eq!(receipt["ledger_snapshot"]["calls"],2);assert_eq!(receipt["ledger_snapshot"]["unknown_token_calls"],1);
        let fixed_ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM provider_configs WHERE role IN ('stt','tts') AND scope='deployment'").fetch_all(pool).await?;
        let mut restricted=state.clone();restricted.inference_policy=Arc::new(AudioConstraint(RoutingConstraints {
            allowed_providers:Some(fixed_ids.into_iter().collect()),..Default::default()}));
        let run=Box::pin(RoutedRun::pending_voice(&restricted,context(&actor),ModelSelection::Default,&[FixedRole::Stt,FixedRole::Tts],&tokio::sync::Notify::new())).await?;
        Box::pin(run.transcribe(&restricted,b"audio".to_vec(),"audio/wav",None)).await?;let before=calls.load(Ordering::SeqCst);
        assert!(Box::pin(run.select_voice(&restricted,WorkloadRequirements {output_tokens:64,..Default::default()},"Synthetic transcript",&[],&tokio::sync::Notify::new())).await.is_err(),
            "primary-purpose policy cannot relax restrictions already accepted for the run");
        assert_eq!(calls.load(Ordering::SeqCst),before);
        let mut timed=state.clone();timed.inference_policy=Arc::new(AudioConstraint(RoutingConstraints {max_duration_ms:Some(1500),..Default::default()}));
        let run=Box::pin(RoutedRun::pending_voice(&timed,context(&actor),ModelSelection::Auto {objective:RoutingObjective::Economy,policy_id:Some(policy.id)},
            &[FixedRole::Stt,FixedRole::Tts],&tokio::sync::Notify::new())).await?;
        Box::pin(run.transcribe(&timed,b"audio".to_vec(),"audio/wav",None)).await?;
        tokio::time::sleep(Duration::from_millis(1600)).await;let before=calls.load(Ordering::SeqCst);
        assert!(Box::pin(run.select_voice(&timed,WorkloadRequirements {output_tokens:64,..Default::default()},"Synthetic transcript",&[],&tokio::sync::Notify::new())).await.is_err(),
            "selection must not restart the audio phase deadline");
        assert_eq!(calls.load(Ordering::SeqCst),before);
        super::voice_live_cases::check(&state,&actor,admin,policy.id,&calls,&mode,&entered).await?;
        super::phone_cases::check(&state,&actor,admin,policy.id,&calls,&mode,&entered).await?;
        policy=super::regenerate_cases::check(&state,&actor,admin,provider,policy,&calls,&mode,&entered).await?;
        super::artefact_cases::check(&state,&actor,admin,provider,policy.id).await?;
        mode.store(0,Ordering::SeqCst);
        // Policy revision and hard token budgets fail before any new dispatch.
        let stale=Box::pin(RoutedRun::fixed_voice(&state,context(&actor),&[FixedRole::Stt],&tokio::sync::Notify::new())).await?;
        let stale_selection=Box::pin(RoutedRun::pending_voice(&state,context(&actor),ModelSelection::Auto {objective:RoutingObjective::Economy,policy_id:Some(policy.id)},
            &[FixedRole::Stt,FixedRole::Tts],&tokio::sync::Notify::new())).await?;
        let mut limited=policy.clone();limited.constraints.max_total_tokens=Some(1000);
        store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Fixed limits".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:limited}).await?;
        let before=calls.load(Ordering::SeqCst);
        assert!(Box::pin(stale.transcribe(&state,b"audio".to_vec(),"audio/wav",None)).await.is_err());
        assert!(Box::pin(stale_selection.select_voice(&state,WorkloadRequirements {output_tokens:64,..Default::default()},"Synthetic transcript",&[],&tokio::sync::Notify::new())).await.is_err());
        assert!(request(state.clone(),actor.clone(),false).await?.is_err());assert_eq!(calls.load(Ordering::SeqCst),before);
        assert!(serde_json::from_value::<http::SpeakRequest>(json!({"text":"test","model_selection":{"mode":"auto"}})).is_err());
        let audit:Vec<(Option<Value>,Option<Value>)>=sqlx::query_as("SELECT model_agent_traceability,token_usage FROM audit_events WHERE actor_user_id=$1 AND action_type LIKE 'routing.audio.%'")
            .bind(uid).fetch_all(pool).await?;
        let audit=serde_json::to_string(&audit)?;assert!(!audit.contains("Synthetic") && !audit.contains("api_key"));
        gate(pool,false).await?;server.abort();Ok(())
    })
}
