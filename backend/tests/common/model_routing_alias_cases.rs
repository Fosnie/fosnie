// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{future::Future,pin::Pin,sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use axum::{Json,extract::{State,Path},body::{Body,to_bytes},http::{Request,StatusCode},response::Response};
use serde_json::{json,Value};
use tokio::sync::Notify;
use tower::ServiceExt;
use uuid::Uuid;
use fosnie_backend::{AppState,AppError,auth::{AuthContext,PlatformRole,keycloak::AuthUser,device::MaybeDevice},
    config::BootConfig,state::AppStateBuilder,http::{v1,providers},model_routing::*};

struct Features(bool);
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Features {
    async fn enabled_for_user(&self,_:&AppState,_:Option<Uuid>,feature:&str)->bool {self.0 && feature=="public_api"}
}
struct Delegated;
#[async_trait::async_trait]
impl fosnie_backend::ext::RbacPolicy for Delegated {
    async fn can(&self,p:&sqlx::PgPool,a:&AuthContext,k:fosnie_backend::auth::rbac::ResourceType,id:Uuid,m:fosnie_backend::auth::rbac::Permission)->fosnie_backend::Result<bool> {
        fosnie_backend::auth::rbac::FlatRbacPolicy.can(p,a,k,id,m).await
    }
    async fn may_grant(&self,_:&sqlx::PgPool,_:&AuthContext,_:fosnie_backend::auth::rbac::ResourceType,_:Uuid)->fosnie_backend::Result<bool>{Ok(false)}
    async fn has_permission(&self,_:&sqlx::PgPool,a:&AuthContext,p:&str)->fosnie_backend::Result<bool>{Ok(p==fosnie_backend::auth::permissions::PROVIDERS_MANAGE && a.display_name.as_deref()==Some("delegated"))}
}
struct Deny;
#[async_trait::async_trait]
impl InferencePolicy for Deny {
    async fn constraints(&self,_:&sqlx::PgPool,_:&RoutingContext)->fosnie_backend::Result<RoutingConstraints>{Ok(RoutingConstraints{allowed_providers:Some(Default::default()),..Default::default()})}
}
async fn value(response:Response)->anyhow::Result<Value>{Ok(serde_json::from_slice(&to_bytes(response.into_body(),1_000_000).await?)?)}
async fn request(state:AppState,token:String,body:Value,chat:Option<Uuid>)->anyhow::Result<Response>{
    Ok(tokio::spawn(async move{
        let mut req=Request::builder().method("POST").uri("/v1/chat/completions").header("authorization",format!("Bearer {token}")).header("content-type","application/json");
        if let Some(chat)=chat {req=req.header("x-fosnie-chat-id",chat.to_string());}
        v1::router(&state).with_state(state).oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap()
    }).await?)
}
async fn get(state:AppState,token:&str,path:&str)->anyhow::Result<Response>{
    Ok(v1::router(&state).with_state(state.clone()).oneshot(Request::builder().uri(path).header("authorization",format!("Bearer {token}")).body(Body::empty())?).await?)
}
async fn configure(state:&AppState,manager:&AuthContext,name:&str,enabled:bool)->fosnie_backend::Result<Json<Value>> {
    let revision=v1::alias::describe(state).await?["revision"].as_i64().unwrap();
    v1::alias::save(State(state.clone()),AuthUser(manager.clone()),Json(v1::alias::Save{name:name.into(),enabled,objective:RoutingObjective::Economy,expected_revision:revision})).await
}
fn upsert(label:&str)->providers::UpsertLlm {providers::UpsertLlm{label:label.into(),model:Some("synthetic".into()),base_url:Some("http://alias.test/v1".into()),api_key:None,enabled:false,reasoning_mode:None}}
pub fn check(pool:&sqlx::PgPool)->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'_>> {
    Box::pin(async move {
        let calls=Arc::new(AtomicUsize::new(0));let judges=Arc::new(AtomicUsize::new(0));let mode=Arc::new(AtomicUsize::new(0));let entered=Arc::new(Notify::new());
        let app=axum::Router::new().route("/generate",axum::routing::post({let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();
            move |Json(body):Json<Value>| {let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);let current_mode=mode.load(Ordering::SeqCst);
                let routed=!body["overrides"]["routing"].is_null();
                if routed {assert_eq!(body["overrides"]["routing"]["assignments"]["final"]["model"],"alias-local");assert!(body["model"].is_null());}
                if current_mode==1 {entered.notify_one();}
                let mut done=json!({"type":"done","finish_reason":"stop","model":"alias-local","usage":{"prompt_tokens":100,"completion_tokens":40}});
                if routed {done["routing_report"]=super::routing_mock_report(&body);}
                let lines=vec![json!({"type":"token","delta":"Alias answer."}).to_string()+"\n",done.to_string()+"\n"];
                Body::from_stream(futures_util::stream::unfold((lines.into_iter(),0),move |(mut lines,index)|async move {
                    let line=lines.next()?;if current_mode==1 && index==1 {tokio::time::sleep(Duration::from_secs(2)).await;}
                    Some((Ok::<_,std::convert::Infallible>(line),(lines,index+1)))
                }))
            }}
        })).route("/chat-step",axum::routing::post({let calls=calls.clone();let judges=judges.clone();let mode=mode.clone();let entered=entered.clone();
            move |Json(body):Json<Value>| {let calls=calls.clone();let judges=judges.clone();let mode=mode.clone();let entered=entered.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);judges.fetch_add(1,Ordering::SeqCst);assert_eq!(body["overrides"]["routing"]["purpose"],"router_classify");
                if mode.load(Ordering::SeqCst)==2 {entered.notify_one();tokio::time::sleep(Duration::from_secs(4)).await;}
                Json(json!({"content":json!({"task":"general","minimum_tier":"balanced","fresh_information":false,"tools":false,"vision":false,"structured_output":false,"confidence":0.95,"uncertain":false}).to_string(),
                    "routing_report":super::routing_mock_report(&body)}))
            }}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
        let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap();});
        let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{address}");boot.features.agents_enabled=true;
        let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot.clone())).with_features(Arc::new(Features(true))).with_rbac(Arc::new(Delegated)).build();
        let user=Uuid::now_v7();let manager_id=Uuid::now_v7();let foreign=Uuid::now_v7();let key=Uuid::now_v7();let agent=Uuid::now_v7();
        for id in [user,manager_id,foreign] {sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'API alias','user')").bind(id).bind(format!("{id}@routing.test")).execute(pool).await?;}
        let actor=AuthContext{user_id:Some(user),role:PlatformRole::User,email:None,display_name:None,break_glass:false,mfa_enroll_only:false};
        let manager=AuthContext{user_id:Some(manager_id),display_name:Some("delegated".into()),..actor.clone()};assert!(!manager.is_admin());
        let (token,hash,prefix)=fosnie_backend::auth::api_key::mint();
        sqlx::query("INSERT INTO api_keys(id,user_id,name,token_hash,display_prefix) VALUES($1,$2,'alias',$3,$4)").bind(key).bind(user).bind(hash).bind(prefix).execute(pool).await?;
        sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'Alias agent','Alias agent instruction','{\"max_tokens\":512}',$2,ARRAY['general'])")
            .bind(agent).bind(user).execute(pool).await?;
        let local=Uuid::now_v7();let cloud=Uuid::now_v7();let old_label=Uuid::now_v7();
        let mut local_profile=None;
        for (id,label,model,cost) in [(local,"Local route","alias-local",0),(cloud,"Cloud route","alias-cloud",1_000_000),(old_label,"fosnie/auto","legacy-alias",0)] {
            sqlx::query("INSERT INTO provider_configs(id,role,scope,label,model,base_url) VALUES($1,'llm','deployment',$2,$3,'http://alias.test/v1')").bind(id).bind(label).bind(model).execute(pool).await?;
            let profile:ModelProfile=serde_json::from_value(json!({"provider_id":id,"provider_revision":1,"revision":1,"model":model,"adapter":"openai_chat","locality":"cloud",
                "quality_status":"declared","tier":"balanced","auto_enabled":true,"capabilities":{"context_tokens":32000,"max_output_tokens":8192,"structured_output":"declared"},
                "pricing":{"input_microusd_per_million":cost,"output_microusd_per_million":cost}}))?;
            let profile=store::save_profile(&state,&manager,id,store::SaveProfile{expected_revision:0,profile}).await?;
            if id==local {local_profile=Some(profile);}
        }
        let mut local_profile=local_profile.unwrap();
        for (scope,label,model,owner,enabled) in [("deployment",Some("auto"),"legacy-auto",None,true),("user",Some("hidden-alias"),"hidden",Some(foreign),false),("deployment",None,"fallback-alias",None,false)] {
            sqlx::query("INSERT INTO provider_configs(id,role,scope,scope_id,label,model,enabled,base_url) VALUES($1,'llm',$2,$3,$4,$5,$6,'http://alias.test/v1')")
                .bind(Uuid::now_v7()).bind(scope).bind(owner).bind(label).bind(model).bind(enabled).execute(pool).await?;
        }
        let policy:RoutingPolicy=serde_json::from_value(json!({"id":Uuid::now_v7(),"revision":1,"rollout":"active","strategy":"rules","default_provider_id":local,"classifier_provider_id":cloud,
            "rules":[{"id":Uuid::now_v7(),"priority":10,"conditions":{},"action":{"kind":"select","provider_id":local}}],"constraints":{"max_calls":12,"max_duration_ms":20000}}))?;
        let mut policy=store::save_policy(&state,&manager,policy.id,store::SavePolicy{label:"Alias rules".into(),scope:"deployment".into(),expected_revision:0,is_default:true,policy}).await?;
        let body=|name:&str|json!({"model":name,"messages":[{"role":"user","content":"Synthetic alias question"}]});
        assert_eq!(v1::alias::describe(&state).await?["enabled"],false);
        assert!(configure(&state,&actor,"fosnie-route/auto",true).await.is_err());
        for name in ["fosnie/auto","auto","hidden-alias","fallback-alias"] {
            assert!(matches!(configure(&state,&manager,name,true).await,Err(AppError::Conflict(_))),"collision {name}");
        }
        for invalid in ["agent/test","bad alias",""] {assert!(configure(&state,&manager,invalid,false).await.is_err());}
        for legacy in ["auto","fosnie/auto"] {
            let response=request(state.clone(),token.clone(),body(legacy),None).await?;assert_eq!(response.status(),StatusCode::OK);assert!(value(response).await?["fosnie_routing"].is_null());
        }
        let name="fosnie-route/auto";
        let saved=configure(&state,&manager,name,true).await?.0;assert_eq!(saved["status"],"routing_disabled");
        let discovery=value(get(state.clone(),&token,"/v1/models").await?).await?;assert!(!discovery["data"].as_array().unwrap().iter().any(|m|m["id"]==name));
        assert_eq!(request(state.clone(),token.clone(),body(name),None).await?.status(),StatusCode::SERVICE_UNAVAILABLE);
        sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES('routing.public_api_enabled','true','bool') ON CONFLICT(key) DO UPDATE SET value='true',value_type='bool'").execute(pool).await?;
        let discovery=value(get(state.clone(),&token,"/v1/models").await?).await?;
        assert_eq!(discovery["data"].as_array().unwrap().iter().filter(|m|m["id"]==name).count(),1);
        assert!(discovery["data"].as_array().unwrap().iter().any(|m|m["id"]=="fosnie/auto" && m["owned_by"]=="fosnie"));
        assert_eq!(value(get(state.clone(),&token,"/v1/routing").await?).await?["virtual_alias"],name);
        let stale=v1::alias::save(State(state.clone()),AuthUser(manager.clone()),Json(v1::alias::Save{name:name.into(),enabled:true,objective:RoutingObjective::Economy,expected_revision:1})).await;
        assert!(stale.is_err());
        assert!(matches!(providers::create_admin_llm(State(state.clone()),AuthUser(manager.clone()),Json(upsert(name))).await,Err(AppError::Conflict(_))));
        assert!(matches!(providers::create_my_llm(State(state.clone()),AuthUser(actor.clone()),MaybeDevice(None),Json(upsert(name))).await,Err(AppError::Conflict(_))));
        assert!(matches!(providers::update_admin_llm(State(state.clone()),AuthUser(manager.clone()),Path(old_label),Json(upsert(name))).await,Err(AppError::Conflict(_))));
        let fallback=sqlx::query("INSERT INTO provider_configs(id,role,scope,model) VALUES($1,'llm','deployment',$2)").bind(Uuid::now_v7()).bind(name).execute(pool).await;
        assert!(fallback.is_err(),"unlabelled provider fallback name is guarded too");
        let before=calls.load(Ordering::SeqCst);
        for invalid in [json!({"model_selection":{"mode":"default"}}),json!({"extra_unknown":true}),json!({"fosnie_agent_id":null}),json!({"tools":[{"type":"function"}]})] {
            let mut b=body(name);b.as_object_mut().unwrap().extend(invalid.as_object().unwrap().clone());assert_eq!(request(state.clone(),token.clone(),b,None).await?.status(),StatusCode::BAD_REQUEST);
        }
        let response=request(state.clone(),token.clone(),body(name),Some(Uuid::now_v7())).await?;assert_eq!(response.status(),StatusCode::BAD_REQUEST);assert_eq!(calls.load(Ordering::SeqCst),before);
        let response=request(state.clone(),token.clone(),body(name),None).await?;let status=response.status();let response=value(response).await?;
        assert_eq!(status,StatusCode::OK,"{response}");assert_eq!(response["model"],name);assert_eq!(response["fosnie_routing"]["requested_model"],name);
        assert_eq!(response["fosnie_routing"]["decision"]["actual"]["model"],"alias-local");assert_eq!(response["fosnie_routing"]["selection"]["mode"],"auto");
        assert_eq!(response["fosnie_routing"]["usage"]["calls"],1);assert_eq!(judges.load(Ordering::SeqCst),0);
        let mut b=body(name);b["stream"]=json!(true);let response=request(state.clone(),token.clone(),b,None).await?;
        let text=String::from_utf8(to_bytes(response.into_body(),1_000_000).await?.to_vec())?;
        let events:Vec<Value>=text.lines().filter_map(|s|s.strip_prefix("data: ")).filter(|s|*s!="[DONE]").map(serde_json::from_str).collect::<std::result::Result<_,_>>()?;
        assert!(events.iter().all(|e|e["model"]==name));assert_eq!(events.last().unwrap()["fosnie_routing"]["decision"]["actual"]["model"],"alias-local");
        let mut explicit=body("");explicit["model_selection"]=json!({"mode":"auto"});let response=value(request(state.clone(),token.clone(),explicit,None).await?).await?;
        assert_eq!(response["model"],"alias-local");assert!(response["fosnie_routing"]["requested_model"].is_null(),"explicit extension contract remains unchanged");
        let mut b=body(name);b["fosnie_agent_id"]=json!(agent);let response=request(state.clone(),token.clone(),b.clone(),None).await?;
        let status=response.status();let chat:Uuid=response.headers()["x-fosnie-chat-id"].to_str()?.parse()?;let response=value(response).await?;
        assert_eq!(status,StatusCode::OK,"{response}");assert_eq!(response["model"],name);assert_eq!(response["fosnie_routing"]["agent_id"],agent.to_string());
        assert_eq!(response["fosnie_routing"]["decision"]["actual"]["model"],"alias-local");
        b["stream"]=json!(true);let response=request(state.clone(),token.clone(),b,Some(chat)).await?;
        let text=String::from_utf8(to_bytes(response.into_body(),1_000_000).await?.to_vec())?;assert!(text.contains("fosnie_routing") && text.contains("alias-local") && text.contains(name));
        let response=request(state.clone(),token.clone(),body(&format!("agent/{agent}")),Some(chat)).await?;assert_eq!(response.status(),StatusCode::BAD_REQUEST,"adopted alias chat never silently becomes legacy");
        let denied=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot.clone())).with_features(Arc::new(Features(true))).with_inference_policy(Arc::new(Deny)).build();
        let before=calls.load(Ordering::SeqCst);assert_ne!(request(denied,token.clone(),body(name),None).await?.status(),StatusCode::OK);assert_eq!(calls.load(Ordering::SeqCst),before);
        let unavailable=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot)).with_features(Arc::new(Features(false))).build();
        assert_eq!(get(unavailable,&token,"/v1/models").await?.status(),StatusCode::NOT_FOUND);
        // No invented zero prices under a monetary cap.
        policy.constraints.max_cost_microusd=Some(1000);let revision=policy.revision;
        policy=store::save_policy(&state,&manager,policy.id,store::SavePolicy{label:"Alias budget".into(),scope:"deployment".into(),expected_revision:revision,is_default:true,policy}).await?;
        local_profile.pricing.input_microusd_per_million=None;let revision=local_profile.revision;
        local_profile=store::save_profile(&state,&manager,local,store::SaveProfile{expected_revision:revision,profile:local_profile}).await?;
        assert_ne!(request(state.clone(),token.clone(),body(name),None).await?.status(),StatusCode::OK);assert_eq!(calls.load(Ordering::SeqCst),before);
        local_profile.pricing.input_microusd_per_million=Some(0);let revision=local_profile.revision;
        let _=store::save_profile(&state,&manager,local,store::SaveProfile{expected_revision:revision,profile:local_profile}).await?;
        policy.constraints.max_cost_microusd=None;let revision=policy.revision;
        policy=store::save_policy(&state,&manager,policy.id,store::SavePolicy{label:"Alias calls".into(),scope:"deployment".into(),expected_revision:revision,is_default:true,policy}).await?;
        // Alias changes interrupt an open stream and planning without replay.
        mode.store(1,Ordering::SeqCst);let mut b=body(name);b["stream"]=json!(true);let response=request(state.clone(),token.clone(),b,None).await?;
        entered.notified().await;let before=calls.load(Ordering::SeqCst);let _=configure(&state,&manager,name,false).await?;
        let text=String::from_utf8(to_bytes(response.into_body(),1_000_000).await?.to_vec())?;assert!(text.contains("error"));assert!(!text.contains("\"finish_reason\":\"stop\""));assert_eq!(calls.load(Ordering::SeqCst),before);
        let _=configure(&state,&manager,name,true).await?;policy.rules.clear();policy.strategy=RoutingStrategy::Classifier;let revision=policy.revision;
        let _=store::save_policy(&state,&manager,policy.id,store::SavePolicy{label:"Alias judge".into(),scope:"deployment".into(),expected_revision:revision,is_default:true,policy}).await?;
        mode.store(2,Ordering::SeqCst);let pending=tokio::spawn({let state=state.clone();let token=token.clone();let b=body(name);async move{request(state,token,b,None).await}});
        tokio::time::timeout(Duration::from_secs(3),entered.notified()).await?;let before=calls.load(Ordering::SeqCst);let _=configure(&state,&manager,name,false).await?;
        let response=tokio::time::timeout(Duration::from_secs(2),pending).await???;assert_ne!(response.status(),StatusCode::OK);assert_eq!(calls.load(Ordering::SeqCst),before);
        mode.store(0,Ordering::SeqCst);
        // Concurrent CRUD/activation cannot both win; namespace writes serialize.
        let _=configure(&state,&manager,"race-create",false).await?;let mut creator=pool.begin().await?;
        sqlx::query("INSERT INTO provider_configs(id,role,scope,label) VALUES($1,'llm','deployment','race-create')").bind(Uuid::now_v7()).execute(&mut *creator).await?;
        let mut pending=tokio::spawn({let state=state.clone();let manager=manager.clone();async move{configure(&state,&manager,"race-create",true).await}});
        assert!(tokio::time::timeout(Duration::from_millis(60),&mut pending).await.is_err());creator.commit().await?;assert!(matches!(pending.await?,Err(AppError::Conflict(_))));
        let _=configure(&state,&manager,"race-activate",false).await?;let mut activation=pool.begin().await?;
        sqlx::query("UPDATE routing_api_alias SET enabled=true,revision=nextval('routing_revision_generation') WHERE id=1").execute(&mut *activation).await?;
        let mut pending=tokio::spawn({let pool=pool.clone();async move{sqlx::query("INSERT INTO provider_configs(id,role,scope,label) VALUES($1,'llm','deployment','race-activate')").bind(Uuid::now_v7()).execute(&pool).await}});
        assert!(tokio::time::timeout(Duration::from_millis(60),&mut pending).await.is_err());activation.commit().await?;assert!(pending.await?.is_err());
        let _=configure(&state,&manager,"repeatable-activate",false).await?;
        let mut repeatable=pool.begin().await?;sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ").execute(&mut *repeatable).await?;
        let _:i64=sqlx::query_scalar("SELECT namespace_epoch FROM routing_api_alias WHERE id=1").fetch_one(&mut *repeatable).await?;
        sqlx::query("INSERT INTO provider_configs(id,role,scope,label) VALUES($1,'llm','deployment','repeatable-activate')").bind(Uuid::now_v7()).execute(pool).await?;
        let error=sqlx::query("UPDATE routing_api_alias SET enabled=true WHERE id=1").execute(&mut *repeatable).await.unwrap_err();assert_eq!(error.as_database_error().unwrap().code().as_deref(),Some("40001"));repeatable.rollback().await?;
        let _=configure(&state,&manager,"repeatable-provider",false).await?;
        let mut repeatable=pool.begin().await?;sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ").execute(&mut *repeatable).await?;
        let _:i64=sqlx::query_scalar("SELECT namespace_epoch FROM routing_api_alias WHERE id=1").fetch_one(&mut *repeatable).await?;
        let _=configure(&state,&manager,"repeatable-provider",true).await?;
        let error=sqlx::query("INSERT INTO provider_configs(id,role,scope,label) VALUES($1,'llm','deployment','repeatable-provider')").bind(Uuid::now_v7()).execute(&mut *repeatable).await.unwrap_err();
        assert_eq!(error.as_database_error().unwrap().code().as_deref(),Some("40001"));repeatable.rollback().await?;
        let traces:Vec<Value>=sqlx::query_scalar("SELECT model_agent_traceability FROM audit_events WHERE actor_user_id=$1 AND action_type IN ('api.completion.routed','api.agent.routed')").bind(user).fetch_all(pool).await?;
        assert!(traces.iter().any(|t|t["routing"]["requested_model"]==name && t["routing"]["decision"]["actual"]["model"]=="alias-local"));
        assert!(!serde_json::to_string(&traces)?.contains("Synthetic alias question"));
        server.abort();Ok(())
    })
}
