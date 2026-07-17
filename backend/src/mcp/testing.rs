//! In-crate mock authorisation-server suite for the MCP OAuth flow. Exercises
//! discovery, the authorize leg, DCR, every refusal fork, AND the token REFRESH leg
//! against a LOCAL self-signed TLS authorisation server. No internet, no OAuth app, no
//! `PAI_E2E`, no ML - only `DATABASE_URL` + Redis.
//!
//! Compiled only under `--features test-mocks`, which enables a hook in
//! `mcp::client::build_hardened_client` that trusts one extra root cert (the mock's).
//! That hook can ONLY add a trusted root; it cannot weaken any check. This module lives
//! in `src/` (not `tests/`) so the refresh tests can reach module-private items:
//! `oauth_flow::base_manager` and the reauth mapping inside `mcp::dispatch`.
//!
//! What is and is not reachable through rmcp 1.7.0:
//! - Code exchange (`exchange_code_for_token`, auth.rs:1167) builds its OWN reqwest
//!   client (redirect-safe but untrusting of a self-signed root), so the code-exchange
//!   token POST is not driven here. The `resource`/no-trailing-slash wire format is
//!   proven on the `/authorize` leg instead (identical `base_url.to_string()` path).
//! - Refresh (`refresh_token`, auth.rs:1287) uses `self.http_client` - OUR injected,
//!   cert-trusting client - so the refresh leg IS driven, over TLS, here.
#![allow(dead_code)] // harness items are consumed only by the `#[cfg(test)]` tests

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde_json::json;
use uuid::Uuid;

use crate::auth::{AuthContext, PlatformRole};
use crate::config::BootConfig;
use crate::mcp::oauth_flow::{run_discovery, DcrRegistration, OAuthClientRow, ValidatedDiscovery};
use crate::state::AppState;
use crate::{cache, db};

const TEST_KEY_B64: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

// ── Mock configuration (per test; happy defaults) ─────────────────────────────

#[derive(Clone)]
struct MockConfig {
    /// Advertise `code_challenge_methods_supported: ["S256"]` (else omit it).
    advertise_s256: bool,
    /// Advertise `authorization_response_iss_parameter_supported: true`.
    iss_supported: bool,
    /// PRM `authorization_servers` (defaults to `[self]`).
    authorization_servers: Option<Vec<String>>,
    /// Force the metadata `issuer`/`authorization_endpoint` at a cloud-metadata host.
    cloud_metadata_endpoints: bool,
    /// Append `&iss=` (the metadata issuer) on the /authorize redirect.
    append_iss: bool,
    /// Refresh: respond WITHOUT a new `refresh_token` (the COALESCE case).
    refresh_omit_new_refresh: bool,
    /// Refresh: respond `400 invalid_grant` (the refresh-failure case).
    refresh_invalid_grant: bool,
}

impl Default for MockConfig {
    fn default() -> Self {
        MockConfig {
            advertise_s256: true,
            iss_supported: false,
            authorization_servers: None,
            cloud_metadata_endpoints: false,
            append_iss: false,
            refresh_omit_new_refresh: false,
            refresh_invalid_grant: false,
        }
    }
}

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    query: String,
    body: String,
    auth_header: Option<String>,
}

struct MockState {
    base: String,
    callback: String,
    config: MockConfig,
    recorded: Mutex<Vec<Recorded>>,
}

impl MockState {
    fn record(&self, method: &str, path: &str, query: &str, body: &[u8], headers: &HeaderMap) {
        self.recorded.lock().unwrap().push(Recorded {
            method: method.into(),
            path: path.into(),
            query: query.into(),
            body: String::from_utf8_lossy(body).into_owned(),
            auth_header: headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(String::from),
        });
    }
    fn issuer(&self) -> String {
        if self.config.cloud_metadata_endpoints {
            "https://metadata.google.internal".to_string()
        } else {
            self.base.clone()
        }
    }
    fn recorded(&self) -> Vec<Recorded> {
        self.recorded.lock().unwrap().clone()
    }
}

type St = Arc<MockState>;

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn prm(State(s): State<St>) -> Response {
    let auth_servers = s
        .config
        .authorization_servers
        .clone()
        .unwrap_or_else(|| vec![s.base.clone()]);
    (
        StatusCode::OK,
        axum::Json(json!({
            "resource": format!("{}/mcp", s.base),
            "authorization_servers": auth_servers,
            "scopes_supported": ["read", "write"],
        })),
    )
        .into_response()
}

async fn as_metadata(State(s): State<St>) -> Response {
    let issuer = s.issuer();
    let (auth_ep, token_ep) = if s.config.cloud_metadata_endpoints {
        (
            "https://metadata.google.internal/authorize".to_string(),
            "https://metadata.google.internal/token".to_string(),
        )
    } else {
        (format!("{}/authorize", s.base), format!("{}/token", s.base))
    };
    let mut meta = json!({
        "issuer": issuer,
        "authorization_endpoint": auth_ep,
        "token_endpoint": token_ep,
        "registration_endpoint": format!("{}/register", s.base),
        "revocation_endpoint": format!("{}/revoke", s.base),
        "scopes_supported": ["read", "write"],
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
    });
    if s.config.advertise_s256 {
        meta["code_challenge_methods_supported"] = json!(["S256"]);
    }
    if s.config.iss_supported {
        meta["authorization_response_iss_parameter_supported"] = json!(true);
    }
    (StatusCode::OK, axum::Json(meta)).into_response()
}

async fn register(State(s): State<St>, headers: HeaderMap, body: Bytes) -> Response {
    s.record("POST", "/register", "", &body, &headers);
    let client_id = format!("mock-client-{}", Uuid::now_v7());
    (
        StatusCode::CREATED,
        axum::Json(json!({
            "client_id": client_id,
            "registration_client_uri": format!("{}/register/{}", s.base, client_id),
            "registration_access_token": "mock-mgmt-token",
        })),
    )
        .into_response()
}

async fn authorize(
    State(s): State<St>,
    raw_query: axum::extract::RawQuery,
    headers: HeaderMap,
) -> Response {
    let query = raw_query.0.unwrap_or_default();
    s.record("GET", "/authorize", &query, &[], &headers);
    let state_param = url_param(&query, "state").unwrap_or_default();
    let mut loc = format!("{}?code=mock-auth-code&state={}", s.callback, state_param);
    if s.config.append_iss {
        let iss = s.issuer();
        loc.push_str(&format!("&iss={}", urlencoding_min(&iss)));
    }
    let mut resp = Response::new(axum::body::Body::empty());
    *resp.status_mut() = StatusCode::FOUND;
    resp.headers_mut()
        .insert(axum::http::header::LOCATION, loc.parse().unwrap());
    resp
}

/// Input-derived `/token`: branches on the received `grant_type`, and for a refresh on
/// the presented `refresh_token`, so a test cannot pass against a constant response.
async fn token(State(s): State<St>, headers: HeaderMap, body: Bytes) -> Response {
    s.record("POST", "/token", "", &body, &headers);
    let raw = String::from_utf8_lossy(&body);
    let grant = form_param(&raw, "grant_type").unwrap_or_default();

    if grant == "refresh_token" {
        if s.config.refresh_invalid_grant {
            return (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": "invalid_grant" })))
                .into_response();
        }
        let presented = form_param(&raw, "refresh_token").unwrap_or_default();
        // New access token derived from the presented refresh token (never constant).
        let mut tok = json!({
            "access_token": format!("rotated-from-{presented}"),
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "read write",
        });
        if !s.config.refresh_omit_new_refresh {
            tok["refresh_token"] = json!(format!("new-refresh-{presented}"));
        }
        return (StatusCode::OK, axum::Json(tok)).into_response();
    }

    // authorization_code (not driven through rmcp here).
    (
        StatusCode::OK,
        axum::Json(json!({
            "access_token": "mock-access",
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "mock-refresh",
            "scope": "read write",
        })),
    )
        .into_response()
}

async fn delete_register(
    State(s): State<St>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
) -> Response {
    s.record("DELETE", &format!("/register/{id}"), "", &[], &headers);
    StatusCode::NO_CONTENT.into_response()
}

async fn revoke(State(s): State<St>, headers: HeaderMap, body: Bytes) -> Response {
    s.record("POST", "/revoke", "", &body, &headers);
    StatusCode::OK.into_response()
}

/// Any other path (including the MCP resource endpoint `/mcp`) → 401, so rmcp falls
/// back to the well-known PRM path.
async fn unauthorized(State(s): State<St>, headers: HeaderMap) -> Response {
    s.record("GET", "/*", "", &[], &headers);
    StatusCode::UNAUTHORIZED.into_response()
}

fn url_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        if it.next() == Some(key) {
            return it.next().map(|v| v.to_string());
        }
    }
    None
}

fn urlencoding_min(s: &str) -> String {
    s.replace(':', "%3A").replace('/', "%2F")
}

/// Parse `key`'s (decoded) value from a raw query/form string.
fn form_param(raw: &str, key: &str) -> Option<String> {
    let dummy = format!("http://x/?{raw}");
    reqwest::Url::parse(&dummy)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

// ── Harness ────────────────────────────────────────────────────────────────

struct Mock {
    base: String,
    state: St,
    cert: reqwest::Certificate,
}

impl Mock {
    fn recorded_for(&self, path_contains: &str) -> Vec<Recorded> {
        self.state
            .recorded()
            .into_iter()
            .filter(|r| r.path.contains(path_contains))
            .collect()
    }
    /// A client that acts as the user's browser for the /authorize leg: trusts the mock
    /// cert, does NOT follow redirects (so we can read the callback Location).
    fn browser(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .add_root_certificate(self.cert.clone())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }
}

struct SharedCert {
    cert_pem: String,
    key_pem: String,
    reqwest_cert: reqwest::Certificate,
}

/// One self-signed cert (SAN IP:127.0.0.1) for the WHOLE test binary. The flow's
/// hardened client trusts exactly one installed root (first-wins `OnceLock`), so every
/// mock server across every test must present this same cert - which they all can, as
/// they all serve on `127.0.0.1`. Also lets one test run two mock servers (cross-origin).
fn shared_cert() -> &'static SharedCert {
    static CERT: std::sync::OnceLock<SharedCert> = std::sync::OnceLock::new();
    CERT.get_or_init(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.subject_alt_names = vec![rcgen::SanType::IpAddress("127.0.0.1".parse().unwrap())];
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let cert_pem = cert.pem();
        let key_pem = key.serialize_pem();
        let reqwest_cert = reqwest::Certificate::from_pem(cert_pem.as_bytes()).unwrap();
        crate::mcp::client::install_test_root(reqwest_cert.clone());
        SharedCert { cert_pem, key_pem, reqwest_cert }
    })
}

/// Spawn a self-signed TLS mock AS on 127.0.0.1:0, sharing the binary's one trusted
/// cert, and return the running mock.
async fn spawn_mock(config: MockConfig, callback: String) -> Mock {
    let sc = shared_cert();
    let cert_pem = sc.cert_pem.clone();
    let key_pem = sc.key_pem.clone();
    let reqwest_cert = sc.reqwest_cert.clone();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("https://127.0.0.1:{port}");

    let state = Arc::new(MockState {
        base: base.clone(),
        callback,
        config,
        recorded: Mutex::new(Vec::new()),
    });

    let app = Router::new()
        .route("/.well-known/oauth-protected-resource", get(prm))
        .route("/.well-known/oauth-authorization-server", get(as_metadata))
        .route("/.well-known/openid-configuration", get(as_metadata))
        .route("/register", post(register))
        .route("/register/{id}", delete(delete_register))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .route("/revoke", post(revoke))
        .fallback(unauthorized)
        .with_state(state.clone());

    let cfg =
        axum_server::tls_rustls::RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes())
            .await
            .unwrap();
    let std_listener = listener;
    std_listener.set_nonblocking(false).unwrap();
    tokio::spawn(async move {
        let _ = axum_server::from_tcp_rustls(std_listener, cfg)
            .serve(app.into_make_service())
            .await;
    });

    Mock { base, state, cert: reqwest_cert }
}

async fn app_state() -> Option<(AppState, sqlx::PgPool)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig {
        database_url: db_url,
        redis_url,
        message_encryption_key: TEST_KEY_B64.to_string(),
        ..BootConfig::default()
    };
    boot.server.public_url = "https://callback.test".into();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));
    let _ = crate::config::runtime::set(
        &pg,
        "integration.mcp.enabled",
        "true",
        crate::config::runtime::ConfigValueType::Bool,
        "deployment",
        None,
        "system",
    )
    .await;
    Some((state, pg))
}

fn admin_ctx() -> AuthContext {
    AuthContext {
        user_id: Some(Uuid::now_v7()),
        email: None,
        display_name: None,
        role: PlatformRole::ClientAdmin,
        break_glass: false,
        mfa_enroll_only: false,
    }
}

async fn existing_user(pg: &sqlx::PgPool) -> Option<Uuid> {
    sqlx::query_scalar("SELECT id FROM users WHERE deactivated_at IS NULL LIMIT 1")
        .fetch_optional(pg)
        .await
        .unwrap()
}

async fn seed_server(pg: &sqlx::PgPool, url: &str) -> (Uuid, String) {
    let id = Uuid::now_v7();
    let slug = format!("oauth{}", id.simple());
    sqlx::query(
        "INSERT INTO mcp_servers (id, slug, name, transport, url, status, enabled, auth_type) \
         VALUES ($1,$2,$2,'http',$3,'active',true,'oauth')",
    )
    .bind(id)
    .bind(&slug)
    .bind(url)
    .execute(pg)
    .await
    .unwrap();
    (id, slug)
}

async fn seed_client(
    pg: &sqlx::PgPool,
    server_id: Uuid,
    disc: &ValidatedDiscovery,
    reg: &DcrRegistration,
) -> Uuid {
    let id = Uuid::now_v7();
    let meta = serde_json::to_value(&disc.metadata).unwrap();
    let reg_tok_enc = reg
        .registration_access_token
        .as_ref()
        .map(|t| crate::crypto::encrypt_at_rest(t).unwrap());
    sqlx::query(
        "INSERT INTO mcp_oauth_clients \
           (id, mcp_server_id, issuer, client_id, registration_source, registration_client_uri, \
            registration_access_token_enc, scopes, metadata, approved_at) \
         VALUES ($1,$2,$3,$4,'dcr',$5,$6,$7,$8, now())",
    )
    .bind(id)
    .bind(server_id)
    .bind(&disc.issuer)
    .bind(&reg.client_id)
    .bind(reg.registration_client_uri.as_deref())
    .bind(reg_tok_enc)
    .bind(vec!["read".to_string(), "write".to_string()])
    .bind(meta)
    .execute(pg)
    .await
    .unwrap();
    id
}

async fn seed_connection(pg: &sqlx::PgPool, server_id: Uuid, client_id: Uuid, user_id: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO mcp_oauth_connections (id, mcp_server_id, oauth_client_id, user_id, status) \
         VALUES ($1,$2,$3,$4,'pending')",
    )
    .bind(id)
    .bind(server_id)
    .bind(client_id)
    .bind(user_id)
    .execute(pg)
    .await
    .unwrap();
    id
}

fn client_row(id: Uuid, disc: &ValidatedDiscovery, reg: &DcrRegistration) -> OAuthClientRow {
    OAuthClientRow {
        id,
        issuer: disc.issuer.clone(),
        client_id: reg.client_id.clone(),
        client_secret: None,
        scopes: vec!["read".to_string(), "write".to_string()],
        metadata: serde_json::to_value(&disc.metadata).unwrap(),
    }
}

async fn cleanup(pg: &sqlx::PgPool, server_id: Uuid) {
    let _ = sqlx::query("DELETE FROM mcp_oauth_connections WHERE mcp_server_id=$1").bind(server_id).execute(pg).await;
    let _ = sqlx::query("DELETE FROM mcp_oauth_clients WHERE mcp_server_id=$1").bind(server_id).execute(pg).await;
    let _ = sqlx::query("DELETE FROM mcp_servers WHERE id=$1").bind(server_id).execute(pg).await;
}

/// D2: mint a fully AUTHORISED oauth connection with NO code exchange - an active
/// connection whose access token is already EXPIRED and whose refresh token is live, so
/// the next `get_access_token` triggers a refresh. `metadata` is the real discovered
/// metadata (its `token_endpoint` points at the mock), exactly as an approval persists;
/// the runtime never re-discovers, so seeding it is legitimate, not a cheat.
struct Authorised {
    server_id: Uuid,
    slug: String,
    connection_id: Uuid,
    row: OAuthClientRow,
    server_url: String,
    user_id: Uuid,
}

async fn seed_authorised_connection(
    state: &AppState,
    pg: &sqlx::PgPool,
    mock: &Mock,
    refresh: &str,
) -> Authorised {
    let server_url = format!("{}/mcp", mock.base);
    // Seed the server WITH a pinned tool in its catalogue, so a `slug__*` grant permits
    // it and dispatch reaches the OAuth connection resolve (gate 6) rather than stopping
    // at the catalogue gate.
    let server_id = Uuid::now_v7();
    let slug = format!("oauth{}", server_id.simple());
    let catalogue = json!([{ "name": "anytool", "description": "", "schema": {}, "side_effecting": false }]);
    sqlx::query(
        "INSERT INTO mcp_servers (id, slug, name, transport, url, status, enabled, auth_type, tools_catalog) \
         VALUES ($1,$2,$2,'http',$3,'active',true,'oauth',$4)",
    )
    .bind(server_id)
    .bind(&slug)
    .bind(&server_url)
    .bind(&catalogue)
    .execute(pg)
    .await
    .unwrap();
    let disc = run_discovery(state, &admin_ctx(), &server_url, None).await.unwrap();
    let meta = serde_json::to_value(&disc.metadata).unwrap();
    let user_id = existing_user(pg).await.expect("a seeded user");

    let client_uuid = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO mcp_oauth_clients \
           (id, mcp_server_id, issuer, client_id, registration_source, scopes, metadata, approved_at) \
         VALUES ($1,$2,$3,'seed-client','manual',$4,$5, now())",
    )
    .bind(client_uuid)
    .bind(server_id)
    .bind(&disc.issuer)
    .bind(vec!["read".to_string(), "write".to_string()])
    .bind(&meta)
    .execute(pg)
    .await
    .unwrap();

    let connection_id = Uuid::now_v7();
    let access_enc = crate::crypto::encrypt_at_rest("old-access").unwrap();
    let refresh_enc = crate::crypto::encrypt_at_rest(refresh).unwrap();
    let past = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    sqlx::query(
        "INSERT INTO mcp_oauth_connections \
           (id, mcp_server_id, oauth_client_id, user_id, status, access_token_enc, refresh_token_enc, expires_at, scopes) \
         VALUES ($1,$2,$3,$4,'active',$5,$6,$7,$8)",
    )
    .bind(connection_id)
    .bind(server_id)
    .bind(client_uuid)
    .bind(user_id)
    .bind(access_enc)
    .bind(refresh_enc)
    .bind(past)
    .bind(vec!["read".to_string(), "write".to_string()])
    .execute(pg)
    .await
    .unwrap();

    let row = OAuthClientRow {
        id: client_uuid,
        issuer: disc.issuer.clone(),
        client_id: "seed-client".to_string(),
        client_secret: None,
        scopes: vec!["read".to_string(), "write".to_string()],
        metadata: meta,
    };
    Authorised { server_id, slug, connection_id, row, server_url, user_id }
}

async fn conn_field(pg: &sqlx::PgPool, id: Uuid) -> (String, Option<String>, Option<String>) {
    use sqlx::Row;
    let r = sqlx::query("SELECT status, access_token_enc, refresh_token_enc FROM mcp_oauth_connections WHERE id=$1")
        .bind(id)
        .fetch_one(pg)
        .await
        .unwrap();
    (r.get("status"), r.get("access_token_enc"), r.get("refresh_token_enc"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::oauth_flow::{begin_authorize, complete_callback, register_dcr};

    // ── Flow helper for the authorize-leg + callback tests ────────────────────
    struct Flow {
        disc: ValidatedDiscovery,
        reg: DcrRegistration,
        server_id: Uuid,
        client_uuid: Uuid,
        authorize_url: String,
        callback_loc: String,
    }

    async fn drive_to_authorize(
        state: &AppState,
        pg: &sqlx::PgPool,
        mock: &Mock,
        user_id: Uuid,
        callback: &str,
    ) -> Flow {
        let server_url = format!("{}/mcp", mock.base);
        let (server_id, _slug) = seed_server(pg, &server_url).await;
        let disc = run_discovery(state, &admin_ctx(), &server_url, None).await.unwrap();
        let reg = register_dcr(&disc.metadata, callback, &disc.scopes_supported).await.unwrap();
        let client_uuid = seed_client(pg, server_id, &disc, &reg).await;
        let connection_id = seed_connection(pg, server_id, client_uuid, user_id).await;
        let authorize_url = begin_authorize(
            state,
            &admin_ctx(),
            &server_url,
            &client_row(client_uuid, &disc, &reg),
            Some(user_id),
            server_id,
            connection_id,
            None,
        )
        .await
        .unwrap();
        let resp = mock.browser().get(&authorize_url).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 302, "the mock /authorize redirects to the callback");
        let callback_loc = resp.headers().get("location").unwrap().to_str().unwrap().to_string();
        Flow { disc, reg, server_id, client_uuid, authorize_url, callback_loc }
    }

    // ── Discovery + authorize + DCR + refusals (the original 8) ───────────────

    #[tokio::test]
    async fn discovery_happy_path_validates() {
        let Some((state, _pg)) = app_state().await else {
            eprintln!("skip: DATABASE_URL unset");
            return;
        };
        let mock = spawn_mock(MockConfig::default(), "https://callback.test/api/mcp/oauth/callback".into()).await;
        let server_url = format!("{}/mcp", mock.base);
        let disc = run_discovery(&state, &admin_ctx(), &server_url, None).await.expect("discovery must succeed");
        assert_eq!(disc.issuer, mock.base, "issuer is the mock base");
        assert!(disc.s256_ok, "S256 advertised ⇒ ok");
        assert!(disc.dcr_available, "registration_endpoint present ⇒ DCR available");
        assert!(!mock.state.recorded().is_empty(), "the mock was actually hit over TLS");
    }

    #[tokio::test]
    async fn authorize_leg_carries_s256_and_resource_on_the_wire() {
        let Some((state, pg)) = app_state().await else {
            eprintln!("skip: DATABASE_URL unset");
            return;
        };
        let Some(user_id) = existing_user(&pg).await else {
            eprintln!("skip: no seeded user");
            return;
        };
        let callback = crate::mcp::oauth_flow::callback_url(&state);
        let mock = spawn_mock(MockConfig::default(), callback.clone()).await;
        let server_url = format!("{}/mcp", mock.base);
        let flow = drive_to_authorize(&state, &pg, &mock, user_id, &callback).await;

        let authz = mock.recorded_for("/authorize");
        let aq = &authz.first().expect("an /authorize request was recorded").query;
        assert_eq!(form_param(aq, "code_challenge_method").as_deref(), Some("S256"), "S256 on the wire");
        assert_eq!(
            form_param(aq, "resource").as_deref(),
            Some(server_url.as_str()),
            "resource on the wire, no trailing slash"
        );
        assert!(!server_url.ends_with('/'));
        assert!(flow.authorize_url.contains("code_challenge_method=S256"));
        cleanup(&pg, flow.server_id).await;
    }

    #[tokio::test]
    async fn dcr_registers_with_web_type_and_deletes_with_mgmt_token() {
        let Some((state, pg)) = app_state().await else {
            return;
        };
        let callback = crate::mcp::oauth_flow::callback_url(&state);
        let mock = spawn_mock(MockConfig::default(), callback.clone()).await;
        let server_url = format!("{}/mcp", mock.base);
        let (server_id, _slug) = seed_server(&pg, &server_url).await;

        let disc = run_discovery(&state, &admin_ctx(), &server_url, None).await.unwrap();
        let reg = register_dcr(&disc.metadata, &callback, &disc.scopes_supported).await.unwrap();

        let regs = mock.recorded_for("/register");
        let rb = &regs.first().expect("a /register POST was recorded").body;
        let body: serde_json::Value = serde_json::from_str(rb).unwrap();
        assert_eq!(body["application_type"], "web", "DCR sends application_type: web");
        assert_eq!(body["token_endpoint_auth_method"], "none");
        assert!(body["redirect_uris"].as_array().unwrap().iter().any(|u| u == &json!(callback)));

        let reg_uri = reg.registration_client_uri.clone().expect("registration_client_uri");
        let token = reg.registration_access_token.clone().expect("registration_access_token");
        let _ = crate::mcp::client::hardened_client().unwrap().delete(&reg_uri).bearer_auth(&token).send().await;
        let dels = mock.recorded_for("/register/");
        let del = dels.first().expect("an RFC 7592 DELETE was recorded");
        assert_eq!(del.method, "DELETE");
        assert_eq!(del.auth_header.as_deref(), Some(format!("Bearer {token}").as_str()), "DELETE carried the mgmt token");
        cleanup(&pg, server_id).await;
    }

    #[tokio::test]
    async fn undeclared_cross_origin_as_is_refused_declared_is_accepted() {
        let Some((state, _pg)) = app_state().await else {
            return;
        };
        let callback = "https://callback.test/api/mcp/oauth/callback".to_string();
        let as_mock = spawn_mock(MockConfig::default(), callback.clone()).await;
        let mut res_cfg = MockConfig::default();
        res_cfg.authorization_servers = Some(vec![as_mock.base.clone()]);
        let res_mock = spawn_mock(res_cfg, callback).await;
        let server_url = format!("{}/mcp", res_mock.base);

        let refused = run_discovery(&state, &admin_ctx(), &server_url, None).await;
        assert!(refused.is_err(), "an undeclared cross-origin AS must be refused");
        assert!(format!("{:?}", refused.err().unwrap()).contains("origin"));

        let ok = run_discovery(&state, &admin_ctx(), &server_url, Some(&as_mock.base)).await;
        assert!(ok.is_ok(), "declaring the AS origin admits it");
    }

    #[tokio::test]
    async fn cloud_metadata_endpoint_is_refused_even_when_declared() {
        let Some((state, _pg)) = app_state().await else {
            return;
        };
        let mut cfg = MockConfig::default();
        cfg.cloud_metadata_endpoints = true;
        let mock = spawn_mock(cfg, "https://callback.test/api/mcp/oauth/callback".into()).await;
        let server_url = format!("{}/mcp", mock.base);
        let refused = run_discovery(&state, &admin_ctx(), &server_url, Some("https://metadata.google.internal")).await;
        assert!(refused.is_err(), "a cloud-metadata endpoint must be refused even when declared");
        assert!(format!("{:?}", refused.err().unwrap()).contains("cloud"));
    }

    #[tokio::test]
    async fn missing_s256_is_flagged_by_our_check() {
        let Some((state, _pg)) = app_state().await else {
            return;
        };
        let mut cfg = MockConfig::default();
        cfg.advertise_s256 = false;
        let mock = spawn_mock(cfg, "https://callback.test/api/mcp/oauth/callback".into()).await;
        let server_url = format!("{}/mcp", mock.base);
        let disc = run_discovery(&state, &admin_ctx(), &server_url, None).await.expect("discovery succeeds; S256 gap reported");
        assert!(!disc.s256_ok, "a server without S256 must be flagged");
    }

    #[tokio::test]
    async fn callback_iss_mismatch_is_refused() {
        let Some((state, pg)) = app_state().await else {
            return;
        };
        let Some(user_id) = existing_user(&pg).await else {
            return;
        };
        let callback = crate::mcp::oauth_flow::callback_url(&state);
        for tweak in ["case", "slash"] {
            let mut cfg = MockConfig::default();
            cfg.iss_supported = true;
            cfg.append_iss = true;
            let mock = spawn_mock(cfg, callback.clone()).await;
            let bad_iss = match tweak {
                "case" => mock.base.to_uppercase(),
                _ => format!("{}/", mock.base),
            };
            let flow = drive_to_authorize(&state, &pg, &mock, user_id, &callback).await;
            let loc = reqwest::Url::parse(&flow.callback_loc).unwrap();
            let code = loc.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.into_owned()).unwrap();
            let st = loc.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.into_owned()).unwrap();
            let su = format!("{}/mcp", mock.base);
            let row = client_row(flow.client_uuid, &flow.disc, &flow.reg);
            let slug = String::new();
            let res = complete_callback(&state, &code, &st, Some(&bad_iss), move |_s, _c| async move {
                Ok((su, slug, row))
            })
            .await;
            assert!(res.is_err(), "a {tweak}-differing iss must be refused");
            assert!(format!("{:?}", res.err().unwrap()).contains("iss"));
            cleanup(&pg, flow.server_id).await;
        }
    }

    #[tokio::test]
    async fn callback_state_is_single_use() {
        let Some((state, pg)) = app_state().await else {
            return;
        };
        let Some(user_id) = existing_user(&pg).await else {
            return;
        };
        let callback = crate::mcp::oauth_flow::callback_url(&state);
        let mock = spawn_mock(MockConfig::default(), callback.clone()).await;
        let flow = drive_to_authorize(&state, &pg, &mock, user_id, &callback).await;
        let loc = reqwest::Url::parse(&flow.callback_loc).unwrap();
        let code = loc.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.into_owned()).unwrap();
        let st = loc.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.into_owned()).unwrap();

        let su = format!("{}/mcp", mock.base);
        let row1 = client_row(flow.client_uuid, &flow.disc, &flow.reg);
        let _ = complete_callback(&state, &code, &st, None, move |_s, _c| async move {
            Ok((su, String::new(), row1))
        })
        .await;

        let su2 = format!("{}/mcp", mock.base);
        let row2 = client_row(flow.client_uuid, &flow.disc, &flow.reg);
        let replay = complete_callback(&state, &code, &st, None, move |_s, _c| async move {
            Ok((su2, String::new(), row2))
        })
        .await;
        assert!(replay.is_err(), "replaying a used state must fail");
        assert!(format!("{:?}", replay.err().unwrap()).contains("unknown or expired"));
        cleanup(&pg, flow.server_id).await;
    }

    // ── Refresh leg (the three new tests) ─────────────────────────────────────

    /// Refresh happy path: an expired access token is refreshed automatically over TLS
    /// through OUR client, the new token is persisted, and (FLAG) we record whether the
    /// refresh carries an RFC 8707 `resource` param.
    #[tokio::test]
    async fn refresh_rotates_token_and_records_resource_presence() {
        let Some((state, pg)) = app_state().await else {
            eprintln!("skip: DATABASE_URL unset");
            return;
        };
        if existing_user(&pg).await.is_none() {
            eprintln!("skip: no seeded user");
            return;
        }
        let mock = spawn_mock(MockConfig::default(), "https://callback.test/api/mcp/oauth/callback".into()).await;
        let a = seed_authorised_connection(&state, &pg, &mock, "seed-refresh").await;

        let mut mgr = crate::mcp::oauth_flow::base_manager(
            &a.server_url,
            &a.row,
            &crate::mcp::oauth_flow::callback_url(&state),
        )
        .await
        .unwrap();
        mgr.set_credential_store(crate::mcp::oauth_store::PgCredentialStore::new(
            pg.clone(),
            a.connection_id,
            true,
        ));

        let tok = mgr.get_access_token().await.expect("refresh succeeds with no human step");
        assert_eq!(tok, "rotated-from-seed-refresh", "the new access token is returned");

        // Persisted: access rotated, still active.
        let (status, access_enc, _refresh_enc) = conn_field(&pg, a.connection_id).await;
        assert_eq!(status, "active");
        let access = crate::crypto::decrypt_at_rest(&access_enc.unwrap()).unwrap();
        assert_eq!(access, "rotated-from-seed-refresh", "new access token persisted");

        // On the wire: grant_type=refresh_token + the stored refresh token.
        let toks = mock.recorded_for("/token");
        let tb = &toks.first().expect("a /token refresh was recorded").body;
        assert_eq!(form_param(tb, "grant_type").as_deref(), Some("refresh_token"));
        assert_eq!(form_param(tb, "refresh_token").as_deref(), Some("seed-refresh"), "the stored refresh token was presented");

        // FLAG (answered in writing): rmcp's refresh_token (auth.rs:1287) does NOT add
        // an RFC 8707 `resource` param (only the code-exchange leg does). Record it - a
        // strict AS could reject or mis-scope a refresh for lack of it.
        let has_resource = form_param(tb, "resource").is_some();
        eprintln!("FINDING: refresh /token carries `resource`? {has_resource} (rmcp 1.7.0 omits it)");
        assert!(!has_resource, "rmcp does NOT put `resource` on the refresh leg (documented rmcp gap)");

        cleanup(&pg, a.server_id).await;
    }

    /// COALESCE: a refresh response WITHOUT a new refresh_token must leave the stored one
    /// intact while rotating the access token (providers legitimately omit it).
    #[tokio::test]
    async fn refresh_without_new_refresh_token_keeps_the_old_one() {
        let Some((state, pg)) = app_state().await else {
            return;
        };
        if existing_user(&pg).await.is_none() {
            return;
        }
        let mut cfg = MockConfig::default();
        cfg.refresh_omit_new_refresh = true;
        let mock = spawn_mock(cfg, "https://callback.test/api/mcp/oauth/callback".into()).await;
        let a = seed_authorised_connection(&state, &pg, &mock, "keep-me").await;

        let mut mgr = crate::mcp::oauth_flow::base_manager(
            &a.server_url,
            &a.row,
            &crate::mcp::oauth_flow::callback_url(&state),
        )
        .await
        .unwrap();
        mgr.set_credential_store(crate::mcp::oauth_store::PgCredentialStore::new(pg.clone(), a.connection_id, true));
        let _ = mgr.get_access_token().await.expect("refresh succeeds");

        let (_status, access_enc, refresh_enc) = conn_field(&pg, a.connection_id).await;
        let access = crate::crypto::decrypt_at_rest(&access_enc.unwrap()).unwrap();
        let refresh = crate::crypto::decrypt_at_rest(&refresh_enc.expect("refresh token retained")).unwrap();
        assert_eq!(access, "rotated-from-keep-me", "access token rotated");
        assert_eq!(refresh, "keep-me", "the old refresh token was KEPT (COALESCE), not nulled");

        cleanup(&pg, a.server_id).await;
    }

    /// Refresh failure through the REAL dispatch. NOTE (a finding, per the ТЗ's
    /// report-what-happens guidance): a failed refresh surfaces during the connection
    /// resolve inside `authorize_call` (gate 6), NOT at the tool-call reauth mapping
    /// (mcp/mod.rs:581-604). rmcp's `try_refresh_or_reauth` returns
    /// `AuthorizationRequired` WITHOUT calling `clear()`, so the connection is NOT
    /// auto-flipped to `reauth_required`; the caller gets a recoverable
    /// connection-error string. We assert the ACTUAL behaviour.
    #[tokio::test]
    async fn failed_refresh_is_recoverable_at_the_connection_gate() {
        let Some((state, pg)) = app_state().await else {
            return;
        };
        if existing_user(&pg).await.is_none() {
            return;
        }
        let mut cfg = MockConfig::default();
        cfg.refresh_invalid_grant = true;
        let mock = spawn_mock(cfg, "https://callback.test/api/mcp/oauth/callback".into()).await;
        let a = seed_authorised_connection(&state, &pg, &mock, "dead-refresh").await;

        // Dispatch a tool call as the connection's user; the connect path will attempt
        // the (failing) refresh.
        let ctx = AuthContext {
            user_id: Some(a.user_id),
            email: None,
            display_name: None,
            role: PlatformRole::User,
            break_glass: false,
            mfa_enroll_only: false,
        };
        // RBAC read on the server for this user.
        sqlx::query(
            "INSERT INTO access_grants (id, resource_type, resource_id, principal_type, principal_id, permission) \
             VALUES ($1,'mcp_server'::grant_resource_type,$2,'user'::principal_type,$3,'read'::permission) ON CONFLICT DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(a.server_id)
        .bind(a.user_id)
        .execute(&pg)
        .await
        .unwrap();

        let grants = crate::mcp::parse_grants(&[format!("{}__*", a.slug)]);
        let name = format!("{}__anytool", a.slug);
        let result = crate::mcp::dispatch(&state, &ctx, &grants, Uuid::now_v7(), &name, &json!({}), false).await;

        // Observe + assert actual behaviour.
        let (status, _a, _r) = conn_field(&pg, a.connection_id).await;
        eprintln!(
            "FINDING: failed refresh via dispatch -> result={:?}, connection status={status}",
            result.as_ref().map(|s| s.chars().take(60).collect::<String>())
        );
        // The failure is recoverable (Ok(String) starting with "error:"), not an Err -
        // the model can recover.
        let body = result.expect("a failed refresh must be recoverable, not an Err");
        assert!(body.starts_with("error:"), "recoverable message: {body}");

        // FINDING (asserted, not just printed): a failed refresh does NOT flip the
        // connection to `reauth_required` and emits NO `mcp.oauth.reauth_required` audit.
        // rmcp's `try_refresh_or_reauth` returns `AuthorizationRequired` without calling
        // `clear()`, and the failure surfaces during the connection resolve (gate 6 of
        // `authorize_call`), before the tool-call reauth mapping (mcp/mod.rs:581-604) -
        // which is reached only by a tool-call insufficient-scope, a different trigger.
        // This diverges from the ТЗ's assumed outcome; asserted here rather than forced.
        assert_eq!(status, "active", "a failed refresh leaves the connection 'active' (not reauth_required)");
        let reauth_audits: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events WHERE action_type='mcp.oauth.reauth_required' AND resource_id=$1",
        )
        .bind(a.server_id)
        .fetch_one(&pg)
        .await
        .unwrap();
        assert_eq!(reauth_audits, 0, "a refresh failure emits no reauth_required audit");

        cleanup(&pg, a.server_id).await;
    }
}
