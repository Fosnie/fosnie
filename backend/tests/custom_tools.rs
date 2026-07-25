//! Custom HTTP tools end-to-end against a loopback
//! server — no Keycloak/ML needed, only a Postgres (`DATABASE_URL`). Skips when
//! unset. Exercises: the egress gate + dual-mode SSRF, `{{param}}` substitution,
//! JSON-Pointer response extraction, and the enabled/approved advertise filter.

use std::sync::Arc;

use axum::routing::get;
use axum::Json;
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::integrations::{self, ConnectorKind};
use fosnie_backend::state::AppState;
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{cache, db, tools};
use tokio::sync::mpsc;
use uuid::Uuid;

async fn state() -> Option<(sqlx::PgPool, AppState)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));
    Some((pg, state))
}

fn admin_ctx(user_id: Option<Uuid>) -> AuthContext {
    AuthContext {
        user_id,
        email: None,
        display_name: None,
        role: PlatformRole::ClientAdmin,
        break_glass: false, mfa_enroll_only: false,
    }
}

/// Mint an authorisation witness through the real seam, then dispatch — the only
/// way to reach the witness-gated `tools::dispatch`. The custom tool under test is
/// present in `custom`, so it enters the per-turn authorised set as production
/// would (via `load_enabled_custom`).
#[allow(clippy::too_many_arguments)]
async fn dispatch_via_seam(
    st: &AppState,
    ctx: &AuthContext,
    project_id: Option<Uuid>,
    chat_id: Uuid,
    turn: Uuid,
    tx: &mpsc::Sender<ServerFrame>,
    custom: &std::collections::HashMap<String, tools::custom::CustomToolRow>,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, fosnie_backend::AppError> {
    let authorised =
        tools::AuthorisedTools::build(&[name.to_string()], &[name.to_string()], false, custom);
    let overrides = std::collections::HashMap::new();
    match tools::authorize_native_call(st, ctx, chat_id, &authorised, &overrides, name, project_id)
        .await
    {
        tools::NativeDecision::Allowed(w) => {
            tools::dispatch(st, ctx, chat_id, turn, tx, None, None, None, &[], custom, &w, args).await
        }
        tools::NativeDecision::Recoverable(m) => Ok(m),
        tools::NativeDecision::Denied(e) => Err(e),
    }
}

/// A loopback JSON endpoint: returns `{"data":{"rate":"1.27"}}`.
async fn spawn_echo() -> u16 {
    let app = axum::Router::new()
        .route("/rate", get(|| async { Json(serde_json::json!({ "data": { "rate": "1.27" } })) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

async fn insert_tool(pg: &sqlx::PgPool, name: &str, url: &str, requires_egress: bool, enabled: bool) -> Uuid {
    let id = Uuid::now_v7();
    let config = serde_json::json!({
        "method": "GET",
        "url": url,
        "headers": {},
        "response": { "mode": "pointer", "pointer": "/data/rate" }
    });
    let schema = serde_json::json!({ "type": "object", "properties": { "pair": { "type": "string" } } });
    sqlx::query(
        "INSERT INTO custom_tools (id, name, display_name, description, kind, params_schema, config, \
         requires_egress, side_effecting, enabled, approved_version, version, timeout_secs) \
         VALUES ($1,$2,$2,'test','http',$3,$4,$5,false,$6,1,1,10)",
    )
    .bind(id)
    .bind(name)
    .bind(schema)
    .bind(config)
    .bind(requires_egress)
    .bind(enabled)
    .execute(pg)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn custom_http_tool_end_to_end() {
    let Some((pg, state)) = state().await else { return };
    // A real user id — config_settings.updated_by has an FK to users.
    let uid: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users LIMIT 1").fetch_optional(&pg).await.unwrap();
    let Some(uid) = uid else { return }; // no seeded users → skip
    let ctx = admin_ctx(Some(uid));
    let port = spawn_echo().await;

    // Clean slate + enable the connector globally (else the egress gate blocks).
    let _ = sqlx::query("DELETE FROM custom_tools WHERE name LIKE 'itest_%'").execute(&pg).await;
    integrations::set_enabled(&state, &ctx, ConnectorKind::CustomTool, true).await.unwrap();

    // A private (requires_egress=false) tool may target loopback.
    let url = format!("http://127.0.0.1:{port}/rate?pair=") + "{{pair}}";
    let id = insert_tool(&pg, "itest_fx", &url, false, true).await;

    // Advertised (enabled + approved) → in the per-turn map.
    let (defs, map) = tools::custom::load_enabled_custom(&pg, &["itest_fx".to_string()]).await;
    assert_eq!(defs.len(), 1, "enabled+approved tool must be advertised");
    assert!(map.contains_key("itest_fx"));

    // Dispatch: substitution + JSON-Pointer extraction → the rate string.
    let (tx, _rx) = mpsc::channel::<ServerFrame>(8);
    let out = dispatch_via_seam(
        &state, &ctx, None, Uuid::now_v7(), Uuid::now_v7(), &tx, &map, "itest_fx",
        &serde_json::json!({ "pair": "GBPUSD" }),
    )
    .await
    .unwrap();
    assert_eq!(out, "1.27", "pointer /data/rate should extract the rate, got: {out}");

    // SSRF: a remote (requires_egress=true) tool may NOT reach loopback.
    let id2 = insert_tool(&pg, "itest_ssrf", &url, true, true).await;
    let (_defs2, map2) = tools::custom::load_enabled_custom(&pg, &["itest_ssrf".to_string()]).await;
    let blocked = dispatch_via_seam(
        &state, &ctx, None, Uuid::now_v7(), Uuid::now_v7(), &tx, &map2, "itest_ssrf",
        &serde_json::json!({ "pair": "X" }),
    )
    .await
    .unwrap();
    assert!(blocked.starts_with("error:"), "remote tool at loopback must be refused, got: {blocked}");

    // A disabled tool is neither advertised nor dispatchable.
    sqlx::query("UPDATE custom_tools SET enabled = false WHERE id = $1").bind(id).execute(&pg).await.unwrap();
    let (defs3, _map3) = tools::custom::load_enabled_custom(&pg, &["itest_fx".to_string()]).await;
    assert_eq!(defs3.len(), 0, "disabled tool must drop out of the catalogue");

    // Cleanup.
    let _ = sqlx::query("DELETE FROM custom_tools WHERE id = ANY($1)")
        .bind(vec![id, id2])
        .execute(&pg)
        .await;
    integrations::set_enabled(&state, &ctx, ConnectorKind::CustomTool, false).await.unwrap();
}

/// A script custom tool: on a host without the code-interpreter sandbox (feature
/// off or non-Linux — e.g. this dev box) dispatch refuses cleanly with `error:`
/// text rather than hanging. Real in-VM execution is a Linux-only test.
#[tokio::test]
async fn custom_script_tool_refuses_without_sandbox() {
    let Some((pg, state)) = state().await else { return };
    let ctx = admin_ctx(None);
    let _ = sqlx::query("DELETE FROM custom_tools WHERE name LIKE 'itest_%'").execute(&pg).await;

    let id = Uuid::now_v7();
    let schema = serde_json::json!({ "type": "object", "properties": {} });
    let config = serde_json::json!({ "source": "print('hi')" });
    sqlx::query(
        "INSERT INTO custom_tools (id, name, display_name, description, kind, params_schema, config, \
         requires_egress, side_effecting, enabled, approved_version, version, timeout_secs) \
         VALUES ($1,'itest_script','itest_script','test','script',$2,$3,false,true,true,1,1,10)",
    )
    .bind(id)
    .bind(schema)
    .bind(config)
    .execute(&pg)
    .await
    .unwrap();

    let (defs, map) = tools::custom::load_enabled_custom(&pg, &["itest_script".to_string()]).await;
    assert_eq!(defs.len(), 1, "an enabled+approved script tool is advertised");

    let (tx, _rx) = mpsc::channel::<ServerFrame>(8);
    let out = dispatch_via_seam(
        &state, &ctx, None, Uuid::now_v7(), Uuid::now_v7(), &tx, &map, "itest_script",
        &serde_json::json!({}),
    )
    .await
    .unwrap();
    // On a sandbox-less host the result is an honest refusal (never a panic/hang).
    #[cfg(not(target_os = "linux"))]
    assert!(out.starts_with("error:"), "script must refuse without a Linux sandbox, got: {out}");
    let _ = out; // on Linux with the feature on, this would carry stdout instead.

    let _ = sqlx::query("DELETE FROM custom_tools WHERE id = $1").bind(id).execute(&pg).await;
}
