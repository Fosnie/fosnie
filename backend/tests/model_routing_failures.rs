// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Product fault injection; loopback only, no live models or API credentials.
use std::{sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}},time::Duration};
use axum::{Json,routing::post};
use serde_json::{json,Value};
use sqlx::{Connection,Executor};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,PlatformRole},config::BootConfig,state::AppStateBuilder,
    model_routing::{*,store},chat::browser};
use fosnie_protocol::{RoutedChatRequest,RoutingOptions};

fn report(body:&Value,failed:bool,unknown:bool)->Value {
    let r=&body["overrides"]["routing"];let p=r["purpose"].as_str().unwrap();let t=&r["assignments"][p];
    json!({"lease_id":r["budget"]["id"],"complete":true,"calls":[{"lease_id":r["budget"]["id"],"call_id":Uuid::now_v7(),
        "decision_id":r["decision_id"],"provider_id":t["provider_id"],"provider_revision":t["provider_revision"],"profile_revision":t["profile_revision"],
        "purpose":p,"status":if failed {"failed"}else{"completed"},"latency_ms":1,
        "usage":if unknown {json!({"raw_usage":{}})} else {json!({"input_tokens":12,"output_tokens":8,"total_tokens":20,"raw_usage":{}})}}]})
}
async fn attempt(state:&AppState,actor:&AuthContext,agent:Uuid,selection:ModelSelection)->anyhow::Result<(Uuid,Vec<Value>)> {
    let turn=Uuid::now_v7();let (tx,mut rx)=tokio::sync::mpsc::channel(256);
    let task=tokio::spawn(browser::execute(state.clone(),actor.clone(),RoutedChatRequest{chat_id:None,project_id:None,agent_id:Some(agent),
        content:"Hello, answer briefly".into(),model_selection:Some(selection),options:Some(RoutingOptions{local_only:true,effort:None})},turn,tx,Arc::new(tokio::sync::Notify::new())));
    let mut frames=Vec::new();while let Some(frame)=tokio::time::timeout(Duration::from_secs(10),rx.recv()).await? {frames.push(serde_json::to_value(frame)?);}
    let _=task.await?;Ok((turn,frames))
}
async fn fixture(pool:sqlx::PgPool)->anyhow::Result<()> {
    let a=Uuid::now_v7();let b=Uuid::now_v7();let mode=Arc::new(AtomicUsize::new(0));let calls=Arc::new(Mutex::new(Vec::<Value>::new()));
    let app=axum::Router::new().route("/generate",post({let mode=mode.clone();let calls=calls.clone();move |Json(body):Json<Value>|{
        let mode=mode.clone();let calls=calls.clone();async move {
            let m=mode.load(Ordering::SeqCst);calls.lock().unwrap().push(body.clone());
            let first=body["overrides"]["routing"]["assignments"]["final"]["provider_id"]==json!(a);
            if first || m==5 {
                let prefix=if m==2 {format!("{}\n",json!({"type":"token","delta":"Partial retained"}))} else {String::new()};
                let kind=match m {1=>"quota_exhausted",4=>"unauthorized",_=>"unavailable"};
                format!("{prefix}{}\n",json!({"type":"error","message":"private-vendor-secret","routing_failure":{"schema_version":1,"kind":kind,"before_output":m!=2},
                    "routing_report":report(&body,true,m==3)}))
            } else {format!("{}\n{}\n",json!({"type":"token","delta":"Alternate success"}),json!({"type":"done","model":"failure-alternate","finish_reason":"stop","routing_report":report(&body,false,false)}))}
        }
    }})).route("/chat-step",post(|Json(body):Json<Value>|async move {Json(json!({"content":"A title","routing_report":report(&body,false,false)}))}));
    let socket=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let url=format!("http://{}",socket.local_addr()?);
    let server=tokio::spawn(async move{let _=axum::serve(socket,app).await;});
    let mut boot=BootConfig::default();boot.ml.base_url=url;
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot)).build();
    let uid=Uuid::now_v7();sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Failure fixture','client_admin')").bind(uid).bind(format!("{uid}@routing.test")).execute(&pool).await?;
    let actor=AuthContext{user_id:Some(uid),email:None,display_name:None,role:PlatformRole::ClientAdmin,break_glass:false,mfa_enroll_only:false};
    let agent=Uuid::now_v7();sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'Failure fixture','Answer briefly','{}'::jsonb,$2,ARRAY['general'])")
        .bind(agent).bind(uid).execute(&pool).await?;
    for (id,name) in [(a,"failure-primary"),(b,"failure-alternate")] {
        sqlx::query("INSERT INTO provider_configs(id,role,scope,model,label,base_url) VALUES($1,'llm','deployment',$2,$2,'http://failure.test/v1')").bind(id).bind(name).execute(&pool).await?;
        let profile:ModelProfile=serde_json::from_value(json!({"provider_id":id,"provider_revision":1,"revision":1,"model":name,"adapter":"openai_chat",
            "locality":"local","locality_trusted":true,"auto_enabled":true,"quality_status":"declared","tier":"balanced",
            "pricing":{"input_microusd_per_million":1000,"output_microusd_per_million":1000},"capabilities":{"context_tokens":32768,"max_output_tokens":8192,"structured_output":"declared"}}))?;
        store::save_profile(&state,&actor,id,store::SaveProfile{expected_revision:0,profile}).await?;
    }
    let policy:RoutingPolicy=serde_json::from_value(json!({"id":Uuid::now_v7(),"revision":1,"rollout":"active","strategy":"rules","technical_fallback":true,
        "default_provider_id":b,"constraints":{"allowed_providers":[a,b],"local_only":true,"max_calls":4,"max_duration_ms":15000,"max_total_tokens":100000},
        "rules":[{"id":Uuid::now_v7(),"priority":10,"conditions":{},"action":{"kind":"select","provider_id":a}}]}))?;
    let mut policy=store::save_policy(&state,&actor,policy.id,store::SavePolicy{label:"Faults".into(),scope:"deployment".into(),expected_revision:0,is_default:true,policy}).await?;
    browser::set_enabled(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(actor.clone()),Json(fosnie_backend::http::voice_routing::Enabled{enabled:true})).await?;
    for m in 0..=5 {
        sqlx::query("DELETE FROM routing_health_scopes").execute(&pool).await?;
        sqlx::query("DELETE FROM routing_provider_health").execute(&pool).await?;sqlx::query("DELETE FROM routing_credential_health").execute(&pool).await?;
        policy.constraints.max_calls=Some(if m==3 {1}else{4});
        let previous=policy.revision;let id=policy.id;
        policy=store::save_policy(&state,&actor,id,store::SavePolicy{label:"Faults".into(),scope:"deployment".into(),expected_revision:previous,is_default:true,policy}).await?;
        mode.store(m,Ordering::SeqCst);calls.lock().unwrap().clear();
        let selection=if m==4 {ModelSelection::Manual{provider_id:a}} else {ModelSelection::Auto{objective:RoutingObjective::Economy,policy_id:Some(policy.id)}};
        let (turn,frames)=attempt(&state,&actor,agent,selection).await?;
        let requested=calls.lock().unwrap().clone();
        assert_eq!(requested.len(),if [0,1,5].contains(&m){2}else{1},"mode {m}: {frames:?}");
        let users:i64=sqlx::query_scalar("SELECT count(*) FROM messages WHERE turn_id=$1 AND role='user'").bind(turn).fetch_one(&pool).await?;assert_eq!(users,1);
        assert!(!serde_json::to_string(&frames)?.contains("private-vendor-secret"));
        if [0,1].contains(&m) {
            assert!(frames.iter().any(|f|f["type"]=="chat.completed"),"{frames:?}");
            let id:Uuid=sqlx::query_scalar("SELECT id FROM messages WHERE turn_id=$1 AND role='assistant'").bind(turn).fetch_one(&pool).await?;
            let meta=browser::message_metadata(&state,&actor,&[id]).await?;
            assert_eq!(meta[&id]["actual"]["model"],"failure-alternate");assert_eq!(meta[&id]["attempts"].as_array().unwrap().len(),1);
            assert_eq!(requested[0]["messages"],requested[1]["messages"],"no prompt reconstruction or second judge");
            if m==0 {
                calls.lock().unwrap().clear();
                let (_,next)=attempt(&state,&actor,agent,ModelSelection::Auto{objective:RoutingObjective::Economy,policy_id:Some(policy.id)}).await?;
                assert!(next.iter().any(|f|f["type"]=="chat.completed"),"a terminal rule retains its safe alternate witness during cooldown");
                assert_eq!(calls.lock().unwrap().len(),1,"cooling primary is refused before another native request");
            }
        } else {
            assert!(frames.iter().any(|f|f["type"]=="chat.routing_failure"));
            let row:(String,Option<Value>)=sqlx::query_as("SELECT content,routing_failure FROM messages WHERE turn_id=$1 AND role='assistant'").bind(turn).fetch_one(&pool).await?;
            assert!(row.1.is_some());if m==2 {assert_eq!(row.0,"Partial retained");}
        }
    }
    // Cooldown scope, key grouping and a rotated revision do not leak key material.
    sqlx::query("DELETE FROM routing_health_scopes").execute(&pool).await?;
    sqlx::query("DELETE FROM routing_provider_health").execute(&pool).await?;sqlx::query("DELETE FROM routing_credential_health").execute(&pool).await?;
    sqlx::query("UPDATE provider_configs SET model=model||'-health' WHERE id=ANY($1)").bind(vec![a,b]).execute(&pool).await?;
    let target=|id,model:&str|InferenceTarget{schema_version:1,provider_id:id,provider_revision:2,profile_revision:1,adapter:AdapterKind::OpenaiChat,
        base_url:"https://quota.example/v1".into(),model:model.into(),api_key:"private-shared-key".into(),effort:None,generation:None,variant:None};
    let first=target(a,"failure-primary-health");let alternate=target(b,"failure-alternate-health");
    health::register(&state,&first,CallPurpose::Final).await?;health::register(&state,&alternate,CallPurpose::Final).await?;
    health::note(&state,&first,CallPurpose::Final,&failure::ProviderFailure{schema_version:1,kind:fallback::FailureKind::QuotaExhausted,before_output:true,retry_after_seconds:Some(15)}).await?;
    assert!(health::blocked(&state,&alternate.reference(),CallPurpose::Final).await?.is_some());
    sqlx::query("UPDATE provider_configs SET model=model||'-rotated' WHERE id=$1").bind(a).execute(&pool).await?;
    assert!(health::blocked(&state,&alternate.reference(),CallPurpose::Final).await?.is_some(),"renaming one model cannot erase a shared credential quota");
    let mut rotated=alternate.clone();rotated.api_key="private-rotated-key".into();
    rotated.provider_revision=sqlx::query_scalar("UPDATE provider_configs SET model=model||'-rotated' WHERE id=$1 RETURNING routing_revision").bind(b).fetch_one(&pool).await?;
    rotated.model.push_str("-rotated");
    health::register(&state,&rotated,CallPurpose::Final).await?;
    assert!(health::blocked(&state,&rotated.reference(),CallPurpose::Final).await?.is_none(),"a genuinely changed ungrouped credential has a new binding");
    let raw:Vec<String>=sqlx::query_scalar("SELECT binding_hash FROM routing_credential_bindings").fetch_all(&pool).await?;
    assert!(raw.iter().all(|v|v.len()==64 && !v.contains("private")));
    policy.technical_fallback=false;server.abort();Ok(())
}
#[tokio::test]
async fn browser_failure_scope_and_health()->anyhow::Result<()> {
    let options:sqlx::postgres::PgConnectOptions=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL")?.parse()?;
    anyhow::ensure!(options.get_host()=="127.0.0.1"&&options.get_database().is_some_and(|d|d.starts_with("routing_")),"isolated database required");
    let mut conn=sqlx::PgConnection::connect_with(&options).await?;let schema=format!("routing_failures_{}",Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}; SET search_path TO {schema}"))).execute(&mut conn).await?;
    let result=async {
        for m in sqlx::migrate!("./migrations").iter(){sqlx::raw_sql(m.sql.clone()).execute(&mut conn).await?;}
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(12).after_connect({let schema=schema.clone();move|conn,_|{
            let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));Box::pin(async move{sqlx::raw_sql(sql).execute(conn).await?;Ok(())})}}).connect_with(options).await?;
        let result=tokio::spawn(fixture(pool.clone())).await;pool.close().await;result?
    }.await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO public; DROP SCHEMA {schema} CASCADE"))).execute(&mut conn).await?;result
}
