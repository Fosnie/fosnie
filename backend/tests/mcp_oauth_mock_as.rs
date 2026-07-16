//! Mock authorisation-server suite for the MCP OAuth flow. Exercises discovery, the
//! authorize leg, DCR, and every refusal fork against a LOCAL self-signed TLS
//! authorisation server, with no internet, no OAuth app, no `PAI_E2E`, no ML. Only
//! `DATABASE_URL` + Redis are needed.
//!
//! The whole file is compiled only under `--features test-mocks`, which enables a
//! hook in `mcp::client::build_hardened_client` that trusts one extra root cert (the
//! mock's). That hook can only ADD a trusted root; it cannot weaken any check.
//!
//! LIMITATION (the /token leg): rmcp 1.7.0's `exchange_code_for_token`
//! (`transport/auth.rs:1165`) builds its OWN reqwest client instead of the one we
//! inject via `with_client`. That client uses reqwest's default trust (the platform
//! verifier on Windows), which does not trust our self-signed root, so the token POST
//! cannot be driven through rmcp against a self-signed mock without importing the cert
//! into the OS trust store. Consequently the token-exchange, refresh, and
//! refresh-failure/reauth wire tests are NOT included here. The `resource`/no-trailing-
//! slash property is nonetheless proven on the `/authorize` leg, which is the identical
//! `base_url.to_string()` code path rmcp uses for `/token`. (rmcp's internal token
//! client does set `redirect::Policy::none()` independently, so the no-redirect SSRF
//! protection on the token leg still holds; it just cannot be asserted here.)
#![cfg(feature = "test-mocks")]

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use serde_json::json;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::mcp::oauth_flow::{
    begin_authorize, complete_callback, register_dcr, run_discovery, OAuthClientRow, ValidatedDiscovery,
    DcrRegistration,
};
use fosnie_backend::state::AppState;
use fosnie_backend::{cache, db};

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
}

impl Default for MockConfig {
    fn default() -> Self {
        MockConfig {
            advertise_s256: true,
            iss_supported: false,
            authorization_servers: None,
            cloud_metadata_endpoints: false,
            append_iss: false,
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
    // Echo back the state; a fixed code. The test acts as the browser and reads Location.
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

async fn token(State(s): State<St>, headers: HeaderMap, body: Bytes) -> Response {
    // Recorded for completeness; the /token leg is not driven through rmcp here (see
    // the module note), so no per-mode behaviour is needed.
    s.record("POST", "/token", "", &body, &headers);
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

/// Minimal percent-encoding for the few chars an issuer URL carries (`:` and `/`).
fn urlencoding_min(s: &str) -> String {
    s.replace(':', "%3A").replace('/', "%2F")
}

// ── Harness: spawn a TLS mock, install its root, build AppState ───────────────

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
    /// A client that acts as the user's browser for the /authorize leg: trusts the
    /// mock cert, and does NOT follow redirects (so we can read the callback Location).
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
/// mock server across every test must present this same cert — which they all can, as
/// they all serve on `127.0.0.1`. Also lets one test run two mock servers (the
/// cross-origin case) that both verify.
fn shared_cert() -> &'static SharedCert {
    static CERT: std::sync::OnceLock<SharedCert> = std::sync::OnceLock::new();
    CERT.get_or_init(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.subject_alt_names =
            vec![rcgen::SanType::IpAddress("127.0.0.1".parse().unwrap())];
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let cert_pem = cert.pem();
        let key_pem = key.serialize_pem();
        let reqwest_cert = reqwest::Certificate::from_pem(cert_pem.as_bytes()).unwrap();
        fosnie_backend::mcp::client::install_test_root(reqwest_cert.clone());
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

    // Bind first to learn the port, then build the router with a known base URL.
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

    let cfg = axum_server::tls_rustls::RustlsConfig::from_pem(
        cert_pem.into_bytes(),
        key_pem.into_bytes(),
    )
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
    // The MCP connector must be enabled or guard_egress refuses at the first gate.
    let _ = fosnie_backend::config::runtime::set(
        &pg,
        "integration.mcp.enabled",
        "true",
        fosnie_backend::config::runtime::ConfigValueType::Bool,
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
        .map(|t| fosnie_backend::crypto::encrypt_at_rest(t).unwrap());
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

/// Parse `key`'s (decoded) value from a raw query/form string.
fn form_param(raw: &str, key: &str) -> Option<String> {
    let dummy = format!("http://x/?{raw}");
    reqwest::Url::parse(&dummy)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

async fn cleanup(pg: &sqlx::PgPool, server_id: Uuid) {
    let _ = sqlx::query("DELETE FROM mcp_oauth_connections WHERE mcp_server_id=$1").bind(server_id).execute(pg).await;
    let _ = sqlx::query("DELETE FROM mcp_oauth_clients WHERE mcp_server_id=$1").bind(server_id).execute(pg).await;
    let _ = sqlx::query("DELETE FROM mcp_servers WHERE id=$1").bind(server_id).execute(pg).await;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Drive discovery → DCR → begin_authorize → the browser /authorize GET, returning
/// everything needed for the leg assertions. (The /token leg is not driveable through
/// rmcp against a self-signed mock — see the module-level note.)
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

/// D3.1 + D3.2 (authorize leg): the `/authorize` request rmcp builds carries
/// `code_challenge_method=S256` and a `resource` param equal to the server URL with NO
/// trailing slash — proven both in the returned URL AND in what the mock received.
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
    let callback = fosnie_backend::mcp::oauth_flow::callback_url(&state);
    let mock = spawn_mock(MockConfig::default(), callback.clone()).await;
    let server_url = format!("{}/mcp", mock.base);
    let flow = drive_to_authorize(&state, &pg, &mock, user_id, &callback).await;

    // From what the mock actually received on /authorize:
    let authz = mock.recorded_for("/authorize");
    let aq = &authz.first().expect("an /authorize request was recorded").query;
    assert_eq!(form_param(aq, "code_challenge_method").as_deref(), Some("S256"), "S256 on the wire");
    assert_eq!(
        form_param(aq, "resource").as_deref(),
        Some(server_url.as_str()),
        "resource on the wire, no trailing slash (would be `/` if the URL were path-less)"
    );
    assert!(!server_url.ends_with('/'), "sanity: the server URL has a path, so a trailing slash would be a real bug");
    // And in the URL rmcp handed back:
    assert!(flow.authorize_url.contains("code_challenge_method=S256"));

    cleanup(&pg, flow.server_id).await;
}

/// D3.3: dynamic client registration — our RFC 7591 POST carries `application_type:
/// "web"` + the callback, and the RFC 7592 management credentials round-trip: a DELETE
/// to the registration URI carries the exact management token we were issued.
#[tokio::test]
async fn dcr_registers_with_web_type_and_deletes_with_mgmt_token() {
    let Some((state, pg)) = app_state().await else {
        return;
    };
    let callback = fosnie_backend::mcp::oauth_flow::callback_url(&state);
    let mock = spawn_mock(MockConfig::default(), callback.clone()).await;
    let server_url = format!("{}/mcp", mock.base);
    let (server_id, _slug) = seed_server(&pg, &server_url).await;

    let disc = run_discovery(&state, &admin_ctx(), &server_url, None).await.unwrap();
    let reg = register_dcr(&disc.metadata, &callback, &disc.scopes_supported).await.unwrap();

    // The registration POST as the mock saw it.
    let regs = mock.recorded_for("/register");
    let rb = &regs.first().expect("a /register POST was recorded").body;
    let body: serde_json::Value = serde_json::from_str(rb).unwrap();
    assert_eq!(body["application_type"], "web", "DCR sends application_type: web");
    assert_eq!(body["token_endpoint_auth_method"], "none");
    assert!(body["redirect_uris"].as_array().unwrap().iter().any(|u| u == &json!(callback)), "callback in redirect_uris");

    // The management credentials came back and can be used for the RFC 7592 DELETE.
    let reg_uri = reg.registration_client_uri.clone().expect("registration_client_uri returned");
    let token = reg.registration_access_token.clone().expect("registration_access_token returned");
    let _ = fosnie_backend::mcp::client::hardened_client()
        .unwrap()
        .delete(&reg_uri)
        .bearer_auth(&token)
        .send()
        .await;
    let dels = mock.recorded_for("/register/");
    let del = dels.first().expect("an RFC 7592 DELETE was recorded");
    assert_eq!(del.method, "DELETE");
    assert_eq!(del.auth_header.as_deref(), Some(format!("Bearer {token}").as_str()), "DELETE carried the exact mgmt token");

    cleanup(&pg, server_id).await;
}

/// D3.6 (the single most valuable): an authorisation server on a DIFFERENT origin than
/// the MCP resource is refused unless the admin declares its origin. Same-origin is the
/// default trust; a cross-origin issuer must be named to be trusted.
#[tokio::test]
async fn undeclared_cross_origin_as_is_refused_declared_is_accepted() {
    let Some((state, _pg)) = app_state().await else {
        return;
    };
    let callback = "https://callback.test/api/mcp/oauth/callback".to_string();
    // The AS lives on port B (its own server); the resource on port A points its PRM at B.
    let as_mock = spawn_mock(MockConfig::default(), callback.clone()).await;
    let mut res_cfg = MockConfig::default();
    res_cfg.authorization_servers = Some(vec![as_mock.base.clone()]);
    let res_mock = spawn_mock(res_cfg, callback).await;
    let server_url = format!("{}/mcp", res_mock.base);

    // Undeclared cross-origin AS ⇒ refused.
    let refused = run_discovery(&state, &admin_ctx(), &server_url, None).await;
    assert!(refused.is_err(), "an undeclared cross-origin AS must be refused");
    assert!(format!("{:?}", refused.err().unwrap()).contains("origin"), "refusal names the origin mismatch");

    // Declaring the AS origin ⇒ accepted.
    let ok = run_discovery(&state, &admin_ctx(), &server_url, Some(&as_mock.base)).await;
    assert!(ok.is_ok(), "declaring the AS origin admits it");
}

/// D3.7: an AS whose metadata points at a cloud instance-metadata host is refused even
/// when its origin is declared (the cloud-metadata check is unconditional).
#[tokio::test]
async fn cloud_metadata_endpoint_is_refused_even_when_declared() {
    let Some((state, pg)) = app_state().await else {
        return;
    };
    let _ = &pg;
    let mut cfg = MockConfig::default();
    cfg.cloud_metadata_endpoints = true;
    let mock = spawn_mock(cfg, "https://callback.test/api/mcp/oauth/callback".into()).await;
    let server_url = format!("{}/mcp", mock.base);

    let refused = run_discovery(
        &state,
        &admin_ctx(),
        &server_url,
        Some("https://metadata.google.internal"),
    )
    .await;
    assert!(refused.is_err(), "a cloud-metadata endpoint must be refused even when declared");
    assert!(format!("{:?}", refused.err().unwrap()).contains("cloud"), "refusal names the cloud-metadata host");
}

/// D3.8: metadata that does not advertise S256 is caught by OUR check — rmcp is silent
/// about it during discovery, so `s256_ok` is false (approval refuses on that).
#[tokio::test]
async fn missing_s256_is_flagged_by_our_check() {
    let Some((state, pg)) = app_state().await else {
        return;
    };
    let _ = &pg;
    let mut cfg = MockConfig::default();
    cfg.advertise_s256 = false;
    let mock = spawn_mock(cfg, "https://callback.test/api/mcp/oauth/callback".into()).await;
    let server_url = format!("{}/mcp", mock.base);

    let disc = run_discovery(&state, &admin_ctx(), &server_url, None)
        .await
        .expect("discovery itself succeeds; the S256 gap is reported, not fatal");
    assert!(!disc.s256_ok, "a server without S256 must be flagged (approval refuses on this)");
}

/// D3.9: the callback `iss` is checked against the approved issuer by EXACT string
/// comparison — a case-differing and a trailing-slash-differing iss both fail.
#[tokio::test]
async fn callback_iss_mismatch_is_refused() {
    let Some((state, pg)) = app_state().await else {
        return;
    };
    let Some(user_id) = existing_user(&pg).await else {
        return;
    };
    let callback = fosnie_backend::mcp::oauth_flow::callback_url(&state);

    for tweak in ["case", "slash"] {
        let mut cfg = MockConfig::default();
        cfg.iss_supported = true;
        cfg.append_iss = true;
        // The mock is at `base`; the approved issuer is `base`. Append a DIFFERENT iss.
        let mock = spawn_mock(cfg, callback.clone()).await;
        let bad_iss = match tweak {
            "case" => mock.base.to_uppercase(),
            _ => format!("{}/", mock.base),
        };
        // The mock's callback appends its own (correct) iss; we ignore it and pass a
        // DIFFERENT iss to complete_callback to prove the exact-match rejection.
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
        assert!(format!("{:?}", res.err().unwrap()).contains("iss"), "refusal names the iss");
        cleanup(&pg, flow.server_id).await;
    }
}

/// D3.11: the parked flow state is single-use (Redis GETDEL). The first callback
/// consumes it (even though the token leg then fails); replaying the same state finds
/// nothing.
#[tokio::test]
async fn callback_state_is_single_use() {
    let Some((state, pg)) = app_state().await else {
        return;
    };
    let Some(user_id) = existing_user(&pg).await else {
        return;
    };
    let callback = fosnie_backend::mcp::oauth_flow::callback_url(&state);
    let mock = spawn_mock(MockConfig::default(), callback.clone()).await;
    let flow = drive_to_authorize(&state, &pg, &mock, user_id, &callback).await;
    let loc = reqwest::Url::parse(&flow.callback_loc).unwrap();
    let code = loc.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.into_owned()).unwrap();
    let st = loc.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.into_owned()).unwrap();

    let su = format!("{}/mcp", mock.base);
    let row1 = client_row(flow.client_uuid, &flow.disc, &flow.reg);
    // First use consumes the flow record (the token leg then fails, which is fine).
    let _ = complete_callback(&state, &code, &st, None, move |_s, _c| async move {
        Ok((su, String::new(), row1))
    })
    .await;

    // Replay the same state ⇒ the record is gone.
    let su2 = format!("{}/mcp", mock.base);
    let row2 = client_row(flow.client_uuid, &flow.disc, &flow.reg);
    let replay = complete_callback(&state, &code, &st, None, move |_s, _c| async move {
        Ok((su2, String::new(), row2))
    })
    .await;
    assert!(replay.is_err(), "replaying a used state must fail");
    assert!(
        format!("{:?}", replay.err().unwrap()).contains("unknown or expired"),
        "replay is refused as an unknown/expired state, not for another reason"
    );
    cleanup(&pg, flow.server_id).await;
}


/// Derisk: discovery succeeds against the mock over real TLS — the PRM + AS metadata
/// are fetched, every endpoint validates, and S256/DCR are reported.
#[tokio::test]
async fn discovery_happy_path_validates() {
    let Some((state, _pg)) = app_state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let mock = spawn_mock(MockConfig::default(), "https://callback.test/api/mcp/oauth/callback".into()).await;
    let server_url = format!("{}/mcp", mock.base);

    let disc = fosnie_backend::mcp::oauth_flow::run_discovery(&state, &admin_ctx(), &server_url, None)
        .await
        .expect("discovery must succeed against the mock");

    assert_eq!(disc.issuer, mock.base, "issuer is the mock base");
    assert!(disc.s256_ok, "S256 advertised ⇒ ok");
    assert!(disc.dcr_available, "registration_endpoint present ⇒ DCR available");
    // The well-known PRM + AS metadata were actually fetched over TLS (the 401 probe
    // to /mcp was recorded).
    assert!(!mock.recorded_for("/*").is_empty() || !mock.state.recorded().is_empty());
}
