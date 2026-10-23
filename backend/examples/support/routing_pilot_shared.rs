// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Operator-authorised loopback pilot. Isolated schema; in-memory credentials.
use axum::{
    routing::{get, post, put},
    Router,
};
use fosnie_backend::{
    auth::{AuthContext, PlatformRole},
    config::BootConfig,
    ext::{AuthProvider, ProviderRegistry, ResolvedProvider},
    model_routing::{store, *},
    state::AppStateBuilder,
    AppState,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::{Connection, Executor};
use std::{
    collections::HashMap,
    io::{BufRead, Write},
    sync::Arc,
};
use uuid::Uuid;
#[path = "routing_tool_pilot.rs"]
mod tool_pilot;
#[path = "routing_web_pilot.rs"]
mod web_pilot;

#[path="routing_ingestion_pilot.rs"]
mod ingestion_pilot;
#[path = "routing_flow_pilot.rs"]
mod flow_pilot;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Model {
    name: String,
    model: String,
    adapter: String,
    locality: String,
    tier: String,
    base_url: String,
    key: String,
    #[serde(default = "default_context_tokens")]
    context_tokens: u32,
    input_price: u64,
    output_price: u64,
    auto: bool,
    #[serde(default)]
    efforts: Vec<String>,
    #[serde(default)]
    auto_effort: Option<String>,
    #[serde(default)]
    task_exclusions: Vec<String>,
    #[serde(default)]
    structured_output_efforts: Option<std::collections::BTreeSet<String>>,
    #[serde(default)]
    tools: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    edition_fixture: Option<serde_json::Value>,
    #[serde(default)]
    documents_dir: Option<String>,
    ml_url: String,
    ml_secret: String,
    auth_token: String,
    models: Vec<Model>,
    #[serde(default)]
    web_pilot: Option<web_pilot::Config>,
    #[serde(default)]
    flow_pilot: Option<flow_pilot::Config>,
    #[serde(default = "default_cloud_judge_timeout")]
    cloud_judge_timeout_ms: u64,
    #[serde(default)]
    judge_efforts: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_local_judge")]
    local_judge_name: String,
    #[serde(default = "default_evaluation_output")]
    evaluation_max_output_tokens: u32,
}
fn default_context_tokens() -> u32 {8192}
fn default_evaluation_output() -> u32 {1024}
fn default_cloud_judge_timeout() -> u64 {
    15_000
}
fn default_local_judge() -> String {
    "llama3b".into()
}
struct Auth {
    actor: AuthContext,
    token: String,
}
#[async_trait::async_trait]
impl AuthProvider for Auth {
    async fn authenticate(
        &self,
        p: &mut axum::http::request::Parts,
        _: &AppState,
    ) -> fosnie_backend::Result<AuthContext> {
        if p.headers.get("authorization").and_then(|v| v.to_str().ok())
            != Some(format!("Bearer {}", self.token).as_str())
        {
            return Err(RoutingError::Forbidden.into());
        }
        Ok(self.actor.clone())
    }
}
struct Registry(HashMap<Uuid, String>);
#[async_trait::async_trait]
impl ProviderRegistry for Registry {
    async fn resolve_fixed_id(&self, p: &sqlx::PgPool, role: &str, user: Option<Uuid>, id: Uuid)
        -> fosnie_backend::Result<Option<ResolvedProvider>> {
        fosnie_backend::providers::DbProviderRegistry::new(None).resolve_fixed_id(p, role, user, id).await
    }
    async fn resolve_active_embed(&self,p:&sqlx::PgPool,id:Uuid,revision:i64) -> fosnie_backend::Result<Option<ResolvedProvider>> {
        fosnie_backend::providers::DbProviderRegistry::new(None).resolve_active_embed(p,id,revision).await
    }
    async fn resolve_desired_embed(&self,p:&sqlx::PgPool,id:Uuid,revision:i64) -> fosnie_backend::Result<Option<ResolvedProvider>> {
        fosnie_backend::providers::DbProviderRegistry::new(None).resolve_desired_embed(p,id,revision).await
    }
    async fn resolve(
        &self,
        _: &sqlx::PgPool,
        _: &str,
        _: Option<Uuid>,
    ) -> fosnie_backend::Result<Option<ResolvedProvider>> {
        Err(RoutingError::Forbidden.into())
    }
    async fn resolve_llm_id(
        &self,
        p: &sqlx::PgPool,
        u: Option<Uuid>,
        id: Uuid,
    ) -> fosnie_backend::Result<Option<ResolvedProvider>> {
        let mut result = fosnie_backend::providers::DbProviderRegistry::new(None)
            .resolve_llm_id(p, u, id)
            .await?;
        if let Some(provider) = &mut result {
            provider.api_key = self.0.get(&id).cloned();
        }
        Ok(result)
    }
}
fn loopback(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        u.scheme() == "http"
            && u.host_str() == Some("127.0.0.1")
            && u.port().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
    })
}
pub struct EditionFixture {pub info:serde_json::Value,pub routes:Router<AppState>}
#[async_trait::async_trait]
pub trait PilotEdition:Send+Sync {
    fn initialise(&self)->anyhow::Result<()> {Ok(())}
    fn actor_role(&self)->PlatformRole {PlatformRole::ClientAdmin}
    fn configure(&self,builder:AppStateBuilder)->AppStateBuilder {builder}
    async fn bootstrap(&self,_:&AppState,_:&AuthContext)->anyhow::Result<()> {Ok(())}
    async fn prepare(&self,_:&AppState,_:&AuthContext,_:Option<&serde_json::Value>,_:Option<&serde_json::Value>)->anyhow::Result<EditionFixture> {
        Ok(EditionFixture {info:serde_json::Value::Null,routes:Router::new()})
    }
}
pub struct CoreEdition;
#[async_trait::async_trait]
impl PilotEdition for CoreEdition {}

async fn serve(pool: sqlx::PgPool, c: Config, edition:Arc<dyn PilotEdition>) -> anyhow::Result<()> {
    anyhow::ensure!(
        loopback(&c.ml_url) && c.models.len() == 4 && c.auth_token.len() >= 32,
        "pilot configuration"
    );
    let uid = Uuid::now_v7();
    let role=edition.actor_role();
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Authorised model pilot',$3)")
        .bind(uid).bind(format!("{uid}@routing.test")).bind(role).execute(&pool).await?;
    let actor = AuthContext {
        user_id: Some(uid),
        role,
        email: None,
        display_name: None,
        break_glass: false,
        mfa_enroll_only: false,
    };
    let mut boot = BootConfig::default();
    boot.ml.base_url = c.ml_url;
    boot.ml.shared_secret = c.ml_secret;
    if let Some(path)=&c.documents_dir {
        let root=std::path::Path::new(path);
        anyhow::ensure!(root.is_absolute() && root.is_dir(),"existing private document root required");
        boot.storage.documents_dir=path.clone();
        let workspace=root.join("workspace");tokio::fs::create_dir_all(&workspace).await?;
        boot.storage.workspace_dir=workspace.to_string_lossy().into();
        boot.scheduler.worker_threads=1;boot.scheduler.poll_interval_secs=1;boot.scheduler.batch_size=1;
    }
    boot.server.static_dir = "".into();
    boot.server.public_url = "http://127.0.0.1:5189".into();
    let mut keys = HashMap::new();
    let mut ids = HashMap::new();
    for m in &c.models {
        let id = Uuid::now_v7();
        ids.insert(m.name.clone(), id);
        keys.insert(id, m.key.clone());
    }
    let builder = AppStateBuilder::new(
        pool.clone(),
        fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,
        Arc::new(boot),
    )
    .with_auth(Arc::new(Auth {
        actor: actor.clone(),
        token: c.auth_token,
    }))
    .with_jobs(Arc::new(ingestion_pilot::EmptyJobs))
    .with_providers(Arc::new(Registry(keys)));
    let state=edition.configure(builder).build();
    edition.bootstrap(&state,&actor).await?;
    for m in c.models {
        anyhow::ensure!(
            loopback(&m.base_url) && m.model.len() <= 256 && (2048..=32768).contains(&m.context_tokens),
            "loopback-only controlled proxy required"
        );
        let id = ids[&m.name];
        sqlx::query("INSERT INTO provider_configs(id,role,scope,model,label,base_url) VALUES($1,'llm','deployment',$2,$3,$4)")
            .bind(id).bind(&m.model).bind(&m.name).bind(&m.base_url).execute(&pool).await?;
        let profile: ModelProfile = serde_json::from_value(
            json!({"provider_id":id,"provider_revision":1,"revision":1,"model":m.model,
            "adapter":m.adapter,"locality":m.locality,"locality_trusted":true,"tier":m.tier,"auto_enabled":m.auto,"auto_effort":m.auto_effort,"task_exclusions":m.task_exclusions,"quality_status":"declared",
            "capabilities":{"context_tokens":m.context_tokens,"max_output_tokens":2048,"structured_output":"declared","structured_output_efforts":m.structured_output_efforts,"tools":if m.tools {"declared"}else{"unknown"},"efforts":m.efforts},
            "pricing":{"input_microusd_per_million":m.input_price,"output_microusd_per_million":m.output_price,
                "source":"nominal list tariff or zero marginal local fee; actual credits/invoice unverified","checked_at":"2026-10-05"}}),
        )?;
        store::save_profile(
            &state,
            &actor,
            id,
            store::SaveProfile {
                expected_revision: 0,
                profile,
            },
        )
        .await?;
    }
    let mut policies = HashMap::new();
    for (name, strategy, judge) in [
        ("rules", "rules", "gemini-lite"),
        ("auto-cloud-judge", "classifier", "gemini-lite"),
        (
            "auto-local-judge",
            "classifier",
            c.local_judge_name.as_str(),
        ),
    ] {
        let policy: RoutingPolicy = serde_json::from_value(
            json!({"id":Uuid::now_v7(),"revision":1,"rollout":"active","strategy":strategy,
            "default_provider_id":ids["openai"],"classifier_provider_id":if strategy=="rules" {None}else{Some(ids[judge])},
            "objective":"economy","classifier":{"native_schema":true,"effort":c.judge_efforts.get(judge),"max_input_tokens":6144,"max_output_tokens":512,"local_timeout_ms":20000,"cloud_timeout_ms":c.cloud_judge_timeout_ms},"constraints":{"max_calls":2,"max_output_tokens":c.evaluation_max_output_tokens,"max_total_tokens":30000,"max_duration_ms":30000,
                "allowed_providers":[ids["qwen4b"],ids["openai"],ids["gemini-lite"],ids["llama3b"]]},
            "rules":if strategy=="rules" {vec![json!({"id":Uuid::now_v7(),"priority":10,"conditions":{"tasks":["general","editing","extraction","current_fact"]},
                "action":{"kind":"select","provider_id":ids["qwen4b"]}}),json!({"id":Uuid::now_v7(),"priority":0,"conditions":{"tasks":["coding","reasoning"]},
                "action":{"kind":"require","minimum_tier":"advanced"}})]}else{vec![]}}),
        )?;
        let saved = store::save_policy(
            &state,
            &actor,
            policy.id,
            store::SavePolicy {
                label: name.into(),
                scope: "deployment".into(),
                expected_revision: 0,
                is_default: name == "rules",
                policy,
            },
        )
        .await?;
        policies.insert(name, json!({"id":saved.id,"revision":saved.revision}));
    }
    // Private native tool probe uses no classifier/cloud executor or retries.
    let tool_policy: RoutingPolicy = serde_json::from_value(
        json!({"id":Uuid::now_v7(),"revision":1,"rollout":"active","strategy":"rules",
        "default_provider_id":ids["qwen4b"],"classifier_provider_id":null,"constraints":{"local_only":true,"allowed_providers":[ids["qwen4b"]],
            "max_calls":2,"max_output_tokens":256,"max_total_tokens":30_000,"max_duration_ms":30_000},
        "rules":[{"id":Uuid::now_v7(),"priority":1,"conditions":{},"action":{"kind":"select","provider_id":ids["qwen4b"]}}]}),
    )?;
    let saved = store::save_policy(
        &state,
        &actor,
        tool_policy.id,
        store::SavePolicy {
            label: "tool-local".into(),
            scope: "deployment".into(),
            expected_revision: 0,
            is_default: false,
            policy: tool_policy,
        },
    )
    .await?;
    policies.insert(
        "tool-local",
        json!({"id":saved.id,"revision":saved.revision}),
    );
    anyhow::ensure!(c.web_pilot.is_none() || c.flow_pilot.is_none(),"one private browser fixture");
    let flow = match &c.flow_pilot {Some(config)=>Some(flow_pilot::prepare(&state,&actor,&ids,config).await?),None=>None};
    let web = match &c.web_pilot {
        Some(config) => Some(web_pilot::prepare(&state, &actor, ids["qwen4b"], config).await?),
        None => None,
    };
    let extension=edition.prepare(&state,&actor,flow.as_ref(),c.edition_fixture.as_ref()).await?;
    let profiles = store::catalog(&state, &actor).await?;
    let private = Router::new()
        .route(
            "/api/routing/tool-weather-probe",
            post(tool_pilot::evaluate),
        )
        .route(
            "/api/routing/test-judge",
            post(fosnie_backend::http::model_routing::classifier_probe),
        )
        .route(
            "/api/routing/evaluate",
            post(fosnie_backend::http::routing_evaluation::evaluate),
        )
        .route(
            "/api/routing/catalog",
            get(fosnie_backend::http::model_routing::catalog),
        )
        .route("/api/routing/management", get(fosnie_backend::http::model_routing::management))
        .route("/api/routing/policies", get(fosnie_backend::http::model_routing::policies))
        .route("/api/routing/policies/{id}", put(fosnie_backend::http::model_routing::save_policy))
        .route("/api/routing/profiles/{id}", put(fosnie_backend::http::model_routing::save_profile))
        .route("/api/routing/pilot-web/{chat}/receipt", get(web_pilot::receipt));
    let worker=c.documents_dir.as_ref().map(|_|ingestion_pilot::Worker::new());
    let mut extra=Router::new().route("/api/routing/pilot-web/{chat}/receipt", get(web_pilot::receipt)).route("/api/routing/tool-weather-probe",post(tool_pilot::evaluate));
    if let Some(worker)=&worker {extra=extra.merge(ingestion_pilot::routes(worker.clone()));}
    extra=extra.merge(extension.routes);
    let router = if web.is_some() || flow.is_some() {
        fosnie_backend::http::router(state, None, None, Some(extra), None)
    } else { private.with_state(state) };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!(
        "{}",
        json!({"base_url":format!("http://{}",listener.local_addr()?),"providers":ids,"profiles":profiles,"policies":policies,"web_pilot":web,"flow_pilot":flow,"edition_fixture":extension.info})
    );
    std::io::stdout().flush()?;
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    tokio::task::spawn_blocking(|| {
        let mut command = String::new();
        let _ = std::io::stdin().lock().read_line(&mut command);
    })
    .await?;
    server.abort();
    let _ = server.await;
    if let Some(worker)=worker {worker.close().await?;}
    pool.close().await;
    Ok(())
}
pub async fn run_with_edition(edition:Arc<dyn PilotEdition>,migrations:&sqlx::migrate::Migrator) -> anyhow::Result<()> {
    edition.initialise()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    anyhow::ensure!(line.len() <= 64000, "pilot input bound");
    let c: Config = serde_json::from_str(&line)?;
    anyhow::ensure!((1..=2048).contains(&c.evaluation_max_output_tokens),"bounded private evaluation output required");
    let names: std::collections::BTreeSet<_> = c.models.iter().map(|m| m.name.as_str()).collect();
    anyhow::ensure!(
        names
            == ["qwen4b", "llama3b", "openai", "gemini-lite"]
                .into_iter()
                .collect()
            && c.models.len() == 4
            && loopback(&c.ml_url)
            && c.models.iter().all(|m| loopback(&m.base_url)),
        "complete controlled pilot configuration required"
    );
    anyhow::ensure!(
        matches!(c.local_judge_name.as_str(), "qwen4b" | "llama3b"),
        "local judge must be an installed pilot model"
    );
    let options: sqlx::postgres::PgConnectOptions =
        std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL")?.parse()?;
    anyhow::ensure!(
        options.get_host() == "127.0.0.1"
            && options.get_port() == 15439
            && options.get_database() == Some("routing_test"),
        "isolated pilot database only"
    );
    let mut conn = sqlx::PgConnection::connect_with(&options).await?;
    let schema = format!("routing_pilot_{}", Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut conn)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}")))
        .execute(&mut conn)
        .await?;
    use futures_util::FutureExt;
    let result = std::panic::AssertUnwindSafe(async {
        for migration in migrations.iter() {
            sqlx::raw_sql(migration.sql.clone())
                .execute(&mut conn)
                .await?;
        }
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(6)
            .after_connect({
                let schema = schema.clone();
                move |conn, _| {
                    let sql = sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));
                    Box::pin(async move {
                        sqlx::raw_sql(sql).execute(conn).await?;
                        Ok(())
                    })
                }
            })
            .connect_with(options)
            .await?;
        serve(pool, c, edition).await
    }).catch_unwind().await;
    conn.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&mut conn)
        .await?;
    result.map_err(|_| anyhow::anyhow!("isolated pilot panicked; schema removed"))?
}
