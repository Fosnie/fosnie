// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{pin::Pin,future::Future,sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}},time::Duration};
use axum::{body::{Body,to_bytes},http::{Request,StatusCode},response::Response};
use serde_json::{Value,json};
use tower::ServiceExt;
use uuid::Uuid;
use fosnie_backend::{AppState,auth::AuthContext,config::BootConfig,state::AppStateBuilder,
    model_routing::{*,store}};

struct Features(Arc<AtomicBool>);
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Features {
    async fn enabled_for_user(&self,_:&AppState,_:Option<Uuid>,feature:&str)->bool {
        feature=="public_api" && !self.0.load(Ordering::SeqCst)
    }
}
struct Deny;
#[async_trait::async_trait]
impl InferencePolicy for Deny {
    async fn constraints(&self,_:&sqlx::PgPool,_:&RoutingContext)->fosnie_backend::Result<RoutingConstraints> {
        Ok(RoutingConstraints {allowed_providers:Some(Default::default()),..Default::default()})
    }
}
async fn request(state:AppState,token:String,body:Value)->anyhow::Result<Response> {
    // One request per task bounds the Windows test stack.
    Ok(tokio::spawn(async move {
        fosnie_backend::http::v1::router(&state).with_state(state).oneshot(Request::builder()
            .method("POST").uri("/v1/chat/completions").header("authorization",format!("Bearer {token}"))
            .header("content-type","application/json").body(Body::from(body.to_string())).unwrap()).await.unwrap()
    }).await?)
}
async fn value(response:Response)->anyhow::Result<Value> {Ok(serde_json::from_slice(&to_bytes(response.into_body(),1_000_000).await?)?)}
async fn gate(pool:&sqlx::PgPool,on:bool)->anyhow::Result<()> {
    sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES('routing.public_api_enabled',$1,'bool') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,value_type='bool'")
        .bind(if on {"true"} else {"false"}).execute(pool).await?;Ok(())
}
pub fn check<'a>(pool:&'a sqlx::PgPool,provider:Uuid,admin:&'a AuthContext)->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
        let calls=Arc::new(AtomicUsize::new(0));let mode=Arc::new(AtomicUsize::new(0));
        let entered=Arc::new(tokio::sync::Notify::new());
        let app=axum::Router::new().route("/generate",axum::routing::post({
            let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();
            move |axum::Json(body):axum::Json<Value>| {let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);entered.notify_one();
                let mode=mode.load(Ordering::SeqCst);
                let routed=!body["overrides"]["routing"].is_null();
                let mut done=json!({"type":"done","finish_reason":"stop","model":"catalog-model",
                    "usage":{"prompt_tokens":999,"completion_tokens":999}});
                if routed {done["routing_report"]=super::routing_mock_report(&body);}
                if mode==1 {done.as_object_mut().unwrap().remove("routing_report");}
                if mode==4 {done["routing_report"]["calls"][0]["provider_id"]=json!(Uuid::now_v7());}
                if mode==5 {done["routing_report"]["calls"]=json!([]);}
                if mode==6 {done["finish_reason"]=json!("tool_calls");}
                let lines=if mode==2 {vec![json!({"type":"token","delta":"partial"}).to_string()+"\n"]}
                    else if mode==7 {vec![json!({"type":"tool_call","name":"web_search","arguments":{}}).to_string()+"\n"]}
                    else {vec![json!({"type":"token","delta":"Answer"}).to_string()+"\n",done.to_string()+"\n"]};
                let stream=futures_util::stream::unfold((lines.into_iter(),0),move |(mut lines,index)|async move {
                    let line=lines.next()?;
                    if mode==3 && index==1 {tokio::time::sleep(Duration::from_secs(3)).await;}
                    Some((Ok::<_,std::convert::Infallible>(line),(lines,index+1)))
                });
                Body::from_stream(stream)
            }}
        })).route("/chat-step",axum::routing::post({
            let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();let pool=pool.clone();
            move |axum::Json(body):axum::Json<Value>| {let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();let pool=pool.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);
                let receipts:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type='routing.classifier.started' AND model_agent_traceability->>'run_id'=$1")
                    .bind(body["overrides"]["routing"]["decision_id"].as_str()).fetch_one(&pool).await.unwrap();
                assert_eq!(receipts,1,"classifier dispatch requires its durable reservation receipt");
                if mode.load(Ordering::SeqCst)==8 {entered.notify_one();tokio::time::sleep(Duration::from_secs(2)).await;}
                assert_eq!(body["overrides"]["routing"]["purpose"],"router_classify");
                axum::Json(json!({"content":json!({"task":"general","minimum_tier":"economy","fresh_information":false,
                    "tools":false,"vision":false,"structured_output":false,"confidence":0.95,"uncertain":false}).to_string(),
                    "routing_report":super::routing_mock_report(&body)}))
            }}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
        let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
        let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{address}");
        let disabled=Arc::new(AtomicBool::new(false));
        let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
            .with_features(Arc::new(Features(disabled.clone()))).build();
        let uid=Uuid::now_v7();let key=Uuid::now_v7();
        sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'API routing','user')")
            .bind(uid).bind(format!("{uid}@routing.test")).execute(pool).await?;
        let (token,hash,prefix)=fosnie_backend::auth::api_key::mint();
        sqlx::query("INSERT INTO api_keys(id,user_id,name,token_hash,display_prefix) VALUES($1,$2,'routing-test',$3,$4)")
            .bind(key).bind(uid).bind(hash).bind(prefix).execute(pool).await?;
        let mut policy=store::load_policy(&state,admin,None).await?;
        policy.default_provider_id=Some(provider);policy.rollout=RolloutMode::Off;policy.strategy=RoutingStrategy::Rules;
        policy.rules.clear();policy.constraints=RoutingConstraints {max_calls:Some(5),max_total_tokens:Some(100_000),..Default::default()};
        let policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"API test".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy}).await?;
        let body=||json!({"model_selection":{"mode":"default"},"messages":[{"role":"user","content":"Synthetic public API prompt"}]});
        assert_eq!(request(state.clone(),token.clone(),body()).await?.status(),StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(calls.load(Ordering::SeqCst),0);
        // Gate changes are permission checked through the production handler.
        let actor=AuthContext {user_id:Some(uid),role:fosnie_backend::auth::PlatformRole::User,email:None,display_name:None,break_glass:false,mfa_enroll_only:false};
        assert!(fosnie_backend::http::v1::routed::set_enabled(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(actor),axum::Json(fosnie_backend::http::v1::routed::Enabled {enabled:true})).await.is_err());
        gate(pool,true).await?;
        let discovery=fosnie_backend::http::v1::router(&state).with_state(state.clone()).oneshot(Request::builder().uri("/v1/routing")
            .header("authorization",format!("Bearer {token}")).body(Body::empty())?).await?;
        let discovery=value(discovery).await?;assert_eq!(discovery["scope"],"stateless_text_v1");assert_eq!(discovery["virtual_alias"],Value::Null);
        for selection in [json!({"mode":"default"}),json!({"mode":"manual","provider_id":provider}),json!({"mode":"auto","objective":"economy","policy_id":policy.id})] {
            let mut b=body();b["model_selection"]=selection.clone();let response=request(state.clone(),token.clone(),b).await?;
            let status=response.status();let response=value(response).await?;assert_eq!(status,StatusCode::OK,"{response}");
            assert_eq!(response["model"],"catalog-model");assert_eq!(response["choices"][0]["message"]["content"],"Answer");
            assert_eq!(response["usage"]["total_tokens"],140,"usage comes from the validated report");
            assert_eq!(response["fosnie_routing"]["selection"]["mode"],selection["mode"]);
            assert_eq!(response["fosnie_routing"]["decision"]["actual"]["provider_id"],provider.to_string());
            assert_eq!(response["fosnie_routing"]["usage"]["calls"],1);
        }
        let mut judge_policy=policy.clone();judge_policy.rollout=RolloutMode::Active;
        judge_policy.strategy=RoutingStrategy::Classifier;judge_policy.classifier_provider_id=Some(provider);
        let judge_policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"API judge".into(),scope:"deployment".into(),
            expected_revision:policy.revision,is_default:true,policy:judge_policy}).await?;
        let mut b=body();b["model_selection"]=json!({"mode":"auto"});
        let response=request(state.clone(),token.clone(),b).await?;let status=response.status();let response=value(response).await?;
        assert_eq!(status,StatusCode::OK,"{response}");
        assert_eq!(response["fosnie_routing"]["usage"]["calls"],2,"classifier and final share one ledger");
        assert_eq!(response["fosnie_routing"]["usage"]["tokens"],280);
        mode.store(8,Ordering::SeqCst);let _=entered.notified().now_or_never();
        let mut b=body();b["model_selection"]=json!({"mode":"auto"});
        let pending=tokio::spawn({let state=state.clone();let token=token.clone();async move {request(state,token,b).await}});
        tokio::time::timeout(Duration::from_secs(4),entered.notified()).await?;
        sqlx::query("UPDATE api_keys SET revoked_at=now() WHERE id=$1").bind(key).execute(pool).await?;
        let cancelled=tokio::time::timeout(Duration::from_secs(2),pending).await???;
        assert_ne!(cancelled.status(),StatusCode::OK);
        let unmatched:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events s WHERE s.actor_user_id=$1 AND s.action_type='routing.classifier.started' AND NOT EXISTS(SELECT 1 FROM audit_events f WHERE f.action_type='routing.classifier.finished' AND f.resource_id=s.resource_id)")
            .bind(uid).fetch_one(pool).await?;
        assert_eq!(unmatched,1,"cancelled planning leaves durable unknown spend rather than losing the classifier attempt");
        sqlx::query("UPDATE api_keys SET revoked_at=NULL WHERE id=$1").bind(key).execute(pool).await?;mode.store(0,Ordering::SeqCst);
        let mut exhausted=judge_policy.clone();exhausted.constraints.max_calls=Some(1);
        let exhausted=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"API exhausted".into(),scope:"deployment".into(),
            expected_revision:judge_policy.revision,is_default:true,policy:exhausted}).await?;
        let before=calls.load(Ordering::SeqCst);let mut b=body();b["model_selection"]=json!({"mode":"auto"});
        let response=request(state.clone(),token.clone(),b).await?;assert_eq!(response.status(),StatusCode::OK);
        let response=value(response).await?;
        assert_eq!(response["fosnie_routing"]["usage"]["calls"],1);
        assert!(response["fosnie_routing"]["decision"]["reason_codes"].as_array().unwrap().iter().any(|code|code.as_str().is_some_and(|code|code.starts_with("classifier_skipped"))));
        assert_eq!(calls.load(Ordering::SeqCst),before+1,"one-call allowance retains the answer and cannot dispatch a judge");
        store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"API default".into(),scope:"deployment".into(),
            expected_revision:exhausted.revision,is_default:true,policy:policy.clone()}).await?;
        let before=calls.load(Ordering::SeqCst);
        for patch in [json!({"model_selection":null}),json!({"model_selection":{"mode":"wrong"}}),json!({"model":"auto"}),
            json!({"max_tokens":0}),json!({"max_tokens":10,"max_completion_tokens":20}),
            json!({"stop":["x"]}),json!({"response_format":{"type":"json_object"}}),json!({"temperature":3}),
            json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.invalid/a.png"}}]}]})] {
            let mut b=body();for (k,v) in patch.as_object().unwrap() {b[k]=v.clone();}
            let response=request(state.clone(),token.clone(),b).await?;
            assert_eq!(response.status(),StatusCode::BAD_REQUEST);assert!(value(response).await?["error"].is_object());
        }
        let mut denied=state.clone();denied.inference_policy=Arc::new(Deny);
        assert_ne!(request(denied,token.clone(),body()).await?.status(),StatusCode::OK);
        let mut foreign=body();foreign["model_selection"]=json!({"mode":"manual","provider_id":Uuid::now_v7()});
        assert_ne!(request(state.clone(),token.clone(),foreign).await?.status(),StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst),before,"invalid requests never reach inference");
        for fault in [1,2,4,5,6,7] {
            mode.store(fault,Ordering::SeqCst);
            let response=request(state.clone(),token.clone(),body()).await?;
            assert_ne!(response.status(),StatusCode::OK,"fault {fault}");assert!(value(response).await?["error"].is_object());
        }
        // SSE has the same final decision/usage; a truncated stream has an error
        // and [DONE], never a fabricated successful finish_reason.
        for fault in [0,2] {
            mode.store(fault,Ordering::SeqCst);let mut b=body();b["stream"]=json!(true);
            let response=request(state.clone(),token.clone(),b).await?;assert_eq!(response.status(),StatusCode::OK);
            let wire=String::from_utf8(to_bytes(response.into_body(),1_000_000).await?.to_vec())?;
            assert!(wire.contains("[DONE]"));
            if fault==0 {assert!(wire.contains("fosnie_routing"));assert!(wire.contains("\"total_tokens\":140"));}
            else {assert!(wire.contains("\"error\""));assert!(!wire.contains("\"finish_reason\":\"stop\""));}
        }
        // Revocation during idle response consumption; one request, no replay.
        for revoke in 0..4 {
            mode.store(3,Ordering::SeqCst);
            let _=entered.notified().now_or_never();
            let state=state.clone();let token=token.clone();let b=body();
            let pending=tokio::spawn(async move {request(state,token,b).await});
            tokio::time::timeout(Duration::from_secs(5),entered.notified()).await?;
            match revoke {
                0=>{sqlx::query("UPDATE api_keys SET revoked_at=now() WHERE id=$1").bind(key).execute(pool).await?;},
                1=>gate(pool,false).await?,
                2=>disabled.store(true,Ordering::SeqCst),
                _=>{sqlx::query("UPDATE users SET role='power_user' WHERE id=$1").bind(uid).execute(pool).await?;},
            }
            let response=tokio::time::timeout(Duration::from_secs(2),pending).await???;
            assert_ne!(response.status(),StatusCode::OK);
            sqlx::query("UPDATE api_keys SET revoked_at=NULL WHERE id=$1").bind(key).execute(pool).await?;
            sqlx::query("UPDATE users SET role='user' WHERE id=$1").bind(uid).execute(pool).await?;
            gate(pool,true).await?;disabled.store(false,Ordering::SeqCst);
        }
        mode.store(3,Ordering::SeqCst);let mut b=body();b["stream"]=json!(true);
        let response=request(state.clone(),token.clone(),b).await?;drop(response);
        tokio::time::timeout(Duration::from_secs(3),async {
            loop {let n:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE actor_user_id=$1 AND action_type='api.completion.routed' AND model_agent_traceability->>'status'='cancelled'")
                .bind(uid).fetch_one(pool).await.unwrap();if n>0 {break;}tokio::time::sleep(Duration::from_millis(25)).await;}
        }).await?;
        // Existing labels retain direct semantics even while routing is off.
        gate(pool,false).await?;mode.store(0,Ordering::SeqCst);
        for label in ["auto","fosnie/auto"] {
            let id=Uuid::now_v7();sqlx::query("INSERT INTO provider_configs(id,role,scope,model,label,base_url) VALUES($1,'llm','deployment','catalog-model',$2,'http://legacy.test/v1')")
                .bind(id).bind(label).execute(pool).await?;
            let response=request(state.clone(),token.clone(),json!({"model":label,"messages":[{"role":"user","content":"Legacy"}]})).await?;
            assert_eq!(response.status(),StatusCode::OK);let response=value(response).await?;
            assert_eq!(response["model"],label);assert!(response.get("fosnie_routing").is_none());
            sqlx::query("DELETE FROM provider_configs WHERE id=$1").bind(id).execute(pool).await?;
        }
        let traces:Vec<Value>=sqlx::query_scalar("SELECT model_agent_traceability FROM audit_events WHERE actor_user_id=$1 AND action_type='api.completion.routed'")
            .bind(uid).fetch_all(pool).await?;
        assert!(traces.iter().any(|v|v["status"]=="completed"));assert!(traces.iter().any(|v|v["status"]=="failed"));
        assert!(!serde_json::to_string(&traces)?.contains("Synthetic public API prompt"));
        server.abort();
        Ok(())
    })
}
use futures_util::FutureExt;
