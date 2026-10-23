// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! One owned-database D1 lane. Synthetic credentials, no inference or public API.
use base64::Engine;
use fosnie_backend::{
    auth::{AuthContext, PlatformRole},
    config::BootConfig,
    model_routing::{
        vnext::{catalogue::*, connections, deployments, discovery, variants},
        *,
    },
    state::AppStateBuilder,
    AppState,
};
use serde_json::json;
use sqlx::{Connection, Executor};
use std::{collections::BTreeSet, sync::Arc};
use uuid::Uuid;

#[path="common/model_routing_vnext_profiles_cases.rs"]
mod profile_cases;
#[path="common/model_routing_vnext_chat_binding_cases.rs"]
mod chat_binding_cases;

fn actor(id: Uuid, role: PlatformRole) -> AuthContext {
    AuthContext {
        user_id: Some(id),
        role,
        email: None,
        display_name: None,
        break_glass: false,
        mfa_enroll_only: false,
    }
}
fn make_state(pool: sqlx::PgPool, byok: bool) -> AppState {
    let mut boot = BootConfig::default();
    boot.providers.user_byok_enabled = byok;
    boot.message_encryption_key = base64::engine::general_purpose::STANDARD.encode([47u8; 32]);
    AppStateBuilder::new(
        pool,
        fosnie_backend::cache::create_pool(&std::env::var("FOSNIE_ROUTING_TEST_REDIS_URL").unwrap_or_else(|_|"redis://127.0.0.1:16389".into())).unwrap(),
        Arc::new(boot),
    )
    .build()
}
fn profile(id: Uuid, revision: i64, model: &str, local: bool) -> ModelProfile {
    serde_json::from_value(json!({"provider_id":id,"provider_revision":revision,"revision":1,"adapter":"openai_chat","model":model,
        "locality":if local {"local"}else{"cloud"},"locality_trusted":true,"quality_status":"declared","auto_enabled":true,
        "pricing":{"input_microusd_per_million":100000,"output_microusd_per_million":700000},
        "capabilities":{"context_tokens":32768,"max_output_tokens":8192,"efforts":["none","low","high"]}})).unwrap()
}
fn connection_body(expected: i64, url: &str, key: Option<&str>) -> connections::SaveConnection {
    connections::SaveConnection {
        schema_version: 2,
        expected_revision: expected,
        scope: "deployment".into(),
        label: "Fixture connection".into(),
        adapter: AdapterKind::OpenaiChat,
        base_url: url.into(),
        enabled: true,
        quota_group_id: None,
        metadata: connections::ConnectionMetadata {
            locality: Locality::Local,
            locality_trusted: true,
        },
        api_key: key.map(str::to_owned),
        clear_api_key: false,
    }
}
fn variant_body(provider: Uuid, config: i64, scope: &str, effort: &str) -> variants::SaveVariant {
    variants::SaveVariant {
        schema_version: 2,
        expected_revision: 0,
        expected_model_config_revision: config,
        scope: scope.into(),
        provider_id: provider,
        label: format!("{effort} variant"),
        effort: EffortSetting::Explicit {
            value: effort.into(),
        },
        generation: GenerationConfig {
            max_output_tokens: Some(256),
            ..Default::default()
        },
    }
}
async fn save_profile(
    state: &AppState,
    admin: &AuthContext,
    id: Uuid,
    model: &str,
    local: bool,
) -> anyhow::Result<ModelProfile> {
    let revision: i64 =
        sqlx::query_scalar("SELECT routing_revision FROM provider_configs WHERE id=$1")
            .bind(id)
            .fetch_one(&state.pg)
            .await?;
    let expected: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM model_profiles WHERE provider_id=$1")
            .bind(id)
            .fetch_optional(&state.pg)
            .await?;
    Ok(store::save_profile(
        state,
        admin,
        id,
        store::SaveProfile {
            expected_revision: expected.unwrap_or(0),
            profile: profile(id, revision, model, local),
        },
    )
    .await?)
}

#[tokio::test]
async fn d1_upgrade_shared_credentials_scoped_variants_cas_discovery_and_rollback(
) -> anyhow::Result<()> {
    eprintln!("D5 owned DB lane");
    let url = std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL")
        .expect("D1 requires an owned routing_ test database");
    let options: sqlx::postgres::PgConnectOptions = url.parse()?;
    anyhow::ensure!(
        options.get_host() == "127.0.0.1"
            && options
                .get_database()
                .is_some_and(|d| d.starts_with("routing_")),
        "loopback owned routing_ database only"
    );
    let mut root = sqlx::PgConnection::connect_with(&options).await?;
    let schema = format!("routing_vnext_{}", Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut root)
        .await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}")))
        .execute(&mut root)
        .await?;
    let result=async {
        let migrations=sqlx::migrate!("./migrations");
        for m in migrations.iter().filter(|m|m.version<139) {sqlx::raw_sql(m.sql.clone()).execute(&mut root).await?;}
        // The actual old SQLx receipt table is present during rollback.
        root.execute("CREATE TABLE _sqlx_migrations(version BIGINT PRIMARY KEY)").await?;
        root.execute("INSERT INTO _sqlx_migrations VALUES(139),(140),(141),(142),(143),(144),(145),(146),(147)").await?;
        let admin_id=Uuid::now_v7();let user_id=Uuid::now_v7();let other_id=Uuid::now_v7();
        for (id,role) in [(admin_id,"client_admin"),(user_id,"user"),(other_id,"user")] {
            sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'D1 fixture',$3::platform_role)")
                .bind(id).bind(format!("{id}@routing.test")).bind(role).execute(&mut root).await?;
        }
        let legacy=Uuid::now_v7();let ciphertext=fosnie_backend::crypto::encrypt(&[47u8;32],"synthetic-original-key")?;
        sqlx::query("INSERT INTO provider_configs(id,role,scope,label,model,base_url,api_key_encrypted,is_default) VALUES($1,'llm','deployment','Legacy fixture','fixture-legacy','http://127.0.0.1:1/v1',$2,true)")
            .bind(legacy).bind(&ciphertext).execute(&mut root).await?;
        let legacy_profile=profile(legacy,1,"fixture-legacy",true);
        sqlx::query("INSERT INTO model_profiles(provider_id,provider_revision,revision,profile) VALUES($1,1,1,$2)")
            .bind(legacy).bind(serde_json::to_value(&legacy_profile)?).execute(&mut root).await?;
        sqlx::raw_sql(include_str!("../migrations/0139_routing_connections_variants.sql")).execute(&mut root).await?;
        sqlx::raw_sql(include_str!("../migrations/0140_routing_profile_extensions.sql")).execute(&mut root).await?;
        sqlx::raw_sql(include_str!("../migrations/0141_routing_behaviour_profiling.sql")).execute(&mut root).await?;
        sqlx::raw_sql(include_str!("../migrations/0142_routing_quality_calibration.sql")).execute(&mut root).await?;
        sqlx::raw_sql(include_str!("../migrations/0143_routing_query_calibration.sql")).execute(&mut root).await?;
        sqlx::raw_sql(include_str!("../migrations/0144_routing_context_projections.sql")).execute(&mut root).await?;
        sqlx::raw_sql(include_str!("../migrations/0145_routing_health_scopes.sql")).execute(&mut root).await?;
        sqlx::raw_sql(include_str!("../migrations/0146_routing_health_bindings.sql")).execute(&mut root).await?;
        sqlx::raw_sql(include_str!("../migrations/0147_chat_routing_intent.sql")).execute(&mut root).await?;
        let (binding_chat,binding_intent)=Box::pin(chat_binding_cases::exercise(&mut root,user_id,legacy)).await?;
        let after:(i64,bool,Option<String>,Uuid)=sqlx::query_as("SELECT routing_revision,is_default,api_key_encrypted,connection_id FROM provider_configs WHERE id=$1")
            .bind(legacy).fetch_one(&mut root).await?;
        assert_eq!((after.0,after.1,after.2),(1,true,None));
        let copied:String=sqlx::query_scalar("SELECT api_key_encrypted FROM provider_configs WHERE id=$1").bind(after.3).fetch_one(&mut root).await?;
        assert_eq!(copied,ciphertext,"one exact encrypted move, not a new secret store");
        let kept:serde_json::Value=sqlx::query_scalar("SELECT profile FROM model_profiles WHERE provider_id=$1").bind(legacy).fetch_one(&mut root).await?;
        assert_eq!(kept,serde_json::to_value(&legacy_profile)?);
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(8).after_connect({let schema=schema.clone();move |conn,_|{
            let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));Box::pin(async move {sqlx::raw_sql(sql).execute(conn).await?;Ok(())})
        }}).connect_with(options.clone()).await?;
        let checked=exercise(pool.clone(),actor(admin_id,PlatformRole::ClientAdmin),actor(user_id,PlatformRole::User),actor(other_id,PlatformRole::User),legacy,after.3).await;
        pool.close().await;checked?;
        // Rollback leaves old columns/indexes and the migration ledger usable,
        // while retaining variant metadata as inactive, key-free archive data.
        sqlx::raw_sql(include_str!("../deploy/scripts/rollback-routing-vnext.sql")).execute(&mut root).await?;
        Box::pin(chat_binding_cases::rollback(&mut root,binding_chat,&binding_intent)).await?;
        let old_key:Option<String>=sqlx::query_scalar("SELECT api_key_encrypted FROM provider_configs WHERE id=$1").bind(legacy).fetch_one(&mut root).await?;
        assert_eq!(fosnie_backend::crypto::decrypt(&[47u8;32],old_key.as_ref().unwrap())?,"synthetic-rotated-key");
        let receipt:i64=sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE version IN (139,140,141,142,143,144,145,146,147)").fetch_one(&mut root).await?;assert_eq!(receipt,0);
        let archived:i64=sqlx::query_scalar("SELECT count(*) FROM routing_vnext_metadata_archive WHERE kind='variant'").fetch_one(&mut root).await?;assert!(archived>=100);
        let profiles:i64=sqlx::query_scalar("SELECT count(*) FROM routing_vnext_metadata_archive WHERE kind='routing_profile'").fetch_one(&mut root).await?;assert!(profiles>=4);
        let leaked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routing_vnext_metadata_archive WHERE payload::text LIKE '%api_key%')").fetch_one(&mut root).await?;assert!(!leaked);
        sqlx::raw_sql(include_str!("../migrations/0139_routing_connections_variants.sql")).execute(&mut root).await?;
        let keys:i64=sqlx::query_scalar("SELECT count(*) FROM provider_configs WHERE role='llm' AND api_key_encrypted IS NOT NULL").fetch_one(&mut root).await?;assert_eq!(keys,0);
        Ok::<_,anyhow::Error>(())
    }.await;
    root.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&mut root)
        .await?;
    result
}

async fn exercise(
    pool: sqlx::PgPool,
    admin: AuthContext,
    user: AuthContext,
    other: AuthContext,
    legacy: Uuid,
    connection_id: Uuid,
) -> anyhow::Result<()> {
    let state = make_state(pool.clone(), false);
    // The adjusted partial indexes still serve legacy fixed-role upserts.
    for url in ["http://127.0.0.1:1/v1", "http://127.0.0.1:2/v1"] {
        fosnie_backend::http::providers::set_provider(
            axum::extract::State(state.clone()),
            fosnie_backend::auth::keycloak::AuthUser(admin.clone()),
            axum::extract::Path("stt".into()),
            axum::Json(fosnie_backend::http::providers::UpsertProvider {
                base_url: Some(url.into()),
                model: Some("fixed-fixture".into()),
                api_key: None,
                enabled: true,
                reasoning_mode: None,
            }),
        )
        .await?;
    }
    let resolved = state
        .providers
        .resolve_llm_id(&pool, user.user_id, legacy)
        .await?
        .unwrap();
    assert_eq!(resolved.api_key.as_deref(), Some("synthetic-original-key"));
    assert!(state
        .providers
        .resolve(&pool, "connection", user.user_id)
        .await
        .is_err());
    let connection = connections::list(&state, &admin)
        .await?
        .into_iter()
        .find(|c| c.id == connection_id)
        .unwrap();
    let second = Uuid::now_v7();
    let model = deployments::save(
        &state,
        &admin,
        second,
        deployments::SaveDeployment {
            schema_version: 2,
            expected_revision: 0,
            scope: "deployment".into(),
            label: "Second shared model".into(),
            model: "fixture-second".into(),
            connection_id,
            expected_connection_revision: connection.revision,
            enabled: true,
            reasoning_mode: None,
        },
    )
    .await?;
    save_profile(&state, &admin, second, "fixture-second", true).await?;
    assert!(
        deployments::delete(&state, &admin, second, model.revision - 1)
            .await
            .is_err()
    );
    assert_eq!(
        state
            .providers
            .resolve_llm_id(&pool, user.user_id, second)
            .await?
            .unwrap()
            .api_key
            .as_deref(),
        Some("synthetic-original-key")
    );
    let copies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_configs WHERE api_key_encrypted IS NOT NULL",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(copies, 1);
    // Ordinary users can save personal variants of shared models with BYOK off.
    let low = variants::save(
        &state,
        &user,
        Uuid::now_v7(),
        variant_body(second, model.model_config_revision, "user", "low"),
    )
    .await?;
    let high = variants::save(
        &state,
        &user,
        Uuid::now_v7(),
        variant_body(second, model.model_config_revision, "user", "high"),
    )
    .await?;
    assert_ne!(low.behaviour_sha256, high.behaviour_sha256);
    assert!(variants::save(
        &state,
        &user,
        Uuid::now_v7(),
        variant_body(second, model.model_config_revision, "user", "xhigh")
    )
    .await
    .is_err());
    assert!(variants::save(
        &state,
        &user,
        Uuid::now_v7(),
        variant_body(second, model.model_config_revision, "user", "low")
    )
    .await
    .is_err());
    assert!(variants::delete(&state, &other, low.id, low.revision)
        .await
        .is_err());
    assert!(connections::save(
        &state,
        &user,
        connection_id,
        connection_body(connection.revision, "http://127.0.0.1:1/v1", None)
    )
    .await
    .is_err());
    let exported = variants::export(&state, &user, high.id).await?;
    let json = serde_json::to_value(&exported)?;
    assert!(
        json.get("provider_id").is_none()
            && json.get("scope").is_none()
            && json.get("api_key").is_none()
    );
    let mut bad = json.clone();
    bad["api_key"] = json!("forbidden");
    assert!(serde_json::from_value::<variants::VariantExport>(bad).is_err());
    let imported = variants::import(
        &state,
        &other,
        Uuid::now_v7(),
        variants::ImportVariant {
            schema_version: 2,
            scope: "user".into(),
            provider_id: second,
            expected_model_config_revision: model.model_config_revision,
            package: exported,
        },
    )
    .await?;
    assert!(variants::catalogue(&state, &user)
        .await?
        .variants
        .iter()
        .all(|v| v.variant.id != imported.id));
    let before_config: i64 =
        sqlx::query_scalar("SELECT model_config_revision FROM provider_configs WHERE id=$1")
            .bind(second)
            .fetch_one(&pool)
            .await?;
    let current = connections::list(&state, &admin)
        .await?
        .into_iter()
        .find(|c| c.id == connection_id)
        .unwrap();
    let rotated = connections::save(
        &state,
        &admin,
        connection_id,
        connection_body(
            current.revision,
            "http://127.0.0.1:1/v1",
            Some("synthetic-rotated-key"),
        ),
    )
    .await?;
    assert!(rotated.auth_revision > current.auth_revision);
    assert!(connections::save(
        &state,
        &admin,
        connection_id,
        connection_body(current.revision, "http://127.0.0.1:1/v1", None)
    )
    .await
    .is_err());
    let after_config: i64 =
        sqlx::query_scalar("SELECT model_config_revision FROM provider_configs WHERE id=$1")
            .bind(second)
            .fetch_one(&pool)
            .await?;
    assert_eq!(after_config, before_config);
    let view = variants::catalogue(&state, &user).await?;
    let saved = view
        .variants
        .iter()
        .find(|v| v.variant.id == low.id)
        .unwrap();
    assert!(saved.configuration_current && !saved.binding_current);
    assert_eq!(saved.calibration_state, "configured");
    assert_eq!(saved.variant.behaviour_sha256, low.behaviour_sha256);
    assert_eq!(view.selection_versions, vec![1,2]);
    assert_eq!(view.profile_versions, vec![1,2]);
    assert!(
        connections::delete(&state, &admin, connection_id, rotated.revision)
            .await
            .is_err()
    );
    let legacy_edit =
        sqlx::query("UPDATE provider_configs SET api_key_encrypted='unaccepted-copy' WHERE id=$1")
            .bind(legacy)
            .execute(&pool)
            .await;
    assert!(legacy_edit.is_err());
    // Concurrent stale CAS has one winner and keeps exactly one active key.
    let (one, two) = tokio::join!(
        connections::save(
            &state,
            &admin,
            connection_id,
            connection_body(rotated.revision, "http://127.0.0.1:1/v1", None)
        ),
        connections::save(
            &state,
            &admin,
            connection_id,
            connection_body(rotated.revision, "http://127.0.0.1:1/v1", None)
        )
    );
    assert_ne!(one.is_ok(), two.is_ok());
    save_profile(&state, &admin, legacy, "fixture-legacy", true).await?;
    save_profile(&state, &admin, second, "fixture-second", true).await?;
    // One bounded HTTP metadata call to an owned mock, not a model generation.
    let app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(|| async { axum::Json(json!({"data":[{"id":"metadata-fixture"}]})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let latest = connections::list(&state, &admin)
        .await?
        .into_iter()
        .find(|c| c.id == connection_id)
        .unwrap();
    let source = connections::save(
        &state,
        &admin,
        connection_id,
        connection_body(latest.revision, &format!("http://{address}/v1"), None),
    )
    .await?;
    let report = discovery::discover(
        &state,
        &admin,
        connection_id,
        discovery::DiscoveryRequest {
            schema_version: 2,
            expected_revision: source.revision,
        },
    )
    .await?;
    assert_eq!(report.metadata_requests, 1);
    assert_eq!(report.generation_calls, 0);
    assert!(!report.models[0].enabled && !report.credits_known && !report.capabilities_verified);
    server.abort();
    // Scope links cannot cross users, even if the UUID is known.
    let byok = make_state(pool.clone(), true);
    let mut private_body = connection_body(0, "http://127.0.0.1:1/v1", None);
    private_body.scope = "user".into();
    private_body.metadata.locality_trusted = false;
    let private = connections::save(&byok, &user, Uuid::now_v7(), private_body).await?;
    assert!(deployments::save(
        &byok,
        &other,
        Uuid::now_v7(),
        deployments::SaveDeployment {
            schema_version: 2,
            expected_revision: 0,
            scope: "user".into(),
            label: "Cross owner".into(),
            model: "private-fixture".into(),
            connection_id: private.id,
            expected_connection_revision: private.revision,
            enabled: true,
            reasoning_mode: None
        }
    )
    .await
    .is_err());
    // Required catalogue capacity, all fixtures: 10 connections /33 models /
    //100 effective variants. Mocks do not establish real model quality.
    let mut conns = Vec::new();
    let mut model_ids = BTreeSet::new();
    let mut variant_count = 0;
    for _ in 0..10 {
        conns.push(
            connections::save(
                &state,
                &admin,
                Uuid::now_v7(),
                connection_body(0, "http://127.0.0.1:1/v1", None),
            )
            .await?,
        );
    }
    for n in 0..33 {
        let id = Uuid::now_v7();
        let conn = &conns[n % 10];
        let name = format!("large-fixture-{n}");
        let model = deployments::save(
            &state,
            &admin,
            id,
            deployments::SaveDeployment {
                schema_version: 2,
                expected_revision: 0,
                scope: "deployment".into(),
                label: name.clone(),
                model: name.clone(),
                connection_id: conn.id,
                expected_connection_revision: conn.revision,
                enabled: true,
                reasoning_mode: None,
            },
        )
        .await?;
        save_profile(&state, &admin, id, &name, n < 3).await?;
        model_ids.insert(id);
        for effort in ["none", "low", "high"] {
            variants::save(
                &state,
                &admin,
                Uuid::now_v7(),
                variant_body(id, model.model_config_revision, "deployment", effort),
            )
            .await?;
            variant_count += 1;
        }
        if n == 0 {
            let mut extra = variant_body(id, model.model_config_revision, "deployment", "high");
            extra.generation.max_output_tokens = Some(512);
            variants::save(&state, &admin, Uuid::now_v7(), extra).await?;
            variant_count += 1;
        }
    }
    assert_eq!(variant_count, 100);
    let catalogue = variants::catalogue(&state, &user).await?;
    assert_eq!(
        catalogue
            .deployments
            .iter()
            .filter(|m| model_ids.contains(&m.id))
            .count(),
        33
    );
    assert_eq!(
        catalogue
            .variants
            .iter()
            .filter(|v| model_ids.contains(&v.variant.provider_id) && v.configuration_current)
            .count(),
        100
    );
    eprintln!("D5 profile/runtime lane");
    profile_cases::exercise_profiles(&state,&admin,&user,&other).await?;
    let response = serde_json::to_string(&catalogue)?;
    assert!(!response.contains("synthetic-rotated-key") && !response.contains("api_key_encrypted"));
    Ok(())
}
