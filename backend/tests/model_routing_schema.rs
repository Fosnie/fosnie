// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0

//! Explicit integration lane: absent services fail, never silently skip (R38).
use sqlx::{Connection, Executor};
use uuid::Uuid;
#[path="common/model_routing_fixed_cases.rs"]
mod fixed_cases;
#[path="common/model_routing_chat_cases.rs"]
mod chat_cases;
#[path="common/model_routing_web_cases.rs"]
mod web_cases;
#[path="common/model_routing_document_cases.rs"]
mod document_cases;
#[path="common/model_routing_research_cases.rs"]
mod research_cases;
#[path="common/model_routing_research_jobs.rs"]
mod research_job_cases;
#[path="common/model_routing_document_chat_cases.rs"]
mod document_chat_cases;
#[path="common/model_routing_verify_cases.rs"]
mod verify_cases;
#[path="common/model_routing_draft_cases.rs"]
mod draft_cases;
#[path="common/model_routing_job_http_cases.rs"]
mod job_http_cases;
#[path="common/model_routing_workflow_cases.rs"]
mod workflow_cases;
#[path="common/model_routing_api_cases.rs"]
mod api_cases;
#[path="common/model_routing_api_agent_cases.rs"]
mod api_agent_cases;
#[path="common/model_routing_voice_cases.rs"]
mod voice_cases;
#[path="common/model_routing_voice_live_cases.rs"]
mod voice_live_cases;
#[path="common/model_routing_voice_pcm_cases.rs"]
mod voice_pcm_cases;
#[path="common/model_routing_phone_cases.rs"]
mod phone_cases;
#[path="common/model_routing_regenerate_cases.rs"]
mod regenerate_cases;
#[path="common/model_routing_artefact_cases.rs"]
mod artefact_cases;
#[path="common/model_routing_admission_cases.rs"]
mod admission_cases;
#[path="common/model_routing_browser_cases.rs"]
mod browser_cases;
#[path="common/model_routing_management_cases.rs"]
mod management_cases;
#[path="common/model_routing_alias_cases.rs"]
mod alias_cases;
#[path="common/model_routing_desktop_cases.rs"]
mod desktop_cases;
#[path="common/model_routing_workflow_sources.rs"]
mod workflow_source_cases;
#[path="common/model_routing_workflow_agents.rs"]
mod workflow_agent_cases;
#[path="common/model_routing_workflow_delivery.rs"]
mod workflow_delivery_cases;

#[tokio::test]
async fn r03_r18_r35_r38_upgrade_preserves_selection_and_invalidates_profiles() -> anyhow::Result<()> {
    let url = std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL")
        .expect("routing integration requires FOSNIE_ROUTING_TEST_DATABASE_URL; run tools/routing/test.ps1 -Lane integration");
    let options: sqlx::postgres::PgConnectOptions = url.parse()?;
    anyhow::ensure!(options.get_database().is_some_and(|d| d.starts_with("routing_")), "use a dedicated routing_ test database");
    let mut conn = sqlx::PgConnection::connect_with(&options).await?;
    let schema = format!("routing_upgrade_{}", Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&mut conn).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"))).execute(&mut conn).await?;
    let result = async {
        check_upgrade(&mut conn).await?;
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(3)
            .after_connect({ let schema = schema.clone(); move |conn, _| {
                let sql = sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));
                Box::pin(async move { sqlx::raw_sql(sql).execute(conn).await?; Ok(()) })
            }}).connect_with(options).await?;
        let checked = async { check_strict_targets(pool.clone()).await?; fixed_cases::check_fixed(pool.clone()).await?; check_catalog(pool.clone()).await }.await;
        pool.close().await;
        checked
    }.await;
    conn.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE"))).execute(&mut conn).await?;
    result
}

/// A small isolated lane for legacy-admission follow-ups; no inference suite replay.
#[tokio::test]
async fn r38_legacy_admission_preserves_routed_intent() -> anyhow::Result<()> {
    use fosnie_backend::auth::{AuthContext, PlatformRole};
    let url=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL").expect("isolated routing database is required");
    let options:sqlx::postgres::PgConnectOptions=url.parse()?;
    anyhow::ensure!(options.get_database().is_some_and(|d|d.starts_with("routing_")),"use a dedicated routing_ test database");
    let mut conn=sqlx::PgConnection::connect_with(&options).await?;
    let schema=format!("routing_admission_{}",Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&mut conn).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"))).execute(&mut conn).await?;
    let result=async {
        Box::pin(check_upgrade(&mut conn)).await?;
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(3).after_connect({let schema=schema.clone();move |conn,_|{
            let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));
            Box::pin(async move {sqlx::raw_sql(sql).execute(conn).await?;Ok(())})
        }}).connect_with(options).await?;
        let checked=async {
            let user=Uuid::now_v7(); let provider=Uuid::now_v7();
            sqlx::query("INSERT INTO users(id,display_name,email,role) VALUES($1,'Admission',$2,'user')").bind(user).bind(format!("{user}@routing.test")).execute(&pool).await?;
            sqlx::query("INSERT INTO provider_configs(id,role,scope,model) VALUES($1,'llm','deployment','admission-model')").bind(provider).execute(&pool).await?;
            let actor=AuthContext {user_id:Some(user),email:None,display_name:None,role:PlatformRole::User,break_glass:false,mfa_enroll_only:false};
            admission_cases::check(&pool,&actor,provider).await
        }.await;
        pool.close().await;checked
    }.await;
    conn.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE"))).execute(&mut conn).await?;
    result
}

/// Independent focused browser lane; common caller acceptance is not replayed
/// when only this product entry or its fixtures need a repair.
#[tokio::test]
async fn r08_browser_chat_intent_and_actual_metadata() -> anyhow::Result<()> {
    use fosnie_backend::auth::{AuthContext,PlatformRole};
    let url=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL").expect("isolated routing database is required");
    let options:sqlx::postgres::PgConnectOptions=url.parse()?;
    anyhow::ensure!(options.get_database().is_some_and(|d|d.starts_with("routing_")),"use a dedicated routing_ test database");
    let mut conn=sqlx::PgConnection::connect_with(&options).await?;
    let schema=format!("routing_browser_{}",Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&mut conn).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"))).execute(&mut conn).await?;
    let result=async {
        Box::pin(check_upgrade(&mut conn)).await?;
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(3).after_connect({let schema=schema.clone();move |conn,_|{
            let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));
            Box::pin(async move {sqlx::raw_sql(sql).execute(conn).await?;Ok(())})
        }}).connect_with(options).await?;
        let checked=async {
            let user=Uuid::now_v7();
            sqlx::query("INSERT INTO users(id,display_name,email,role) VALUES($1,'Browser manager',$2,'client_admin')").bind(user).bind(format!("{user}@routing.test")).execute(&pool).await?;
            let admin=AuthContext {user_id:Some(user),email:None,display_name:None,role:PlatformRole::ClientAdmin,break_glass:false,mfa_enroll_only:false};
            browser_cases::check(&pool,&admin).await
        }.await;
        pool.close().await;checked
    }.await;
    conn.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE"))).execute(&mut conn).await?;
    result
}

#[tokio::test]
async fn r08_routing_management_and_bounded_judge_probe()->anyhow::Result<()> {
    let url=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL").expect("isolated routing database is required");
    let options:sqlx::postgres::PgConnectOptions=url.parse()?;
    anyhow::ensure!(options.get_database().is_some_and(|d|d.starts_with("routing_")),"use a dedicated routing_ test database");
    let mut conn=sqlx::PgConnection::connect_with(&options).await?;
    let schema=format!("routing_management_{}",Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&mut conn).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"))).execute(&mut conn).await?;
    let result=async {
        Box::pin(check_upgrade(&mut conn)).await?;
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(3).after_connect({let schema=schema.clone();move |conn,_|{
            let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));
            Box::pin(async move {sqlx::raw_sql(sql).execute(conn).await?;Ok(())})
        }}).connect_with(options).await?;
        let checked=management_cases::check(&pool).await;pool.close().await;checked
    }.await;
    conn.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE"))).execute(&mut conn).await?;
    result
}

#[tokio::test]
async fn r08_public_api_alias_collision_and_execution()->anyhow::Result<()> {
    let url=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL").expect("isolated routing database is required");
    let options:sqlx::postgres::PgConnectOptions=url.parse()?;
    anyhow::ensure!(options.get_database().is_some_and(|d|d.starts_with("routing_")),"use a dedicated routing_ test database");
    let mut conn=sqlx::PgConnection::connect_with(&options).await?;
    let schema=format!("routing_alias_{}",Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&mut conn).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"))).execute(&mut conn).await?;
    let result=async {
        Box::pin(check_upgrade(&mut conn)).await?;
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(5).after_connect({let schema=schema.clone();move |conn,_|{
            let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));
            Box::pin(async move{sqlx::raw_sql(sql).execute(conn).await?;Ok(())})
        }}).connect_with(options).await?;
        let checked=alias_cases::check(&pool).await;pool.close().await;checked
    }.await;
    conn.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE"))).execute(&mut conn).await?;
    result
}

#[tokio::test]
async fn r08_desktop_remote_credential_and_compatibility()->anyhow::Result<()> {
    let url=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL").expect("isolated routing database is required");
    let options:sqlx::postgres::PgConnectOptions=url.parse()?;
    anyhow::ensure!(options.get_database().is_some_and(|d|d.starts_with("routing_")),"use a dedicated routing_ test database");
    let mut conn=sqlx::PgConnection::connect_with(&options).await?;let schema=format!("routing_desktop_{}",Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&mut conn).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"))).execute(&mut conn).await?;
    let result=async {
        Box::pin(check_upgrade(&mut conn)).await?;
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(6).after_connect({let schema=schema.clone();move |conn,_|{
            let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));Box::pin(async move{sqlx::raw_sql(sql).execute(conn).await?;Ok(())})
        }}).connect_with(options).await?;
        let checked=desktop_cases::check(&pool).await;pool.close().await;checked
    }.await;
    conn.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE"))).execute(&mut conn).await?;result
}

async fn check_catalog(pool: sqlx::PgPool) -> anyhow::Result<()> {
    use fosnie_backend::{auth::{AuthContext, PlatformRole}, config::BootConfig,
        model_routing::{*, store}, state::AppStateBuilder};
    use std::sync::Arc;
    struct DelegatedProviders;
    #[async_trait::async_trait]
    impl fosnie_backend::ext::RbacPolicy for DelegatedProviders {
        async fn can(&self, _: &sqlx::PgPool, _: &AuthContext, _: fosnie_backend::auth::rbac::ResourceType, _: Uuid, _: fosnie_backend::auth::rbac::Permission) -> fosnie_backend::Result<bool> { Ok(false) }
        async fn may_grant(&self, _: &sqlx::PgPool, _: &AuthContext, _: fosnie_backend::auth::rbac::ResourceType, _: Uuid) -> fosnie_backend::Result<bool> { Ok(false) }
        async fn has_permission(&self, _: &sqlx::PgPool, ctx: &AuthContext, permission: &str) -> fosnie_backend::Result<bool> {
            Ok(permission==fosnie_backend::auth::permissions::PROVIDERS_MANAGE && ctx.display_name.as_deref()==Some("delegated-provider-manager"))
        }
    }
    let state = AppStateBuilder::new(pool.clone(), fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?, Arc::new(BootConfig::default())).with_rbac(Arc::new(DelegatedProviders)).build();
    let user = Uuid::now_v7();
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'Catalog', $2, 'user')")
        .bind(user).bind(format!("{user}@routing.test")).execute(&pool).await?;
    sqlx::query("INSERT INTO provider_configs (id, role, scope, model, label, base_url) VALUES ($1,'llm','deployment','catalog-model',$2,'https://model.test/v1')")
        .bind(id).bind(id.to_string()).execute(&pool).await?;
    let ctx = AuthContext { user_id: Some(user), email: None, display_name: None, role: PlatformRole::User, break_glass: false, mfa_enroll_only: false };
    let mut admin = ctx.clone(); admin.display_name = Some("delegated-provider-manager".into());
    assert!(!admin.is_admin(), "catalog writes use the permission seam, not an admin role shortcut");
    let profile: ModelProfile = serde_json::from_value(serde_json::json!({
        "provider_id":id,"provider_revision":1,"revision":1,"adapter":"openai_chat","model":"catalog-model",
        "auto_enabled":true,"quality_status":"declared","locality":"local","locality_trusted":true,
        "capabilities":{"context_tokens":32000,"max_output_tokens":8192,"structured_output":"declared"}
    }))?;
    assert!(store::save_profile(&state, &ctx, id, store::SaveProfile { expected_revision:0, profile:profile.clone() }).await.is_err());
    let saved = store::save_profile(&state, &admin, id, store::SaveProfile { expected_revision:0, profile:profile.clone() }).await?;
    assert!(saved.revision > 0);
    let personal_id = Uuid::now_v7();
    sqlx::query("INSERT INTO provider_configs (id,role,scope,scope_id,model,label) VALUES ($1,'llm','user',$2,'catalog-model','Personal')")
        .bind(personal_id).bind(user).execute(&pool).await?;
    let mut personal = profile.clone(); personal.provider_id=personal_id; personal.locality_trusted=false;
    let personal = store::save_profile(&state,&ctx,personal_id,store::SaveProfile { expected_revision:0,profile:personal }).await?;
    let mut foreign = ctx.clone(); foreign.user_id=Some(Uuid::now_v7());
    assert!(store::save_profile(&state,&foreign,personal_id,store::SaveProfile { expected_revision:personal.revision,profile:personal.clone() }).await.is_err());
    let mut untrusted = personal.clone(); untrusted.locality_trusted=true;
    assert!(store::save_profile(&state,&ctx,personal_id,store::SaveProfile { expected_revision:personal.revision,profile:untrusted }).await.is_err());
    store::delete_profile(&state,&ctx,personal_id,personal.revision).await?;
    assert!(store::save_profile(&state, &admin, id, store::SaveProfile { expected_revision:0, profile:profile.clone() }).await.is_err());
    let policy_id = Uuid::now_v7();
    let policy: RoutingPolicy = serde_json::from_value(serde_json::json!({"id":policy_id,"revision":1,"default_provider_id":id,"classifier_provider_id":null}))?;
    let body = || store::SavePolicy { label:"Test".into(),scope:"deployment".into(),expected_revision:0,is_default:true,policy:policy.clone() };
    assert!(store::save_policy(&state, &ctx, policy_id, body()).await.is_err());
    let saved_policy = store::save_policy(&state, &admin, policy_id, body()).await?;
    assert!(store::save_policy(&state, &admin, policy_id, body()).await.is_err());
    let fixed_id=fixed::catalog(&state,&admin).await?.into_iter().find(|p|p.role==fixed::FixedRole::Tts).unwrap().id;
    let mut with_fixed=policy.clone(); with_fixed.id=Uuid::now_v7();
    with_fixed.constraints.allowed_providers=Some([id,fixed_id].into());
    let with_fixed=store::save_policy(&state,&admin,with_fixed.id,store::SavePolicy { label:"Fixed allowlist".into(),scope:"deployment".into(),expected_revision:0,is_default:false,policy:with_fixed }).await?;
    let mut wrong_target=with_fixed.clone(); wrong_target.default_provider_id=Some(fixed_id);
    assert!(store::save_policy(&state,&admin,wrong_target.id,store::SavePolicy { label:"Wrong primary".into(),scope:"deployment".into(),expected_revision:with_fixed.revision,is_default:false,policy:wrong_target }).await.is_err(),"fixed roles may be allowed but cannot become the chat model");
    store::delete_policy(&state,&admin,with_fixed.id,with_fixed.revision).await?;
    // A fake default ML URL is never contacted by preview. Successful preview
    // therefore proves it performs no classifier/model invocation or probe.
    let preview = fosnie_backend::http::model_routing::preview(axum::extract::State(state.clone()), fosnie_backend::auth::keycloak::AuthUser(ctx.clone()),
        axum::Json(fosnie_backend::http::model_routing::Preview { policy_id:Some(policy_id), expected_policy_revision:None, objective:RoutingObjective::Economy, purpose:CallPurpose::Primary,
            requirements:WorkloadRequirements { input_tokens:100,output_tokens:100,..Default::default() } })).await?;
    assert_eq!(preview.0["inference_calls"], 0);
    assert_eq!(preview.0["execution_target"]["provider_id"], id.to_string());
    assert_eq!(preview.0["decision"]["proposed"], serde_json::Value::Null);
    assert_eq!(preview.0["decision"]["actual"], serde_json::Value::Null);
    let mut service_admin = admin.clone(); service_admin.role = PlatformRole::ClientAdmin;
    check_auto_service(&pool, &service_admin, id, saved_policy.clone()).await?;
    let catalog = serde_json::to_string(&store::catalog(&state, &ctx).await?)?;
    assert!(!catalog.contains("api_key"));
    assert!(store::delete_profile(&state, &ctx, id, 1).await.is_err());
    let latest_profile=store::current_profiles(store::catalog(&state,&admin).await?).into_iter().find(|p|p.provider_id==id).unwrap();
    store::delete_profile(&state, &admin, id, latest_profile.revision).await?;
    let recreated = store::save_profile(&state, &admin, id, store::SaveProfile { expected_revision:0, profile }).await?;
    assert!(recreated.revision > latest_profile.revision, "profile generation never repeats after delete/recreate");
    assert!(store::delete_policy(&state, &ctx, policy_id, 1).await.is_err());
    assert!(store::delete_policy(&state, &admin, policy_id, i64::MAX).await.is_err());
    let latest = store::load_policy(&state, &admin, Some(policy_id)).await?;
    store::delete_policy(&state, &admin, policy_id, latest.revision).await?;
    Ok(())
}

fn check_auto_service<'a>(pool: &'a sqlx::PgPool, admin: &'a fosnie_backend::auth::AuthContext, provider_id: Uuid, mut policy: fosnie_backend::model_routing::RoutingPolicy)
    ->std::pin::Pin<Box<dyn std::future::Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
    use fosnie_backend::{config::BootConfig,model_routing::{*,store},state::AppStateBuilder};
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    let calls = Arc::new(AtomicUsize::new(0));
    let invalid = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let delay = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let partial = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let final_delay = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let app = axum::Router::new().route("/chat-step", axum::routing::post({
        let calls = calls.clone(); let invalid = invalid.clone(); let delay = delay.clone();
        move |axum::Json(body):axum::Json<serde_json::Value>| {
            let calls = calls.clone(); let invalid = invalid.clone(); let delay = delay.clone();
            async move {
                calls.fetch_add(1,Ordering::SeqCst);
                let report=routing_mock_report(&body);
                if let Some(purpose @ ("utility_title"|"utility_summary"))=body["overrides"]["routing"]["purpose"].as_str() {
                    assert!(body["sampling"].get("reasoning_effort").is_none(),"utility effort is owned by the plan");
                    let content=if purpose=="utility_title" { "Planned Chat Title" } else { "Saved summary." };
                    return axum::Json(serde_json::json!({"content":content,"routing_report":report}));
                }
                if body["overrides"]["routing"]["purpose"] == "tool_loop" {
                    return axum::Json(serde_json::json!({"content":"{\"ready\":true}","usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":report}));
                }
                assert_eq!(body["overrides"]["routing"]["purpose"], "router_classify");
                assert!(body.get("tools").is_none());
                tokio::time::sleep(std::time::Duration::from_millis(delay.load(Ordering::SeqCst))).await;
                let text = if invalid.load(Ordering::SeqCst) { "not valid JSON".to_string() }
                    else { serde_json::json!({"task":"general","minimum_tier":"economy","fresh_information":false,"tools":false,"vision":false,"structured_output":false,"confidence":0.95,"uncertain":false}).to_string() };
                axum::Json(serde_json::json!({"content":text,"usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":report}))
            }
        }
    }));
    let app=app.route("/generate",axum::routing::post({
        let calls=calls.clone(); let partial=partial.clone(); let final_delay=final_delay.clone();
        move |axum::Json(body):axum::Json<serde_json::Value>| {
            let calls=calls.clone(); let partial=partial.clone(); let final_delay=final_delay.clone();
            async move {
                calls.fetch_add(1,Ordering::SeqCst);
                assert_eq!(body["overrides"]["routing"]["purpose"],"final");
                tokio::time::sleep(std::time::Duration::from_millis(final_delay.load(Ordering::SeqCst))).await;
                let mut events=String::from("{\"type\":\"token\",\"delta\":\"partial\"}\n");
                if !partial.load(Ordering::SeqCst) {
                    events.push_str(&serde_json::json!({"type":"done","model":"catalog-model","finish_reason":"stop",
                        "routing_report":routing_mock_report(&body),"usage":{"prompt_tokens":100,"completion_tokens":40}}).to_string());
                    events.push('\n');
                }
                events
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
    let mut boot = BootConfig::default(); boot.ml.base_url = format!("http://{address}");
    let state = AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot)).build();
    let context = RoutingContext { actor:admin.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Primary };
    let req = WorkloadRequirements { minimum_tier:ModelTier::Economy,input_tokens:100,output_tokens:100,..Default::default() };
    policy.rollout = RolloutMode::Active; policy.strategy = RoutingStrategy::Hybrid; policy.classifier_provider_id = Some(provider_id);
    policy = store::save_policy(&state,admin,policy.id,store::SavePolicy { label:"Hybrid".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone() }).await?;
    let notify = tokio::sync::Notify::new();
    let (plan,budget) = service::auto_plan(&state,&context,Some(policy.id),RoutingObjective::Economy,req.clone(),"A simple greeting",&notify).await?;
    assert_eq!(calls.load(Ordering::SeqCst),1);
    assert_eq!(budget.snapshot().calls,1);
    assert_eq!(plan.assignments[&CallPurpose::Primary].provider_id,provider_id);
    assert!(plan.decision.actual.is_none());
    assert!(plan.decision.reason_codes.iter().any(|s| s=="classified_requirements"));
    invalid.store(true,Ordering::SeqCst);
    let (fallback,_) = service::auto_plan(&state,&context,Some(policy.id),RoutingObjective::Economy,req.clone(),"bad output",&notify).await?;
    assert_eq!(calls.load(Ordering::SeqCst),2);
    assert!(fallback.decision.reason_codes.iter().any(|s| s=="classifier_fallback"));
    policy.rules.push(RoutingRule { id:Uuid::now_v7(),priority:10,conditions:Default::default(),action:RuleAction::Select { provider_id,effort:None } });
    policy = store::save_policy(&state,admin,policy.id,store::SavePolicy { label:"Terminal".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone() }).await?;
    let (_,budget) = service::auto_plan(&state,&context,Some(policy.id),RoutingObjective::Economy,req.clone(),"terminal rule",&notify).await?;
    assert_eq!(calls.load(Ordering::SeqCst),2,"terminal hybrid rule skips classifier");
    assert_eq!(budget.snapshot().calls,0);
    policy.strategy = RoutingStrategy::Rules; policy.rollout = RolloutMode::Shadow;
    policy.rules = vec![RoutingRule { id:Uuid::now_v7(),priority:10,conditions:Default::default(),action:RuleAction::Require { minimum_tier:ModelTier::Advanced } }];
    policy = store::save_policy(&state,admin,policy.id,store::SavePolicy { label:"Shadow".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone() }).await?;
    let (shadow,_) = service::auto_plan(&state,&context,Some(policy.id),RoutingObjective::Quality,req.clone(),"simple",&notify).await?;
    assert!(shadow.decision.proposed.is_none(),"ineligible proposal is never shown as actual execution");
    assert_eq!(shadow.assignments[&CallPurpose::Primary].provider_id,provider_id);
    assert_eq!(calls.load(Ordering::SeqCst),2,"rules shadow is inference-free");
    use fosnie_backend::http::model_routing::{probe,Probe,ProbeKind};
    let profile_revision = shadow.assignments[&CallPurpose::Primary].profile_revision;
    let request = || Probe { kind:ProbeKind::JsonResponse,expected_profile_revision:profile_revision,max_cost_microusd:None };
    let checked = probe(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(admin.clone()),axum::extract::Path(provider_id),axum::Json(request())).await?;
    assert_eq!(checked.0["compatible"],true);
    assert_eq!(checked.0["quality_evaluated"],false);
    assert_eq!(calls.load(Ordering::SeqCst),3);
    let mut regular = admin.clone(); regular.role=fosnie_backend::auth::PlatformRole::User;
    assert!(probe(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(regular),axum::extract::Path(provider_id),axum::Json(request())).await.is_err());
    let mut capped = request(); capped.max_cost_microusd=Some(1);
    assert!(probe(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(admin.clone()),axum::extract::Path(provider_id),axum::Json(capped)).await.is_err(),"unknown prices cannot satisfy a hard monetary probe cap");
    assert_eq!(calls.load(Ordering::SeqCst),3,"denied probe makes no inference request");
    policy.strategy=RoutingStrategy::Hybrid; policy.rollout=RolloutMode::Active; policy.rules.clear();
    sqlx::query("DELETE FROM routing_provider_health WHERE provider_id=$1").bind(provider_id).execute(pool).await?;
    policy.constraints.max_duration_ms=Some(50);
    policy = store::save_policy(&state,admin,policy.id,store::SavePolicy { label:"Bounded classifier".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone() }).await?;
    let cancelled = tokio::sync::Notify::new(); cancelled.notify_one();
    assert!(service::auto_plan(&state,&context,Some(policy.id),RoutingObjective::Quality,req.clone(),"cancelled",&cancelled).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst),3,"cancelled classification does not dispatch");
    delay.store(200,Ordering::SeqCst);
    let started=std::time::Instant::now();
    // A tiny allowance can expire during metadata checks or skip the classifier
    // to retain answer time. Neither result authorises a paid classifier call.
    if let Ok((plan,budget))=service::auto_plan(&state,&context,Some(policy.id),RoutingObjective::Quality,req,"timeout",&notify).await {
        assert!(plan.decision.reason_codes.iter().any(|code|code.starts_with("classifier_skipped")));
        assert_eq!(budget.snapshot().calls,0);
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(calls.load(Ordering::SeqCst),3,"an unfittable classifier cannot dispatch or reset the allowance");
    delay.store(0,Ordering::SeqCst);
    policy.strategy=RoutingStrategy::Rules;
    policy.constraints.max_duration_ms=Some(10_000);
    policy.constraints.max_calls=Some(1);
    policy.rules=vec![RoutingRule { id:Uuid::now_v7(),priority:10,conditions:Default::default(),action:RuleAction::Select { provider_id,effort:None } }];
    policy=store::save_policy(&state,admin,policy.id,store::SavePolicy { label:"Dispatch".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone() }).await?;
    let (plan,budget)=service::auto_plan(&state,&context,Some(policy.id),RoutingObjective::Economy,WorkloadRequirements { input_tokens:100,output_tokens:128,..Default::default() },"simple",&notify).await?;
    let mut dispatch_context=context.clone(); dispatch_context.purpose=CallPurpose::ToolLoop;
    let messages=[serde_json::json!({"role":"user","content":"simple"})];
    let sampling=fosnie_backend::ml::Sampling { max_tokens:Some(128),..Default::default() };
    let mut foreign=dispatch_context.clone(); foreign.actor.user_id=Some(Uuid::now_v7());
    assert!(dispatch::chat_step(&state,&foreign,&plan,&budget,&messages,None,&sampling,&notify).await.is_err());
    let tools=[serde_json::json!({"type":"function","function":{"name":"unverified_tool"}})];
    assert!(dispatch::chat_step(&state,&dispatch_context,&plan,&budget,&messages,Some(&tools),&sampling,&notify).await.is_err(),"tools compatibility is checked again at dispatch");
    assert_eq!(calls.load(Ordering::SeqCst),3,"denied actor and capabilities never reach ML");
    eprintln!("D6 direct dispatch contracts");
    let actual=dispatch::chat_step(&state,&dispatch_context,&plan,&budget,&messages,None,&sampling,&notify).await?;
    assert_eq!(actual.content,"{\"ready\":true}");
    assert_eq!(actual.routing_report.as_ref().unwrap().calls[0].provider_id,provider_id);
    assert_eq!(budget.snapshot().calls,1);
    assert_eq!(budget.snapshot().tokens,140);
    assert_eq!(budget.snapshot().unknown_cost_calls,1,"unknown pricing is visible in the ledger");
    assert!(dispatch::chat_step(&state,&dispatch_context,&plan,&budget,&messages,None,&sampling,&notify).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst),4,"second call denied by the shared run budget");
    policy=store::save_policy(&state,admin,policy.id,store::SavePolicy { label:"Edited policy".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone() }).await?;
    assert_ne!(policy.revision,plan.decision.policy_revision);
    let fresh_budget=budget::RunBudget::new(Default::default());
    assert!(dispatch::chat_step(&state,&dispatch_context,&plan,&fresh_budget,&messages,None,&sampling,&notify).await.is_err(),"edited policy requires replanning");
    assert_eq!(calls.load(Ordering::SeqCst),4);
    let (stream_plan,_)=service::auto_plan(&state,&context,Some(policy.id),RoutingObjective::Economy,WorkloadRequirements { input_tokens:100,output_tokens:128,..Default::default() },"stream",&notify).await?;
    let mut stream_context=context.clone(); stream_context.purpose=CallPurpose::Final;
    for interrupted in [false,true] {
        partial.store(interrupted,Ordering::SeqCst);
        let stream_budget=budget::RunBudget::new(stream_plan.constraints.clone());
        let request=fosnie_backend::ml::GenerateRequest { messages:messages.to_vec(),sampling:sampling.clone(),model:None,tools:None,overrides:Default::default() };
        let mut stream=dispatch::generate(&state,&stream_context,&stream_plan,&stream_budget,request,&notify).await?;
        assert!(matches!(stream.recv(&notify).await?,Some(fosnie_backend::ml::GenEvent::Token { .. })));
        if interrupted {
            assert!(stream.recv(&notify).await.is_err(),"EOF after a token is a failure, never a completed answer");
            assert!(stream_budget.snapshot().tokens>140,"lost usage keeps the full reservation");
        } else {
            assert!(matches!(stream.recv(&notify).await?,Some(fosnie_backend::ml::GenEvent::Done { routing_report:Some(_), .. })));
            assert_eq!(stream_budget.snapshot().tokens,140);
        }
        assert!(stream.recv(&notify).await?.is_none());
        assert_eq!(stream_budget.snapshot().calls,1);
    }
    assert_eq!(calls.load(Ordering::SeqCst),6,"partial streams never retry or splice a second answer");
    // The owned runtime keeps classifier/utility spend together across a real
    // spawned title task and summary, using the original authorized chat actor.
    policy.constraints.max_calls=Some(4); policy.strategy=RoutingStrategy::Hybrid;
    policy.rollout=RolloutMode::Active; policy.rules.clear();
    policy=store::save_policy(&state,admin,policy.id,store::SavePolicy { label:"Runtime".into(),scope:"deployment".into(),
        expected_revision:policy.revision,is_default:true,policy:policy.clone() }).await?;
    invalid.store(false,Ordering::SeqCst); delay.store(0,Ordering::SeqCst);
    let chat_id=Uuid::now_v7();
    sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'New chat')").bind(chat_id).bind(admin.user_id).execute(pool).await?;
    for seq in 1..=4i32 {
        sqlx::query("INSERT INTO messages(id,chat_id,role,sequence_number,content) VALUES($1,$2,'user',$3,$4)")
            .bind(Uuid::now_v7()).bind(chat_id).bind(seq).bind("A fact from the original authorized conversation. ".repeat(20)).execute(pool).await?;
    }
    let mut forbidden=admin.clone(); forbidden.user_id=Some(Uuid::now_v7()); forbidden.role=fosnie_backend::auth::PlatformRole::User;
    let requirements=||WorkloadRequirements { output_tokens:1024,..Default::default() };
    assert!(fosnie_backend::chat::routing::prepare(&state,&forbidden,chat_id,Uuid::now_v7(),"hello",Some(policy.id),RoutingObjective::Balanced,requirements(),&[],&notify).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst),6,"unauthorized history never reaches classifier or utility");
    eprintln!("D6 helpers and compaction");
    let run=fosnie_backend::chat::routing::prepare(&state,admin,chat_id,Uuid::now_v7(),"hello",Some(policy.id),RoutingObjective::Balanced,requirements(),&[],&notify).await?;
    assert_eq!(run.usage().calls,1,"classifier reservation survives runtime construction");
    assert_eq!(run.context(CallPurpose::UtilityTitle).actor.user_id,admin.user_id);
    assert_eq!(run.context(CallPurpose::UtilityTitle).chat_id,Some(chat_id));
    let title=fosnie_backend::chat::routing::spawn_title(state.clone(),&run,"hello".into()).await??;
    assert_eq!(title.as_deref(),Some("Planned Chat Title")); assert_eq!(run.usage().calls,2);
    let (tx,mut rx)=tokio::sync::mpsc::channel(8);
    let history=fosnie_backend::chat::routing::history(&state,&run,20,&tx).await?;
    assert!(history[0]["content"].as_str().unwrap().contains("Saved summary."));
    assert_eq!(run.usage().calls,3); assert_eq!(run.usage().tokens,420);
    assert!(rx.try_recv().is_ok());
    fosnie_backend::chat::routing::history(&state,&run,20,&tx).await?;
    assert_eq!(run.usage().calls,3,"persisted summary is not recomputed");
    sqlx::query("UPDATE chats SET title='User renamed' WHERE id=$1").bind(chat_id).execute(pool).await?;
    fosnie_backend::chat::routing::spawn_title(state.clone(),&run,"hello".into()).await??;
    let title:String=sqlx::query_scalar("SELECT title FROM chats WHERE id=$1").bind(chat_id).fetch_one(pool).await?;
    assert_eq!(title,"User renamed"); assert_eq!(run.usage().calls,4);
    for seq in 5..=6i32 {
        sqlx::query("INSERT INTO messages(id,chat_id,role,sequence_number,content) VALUES($1,$2,'user',$3,$4)")
            .bind(Uuid::now_v7()).bind(chat_id).bind(seq).bind("More conversation. ".repeat(20)).execute(pool).await?;
    }
    assert!(fosnie_backend::chat::routing::history(&state,&run,20,&tx).await.is_err(),"routed budget errors must not degrade to legacy compaction");
    assert_eq!(calls.load(Ordering::SeqCst),10);
    run.cancel();
    assert!(fosnie_backend::chat::routing::spawn_title(state.clone(),&run,"late".into()).await?.is_err());
    assert_eq!(calls.load(Ordering::SeqCst),10,"late spawned task sees sticky cancellation before HTTP");
    sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat_id).execute(pool).await?;
    // Exercise the actual chat entry point, including its durable assistant row
    // and protocol terminal frames. The mock has no legacy/model-info endpoints.
    use fosnie_backend::{chat::{run_turn,origin::TurnContext},ws::protocol::ServerFrame};
    eprintln!("D6 chat worker contracts");
    for (interrupted,unnamed) in [(false,false),(true,false),(false,true)] {
        partial.store(interrupted,Ordering::SeqCst);
        let chat=Uuid::now_v7(); let turn=Uuid::now_v7();
        sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,$3)")
            .bind(chat).bind(admin.user_id).bind(if unnamed { "New chat" } else { "Existing chat" }).execute(pool).await?;
        let cancel=Arc::new(tokio::sync::Notify::new());
        let runtime=fosnie_backend::chat::routing::prepare(&state,admin,chat,turn,"hello",Some(policy.id),RoutingObjective::Balanced,requirements(),&[],&cancel).await?;
        let baseline=calls.load(Ordering::SeqCst);
        let (tx,mut rx)=tokio::sync::mpsc::channel(128);
        // The production entry point runs in its own task. Do not poll a large
        // worker under this fixture's entire catalog/helper orchestration stack.
        tokio::spawn({let state=state.clone();let actor=admin.clone();let runtime=runtime.clone();async move {
            run_turn(&state,TurnContext::web(&actor).with_routing(&runtime),turn,Some(chat),None,None,"hello".into(),vec![],vec![],false,None,None,None,None,&tx,cancel).await;
        }}).await?;
        if unnamed {
            tokio::time::timeout(std::time::Duration::from_secs(2),async {
                loop {
                    let title:String=sqlx::query_scalar("SELECT title FROM chats WHERE id=$1").bind(chat).fetch_one(pool).await.unwrap();
                    if title=="Planned Chat Title" { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }).await.expect("run_turn must pass the same runtime to its title child");
        }
        let mut completed=0; let mut errors=Vec::new(); let mut started=0;
        while let Ok(frame)=rx.try_recv() { match frame {
            ServerFrame::ChatCompleted { .. }=>completed+=1,
            ServerFrame::ChatError { message,.. }=>errors.push(message),
            ServerFrame::ChatStarted { .. }=>started+=1,_=>{}
        } }
        assert_eq!(started,1,"chat failed before generation: {errors:?}");
        assert_eq!(completed,usize::from(!interrupted),"terminal frames: {errors:?}");
        assert_eq!(errors.len(),usize::from(interrupted));
        assert_eq!(calls.load(Ordering::SeqCst),baseline+1+usize::from(unnamed),"one final dispatch and only the planned title; no hidden retry");
        let row:(String,bool,bool,serde_json::Value)=sqlx::query_as("SELECT content,completed_at IS NOT NULL,interrupted_at IS NOT NULL,routing_decision FROM messages WHERE chat_id=$1 AND role='assistant'")
            .bind(chat).fetch_one(pool).await?;
        assert_eq!(row.0,"partial"); assert_eq!((row.1,row.2),(!interrupted,interrupted));
        assert_eq!(row.3["actual"]["provider_id"],serde_json::json!(provider_id));
        assert_eq!(runtime.usage().calls,2+u32::from(unnamed),"classifier, final and title share a ledger");
        if interrupted { assert!(runtime.usage().tokens>280,"unreported completion retains reservation"); }
        else { assert_eq!(runtime.usage().tokens,280+140*u64::from(unnamed)); }
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    // Cancel only after the final HTTP request reached the mock. Its reservation
    // must remain held, and cancellation must close the unfinished message.
    let chat=Uuid::now_v7(); let turn=Uuid::now_v7();
    sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Existing chat')").bind(chat).bind(admin.user_id).execute(pool).await?;
    let cancel=Arc::new(tokio::sync::Notify::new());
    let runtime=fosnie_backend::chat::routing::prepare(&state,admin,chat,turn,"hello",Some(policy.id),RoutingObjective::Balanced,requirements(),&[],&cancel).await?;
    let baseline=calls.load(Ordering::SeqCst); final_delay.store(30_000,Ordering::SeqCst);
    let (tx,mut rx)=tokio::sync::mpsc::channel(128);
    let worker=tokio::spawn({let state=state.clone();let actor=admin.clone();let runtime=runtime.clone();let cancel=cancel.clone();async move {
        run_turn(&state,TurnContext::web(&actor).with_routing(&runtime),turn,Some(chat),None,None,"hello".into(),vec![],vec![],false,None,None,None,None,&tx,cancel).await;
    }});
    tokio::time::timeout(std::time::Duration::from_secs(3),async {
        let (worker_result, _) = tokio::join!(
            worker,
            async {
                while calls.load(Ordering::SeqCst)==baseline { tokio::time::sleep(std::time::Duration::from_millis(5)).await; }
                cancel.notify_waiters();
            }
        );
        worker_result?;
        Ok::<_,tokio::task::JoinError>(())
    }).await??;
    final_delay.store(0,Ordering::SeqCst);
    assert!(runtime.is_cancelled()); assert_eq!(runtime.usage().calls,2); assert!(runtime.usage().tokens>280);
    let mut errors=0;
    while let Ok(frame)=rx.try_recv() { match frame { ServerFrame::ChatCompleted { .. }=>panic!("cancelled answer completed"),ServerFrame::ChatError { .. }=>errors+=1,_=>{} } }
    assert_eq!(errors,1);
    let terminal:(bool,bool)=sqlx::query_as("SELECT completed_at IS NOT NULL,interrupted_at IS NOT NULL FROM messages WHERE chat_id=$1 AND role='assistant'").bind(chat).fetch_one(pool).await?;
    assert_eq!(terminal,(false,true));
    sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    // A conflicting manual provider and a foreign caller must be rejected
    // without a generation or a new user-message write.
    let chat=Uuid::now_v7();
    sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Existing chat')").bind(chat).bind(admin.user_id).execute(pool).await?;
    struct LegacyModeration;
    #[async_trait::async_trait]
    impl fosnie_backend::ext::ModerationHook for LegacyModeration {
        async fn on_turn_completed(&self,_:&fosnie_backend::AppState,_:Uuid,_:Uuid,_:Uuid,_:Option<Uuid>,_:String) {
            panic!("a routed turn must never invoke a legacy inference hook");
        }
    }
    for case in 0..5 {
        let turn=Uuid::now_v7(); let cancel=Arc::new(tokio::sync::Notify::new());
        let runtime=fosnie_backend::chat::routing::prepare(&state,admin,chat,turn,"hello",Some(policy.id),RoutingObjective::Balanced,requirements(),&[],&cancel).await?;
        let before=calls.load(Ordering::SeqCst); let (tx,mut rx)=tokio::sync::mpsc::channel(8);
        let actor=if case==1 { &forbidden } else { admin };
        let mut guarded_state=state.clone();
        if case==3 { guarded_state.moderation=Arc::new(LegacyModeration); }
        let libraries=if case==4 { vec![Uuid::now_v7()] } else { vec![] };
        tokio::spawn({let actor=actor.clone();let runtime=runtime.clone();async move {
            run_turn(&guarded_state,TurnContext::web(&actor).with_routing(&runtime),turn,Some(chat),None,None,"hello".into(),vec![],libraries,case==2,None,None,(case==0).then_some(provider_id),None,&tx,cancel).await;
        }}).await?;
        let frames:Vec<_>=std::iter::from_fn(||rx.try_recv().ok()).collect();
        assert!(frames.iter().any(|frame|matches!(frame,ServerFrame::ChatError { .. })));
        assert!(!frames.iter().any(|frame|matches!(frame,ServerFrame::ChatCompleted { .. })));
        assert_eq!(calls.load(Ordering::SeqCst),before);
        let rows:i64=sqlx::query_scalar("SELECT count(*) FROM messages WHERE chat_id=$1").bind(chat).fetch_one(pool).await?;
        assert_eq!(rows,0);
    }
    sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    server.abort();
    // Keep the growing end-to-end chat suite out of the catalog test's stack.
    let policy_id=policy.id;
    eprintln!("D6 RAG/workflow/API contracts");
    // Isolated case tasks mirror production worker boundaries. They share the
    // same owned DB pool and actor, without retaining every earlier poll frame.
    macro_rules! own_case {
        ($pool:ident,$admin:ident,$call:expr)=>{{
            let $pool=(*$pool).clone();let $admin=(*$admin).clone();
            tokio::spawn(async move {let $pool=&$pool;let $admin=&$admin;Box::pin($call).await}).await??;
        }};
    }
    own_case!(pool,admin,chat_cases::check_rag(pool,admin,policy));
    own_case!(pool,admin,web_cases::check_web(pool,admin,policy_id));
    own_case!(pool,admin,document_cases::check_document(pool,admin,policy_id));
    own_case!(pool,admin,document_chat_cases::check_document_chat(pool,admin,policy_id));
    own_case!(pool,admin,verify_cases::check_verify(pool,admin,policy_id));
    own_case!(pool,admin,draft_cases::check_draft(pool,admin,policy_id));
    own_case!(pool,admin,research_cases::check_research(pool,admin,policy_id));
    own_case!(pool,admin,workflow_cases::check(pool,provider_id,admin));
    own_case!(pool,admin,api_cases::check(pool,provider_id,admin));
    own_case!(pool,admin,api_agent_cases::check(pool,provider_id,admin));
    own_case!(pool,admin,voice_cases::check(pool,admin,provider_id));
    own_case!(pool,admin,admission_cases::check(pool,admin,provider_id));
    Ok(())
    })
}

fn routing_mock_report(body: &serde_json::Value) -> serde_json::Value {
    let routing=&body["overrides"]["routing"];
    let purpose=routing["purpose"].as_str().unwrap();
    let target=&routing["assignments"][purpose];
    serde_json::json!({"lease_id":routing["budget"]["id"],"complete":true,"calls":[{
        "lease_id":routing["budget"]["id"],"call_id":Uuid::now_v7(),"decision_id":routing["decision_id"],
        "provider_id":target["provider_id"],"provider_revision":target["provider_revision"],"profile_revision":target["profile_revision"],
        "purpose":purpose,"status":"completed","latency_ms":1,
        "usage":{"input_tokens":100,"output_tokens":40,"total_tokens":140,"reasoning_tokens":null,"cached_input_tokens":null,"raw_usage":{}}
    }]})
}

async fn check_strict_targets(pool: sqlx::PgPool) -> anyhow::Result<()> {
    use fosnie_backend::{auth::{AuthContext, PlatformRole}, config::BootConfig,
        model_routing::*, state::AppStateBuilder, ext::{ProviderRegistry, ResolvedProvider}};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    struct Registry(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl ProviderRegistry for Registry {
        async fn resolve(&self, _: &sqlx::PgPool, _: &str, _: Option<Uuid>) -> fosnie_backend::Result<Option<ResolvedProvider>> { panic!("strict selection must use the named seam") }
        async fn resolve_llm_id(&self, _: &sqlx::PgPool, _: Option<Uuid>, _: Uuid) -> fosnie_backend::Result<Option<ResolvedProvider>> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Some(ResolvedProvider { base_url: Some("http://model.test/v1".into()), model: Some("test".into()), api_key: None, enabled: true, reasoning_mode: None }))
        }
    }
    struct LocalOnly;
    #[async_trait::async_trait]
    impl InferencePolicy for LocalOnly {
        async fn constraints(&self, _: &sqlx::PgPool, _: &RoutingContext) -> fosnie_backend::Result<RoutingConstraints> {
            Ok(RoutingConstraints { local_only: true, ..Default::default() })
        }
    }
    let user = Uuid::now_v7();
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'Targets', $2, 'user')")
        .bind(user).bind(format!("{user}@routing.test")).execute(&pool).await?;
    sqlx::query("INSERT INTO provider_configs (id, role, scope, model, label, base_url) VALUES ($1, 'llm', 'deployment', 'test', $2, 'http://model.test/v1')")
        .bind(id).bind(id.to_string()).execute(&pool).await?;
    let mut profile: ModelProfile = serde_json::from_value(serde_json::json!({
        "provider_id": id, "provider_revision": 1, "revision": 1, "adapter": "openai_chat", "model": "test", "locality": "cloud"
    }))?;
    sqlx::query("INSERT INTO model_profiles (provider_id, provider_revision, profile) VALUES ($1, 1, $2)")
        .bind(id).bind(serde_json::to_value(&profile)?).execute(&pool).await?;
    let count = Arc::new(AtomicUsize::new(0));
    let mut boot = BootConfig::default();
    boot.providers.user_byok_enabled = false;
    let state = AppStateBuilder::new(pool.clone(), fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?, Arc::new(boot))
        .with_providers(Arc::new(Registry(count.clone()))).with_inference_policy(Arc::new(LocalOnly)).build();
    let mut context = RoutingContext { actor: AuthContext { user_id: Some(user), email: None, display_name: None,
        role: PlatformRole::User, break_glass: false, mfa_enroll_only: false }, chat_id: None, project_id: None, run_id: Uuid::now_v7(), purpose: CallPurpose::Primary };
    let reference = TargetReference { provider_id: id, provider_revision: 1, profile_revision: 1, model: "test".into(), effort: None , variant:None };
    // Every inference purpose must respect the extension BEFORE secret access.
    for purpose in [CallPurpose::Primary, CallPurpose::RouterClassify, CallPurpose::UtilityTitle, CallPurpose::ResearchWrite, CallPurpose::Voice] {
        context.purpose = purpose;
        assert!(resolve_target(&state, &context, &reference, &RoutingConstraints::default()).await.is_err());
    }
    assert_eq!(count.load(Ordering::SeqCst), 0);
    profile.locality = Locality::Local;
    profile.locality_trusted = true;
    sqlx::query("UPDATE model_profiles SET profile=$2 WHERE provider_id=$1").bind(id).bind(serde_json::to_value(&profile)?).execute(&pool).await?;
    let target = resolve_target(&state, &context, &reference, &RoutingConstraints::default()).await?;
    assert_eq!(target.api_key, "", "explicit unauthenticated target cannot inherit an environment key");
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let denied = RoutingConstraints { allowed_providers: Some(Default::default()), ..Default::default() };
    assert!(resolve_target(&state, &context, &reference, &denied).await.is_err());
    assert!(sqlx::query("UPDATE provider_configs SET scope='user', scope_id=$2 WHERE id=$1").bind(id).bind(user).execute(&pool).await.is_err(),"scope cannot be changed on an existing provider");
    let own=Uuid::now_v7();
    sqlx::query("INSERT INTO provider_configs(id,role,scope,scope_id,model,label,base_url) VALUES($1,'llm','user',$2,'test','Private fixture','http://model.test/v1')").bind(own).bind(user).execute(&pool).await?;
    profile.provider_id=own;
    sqlx::query("INSERT INTO model_profiles(provider_id,provider_revision,profile) VALUES($1,1,$2)").bind(own).bind(serde_json::to_value(&profile)?).execute(&pool).await?;
    let private=TargetReference{provider_id:own,..reference.clone()};
    assert!(resolve_target(&state, &context, &private, &RoutingConstraints::default()).await.is_err(), "BYOK disabled denies even the owner");
    context.actor.user_id = Some(Uuid::now_v7());
    assert!(resolve_target(&state, &context, &private, &RoutingConstraints::default()).await.is_err(), "foreign target");
    sqlx::query("UPDATE provider_configs SET model='changed' WHERE id=$1").bind(id).execute(&pool).await?;
    assert!(resolve_target(&state, &context, &reference, &RoutingConstraints::default()).await.is_err(), "stale evaluation");
    assert_eq!(count.load(Ordering::SeqCst), 1, "forbidden and stale targets never reach credential resolution");
    Ok(())
}

async fn check_upgrade(conn: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    let migrations = sqlx::migrate!("./migrations");
    for migration in migrations.iter().filter(|m| m.version < 114) {
        sqlx::raw_sql(migration.sql.clone()).execute(&mut *conn).await?;
    }
    let user = Uuid::now_v7();
    let provider = Uuid::now_v7();
    let chat = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'Routing', $2, 'user')")
        .bind(user).bind(format!("{user}@routing.test")).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO provider_configs (id, role, scope, model, label, is_default) VALUES ($1, 'llm', 'deployment', 'test-model', 'Routing test', true)")
        .bind(provider).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO chats (id, owner_user_id, llm_provider_id) VALUES ($1, $2, $3)")
        .bind(chat).bind(user).bind(provider).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO embedding_index(id,embed_model,embed_base_url,embed_api_key_encrypted,dim,collection_name) VALUES(1,'legacy-embed','http://legacy.test/v1','unchanged-ciphertext',768,'legacy_collection')")
        .execute(&mut *conn).await?;
    for migration in migrations.iter().filter(|m| m.version >= 114) {
        sqlx::raw_sql(migration.sql.clone()).execute(&mut *conn).await?;
        if migration.version==117 {
            sqlx::query("INSERT INTO provider_configs(id,role,scope,model) VALUES($1,'ocr','deployment','legacy-fixed')").bind(chat).execute(&mut *conn).await?;
            sqlx::query("INSERT INTO fixed_inference_profiles(provider_id,revision,profile) VALUES($1,42,'{\"legacy\":true}')").bind(chat).execute(&mut *conn).await?;
        }
    }
    let stage:(String,i64,serde_json::Value)=sqlx::query_as("SELECT slot,revision,profile FROM fixed_inference_profiles WHERE provider_id=$1").bind(chat).fetch_one(&mut *conn).await?;
    assert_eq!(stage,("ocr".into(),42,serde_json::json!({"legacy":true})),"118 preserves existing fixed profiles and revisions");
    sqlx::query("DELETE FROM provider_configs WHERE id=$1").bind(chat).execute(&mut *conn).await?;
    let provenance:(String,Option<String>,Option<String>,i32,String,i64)=sqlx::query_as("SELECT embed_model,embed_base_url,embed_api_key_encrypted,dim,collection_name,routing_revision FROM embedding_index WHERE id=1")
        .fetch_one(&mut *conn).await?;
    assert_eq!(provenance.0,"legacy-embed");
    assert_eq!(provenance.1.as_deref(),Some("http://legacy.test/v1"));
    assert_eq!(provenance.2.as_deref(),Some("unchanged-ciphertext"));
    assert_eq!((provenance.3,provenance.4.as_str()),(768,"legacy_collection"));
    assert!(provenance.5>0);
    sqlx::query("DELETE FROM embedding_index WHERE id=1").execute(&mut *conn).await?;
    let selected: serde_json::Value = sqlx::query_scalar("SELECT model_selection FROM chats WHERE id=$1").bind(chat).fetch_one(&mut *conn).await?;
    assert_eq!(selected, serde_json::json!({"mode":"manual", "provider_id":provider}));
    sqlx::query("UPDATE chats SET model_selection = '{\"mode\":\"auto\"}' WHERE id=$1").bind(chat).execute(&mut *conn).await?;
    let pointer: Option<Uuid> = sqlx::query_scalar("SELECT llm_provider_id FROM chats WHERE id=$1").bind(chat).fetch_one(&mut *conn).await?;
    assert_eq!(pointer, None);
    sqlx::query("UPDATE chats SET llm_provider_id = $2 WHERE id=$1").bind(chat).bind(provider).execute(&mut *conn).await?;
    let mode: String = sqlx::query_scalar("SELECT model_selection->>'mode' FROM chats WHERE id=$1").bind(chat).fetch_one(&mut *conn).await?;
    assert_eq!(mode, "manual");
    let inserted: serde_json::Value = sqlx::query_scalar("INSERT INTO chats (id, owner_user_id, llm_provider_id) VALUES ($1, $2, $3) RETURNING model_selection")
        .bind(Uuid::now_v7()).bind(user).bind(provider).fetch_one(&mut *conn).await?;
    assert_eq!(inserted, serde_json::json!({"mode":"manual", "provider_id":provider}));
    sqlx::query("UPDATE provider_configs SET model='new-model' WHERE id=$1").bind(provider).execute(&mut *conn).await?;
    let revision: i64 = sqlx::query_scalar("SELECT routing_revision FROM provider_configs WHERE id=$1").bind(provider).fetch_one(&mut *conn).await?;
    assert_eq!(revision, 2);
    sqlx::query("UPDATE provider_configs SET label='New label' WHERE id=$1").bind(provider).execute(&mut *conn).await?;
    let revision: i64 = sqlx::query_scalar("SELECT routing_revision FROM provider_configs WHERE id=$1").bind(provider).fetch_one(&mut *conn).await?;
    assert_eq!(revision, 2, "display labels do not invalidate model evaluations");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM routing_policies").fetch_one(&mut *conn).await?;
    assert_eq!(count, 0, "upgrade must not turn on Auto");
    let enabled:bool=sqlx::query_scalar("SELECT enabled FROM routing_api_alias WHERE id=1").fetch_one(&mut *conn).await?;
    assert!(!enabled,"upgrade must not reserve an API alias");
    Ok(())
}
