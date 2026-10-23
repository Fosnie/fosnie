// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Real Core HTTP/WS -> real ML -> loopback deterministic provider -> product UI.
use std::{path::PathBuf,sync::{Arc,Mutex},process::Stdio,time::Duration};
use axum::{Json,response::IntoResponse};
use serde_json::{json,Value};
use sqlx::{Connection,Executor};
use tokio::io::{AsyncBufReadExt,BufReader};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,PlatformRole},config::BootConfig,state::AppStateBuilder,model_routing::*};

struct Auth(AuthContext);
#[async_trait::async_trait]
impl fosnie_backend::ext::AuthProvider for Auth {
    async fn authenticate(&self,_:&mut axum::http::request::Parts,_:&AppState)->fosnie_backend::Result<AuthContext> {Ok(self.0.clone())}
}
struct Servers(Vec<tokio::task::JoinHandle<()>>);
#[path="common/model_routing_builder_cases.rs"]
mod builder_cases;
impl Drop for Servers {fn drop(&mut self){for server in &self.0 {server.abort();}}}

async fn stack(pool:sqlx::PgPool)->anyhow::Result<()> {
    let core=PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let python=std::env::var_os("FOSNIE_ROUTING_PYTHON").map(PathBuf::from).unwrap_or_else(||core.join(if cfg!(windows){"ml/.venv/Scripts/python.exe"}else{"ml/.venv/bin/python"}));
    anyhow::ensure!(python.is_file(),"ML Python runtime required; acceptance cannot skip");
    anyhow::ensure!(core.join("frontend/node_modules/@playwright/test/cli.js").is_file(),"Existing frontend/Playwright environment required");
    let trace=Arc::new(Mutex::new(Vec::<Value>::new()));let mut servers=Servers(vec![]);
    let upstream=axum::Router::new().route("/v1/chat/completions",axum::routing::post({let trace=trace.clone();move |Json(body):Json<Value>|{
        let trace=trace.clone();async move {
            let model=body["model"].as_str().unwrap();assert!(matches!(model,"routing-e2e-local"|"routing-e2e-cloud"|"routing-e2e-judge"));
            let complex=body["messages"].to_string().contains("honours project");
            let content=if model=="routing-e2e-judge" {json!({"task":if complex{"research"}else{"general"},"minimum_tier":if complex{"advanced"}else{"economy"},"fresh_information":false,
                "tools":false,"vision":false,"structured_output":false,"confidence":0.95,"uncertain":false,"difficulty":if complex{"complex"}else{"routine"}}).to_string()}
                else {"Fosnie routing stack answer.".into()};
            let stream=body["stream"]==true;let model=model.to_string();trace.lock().unwrap().push(body);
            let usage=json!({"prompt_tokens":32,"completion_tokens":32,"total_tokens":64});
            if stream {
                let chunk=json!({"id":"synthetic","object":"chat.completion.chunk","model":model,"choices":[{"index":0,"delta":{"content":content},"finish_reason":null}]});
                let done=json!({"id":"synthetic","object":"chat.completion.chunk","model":model,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":usage});
                ([(axum::http::header::CONTENT_TYPE,"text/event-stream")],format!("data: {chunk}\n\ndata: {done}\n\ndata: [DONE]\n\n")).into_response()
            } else {Json(json!({"id":"synthetic","object":"chat.completion","model":model,"choices":[{"index":0,
                "message":{"role":"assistant","content":content},"finish_reason":"stop"}],"usage":usage})).into_response()}
        }
    }}));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let upstream_url=format!("http://{}/v1",listener.local_addr()?);
    servers.0.push(tokio::spawn(async move{axum::serve(listener,upstream).await.unwrap();}));
    let mut ml=tokio::process::Command::new(python).arg(core.join("tools/routing/e2e_ml.py")).current_dir(&core)
        .stdout(Stdio::piped()).stderr(Stdio::inherit()).kill_on_drop(true).spawn()?;
    let mut ready=String::new();
    tokio::time::timeout(Duration::from_secs(30),BufReader::new(ml.stdout.take().unwrap()).read_line(&mut ready)).await??;
    let ready:Value=serde_json::from_str(&ready)?;let ml_url=ready["url"].as_str().ok_or_else(||anyhow::anyhow!("ML readiness"))?;
    let client=reqwest::Client::new();let mut healthy=false;
    for _ in 0..100 {if client.get(format!("{ml_url}/health")).send().await.is_ok_and(|r|r.status().is_success()){healthy=true;break;}
        tokio::time::sleep(Duration::from_millis(100)).await;}
    anyhow::ensure!(healthy,"Real ML service failed readiness");
    let uid=Uuid::now_v7();let actor=AuthContext{user_id:Some(uid),email:None,display_name:None,role:PlatformRole::ClientAdmin,break_glass:false,mfa_enroll_only:false};
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Routing stack fixture','client_admin')")
        .bind(uid).bind(format!("{uid}@routing.test")).execute(&pool).await?;
    let agent=Uuid::now_v7();
    // Keep the real seeded agents intact; explicitly pick a supported fixture.
    sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'Routing stack assistant','Answer the synthetic routing task.','{}'::jsonb,$2,ARRAY['general'])")
        .bind(agent).bind(uid).execute(&pool).await?;
    let mut boot=BootConfig::default();boot.ml.base_url=ml_url.into();boot.ml.shared_secret="routing-e2e-ml".into();
    boot.providers.user_byok_enabled=false;
    boot.server.static_dir="".into();boot.server.public_url="http://127.0.0.1:5189".into();
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
        .with_auth(Arc::new(Auth(actor.clone()))).build();
    let (local,cloud,judge)=(Uuid::now_v7(),Uuid::now_v7(),Uuid::now_v7());
    for (id,model,locality,cost,auto) in [(local,"routing-e2e-local",Locality::Local,1000,true),(cloud,"routing-e2e-cloud",Locality::Cloud,1_000_000,true),
        (judge,"routing-e2e-judge",Locality::Local,1000,false)] {
        sqlx::query("INSERT INTO provider_configs(id,role,scope,model,label,base_url,is_default) VALUES($1,'llm','deployment',$2,$2,$3,$4)")
            .bind(id).bind(model).bind(&upstream_url).bind(id==cloud).execute(&pool).await?;
        let profile:ModelProfile=serde_json::from_value(json!({"provider_id":id,"provider_revision":1,"revision":1,"adapter":"openai_chat","model":model,
            "locality":locality,"locality_trusted":true,"quality_status":"declared","tier":if id==cloud{"advanced"}else{"economy"},"auto_enabled":auto,
            "pricing":{"input_microusd_per_million":cost,"output_microusd_per_million":cost},
            "capabilities":{"context_tokens":128000,"max_output_tokens":8192,"structured_output":"declared","tools":"declared"}}))?;
        store::save_profile(&state,&actor,id,store::SaveProfile{expected_revision:0,profile}).await?;
    }
    let policy:RoutingPolicy=serde_json::from_value(json!({"id":Uuid::now_v7(),"revision":1,"strategy":"classifier","rollout":"active",
        "default_provider_id":cloud,"classifier_provider_id":judge,"constraints":{"max_calls":8,"max_duration_ms":15000}}))?;
    store::save_policy(&state,&actor,policy.id,store::SavePolicy{label:"Routing stack policy".into(),scope:"deployment".into(),expected_revision:0,is_default:true,policy}).await?;
    let _=fosnie_backend::chat::browser::set_enabled(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(actor.clone()),
        Json(fosnie_backend::http::voice_routing::Enabled{enabled:true})).await?;
    let router=fosnie_backend::http::router(state.clone(),None,None,None,None);
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let base=format!("http://{}",listener.local_addr()?);
    servers.0.push(tokio::spawn(async move{axum::serve(listener,router).await.unwrap();}));
    let status=tokio::time::timeout(Duration::from_secs(120),tokio::process::Command::new("node").args(["node_modules/@playwright/test/cli.js","test","-c","playwright.routing-stack.config.ts"])
        .current_dir(core.join("frontend")).env("FOSNIE_E2E_BACKEND",base).env("FOSNIE_E2E_AGENT",agent.to_string()).env_remove("EDITION_ENTRY").kill_on_drop(true).status()).await??;
    if !status.success() {eprintln!("Synthetic upstream trace: {:?}",trace.lock().unwrap().iter()
        .map(|r|json!({"model":r["model"],"stream":r["stream"]})).collect::<Vec<_>>());}
    anyhow::ensure!(status.success(),"Real routing-stack browser acceptance failed");
    {let calls=trace.lock().unwrap();
        assert_eq!(calls.iter().filter(|r|r["model"]=="routing-e2e-judge").count(),1,"first Auto classifies; Off and local singleton do not");
        assert!(!calls.iter().any(|r|r["model"]=="routing-e2e-cloud"),"local-only is preserved when Auto is Off");
        assert_eq!(calls.iter().filter(|r|r["stream"]==true).count(),3,"no model replay for refused turn");
    }
    let decisions:Vec<Value>=sqlx::query_scalar("SELECT routing_decision FROM messages WHERE role='assistant' AND routing_decision IS NOT NULL ORDER BY created_at")
        .fetch_all(&pool).await?;
    assert_eq!(decisions.len(),3);assert!(decisions.iter().all(|d|d["actual"]["provider_id"]==json!(local)));
    builder_cases::exercise(&state,&actor,local,cloud,judge,&trace).await?;
    ml.kill().await?;eprintln!("P11 actual Rust + ML + product browser: Auto, reload, Off/local-only, reenable and gate refusal passed");Ok(())
}

#[tokio::test(flavor="multi_thread",worker_threads=4)]
async fn r01_r14_r35_real_core_routing_stack()->anyhow::Result<()> {
    let options:sqlx::postgres::PgConnectOptions=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL").expect("Isolated routing_ DB required").parse()?;
    anyhow::ensure!(options.get_database().is_some_and(|d|d.starts_with("routing_")),"Dedicated routing_ DB required");
    let mut conn=sqlx::PgConnection::connect_with(&options).await?;let schema=format!("routing_stack_{}",Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&mut conn).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"))).execute(&mut conn).await?;
    let result=async {
        for migration in sqlx::migrate!("./migrations").iter(){sqlx::raw_sql(migration.sql.clone()).execute(&mut conn).await?;}
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(8).after_connect({let schema=schema.clone();move|conn,_|{
            let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));Box::pin(async move{sqlx::raw_sql(sql).execute(conn).await?;Ok(())})
        }}).connect_with(options).await?;
        let result=tokio::spawn(stack(pool.clone())).await;pool.close().await;result?
    }.await;
    conn.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE"))).execute(&mut conn).await?;result
}
