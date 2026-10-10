// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit isolated migration/off-switch rehearsal; never starts an inference server.
use std::{io::BufRead,sync::Arc};
use axum::{Json,extract::State};
use fosnie_backend::{auth::{AuthContext,PlatformRole,keycloak::AuthUser},config::BootConfig,
    model_routing::*,state::AppStateBuilder};
use serde::Deserialize;
use serde_json::{json,Value};
use sqlx::Connection;
use uuid::Uuid;

const LOCAL:Uuid=Uuid::from_u128(0x48001);
const CLOUD:Uuid=Uuid::from_u128(0x48002);
const USER:Uuid=Uuid::from_u128(0x48003);
const POLICY:Uuid=Uuid::from_u128(0x48004);
const AUTO_CHAT:Uuid=Uuid::from_u128(0x48005);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {schema:String,command:String}
fn actor()->AuthContext {AuthContext{user_id:Some(USER),email:None,display_name:Some("Release rehearsal".into()),
    role:PlatformRole::ClientAdmin,break_glass:false,mfa_enroll_only:false}}

async fn snapshot(pool:&sqlx::PgPool)->anyhow::Result<Value> {
    Ok(sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('choices',(SELECT jsonb_agg(jsonb_build_array(id,model_selection,routing_options,llm_provider_id) ORDER BY id) FROM chats WHERE owner_user_id=$1),'history',(SELECT jsonb_agg(jsonb_build_array(id,role,content,turn_id,completed_at) ORDER BY sequence_number) FROM messages WHERE chat_id=$2),'profiles',(SELECT jsonb_agg(jsonb_build_array(provider_id,provider_revision,revision,profile) ORDER BY provider_id) FROM model_profiles WHERE provider_id=ANY($3)),'policy',(SELECT jsonb_build_array(id,revision,is_default,policy) FROM routing_policies WHERE id=$4))")
        .bind(USER).bind(AUTO_CHAT).bind(vec![LOCAL,CLOUD]).bind(POLICY).fetch_one(pool).await?)
}
async fn seed(state:&fosnie_backend::AppState)->anyhow::Result<()> {
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,'release-rehearsal@routing.test','Release rehearsal','client_admin')").bind(USER).execute(&state.pg).await?;
    for (id,name,locality) in [(LOCAL,"rehearsal-local","local"),(CLOUD,"rehearsal-cloud","cloud")] {
        sqlx::query("INSERT INTO provider_configs(id,role,scope,model,label,base_url,is_default) VALUES($1,'llm','deployment',$2,$2,'http://127.0.0.1:9/v1',$3)").bind(id).bind(name).bind(id==LOCAL).execute(&state.pg).await?;
        let profile:ModelProfile=serde_json::from_value(json!({"provider_id":id,"provider_revision":1,"revision":1,"model":name,
            "adapter":"openai_chat","locality":locality,"locality_trusted":locality=="local","auto_enabled":true,"quality_status":"declared",
            "capabilities":{"context_tokens":4096,"max_output_tokens":1024}}))?;
        store::save_profile(state,&actor(),id,store::SaveProfile{expected_revision:0,profile}).await?;
    }
    let policy:RoutingPolicy=serde_json::from_value(json!({"id":POLICY,"revision":1,"rollout":"active","strategy":"rules",
        "default_provider_id":LOCAL,"constraints":{"allowed_providers":[LOCAL,CLOUD],"local_only":true,"max_calls":4,"max_total_tokens":8192,"max_duration_ms":10000},
        "rules":[{"id":Uuid::from_u128(0x48010),"priority":10,"conditions":{},"action":{"kind":"select","provider_id":LOCAL}}]}))?;
    store::save_policy(state,&actor(),POLICY,store::SavePolicy{label:"Release rehearsal".into(),scope:"deployment".into(),expected_revision:0,is_default:true,policy}).await?;
    for (id,selection) in [(AUTO_CHAT,json!({"mode":"auto","objective":"balanced","policy_id":POLICY})),
        (Uuid::from_u128(0x48006),json!({"mode":"manual","provider_id":LOCAL})),(Uuid::from_u128(0x48007),json!({"mode":"default"}))] {
        sqlx::query("INSERT INTO chats(id,owner_user_id,title,model_selection,routing_options) VALUES($1,$2,'Preserved choice',$3,$4)")
            .bind(id).bind(USER).bind(selection).bind(json!({"local_only":true,"effort":null})).execute(&state.pg).await?;
    }
    sqlx::query("INSERT INTO browser_routed_chats(chat_id) VALUES($1)").bind(AUTO_CHAT).execute(&state.pg).await?;
    sqlx::query("INSERT INTO messages(id,chat_id,role,sequence_number,content,turn_id,completed_at) VALUES($1,$2,'user',1,'Synthetic rehearsal question',$3,now()),($4,$2,'assistant',2,'Preserved synthetic answer',$3,now())")
        .bind(Uuid::from_u128(0x48008)).bind(AUTO_CHAT).bind(Uuid::from_u128(0x48009)).bind(Uuid::from_u128(0x48011)).execute(&state.pg).await?;
    let _=fosnie_backend::chat::browser::set_enabled(State(state.clone()),AuthUser(actor()),Json(fosnie_backend::http::voice_routing::Enabled{enabled:false})).await?;
    Ok(())
}
async fn check_plan(state:&fosnie_backend::AppState,selection:ModelSelection)->anyhow::Result<ExecutionPlan> {
    let ctx=RoutingContext{actor:actor(),chat_id:Some(AUTO_CHAT),project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Primary};
    let (plan,budget)=service::selected_plan(state,&ctx,selection,WorkloadRequirements{task:TaskKind::General,input_tokens:128,output_tokens:32,..Default::default()},"Synthetic rehearsal question",&tokio::sync::Notify::new()).await?;
    anyhow::ensure!(plan.assignments[&CallPurpose::Primary].provider_id==LOCAL && plan.constraints.local_only && budget.snapshot().calls==0,"checked local plan required");
    Ok(plan)
}

#[tokio::main]
async fn main()->anyhow::Result<()> {
    anyhow::ensure!(std::env::var("FOSNIE_ROUTING_RELEASE_REHEARSAL").as_deref()==Ok("isolated"),"explicit isolated rehearsal required");
    let options:sqlx::postgres::PgConnectOptions=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL")?.parse()?;
    anyhow::ensure!(options.get_host()=="127.0.0.1" && options.get_port()==15442 && options.get_database()==Some("routing_release_v48"),"dedicated rehearsal database required");
    let request:Request=serde_json::from_str(&std::io::stdin().lines().next().ok_or_else(||anyhow::anyhow!("request required"))??)?;
    anyhow::ensure!(request.schema.starts_with("routing_release_") && request.schema.len()==48 && request.schema[16..].bytes().all(|b|b.is_ascii_hexdigit()&&!b.is_ascii_uppercase()),"owned schema required");
    let mut conn=sqlx::PgConnection::connect_with(&options).await?;
    if request.command=="baseline" {sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {}",request.schema))).execute(&mut conn).await?;}
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {}",request.schema))).execute(&mut conn).await?;
    let pool=sqlx::postgres::PgPoolOptions::new().max_connections(3).after_connect({let schema=request.schema.clone();move|conn,_|{
        let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));Box::pin(async move{sqlx::raw_sql(sql).execute(conn).await?;Ok(())})
    }}).connect_with(options).await?;
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16490")?,Arc::new(BootConfig::default())).build();
    let full=sqlx::migrate!("./migrations");
    let mut out=json!({"command":request.command,"schema":request.schema,"external_model_api_calls":0,"production_changed":false});
    match request.command.as_str() {
        "baseline"=>{let migrations=full.iter().filter(|m|m.version<138).cloned().collect();sqlx::migrate::Migrator::with_migrations(migrations).run(&mut conn).await?;seed(&state).await?;},
        "upgrade"=>{
            let before=snapshot(&pool).await?;full.run(&mut conn).await?;full.run(&mut conn).await?;
            anyhow::ensure!(before==snapshot(&pool).await?,"upgrade changed saved data");
            let tables:i64=sqlx::query_scalar("SELECT count(*) FROM information_schema.tables WHERE table_schema=current_schema() AND table_name IN ('routing_provider_health','routing_credential_bindings','routing_credential_health')").fetch_one(&pool).await?;
            anyhow::ensure!(tables==3 && !fosnie_backend::chat::browser::enabled(&state).await?,"additive migration cannot activate routing");
            out["repeat_migration_safe"]=json!(true);out["health_tables"]=json!(tables);
        },
        "off_on"=>{
            let before=snapshot(&pool).await?;
            let mut policy=store::load_policy(&state,&actor(),Some(POLICY)).await?;
            let original=policy.clone();policy.rollout=RolloutMode::Off;
            policy=store::save_policy(&state,&actor(),POLICY,store::SavePolicy{label:"Release rehearsal".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy}).await?;
            let plan=check_plan(&state,ModelSelection::Auto{objective:RoutingObjective::Quality,policy_id:Some(POLICY)}).await?;
            anyhow::ensure!(plan.decision.reason_codes.iter().any(|r|r=="routing_off"),"off reason required");
            let ctx=RoutingContext{actor:actor(),chat_id:Some(AUTO_CHAT),project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Primary};
            anyhow::ensure!(service::selected_plan(&state,&ctx,ModelSelection::Manual{provider_id:CLOUD},WorkloadRequirements{output_tokens:32,..Default::default()},"Synthetic rehearsal",&tokio::sync::Notify::new()).await.is_err(),"off must retain locality");
            let _=fosnie_backend::chat::browser::set_enabled(State(state.clone()),AuthUser(actor()),Json(fosnie_backend::http::voice_routing::Enabled{enabled:false})).await?;
            anyhow::ensure!(fosnie_backend::model_routing::admission::require_legacy_chat(&state,Some(AUTO_CHAT)).await.is_err(),"adopted chat cannot downgrade to legacy");
            let after=snapshot(&pool).await?;for key in ["choices","history","profiles"] {anyhow::ensure!(before[key]==after[key],"off changed retained data");}
            let revision=policy.revision;policy=original;policy.revision=revision;
            store::save_policy(&state,&actor(),POLICY,store::SavePolicy{label:"Release rehearsal".into(),scope:"deployment".into(),expected_revision:revision,is_default:true,policy}).await?;
            let _=fosnie_backend::chat::browser::set_enabled(State(state.clone()),AuthUser(actor()),Json(fosnie_backend::http::voice_routing::Enabled{enabled:true})).await?;
            anyhow::ensure!(fosnie_backend::chat::browser::enabled(&state).await?,"explicit re-enable required");
            check_plan(&state,ModelSelection::Auto{objective:RoutingObjective::Balanced,policy_id:Some(POLICY)}).await?;
            for key in ["choices","history","profiles"] {anyhow::ensure!(before[key]==snapshot(&pool).await?[key],"re-enable changed retained data");}
            out["off_preserves_constraints_history_profiles_intent"]=json!(true);out["explicit_reenable"]=json!(true);
        },
        "verify"=>{},
        _=>anyhow::bail!("unknown rehearsal command"),
    }
    out["migration_version"]=json!(sqlx::query_scalar::<_,i64>("SELECT max(version) FROM _sqlx_migrations").fetch_one(&pool).await?);
    out["snapshot"]=snapshot(&pool).await?;
    pool.close().await;println!("{}",out);Ok(())
}
