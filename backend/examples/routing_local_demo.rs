// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit loopback demo with an isolated schema and synthetic identity.
use std::{io::BufRead, path::PathBuf, sync::Arc};
use axum::{Json, routing::get};
use fosnie_backend::{AppState, auth::{AuthContext, PlatformRole}, config::BootConfig,
    model_routing::*, state::AppStateBuilder};
use serde::Deserialize;
use serde_json::json;
use sqlx::{Connection, Executor};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {ml_url:String,ml_secret:String,token:String,storage:String,models_url:String,redis_url:String,ui_url:String,provider_url:String,cloud_models:Vec<String>}
struct Auth {actor:AuthContext,token:String}
#[async_trait::async_trait]
impl fosnie_backend::ext::AuthProvider for Auth {
    async fn authenticate(&self,p:&mut axum::http::request::Parts,_:&AppState)->fosnie_backend::Result<AuthContext> {
        if p.headers.get("authorization").and_then(|v|v.to_str().ok()) != Some(format!("Bearer {}",self.token).as_str()) {
            return Err(RoutingError::Forbidden.into());
        }
        Ok(self.actor.clone())
    }
}
fn local_url(value:&str,scheme:&str)->bool {
    reqwest::Url::parse(value).is_ok_and(|u|u.scheme()==scheme && u.host_str()==Some("127.0.0.1")
        && u.port().is_some() && u.username().is_empty() && u.password().is_none() && u.query().is_none() && u.fragment().is_none())
}
async fn serve(pool:sqlx::PgPool,c:Config)->anyhow::Result<()> {
    anyhow::ensure!(local_url(&c.ml_url,"http") && local_url(&c.models_url,"http") && local_url(&c.redis_url,"redis")
        && local_url(&c.ui_url,"http") && c.token.len()>=32,"isolated loopback configuration required");
    let root=PathBuf::from(&c.storage);anyhow::ensure!(root.is_absolute() && root.is_dir(),"dedicated storage required");
    let uid=Uuid::now_v7();
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Local routing demo','client_admin')")
        .bind(uid).bind(format!("{uid}@routing.test")).execute(&pool).await?;
    let actor=AuthContext{user_id:Some(uid),email:None,display_name:Some("Local demo".into()),role:PlatformRole::ClientAdmin,break_glass:false,mfa_enroll_only:false};
    let agent=Uuid::now_v7();
    sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'Local routing demo','Answer briefly in the language of the user.','{}'::jsonb,$2,ARRAY['general'])")
        .bind(agent).bind(uid).execute(&pool).await?;
    let mut boot=BootConfig::default();boot.ml.base_url=c.ml_url;boot.ml.shared_secret=c.ml_secret;
    boot.features.agents_enabled=true;
    boot.server.static_dir="".into();boot.server.public_url=c.ui_url;
    macro_rules! folder {($field:ident)=>{{let p=root.join(stringify!($field));tokio::fs::create_dir_all(&p).await?;boot.storage.$field=p.to_string_lossy().into();}};}
    folder!(branding_dir);folder!(artefacts_dir);folder!(documents_dir);folder!(workspace_dir);folder!(skills_dir);
    folder!(prompts_dir);folder!(exports_dir);folder!(message_attachments_dir);folder!(avatars_dir);
    folder!(recordings_dir);folder!(chat_attachments_dir);folder!(desktop_installer_dir);
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool(&c.redis_url)?,Arc::new(boot))
        .with_auth(Arc::new(Auth{actor:actor.clone(),token:c.token})).build();
    let qwen=Uuid::now_v7();let llama=Uuid::now_v7();
    for (id,label,model,tier,json_support,efforts,preset) in [
        (qwen,"Qwen 4B · local","huihui_ai/qwen3.5-abliterated:4b-Claude","balanced","unknown",vec!["none"],Some("none")),
        (llama,"Llama 3B · local","hf.co/QuantFactory/Llama-3.2-3B-Instruct-abliterated-GGUF:Q4_K_M","economy","declared",vec![],None)] {
        sqlx::query("INSERT INTO provider_configs(id,role,scope,model,label,base_url,is_default) VALUES($1,'llm','deployment',$2,$3,$4,$5)")
            .bind(id).bind(model).bind(label).bind(&c.models_url).bind(id==qwen).execute(&pool).await?;
        let profile:ModelProfile=serde_json::from_value(json!({"provider_id":id,"provider_revision":1,"revision":1,
            "adapter":"openai_chat","model":model,"locality":"local","locality_trusted":true,
            "quality_status":"declared","tier":tier,"auto_enabled":true,"auto_effort":preset,
            "capabilities":{"context_tokens":8192,"max_output_tokens":2048,"structured_output":json_support,"tools":if id==qwen {"declared"} else {"unknown"},"efforts":efforts}}))?;
        store::save_profile(&state,&actor,id,store::SaveProfile{expected_revision:0,profile}).await?;
    }
    let mut policies=Vec::new();
    for (strategy,label) in [("hybrid","Local Hybrid"),("rules","Local Rules: Qwen"),("classifier","Local Classifier")] {
        let rules=if strategy=="rules" {json!([{"id":Uuid::now_v7(),"priority":10,"conditions":{},"action":{"kind":"select","provider_id":qwen}}])}
            else {json!([])};
        let policy:RoutingPolicy=serde_json::from_value(json!({"id":Uuid::now_v7(),"revision":1,"strategy":strategy,"rollout":"active",
            "default_provider_id":qwen,"classifier_provider_id":llama,"rules":rules,"technical_fallback":true,
            "constraints":{"allowed_providers":[qwen,llama],"local_only":true,"max_calls":12,"max_output_tokens":1024,"max_total_tokens":65536,"max_duration_ms":150000},
            "classifier":{"native_schema":false,"min_confidence":0.75,"max_input_tokens":4096,"max_output_tokens":256,
                "local_timeout_ms":30000,"cloud_timeout_ms":30000,"answer_reserve_ms":15000}}))?;
        let saved=store::save_policy(&state,&actor,policy.id,store::SavePolicy{label:label.into(),scope:"deployment".into(),
            expected_revision:0,is_default:strategy=="hybrid" && c.cloud_models.is_empty(),policy}).await?;
        policies.push(json!({"id":saved.id,"strategy":strategy}));
    }
    let web_agent=Uuid::now_v7();
    sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'Quick web demo','For current information call web_search with depth quick. Use only dated evidence, distinguish forecasts from observations and cite sources. If snippets do not establish the current fact, say it is unavailable. Answer briefly.', $2,$3,ARRAY['general'])")
        .bind(web_agent).bind(json!({"web_depth_max":"quick","web_max_fetches":2,"max_steps":3,"wall_clock_secs":150})).bind(uid).execute(&pool).await?;
    sqlx::query("INSERT INTO agent_tools(agent_id,tool_name) VALUES($1,'web_search')").bind(web_agent).execute(&pool).await?;
    for key in ["integration.web_search.enabled","web_search.quick_snippets_only"] {
        sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,'true','bool') ON CONFLICT(key) DO UPDATE SET value='true',value_type='bool'")
            .bind(key).execute(&pool).await?;
    }
    let mut cloud_ids=Vec::new();let mut openai=None;let mut gemini=None;
    for name in &c.cloud_models {
        let (model,label,adapter,tier,path,efforts,preset)=match name.as_str() {
            "openai"=>("gpt-5.4-mini","GPT-5.4 Mini · cloud","openai_responses","advanced","/openai/v1",vec!["none","low","medium","high","xhigh"],Some("none")),
            "gemini"=>("gemini-3.1-flash-lite","Gemini Lite · cloud","gemini","balanced","/gemini/v1beta",vec![],None),
            _=>anyhow::bail!("unknown demo cloud provider"),
        };
        let id=Uuid::now_v7();
        sqlx::query("INSERT INTO provider_configs(id,role,scope,model,label,base_url) VALUES($1,'llm','deployment',$2,$3,$4)")
            .bind(id).bind(model).bind(label).bind(format!("{}{}",c.provider_url,path)).execute(&pool).await?;
        let profile:ModelProfile=serde_json::from_value(json!({"provider_id":id,"provider_revision":1,"revision":1,
            "adapter":adapter,"model":model,"locality":"cloud","quality_status":"declared","tier":tier,"auto_enabled":true,"auto_effort":preset,
            "capabilities":{"context_tokens":32768,"max_output_tokens":2048,"structured_output":"declared","tools":"declared","efforts":efforts}}))?;
        store::save_profile(&state,&actor,id,store::SaveProfile{expected_revision:0,profile}).await?;
        cloud_ids.push(id);if name=="openai" {openai=Some(id);} else {gemini=Some(id);}
    }
    if let (Some(primary),Some(judge))=(openai,gemini) {
        for (label,allowed,local_default) in [("Mixed Hybrid",vec![qwen,llama,primary,judge],qwen),
                                             ("Cloud-only Classifier",cloud_ids.clone(),primary)] {
            let policy:RoutingPolicy=serde_json::from_value(json!({"id":Uuid::now_v7(),"revision":1,"rollout":"active",
                "strategy":if label.starts_with("Mixed") {"hybrid"} else {"classifier"},"default_provider_id":local_default,"classifier_provider_id":judge,
                "technical_fallback":true,"constraints":{"allowed_providers":allowed,"max_calls":12,"max_total_tokens":65536,"max_output_tokens":1024,"max_duration_ms":150000},
                "classifier":{"native_schema":false,"min_confidence":0.75,"max_input_tokens":4096,"max_output_tokens":512,
                    "local_timeout_ms":30000,"cloud_timeout_ms":30000,"answer_reserve_ms":20000}}))?;
            let saved=store::save_policy(&state,&actor,policy.id,store::SavePolicy{label:label.into(),scope:"deployment".into(),expected_revision:0,is_default:label=="Mixed Hybrid",policy}).await?;
            policies.push(json!({"id":saved.id,"strategy":saved.strategy,"label":label}));
        }
    }

    fosnie_backend::chat::browser::set_enabled(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(actor.clone()),
        Json(fosnie_backend::http::voice_routing::Enabled{enabled:true})).await?;
    let info=json!({"agent_id":agent,"providers":{"qwen":qwen,"llama":llama},"policies":policies,
        "web_agent_id":web_agent,"local_only":false,"cloud_models_connected":!cloud_ids.is_empty(),"cloud_providers":{"openai":openai,"gemini":gemini},"quick_snippets_only":true,"error_handling_stage":"V45","technical_fallback":true,"profiles":"operator declarations, not verified model quality",
        "scope":"general remote text and explicitly selected quick web agent; cloud allowance12 calls/15min, no documents or production data"});
    let router=fosnie_backend::http::router(state.clone(),None,None,None,None)
        .route("/demo/info",get(move ||{let value=info.clone();async move{Json(value)}}));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("{}",json!({"url":format!("http://{}",listener.local_addr()?)}));
    use std::io::Write;std::io::stdout().flush()?;
    axum::serve(listener,router).with_graceful_shutdown(async{let _=tokio::signal::ctrl_c().await;}).await?;Ok(())
}
#[tokio::main]
async fn main()->anyhow::Result<()> {
    anyhow::ensure!(std::env::var("FOSNIE_ROUTING_LOCAL_DEMO").as_deref()==Ok("isolated"),"explicit isolated demo required");
    let options:sqlx::postgres::PgConnectOptions=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL")?.parse()?;
    anyhow::ensure!(options.get_host()=="127.0.0.1" && options.get_database()==Some("routing_demo"),"dedicated loopback demo database required");
    let c:Config=serde_json::from_str(&std::io::stdin().lines().next().ok_or_else(||anyhow::anyhow!("demo config required"))??)?;
    let mut conn=sqlx::PgConnection::connect_with(&options).await?;let schema=format!("routing_demo_{}",Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&mut conn).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"))).execute(&mut conn).await?;
    let result=async {
        for migration in sqlx::migrate!("./migrations").iter(){sqlx::raw_sql(migration.sql.clone()).execute(&mut conn).await?;}
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(12).after_connect({let schema=schema.clone();move|conn,_|{
            let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));Box::pin(async move{sqlx::raw_sql(sql).execute(conn).await?;Ok(())})
        }}).connect_with(options).await?;
        let result=serve(pool.clone(),c).await;pool.close().await;result
    }.await;
    conn.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE"))).execute(&mut conn).await?;result
}
