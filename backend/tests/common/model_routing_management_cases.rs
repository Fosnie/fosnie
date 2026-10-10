// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{future::Future,pin::Pin,sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use axum::{extract::State,Json};
use serde_json::{json,Value};
use tokio::sync::Notify;
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,PlatformRole,keycloak::AuthUser},config::BootConfig,state::AppStateBuilder,
    http::model_routing as http,model_routing::*};

struct Delegated;
#[async_trait::async_trait]
impl fosnie_backend::ext::RbacPolicy for Delegated {
    async fn can(&self,_:&sqlx::PgPool,_:&AuthContext,_:fosnie_backend::auth::rbac::ResourceType,_:Uuid,_:fosnie_backend::auth::rbac::Permission)->fosnie_backend::Result<bool>{Ok(false)}
    async fn may_grant(&self,_:&sqlx::PgPool,_:&AuthContext,_:fosnie_backend::auth::rbac::ResourceType,_:Uuid)->fosnie_backend::Result<bool>{Ok(false)}
    async fn has_permission(&self,_:&sqlx::PgPool,c:&AuthContext,p:&str)->fosnie_backend::Result<bool>{Ok(p==fosnie_backend::auth::permissions::PROVIDERS_MANAGE && c.display_name.as_deref()==Some("delegated"))}
}
struct LocalOnly;
#[async_trait::async_trait]
impl InferencePolicy for LocalOnly {
    async fn constraints(&self,_:&sqlx::PgPool,_:&RoutingContext)->fosnie_backend::Result<RoutingConstraints>{Ok(RoutingConstraints{local_only:true,..Default::default()})}
}
async fn probe(state:&AppState,actor:&AuthContext,policy:&RoutingPolicy)->fosnie_backend::Result<Json<Value>> {
    http::classifier_probe(State(state.clone()),AuthUser(actor.clone()),Json(http::ClassifierProbe {policy_id:policy.id,expected_policy_revision:policy.revision,
        conversation:"What is the weather in Glasgow?".into(),max_cost_microusd:10_000})).await
}
async fn save(state:&AppState,actor:&AuthContext,policy:RoutingPolicy,expected:i64,scope:&str)->anyhow::Result<RoutingPolicy> {
    Ok(store::save_policy(state,actor,policy.id,store::SavePolicy{label:"Management fixture".into(),scope:scope.into(),expected_revision:expected,is_default:scope=="deployment",policy}).await?)
}
pub fn check(pool:&sqlx::PgPool)->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'_>> {
    Box::pin(async move {
        let calls=Arc::new(AtomicUsize::new(0));let mode=Arc::new(AtomicUsize::new(0));let entered=Arc::new(Notify::new());let release=Arc::new(Notify::new());
        let app=axum::Router::new().route("/chat-step",axum::routing::post({let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();let release=release.clone();
            move |Json(body):Json<Value>| {let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();let release=release.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);assert!(body["tools"].is_null());
                let routing=&body["overrides"]["routing"];assert_eq!(routing["purpose"],"router_classify");
                assert_eq!(routing["assignments"]["router_classify"]["model"],"management-cloud");
                assert_eq!(body["sampling"]["max_tokens"],512);
                if mode.load(Ordering::SeqCst)==2 {entered.notify_one();release.notified().await;}
                let content=if mode.load(Ordering::SeqCst)==1 {"{\"provider_id\":\"expensive\"}".into()}
                    else {json!({"task":"current_fact","minimum_tier":"economy","fresh_information":true,"tools":true,"vision":false,"structured_output":false,"confidence":0.95,"uncertain":false}).to_string()};
                Json(json!({"content":content,"routing_report":super::routing_mock_report(&body)}))
            }}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
        let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap();});
        let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{address}");boot.providers.user_byok_enabled=true;
        let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot.clone())).with_rbac(Arc::new(Delegated)).build();
        let user=Uuid::now_v7();sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Routing settings','user')").bind(user).bind(format!("{user}@routing.test")).execute(pool).await?;
        let actor=AuthContext{user_id:Some(user),role:PlatformRole::User,email:None,display_name:None,break_glass:false,mfa_enroll_only:false};
        let mut manager=actor.clone();manager.display_name=Some("delegated".into());assert!(!manager.is_admin());
        let permissions=http::management(State(state.clone()),AuthUser(manager.clone())).await?.0;
        assert_eq!(permissions["deployment_manage"],true);assert_eq!(permissions["personal_policies"],true);
        assert_eq!(http::management(State(state.clone()),AuthUser(actor.clone())).await?.0["deployment_manage"],false);
        let mut enrollment=actor.clone();enrollment.mfa_enroll_only=true;assert!(http::management(State(state.clone()),AuthUser(enrollment)).await.is_err());
        let cloud=Uuid::now_v7();let local=Uuid::now_v7();let own=Uuid::now_v7();
        let mut cloud_profile=None;
        for (id,model,scope,locality) in [(cloud,"management-cloud","deployment","cloud"),(local,"management-local","deployment","local"),(own,"management-own","user","cloud")] {
            sqlx::query("INSERT INTO provider_configs(id,role,scope,scope_id,label,model,base_url) VALUES($1,'llm',$2,$3,$4,$4,'http://management.test/v1')")
                .bind(id).bind(scope).bind(if scope=="user" {Some(user)} else {None}).bind(model).execute(pool).await?;
            let profile:ModelProfile=serde_json::from_value(json!({"provider_id":id,"provider_revision":1,"revision":1,"adapter":"openai_chat","model":model,
                "locality":locality,"locality_trusted":scope=="deployment","quality_status":"declared","auto_enabled":true,"tier":"balanced",
                "pricing":{"input_microusd_per_million":1000,"output_microusd_per_million":1000},
                "capabilities":{"context_tokens":32000,"max_output_tokens":8192,"structured_output":"declared","tools":"declared"}}))?;
            let saved=store::save_profile(&state,if scope=="user" {&actor} else {&manager},id,store::SaveProfile{expected_revision:0,profile}).await?;
            if id==cloud {cloud_profile=Some(saved);}
        }
        let mut cloud_profile=cloud_profile.unwrap();
        let policy:RoutingPolicy=serde_json::from_value(json!({"id":Uuid::now_v7(),"revision":1,"strategy":"hybrid","rollout":"off","default_provider_id":cloud,"classifier_provider_id":cloud}))?;
        // Explicit probing is a one-call operation even when ordinary Auto
        // reserves answer time and the saved policy allows only one total call.
        let mut policy=policy;policy.constraints.max_calls=Some(1);policy.classifier.answer_reserve_ms=Some(30_000);
        let mut policy=save(&state,&manager,policy,0,"deployment").await?;
        let dry=||http::Preview{policy_id:Some(policy.id),expected_policy_revision:Some(policy.revision),objective:RoutingObjective::Economy,purpose:CallPurpose::Primary,
            requirements:WorkloadRequirements{input_tokens:256,output_tokens:1024,..Default::default()}};
        let report=http::preview(State(state.clone()),AuthUser(actor.clone()),Json(dry())).await?.0;
        assert_eq!(report["inference_calls"],0);assert_eq!(report["classifier_required"],false);assert!(report["decision"]["actual"].is_null());
        assert_eq!(report["execution_target"]["model"],"management-cloud");assert_eq!(calls.load(Ordering::SeqCst),0);
        assert!(probe(&state,&actor,&policy).await.is_err(),"regular users cannot spend on a shared policy probe");
        let report=probe(&state,&manager,&policy).await?.0;assert_eq!(report["ok"],true);assert_eq!(report["usage"]["calls"],1);assert_eq!(report["answer_calls"],0);
        assert_eq!(report["requirements"]["task"],"current_fact");assert_eq!(report["requirements"]["minimum_tier"],"economy");assert_eq!(report["quality_evaluated"],false);
        assert_eq!(calls.load(Ordering::SeqCst),1);
        let mut stale=policy.clone();stale.revision-=1;assert!(probe(&state,&manager,&stale).await.is_err());assert_eq!(calls.load(Ordering::SeqCst),1);
        // Personal routing is usable with shared cloud models even without BYOK.
        let mut personal=policy.clone();personal.id=Uuid::now_v7();personal.revision=1;
        let personal=save(&state,&actor,personal,0,"user").await?;
        boot.providers.user_byok_enabled=false;
        let no_byok=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot.clone())).with_rbac(Arc::new(Delegated)).build();
        assert_eq!(http::management(State(no_byok.clone()),AuthUser(actor.clone())).await?.0["user_byok_enabled"],false);
        assert!(!store::catalog(&no_byok,&actor).await?.iter().any(|p|p.id==own));assert_eq!(probe(&no_byok,&actor,&personal).await?.0["ok"],true);
        policy.rollout=RolloutMode::Shadow;policy.rules=vec![RoutingRule{id:Uuid::now_v7(),priority:10,conditions:Default::default(),action:RuleAction::Select{provider_id:local,effort:None}}];
        let old=policy.revision;policy=save(&state,&manager,policy,old,"deployment").await?;
        let report=http::preview(State(state.clone()),AuthUser(actor.clone()),Json(http::Preview{policy_id:Some(policy.id),expected_policy_revision:Some(policy.revision),objective:RoutingObjective::Economy,purpose:CallPurpose::Primary,requirements:WorkloadRequirements{input_tokens:256,output_tokens:1024,..Default::default()}})).await?.0;
        assert_eq!(report["decision"]["proposed"]["model"],"management-local");assert_eq!(report["execution_target"]["model"],"management-cloud");assert_eq!(report["inference_calls"],0);
        let before=calls.load(Ordering::SeqCst);
        policy.constraints.local_only=true;let old=policy.revision;policy=save(&state,&manager,policy,old,"deployment").await?;
        assert_eq!(probe(&state,&manager,&policy).await?.0["usage"]["calls"],0);assert_eq!(calls.load(Ordering::SeqCst),before);
        policy.constraints.local_only=false;let old=policy.revision;policy=save(&state,&manager,policy,old,"deployment").await?;
        let restricted=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot)).with_rbac(Arc::new(Delegated)).with_inference_policy(Arc::new(LocalOnly)).build();
        assert_eq!(probe(&restricted,&manager,&policy).await?.0["usage"]["calls"],0);
        // Unknown prices are not zero; no classifier request leaves the process.
        cloud_profile.pricing.input_microusd_per_million=None;let old=cloud_profile.revision;
        cloud_profile=store::save_profile(&state,&manager,cloud,store::SaveProfile{expected_revision:old,profile:cloud_profile}).await?;
        let report=probe(&state,&manager,&policy).await?.0;assert_eq!(report["ok"],false);assert_eq!(report["usage"]["calls"],0);assert_eq!(calls.load(Ordering::SeqCst),before);
        cloud_profile.pricing.input_microusd_per_million=Some(1000);let old=cloud_profile.revision;
        cloud_profile=store::save_profile(&state,&manager,cloud,store::SaveProfile{expected_revision:old,profile:cloud_profile}).await?;
        mode.store(1,Ordering::SeqCst);let report=probe(&state,&manager,&policy).await?.0;
        assert_eq!(report["ok"],false);assert_eq!(report["usage"]["calls"],1);assert!(report["requirements"].is_null());assert_eq!(calls.load(Ordering::SeqCst),before+1);
        // Suppress a late result after provider/profile or original actor changes.
        for fault in 0..2 {
            mode.store(2,Ordering::SeqCst);let task=tokio::spawn({let state=state.clone();let manager=manager.clone();let policy=policy.clone();async move{probe(&state,&manager,&policy).await}});
            tokio::time::timeout(Duration::from_secs(3),entered.notified()).await?;
            if fault==0 {cloud_profile.latency_ms=Some(42);let old=cloud_profile.revision;cloud_profile=store::save_profile(&state,&manager,cloud,store::SaveProfile{expected_revision:old,profile:cloud_profile}).await?;}
            else {sqlx::query("UPDATE users SET role='power_user' WHERE id=$1").bind(user).execute(pool).await?;}
            release.notify_one();assert!(task.await?.is_err());
            if fault==1 {sqlx::query("UPDATE users SET role='user' WHERE id=$1").bind(user).execute(pool).await?;}
        }
        let receipts:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type='routing.classifier.started'").fetch_one(pool).await?;
        assert_eq!(receipts as usize,calls.load(Ordering::SeqCst),"one durable start receipt per actual judge attempt");
        server.abort();Ok(())
    })
}
