//! Integrations dual-mode framework: dormant-by-default, admin-only activation,
//! the zero-egress gate (audited), and the DMS connector-adapter surface. One
//! sequential test — the connector enabled-flags are global `config_settings`
//! rows, so parallel test fns would race on them. No LLM. Gated on PAI_E2E=1.
//! alice=admin, carol=user (dev realm).

use std::sync::Arc;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::integrations::{self, dms, ConnectorKind};
use fosnie_backend::state::AppState;
use fosnie_backend::ws::protocol::ServerFrame;
use fosnie_backend::{auth, cache, db, http, tools};
use tokio::sync::mpsc;
use uuid::Uuid;

const KC: &str = "http://localhost:8081/realms/fosnie";

fn enabled() -> bool {
    std::env::var("PAI_E2E").as_deref() == Ok("1")
}

/// Mint an authorisation witness through the real seam, then dispatch — the only
/// way to reach the witness-gated `tools::dispatch`. Grants `web_search` so the
/// per-turn authorisation passes; the egress gate still fires inside the tool arm,
/// which is what the dormant-refusal test exercises.
#[allow(clippy::too_many_arguments)]
async fn dispatch_via_seam(
    st: &AppState,
    ctx: &AuthContext,
    project_id: Option<Uuid>,
    chat_id: Uuid,
    turn: Uuid,
    tx: &mpsc::Sender<ServerFrame>,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, fosnie_backend::AppError> {
    let custom = std::collections::HashMap::new();
    let authorised =
        tools::AuthorisedTools::build(&[name.to_string()], &[name.to_string()], false, &custom);
    let overrides = std::collections::HashMap::new();
    match tools::authorize_native_call(st, ctx, chat_id, &authorised, &overrides, name, project_id)
        .await
    {
        tools::NativeDecision::Allowed(w) => {
            tools::dispatch(st, ctx, chat_id, turn, tx, None, None, &[], &custom, &w, args).await
        }
        tools::NativeDecision::Recoverable(m) => Ok(m),
        tools::NativeDecision::Denied(e) => Err(e),
    }
}

async fn token(user: &str) -> Option<String> {
    let c = reqwest::Client::new();
    let r = c
        .post(format!("{KC}/protocol/openid-connect/token"))
        .form(&[
            ("grant_type", "password"),
            ("client_id", "fosnie"),
            ("client_secret", "fosnie-secret"),
            ("username", user),
            ("password", user),
            ("scope", "openid profile email"),
        ])
        .send()
        .await
        .ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json::<serde_json::Value>().await.ok()?["access_token"].as_str().map(String::from)
}

async fn setup() -> (sqlx::PgPool, AppState, u16) {
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    // Allow pointing at an ML service on a non-default port (the dev box's
    // 8045-8144 band is sometimes WinNAT-reserved, forcing ML onto e.g. 9090).
    if let Ok(ml) = std::env::var("PAI__ML__BASE_URL") {
        boot.ml.base_url = ml;
    }
    boot.keycloak.url = "http://localhost:8081".into();
    boot.keycloak.realm = "fosnie".into();
    boot.keycloak.client_id = "fosnie".into();
    boot.server.static_dir = "___no_spa___".into();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));
    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app2 = app.clone();
    tokio::spawn(async move { axum::serve(listener, app2).await.unwrap() });
    (pg, state, port)
}

/// Deep web search runs as a background job that posts its digest + citations
/// back into the chat. Drives the handler directly
/// (the scheduler arm just calls it) against the live ML loop. Gated on PAI_E2E
/// + a reachable ML service (PAI__ML__BASE_URL, e.g. http://localhost:9090).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deep_web_search_posts_result_back_into_chat() {
    if !enabled() || std::env::var("PAI__ML__BASE_URL").is_err() {
        return;
    }
    let (pg, state, _port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{_port}");
    let alice = token("alice").await.expect("alice");
    let alice_id = whoami_id(&api, &base, &alice).await;

    // A chat owned by alice to post the result back into.
    let chat_id = Uuid::now_v7();
    sqlx::query("INSERT INTO chats (id, owner_user_id, title) VALUES ($1, $2, 'deep test')")
        .bind(chat_id)
        .bind(alice_id)
        .execute(&pg)
        .await
        .unwrap();
    let turn_id = Uuid::now_v7();

    let before = count_action(&pg, "web_search.results").await;
    let payload = serde_json::json!({
        "chat_id": chat_id,
        "turn_id": turn_id,
        "user_id": alice_id,
        "role": "client_admin",
        "query": "what is the latest stable release of the Rust programming language",
        "recency": "month",
    });
    fosnie_backend::web_search::run_deep(&state, &payload).await.unwrap();

    // A complete assistant message was posted into the chat …
    let msg = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, content FROM messages WHERE chat_id = $1 AND role = 'assistant' \
         AND completed_at IS NOT NULL ORDER BY sequence_number DESC LIMIT 1",
    )
    .bind(chat_id)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(msg.1.contains("Deep web search results"), "posted message carries the deep digest: {}", msg.1);

    // … its web citations are linked directly to that message …
    let cit: i64 = sqlx::query_scalar("SELECT count(*) FROM web_citations WHERE message_id = $1")
        .bind(msg.0)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert!(cit >= 1, "deep result linked at least one web citation");

    // … and the deep result was audited.
    assert!(count_action(&pg, "web_search.results").await > before, "deep result audited");
}

async fn whoami_id(api: &reqwest::Client, base: &str, tok: &str) -> Uuid {
    let v: serde_json::Value =
        api.get(format!("{base}/api/whoami")).bearer_auth(tok).send().await.unwrap().json().await.unwrap();
    Uuid::parse_str(v["user_id"].as_str().unwrap()).unwrap()
}

async fn count_action(pg: &sqlx::PgPool, action: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type = $1")
        .bind(action)
        .fetch_one(pg)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dormant_by_default_activation_and_egress_gate() {
    if !enabled() {
        return;
    }
    let (pg, state, port) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let alice = token("alice").await.expect("alice");
    let carol = token("carol").await.expect("carol");
    let alice_id = whoami_id(&api, &base, &alice).await;

    // Reset to a known dormant baseline (the flags are global + persistent).
    let alice_ctx = AuthContext {
        user_id: Some(alice_id),
        email: None,
        display_name: None,
        role: PlatformRole::ClientAdmin,
        break_glass: false, mfa_enroll_only: false,
    };
    integrations::set_enabled(&state, &alice_ctx, ConnectorKind::WebSearch, false).await.unwrap();
    integrations::set_enabled(&state, &alice_ctx, ConnectorKind::IManage, false).await.unwrap();

    // 1. Admin list → the full closed set, all dormant.
    let all: Vec<serde_json::Value> = api
        .get(format!("{base}/api/admin/integrations"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(all.len(), 6, "every known connector listed");
    assert!(
        all.iter().any(|c| c["kind"] == "web_search")
            && all.iter().any(|c| c["kind"] == "mcp"),
        "web_search + mcp present"
    );
    assert!(
        all.iter().find(|c| c["kind"] == "web_search").unwrap()["enabled"] == false,
        "web_search dormant by default"
    );

    // 2. Privilege split: connector activation is super-admin-only (active
    //    break-glass). Neither a plain user NOR a persistent client-admin can
    //    toggle a connector over Bearer — both lack the X-Break-Glass header.
    let user_refused = api
        .put(format!("{base}/api/admin/integrations/web_search"))
        .bearer_auth(&carol)
        .json(&serde_json::json!({ "enabled": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(user_refused.status().as_u16(), 401, "carol (user) lacks break-glass");
    let admin_refused = api
        .put(format!("{base}/api/admin/integrations/web_search"))
        .bearer_auth(&alice)
        .json(&serde_json::json!({ "enabled": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(admin_refused.status().as_u16(), 401, "client-admin may not toggle connectors");

    // 3. The egress gate is dormant → tool call is refused, no egress, audited.
    let (tx, _rx) = mpsc::channel::<ServerFrame>(8);
    let blocked_before = count_action(&pg, "integration.blocked").await;
    let dormant = dispatch_via_seam(
        &state,
        &alice_ctx,
        None,
        Uuid::now_v7(),
        Uuid::now_v7(),
        &tx,
        "web_search",
        &serde_json::json!({ "query": "anything" }),
    )
    .await
    .unwrap();
    assert!(dormant.contains("not available"), "dormant web_search refused: {dormant}");
    assert!(count_action(&pg, "integration.blocked").await > blocked_before, "block audited");

    // 4. A super-admin (active break-glass grant) activates web_search → 200,
    //    persists, audited. Mint a grant and present it via X-Break-Glass.
    let activated_before = count_action(&pg, "integration.activated").await;
    let grant = auth::breakglass::issue(&state, 300, "test", "enable web_search").await.unwrap();
    let ok = api
        .put(format!("{base}/api/admin/integrations/web_search"))
        .header("x-break-glass", grant.to_string())
        .json(&serde_json::json!({ "enabled": true }))
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success(), "super-admin enables web_search");
    let val: String = sqlx::query_scalar(
        "SELECT value FROM config_settings WHERE key = 'integration.web_search.enabled'",
    )
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(val, "true", "enabled flag persisted in config_settings");
    assert!(count_action(&pg, "integration.activated").await > activated_before, "activation audited");

    // 5. Now the gate passes (audited as a call) and dispatch reaches the real
    // backend: with the ML service up it returns a sourced digest; without it,
    // the honest "currently unavailable" degradation. Either way, no egress
    // happened before the gate.
    let call_before = count_action(&pg, "integration.call").await;
    let live = dispatch_via_seam(
        &state,
        &alice_ctx,
        None,
        Uuid::now_v7(),
        Uuid::now_v7(),
        &tx,
        "web_search",
        &serde_json::json!({ "query": "anything" }),
    )
    .await
    .unwrap();
    assert!(
        live.contains("Web sources")
            || live.contains("currently unavailable")
            || live.contains("No web results"),
        "enabled web_search returns a digest or degrades honestly: {live}"
    );
    assert!(count_action(&pg, "integration.call").await > call_before, "call audited");

    // 6. DMS adapter: dormant iManage refused at the gate; enabled → reaches the
    // stub which honestly reports it is not built.
    let dms_dormant = dms::dms_search(&state, &alice_ctx, ConnectorKind::IManage, "matter").await;
    assert!(matches!(dms_dormant, Err(fosnie_backend::AppError::Forbidden(_))), "dormant DMS forbidden");

    integrations::set_enabled(&state, &alice_ctx, ConnectorKind::IManage, true).await.unwrap();
    let dms_live = dms::dms_search(&state, &alice_ctx, ConnectorKind::IManage, "matter").await;
    assert!(
        matches!(dms_live, Err(fosnie_backend::AppError::Unavailable(_))),
        "enabled DMS connector is not built (Pass-2)"
    );

    // Tidy: return both connectors to dormant so other suites see the default.
    integrations::set_enabled(&state, &alice_ctx, ConnectorKind::WebSearch, false).await.unwrap();
    integrations::set_enabled(&state, &alice_ctx, ConnectorKind::IManage, false).await.unwrap();

    // 7. The audit chain is intact across all those appends.
    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}
