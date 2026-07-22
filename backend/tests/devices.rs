//! Desktop device pairing and the device-token authentication of the native
//! surface.
//!
//! A header-driven fake `AuthProvider` yields a chosen user for the
//! session-authenticated routes (pairing-code creation, device management); it
//! fails when no `X-Test-User` header is present, which is exactly the condition
//! under which the device-token fallback in the `AuthUser` extractor runs. The
//! pairing endpoint itself is public, and the native surface authenticates a
//! device token as it would in production.
//!
//! Needs Postgres (:5433) + Redis; skips if DATABASE_URL is unset.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::request::Parts;
use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::AppError;
use fosnie_backend::ext::AuthProvider;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::{cache, db, http};

/// Authenticates as whoever `X-Test-User` names; without that header it fails,
/// which is what lets a device token fall through to its own resolution.
/// `X-Test-Enrol-Only` marks the session as one that exists only to finish
/// setting up a second factor.
struct HeaderAuthProvider;

#[async_trait]
impl AuthProvider for HeaderAuthProvider {
    async fn authenticate(&self, parts: &mut Parts, _state: &AppState) -> Result<AuthContext, AppError> {
        let uid = parts
            .headers
            .get("x-test-user")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or_else(|| AppError::Unauthorized("no test user".into()))?;
        let enrol_only = parts.headers.get("x-test-enrol-only").is_some();
        Ok(AuthContext {
            user_id: Some(uid),
            email: None,
            display_name: None,
            role: PlatformRole::User,
            break_glass: false,
            mfa_enroll_only: enrol_only,
        })
    }
}

async fn mk_user(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'device', $2, 'user')")
        .bind(id)
        .bind(format!("device-{}@local.test", id.simple()))
        .execute(pg)
        .await
        .unwrap();
    id
}

async fn mk_admin(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'admin', $2, 'client_admin')")
        .bind(id)
        .bind(format!("admin-{}@local.test", id.simple()))
        .execute(pg)
        .await
        .unwrap();
    id
}

/// Mint an ordinary application (`kind='api'`) key straight into the table, for
/// the cross-surface separation checks.
async fn mk_api_key(pg: &sqlx::PgPool, user_id: Uuid) -> String {
    let (token, hash, prefix) = fosnie_backend::auth::api_key::mint();
    sqlx::query(
        "INSERT INTO api_keys (id, user_id, name, token_hash, display_prefix) \
         VALUES ($1, $2, 'test', $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(hash)
    .bind(prefix)
    .execute(pg)
    .await
    .unwrap();
    token
}

async fn setup() -> Option<(AppState, sqlx::PgPool, u16)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into();
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_auth(Arc::new(HeaderAuthProvider))
        .build();
    let app = http::router(state.clone(), None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((state, pg, port))
}

/// `features.public_api` gates the compatibility surface. One case needs it on
/// (to prove a device token is refused there for the right reason, not because
/// the surface is absent); it is a deployment-wide row, so that case takes the
/// switch exclusively while the rest share it.
static SURFACE: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

async fn set_public_api(pg: &sqlx::PgPool, on: bool) {
    let v = if on { "true" } else { "false" };
    sqlx::query(
        "INSERT INTO config_settings (key, value, value_type, scope) \
         VALUES ('features.public_api', $1, 'bool', 'global') \
         ON CONFLICT (key) DO UPDATE SET value = $1",
    )
    .bind(v)
    .execute(pg)
    .await
    .unwrap();
}

/// Mint a pairing code from `user`'s session. Returns the raw HTTP response so a
/// case can assert its status.
async fn request_code(api: &reqwest::Client, base: &str, user: Uuid) -> reqwest::Response {
    api.post(format!("{base}/api/me/devices/pairing-code"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
}

/// A per-caller synthetic client IP. The pairing endpoint is rate limited per
/// IP, and every case runs against one shared Redis in one process, so without a
/// distinct address one case's attempts would exhaust the bucket for the next.
/// A fresh value per call keeps the buckets from colliding.
fn fresh_ip() -> String {
    Uuid::now_v7().to_string()
}

/// Redeem a code from a given client IP, returning the raw response.
async fn redeem(
    api: &reqwest::Client,
    base: &str,
    code: &str,
    platform: &str,
    ip: &str,
) -> reqwest::Response {
    api.post(format!("{base}/api/device/pair"))
        .header("x-forwarded-for", ip)
        .json(&serde_json::json!({ "code": code, "name": "Work laptop", "platform": platform }))
        .send()
        .await
        .unwrap()
}

/// The happy-path pairing: mint a code, redeem it, return `(device_id, token)`.
async fn pair(api: &reqwest::Client, base: &str, user: Uuid) -> (String, String) {
    let code: serde_json::Value = request_code(api, base, user).await.json().await.unwrap();
    let code = code["code"].as_str().expect("code minted").to_string();
    let paired: serde_json::Value =
        redeem(api, base, &code, "macos", &fresh_ip()).await.json().await.unwrap();
    (
        paired["device_id"].as_str().expect("device id").to_string(),
        paired["token"].as_str().expect("token once").to_string(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_single_use_and_surface_separation() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    set_public_api(&pg, true).await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;

    // A code, minted and redeemed once.
    let code: serde_json::Value = request_code(&api, &base, user).await.json().await.unwrap();
    let code = code["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 8, "eight characters");
    let ip = fresh_ip();

    let paired = redeem(&api, &base, &code, "windows", &ip).await;
    assert_eq!(paired.status(), 201);
    let paired: serde_json::Value = paired.json().await.unwrap();
    let token = paired["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("sk-fosnie-"), "recognisable token");

    // The same code a second time is refused, indistinguishably (404).
    let again = redeem(&api, &base, &code, "linux", &ip).await;
    assert_eq!(again.status(), 404, "a code redeems once");

    // The device token drives the native surface as the owner.
    let who = api
        .get(format!("{base}/api/whoami"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(who.status(), 200);
    let who: serde_json::Value = who.json().await.unwrap();
    assert_eq!(who["user_id"], user.to_string(), "as the paired owner");

    // But not the compatibility surface: a device token is not an application
    // key. This is the regression guard for the kind filter — without it, this
    // token would authenticate here with full rights.
    let v1 = api
        .get(format!("{base}/v1/models"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(v1.status(), 401, "a device token is refused on the compatibility surface");

    // And the converse: an application key is refused on the native surface.
    let apikey = mk_api_key(&pg, user).await;
    let native = api
        .get(format!("{base}/api/whoami"))
        .header("authorization", format!("Bearer {apikey}"))
        .send()
        .await
        .unwrap();
    assert_eq!(native.status(), 401, "an application key is refused on the native surface");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_code_is_refused() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;

    let code: serde_json::Value = request_code(&api, &base, user).await.json().await.unwrap();
    let code = code["code"].as_str().unwrap().to_string();
    // Age it past its life.
    sqlx::query("UPDATE device_pairing_codes SET expires_at = now() - interval '1 hour' WHERE user_id = $1")
        .bind(user)
        .execute(&pg)
        .await
        .unwrap();
    let res = redeem(&api, &base, &code, "linux", &fresh_ip()).await;
    assert_eq!(res.status(), 404, "an expired code pairs nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrol_only_session_cannot_create_a_code() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;

    let res = api
        .post(format!("{base}/api/me/devices/pairing-code"))
        .header("x-test-user", user.to_string())
        .header("x-test-enrol-only", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403, "a half-enrolled session cannot pair a device");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_cannot_create_a_pairing_code() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let (_device_id, token) = pair(&api, &base, user).await;

    // A device token authenticates the route (it is the owner), but pairing a
    // further device from a device is refused: pairing starts from the web.
    let res = api
        .post(format!("{base}/api/me/devices/pairing-code"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403, "a device may not enrol further devices");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_creation_is_rate_limited() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;

    // Five in the window are allowed; the sixth is throttled.
    for _ in 0..5 {
        assert_eq!(request_code(&api, &base, user).await.status(), 200);
    }
    assert_eq!(
        request_code(&api, &base, user).await.status(),
        429,
        "the sixth code request in the window is refused"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_attempts_are_rate_limited_per_ip() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let ip = fresh_ip();

    // Ten redemptions from one address are allowed (each a miss → 404); the
    // eleventh is throttled before it is even checked.
    let code: serde_json::Value = request_code(&api, &base, user).await.json().await.unwrap();
    let code = code["code"].as_str().unwrap().to_string();
    for _ in 0..10 {
        let s = redeem(&api, &base, "AAAAAAAA", "linux", &ip).await.status();
        assert_eq!(s, 404, "a wrong code is a flat not-found");
    }
    // The real code now cannot get through from this address: the guard runs
    // ahead of the check, so even a valid code is refused.
    assert_eq!(
        redeem(&api, &base, &code, "linux", &ip).await.status(),
        429,
        "the eleventh attempt from one address is throttled"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revocation_is_immediate_and_kills_the_token() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let (device_id, token) = pair(&api, &base, user).await;

    // It works, then the owner signs it out.
    assert_eq!(
        api.get(format!("{base}/api/whoami"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let del = api
        .delete(format!("{base}/api/me/devices/{device_id}"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);

    // The very next request with the same token is refused, without waiting for
    // anything to expire.
    assert_eq!(
        api.get(format!("{base}/api/whoami"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap()
            .status(),
        401,
        "the token stops working the moment the device is withdrawn"
    );
    // The token row itself is revoked, not merely shadowed by the device.
    let revoked: bool = sqlx::query_scalar(
        "SELECT revoked_at IS NOT NULL FROM api_keys WHERE device_id = $1",
    )
    .bind(Uuid::parse_str(&device_id).unwrap())
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(revoked, "the device's token is revoked with it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_users_device_cannot_be_revoked() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let owner = mk_user(&pg).await;
    let other = mk_user(&pg).await;
    let (device_id, token) = pair(&api, &base, owner).await;

    // Answered as though it were not there, and the device still works.
    let del = api
        .delete(format!("{base}/api/me/devices/{device_id}"))
        .header("x-test-user", other.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
    assert_eq!(
        api.get(format!("{base}/api/whoami"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "someone else's revoke did nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_user_cascades_to_devices_and_tokens() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let (device_id, _token) = pair(&api, &base, user).await;
    let did = Uuid::parse_str(&device_id).unwrap();

    sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(&pg).await.unwrap();

    let devices: i64 = sqlx::query_scalar("SELECT count(*) FROM devices WHERE id = $1")
        .bind(did)
        .fetch_one(&pg)
        .await
        .unwrap();
    let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys WHERE device_id = $1")
        .bind(did)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(devices, 0, "the device is gone with its owner");
    assert_eq!(tokens, 0, "and so is its token");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_token_cannot_reach_super_admin() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let (_device_id, token) = pair(&api, &base, user).await;

    // The break-glass surface reads its own header and never touches the auth
    // provider, so a device token buys nothing there.
    let res = api
        .get(format!("{base}/api/admin/ping"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert!(
        res.status() == 401 || res.status() == 403,
        "the super-admin surface is unreachable with a device token (got {})",
        res.status()
    );
}

// ---- Sensitive-write fences ---------------------------------------------
// A device carries its owner's rights, but a stolen device token must not be
// able to escalate past the device model. These writes require a web session.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_token_cannot_mint_an_api_key() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    set_public_api(&pg, true).await; // on, so it is the fence not the feature gate
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let (_device_id, token) = pair(&api, &base, user).await;

    let res = api
        .post(format!("{base}/api/me/api-keys"))
        .header("authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "name": "persist" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403, "a device cannot mint a key that would outlive it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_token_cannot_write_a_provider() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let (_device_id, token) = pair(&api, &base, user).await;

    let res = api
        .post(format!("{base}/api/me/providers/llm"))
        .header("authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "label": "evil", "base_url": "https://attacker.example", "model": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403, "a device cannot redirect the owner's model traffic");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_token_cannot_delete_the_account() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let (_device_id, token) = pair(&api, &base, user).await;

    let res = api
        .delete(format!("{base}/api/me/account"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403, "a device cannot delete the account");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_token_cannot_revoke_another_users_device() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    // The device belongs to an administrator, so the fence — not a missing
    // permission — is what refuses the cross-user revoke.
    let admin = mk_admin(&pg).await;
    let (_admin_device, token) = pair(&api, &base, admin).await;
    let victim = mk_user(&pg).await;
    let (victim_device, _vt) = pair(&api, &base, victim).await;

    let res = api
        .delete(format!("{base}/api/admin/users/{victim}/devices/{victim_device}"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403, "a device cannot sign out another user's machine");
    // The victim's device is untouched.
    let revoked: bool = sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM devices WHERE id = $1")
        .bind(Uuid::parse_str(&victim_device).unwrap())
        .fetch_one(&pg)
        .await
        .unwrap();
    assert!(!revoked, "the target device stays live");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_token_cannot_revoke_another_users_api_key() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    set_public_api(&pg, true).await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    // The device belongs to an administrator, so the fence — not a missing
    // permission — is what refuses the cross-user key revoke.
    let admin = mk_admin(&pg).await;
    let (_admin_device, token) = pair(&api, &base, admin).await;
    let victim = mk_user(&pg).await;
    let _victim_key = mk_api_key(&pg, victim).await;
    let key_id: Uuid = sqlx::query_scalar("SELECT id FROM api_keys WHERE user_id = $1 AND kind = 'api'")
        .bind(victim)
        .fetch_one(&pg)
        .await
        .unwrap();

    let res = api
        .delete(format!("{base}/api/admin/users/{victim}/api-keys/{key_id}"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403, "a device cannot revoke another user's key");
    let revoked: bool = sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM api_keys WHERE id = $1")
        .bind(key_id)
        .fetch_one(&pg)
        .await
        .unwrap();
    assert!(!revoked, "the target key stays live");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_can_sign_itself_out() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let (device_id, token) = pair(&api, &base, user).await;

    // Own-device sign-out stays open — that is the point of a trusted device.
    let res = api
        .delete(format!("{base}/api/me/devices/{device_id}"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204, "a device may sign itself out");
    // And the token is dead on the next request.
    assert_eq!(
        api.get(format!("{base}/api/whoami"))
            .header("authorization", format!("Bearer {token}"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
}

// ---- WebSocket + origin provenance --------------------------------------

async fn device_ticket(api: &reqwest::Client, base: &str, token: &str) -> String {
    let ticket: serde_json::Value = api
        .post(format!("{base}/api/ws-ticket"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    ticket["ticket"].as_str().expect("ticket minted").to_string()
}

async fn next_json<S>(socket: &mut S) -> Option<serde_json::Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(Ok(msg)) = socket.next().await {
        if let Message::Text(t) = msg {
            return serde_json::from_str(&t).ok();
        }
    }
    None
}

/// Open a device socket, send one message, and return the created chat id and
/// the socket's resume token (from the greeting). The turn's generation may fail
/// without an ML service — the chat row is created before any of that, which is
/// all these cases check.
async fn send_and_capture(port: u16, ticket: &str) -> (String, String) {
    let (mut socket, _) = connect_async(format!("ws://127.0.0.1:{port}/ws?ticket={ticket}"))
        .await
        .expect("ws connect");
    let hello = next_json(&mut socket).await.expect("hello");
    assert_eq!(hello["type"], "hello");
    let resume = hello["resume_token"].as_str().unwrap_or_default().to_string();

    socket
        .send(Message::Text(
            serde_json::json!({ "type": "chat.send", "content": "hello from a test" })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();

    // The chat is created (and announced) before the turn does any real work.
    let created = timeout(Duration::from_secs(15), async {
        loop {
            let frame = next_json(&mut socket).await.expect("a frame arrives");
            if frame["type"] == "chat.created" {
                return frame["chat_id"].as_str().unwrap().to_string();
            }
        }
    })
    .await
    .expect("chat.created arrives");
    (created, resume)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_desktop_socket_marks_its_chats_desktop() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let (_device_id, token) = pair(&api, &base, user).await;

    let ticket = device_ticket(&api, &base, &token).await;
    let (chat_id, _resume) = send_and_capture(port, &ticket).await;

    let origin: String = sqlx::query_scalar("SELECT origin FROM chats WHERE id = $1")
        .bind(Uuid::parse_str(&chat_id).unwrap())
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(origin, "desktop", "a chat from a device socket is desktop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_web_socket_marks_its_chats_web() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;

    // A session ticket (no device token), the ordinary browser path.
    let ticket: serde_json::Value = api
        .post(format!("{base}/api/ws-ticket"))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ticket = ticket["ticket"].as_str().unwrap().to_string();
    let (chat_id, _resume) = send_and_capture(port, &ticket).await;

    let origin: String = sqlx::query_scalar("SELECT origin FROM chats WHERE id = $1")
        .bind(Uuid::parse_str(&chat_id).unwrap())
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(origin, "web", "a chat from a browser socket is web");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_origin_survives_a_reconnect() {
    let Some((_state, pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let _shared = SURFACE.read().await;
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    let user = mk_user(&pg).await;
    let (_device_id, token) = pair(&api, &base, user).await;

    // First connect via the ticket, capture the resume token it hands back.
    let ticket = device_ticket(&api, &base, &token).await;
    let (_first_chat, resume) = send_and_capture(port, &ticket).await;
    assert!(!resume.is_empty(), "a resume token is issued");

    // Reconnect with the resume token (no fresh ticket) and start a new chat: the
    // desktop provenance must have survived the Redis hop.
    let (chat_id, _resume2) = send_and_capture_resume(port, &resume).await;
    let origin: String = sqlx::query_scalar("SELECT origin FROM chats WHERE id = $1")
        .bind(Uuid::parse_str(&chat_id).unwrap())
        .fetch_one(&pg)
        .await
        .unwrap();
    assert_eq!(origin, "desktop", "a reconnect stays a desktop socket");
}

/// Like `send_and_capture`, but reconnecting with a resume token.
async fn send_and_capture_resume(port: u16, resume: &str) -> (String, String) {
    let (mut socket, _) = connect_async(format!("ws://127.0.0.1:{port}/ws?resume={resume}"))
        .await
        .expect("ws reconnect");
    let hello = next_json(&mut socket).await.expect("hello");
    assert_eq!(hello["type"], "hello");
    let resume2 = hello["resume_token"].as_str().unwrap_or_default().to_string();
    socket
        .send(Message::Text(
            serde_json::json!({ "type": "chat.send", "content": "after reconnect" })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let created = timeout(Duration::from_secs(15), async {
        loop {
            let frame = next_json(&mut socket).await.expect("a frame arrives");
            if frame["type"] == "chat.created" {
                return frame["chat_id"].as_str().unwrap().to_string();
            }
        }
    })
    .await
    .expect("chat.created arrives");
    (created, resume2)
}
