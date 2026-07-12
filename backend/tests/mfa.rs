//! Second factor (TOTP) end-to-end. No Keycloak: builds the local
//! `LocalAuthProvider` router and drives it over HTTP, mirroring `local_auth.rs`.
//! Needs Postgres + Redis; skips if DATABASE_URL is unset. Codes are computed with
//! `mfa::generate_code`, the verification counterpart, so the test never hard-codes
//! a time-dependent value.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;

use fosnie_backend::auth::local::LocalAuthProvider;
use fosnie_backend::auth::mfa;
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppStateBuilder;
use fosnie_backend::{cache, db, http};

async fn setup() -> Option<(sqlx::PgPool, u16)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into(); // mode defaults to local
    // The MFA secret is stored via `encrypt_at_rest`, so the at-rest keyring must be
    // configured (as in a real deployment) or the setup handler 500s.
    boot.message_encryption_key =
        base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_auth(Arc::new(LocalAuthProvider))
        .build();
    let app = http::router(state, None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((pg, port))
}

fn session_from(resp: &reqwest::Response) -> Option<String> {
    let raw = resp.headers().get(reqwest::header::SET_COOKIE)?.to_str().ok()?;
    raw.split(';').next()?.trim().strip_prefix("pai_session=").map(String::from)
}

fn uniq_email() -> String {
    format!("u{}@local.test", uuid::Uuid::now_v7().simple())
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// Full happy-path: enrol → logout → two-step login → recovery-code single-use →
/// wrong codes burn the pending token → disable requires password + code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mfa_enrol_login_disable() {
    let Some((pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    const PW: &str = "correct-horse-battery";
    let email = uniq_email();
    // A per-run client IP so the verify rate-limit buckets (keyed on source IP) do
    // not collide with other tests or persist across re-runs within the window.
    let xff = uuid::Uuid::now_v7().simple().to_string();

    // Register (auto-logged-in, MFA off → normal session).
    let reg = api
        .post(format!("{base}/api/auth/register"))
        .json(&serde_json::json!({ "email": email, "password": PW }))
        .send()
        .await
        .unwrap();
    assert_eq!(reg.status(), 200, "register");
    let cookie = session_from(&reg).expect("register cookie");
    let hdr = |c: &str| "pai_session=".to_string() + c;

    // Status: not enrolled yet.
    let st: serde_json::Value = api
        .get(format!("{base}/api/auth/mfa/status"))
        .header(reqwest::header::COOKIE, hdr(&cookie))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st["enabled"], false);

    // Setup → pending secret + otpauth URL.
    let setup_resp: serde_json::Value = api
        .post(format!("{base}/api/auth/mfa/setup"))
        .header(reqwest::header::COOKIE, hdr(&cookie))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let secret = setup_resp["secret"].as_str().unwrap().to_string();
    assert!(setup_resp["otpauth_url"].as_str().unwrap().starts_with("otpauth://totp/"));

    // Confirm with a live code → MFA enabled + recovery codes returned once.
    let code = mfa::generate_code(&secret, &email, now_secs()).unwrap();
    let confirm = api
        .post(format!("{base}/api/auth/mfa/confirm"))
        .header(reqwest::header::COOKIE, hdr(&cookie))
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(confirm.status(), 200, "confirm");
    let cookie = session_from(&confirm).expect("confirm re-mints the session");
    let confirm_body: serde_json::Value = confirm.json().await.unwrap();
    let recovery: Vec<String> = confirm_body["recovery_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(recovery.len(), 10, "ten recovery codes");

    // Logout, then a fresh login now returns a pending token, NOT a session.
    let _ = api
        .post(format!("{base}/api/auth/logout"))
        .header(reqwest::header::COOKIE, hdr(&cookie))
        .send()
        .await
        .unwrap();
    let login = api
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({ "email": email, "password": PW }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200, "login step 1");
    assert!(session_from(&login).is_none(), "no session cookie before the second step");
    let login_body: serde_json::Value = login.json().await.unwrap();
    assert_eq!(login_body["mfa_required"], true);
    let pending = login_body["pending"].as_str().unwrap().to_string();

    // A wrong code is rejected but does NOT burn the pending token.
    let bad = api
        .post(format!("{base}/api/auth/mfa/verify"))
        .header("x-forwarded-for", &xff)
        .json(&serde_json::json!({ "pending": pending, "code": "000000" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401, "wrong code");

    // The correct code completes the login → full session. Use the NEXT time-step
    // (now + 30s): confirm already consumed the current step, and anti-replay
    // forbids reusing it (a code is accepted at most once per step).
    let code = mfa::generate_code(&secret, &email, now_secs() + 30).unwrap();
    let verify = api
        .post(format!("{base}/api/auth/mfa/verify"))
        .header("x-forwarded-for", &xff)
        .json(&serde_json::json!({ "pending": pending, "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(verify.status(), 200, "verify step 2");
    let cookie = session_from(&verify).expect("verify mints the session");

    // whoami works with the two-step session.
    let who = api
        .get(format!("{base}/api/whoami"))
        .header(reqwest::header::COOKIE, hdr(&cookie))
        .send()
        .await
        .unwrap();
    assert_eq!(who.status(), 200);

    // A recovery code logs in once, then is spent.
    let login2: serde_json::Value = api
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({ "email": email, "password": PW }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pending2 = login2["pending"].as_str().unwrap().to_string();
    let rec_ok = api
        .post(format!("{base}/api/auth/mfa/verify"))
        .header("x-forwarded-for", &xff)
        .json(&serde_json::json!({ "pending": pending2, "code": recovery[0] }))
        .send()
        .await
        .unwrap();
    assert_eq!(rec_ok.status(), 200, "recovery code works once");

    // Reusing the same recovery code fails.
    let login3: serde_json::Value = api
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({ "email": email, "password": PW }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pending3 = login3["pending"].as_str().unwrap().to_string();
    let rec_reuse = api
        .post(format!("{base}/api/auth/mfa/verify"))
        .header("x-forwarded-for", &xff)
        .json(&serde_json::json!({ "pending": pending3, "code": recovery[0] }))
        .send()
        .await
        .unwrap();
    assert_eq!(rec_reuse.status(), 401, "recovery code is single-use");

    // Disable requires the password: a wrong password is rejected even with a good
    // factor. Use a fresh recovery code (a TOTP step would collide with the ones
    // already consumed above within this 30s window); the password is checked first
    // so this code is NOT spent on the rejected attempt.
    let no_pw = api
        .post(format!("{base}/api/auth/mfa/disable"))
        .header(reqwest::header::COOKIE, hdr(&cookie))
        .json(&serde_json::json!({ "password": "wrong", "code": recovery[1] }))
        .send()
        .await
        .unwrap();
    assert_eq!(no_pw.status(), 401, "disable needs the real password");

    // Correct password + a valid factor disables MFA.
    let dis = api
        .post(format!("{base}/api/auth/mfa/disable"))
        .header(reqwest::header::COOKIE, hdr(&cookie))
        .json(&serde_json::json!({ "password": PW, "code": recovery[1] }))
        .send()
        .await
        .unwrap();
    assert_eq!(dis.status(), 200, "disable");

    // Login is single-step again.
    let relog = api
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({ "email": email, "password": PW }))
        .send()
        .await
        .unwrap();
    assert!(session_from(&relog).is_some(), "MFA off → login mints a session directly");

    let _ = sqlx::query("DELETE FROM users WHERE email = $1").bind(&email).execute(&pg).await;
}

/// Anti-replay: a code accepted once cannot be verified a second time (same step).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mfa_code_is_not_replayable() {
    let Some((pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    const PW: &str = "correct-horse-battery";
    let email = uniq_email();

    let reg = api
        .post(format!("{base}/api/auth/register"))
        .json(&serde_json::json!({ "email": email, "password": PW }))
        .send()
        .await
        .unwrap();
    let cookie = session_from(&reg).unwrap();
    let hdr = format!("pai_session={cookie}");

    let setup_resp: serde_json::Value = api
        .post(format!("{base}/api/auth/mfa/setup"))
        .header(reqwest::header::COOKIE, &hdr)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let secret = setup_resp["secret"].as_str().unwrap().to_string();

    let code = mfa::generate_code(&secret, &email, now_secs()).unwrap();
    let ok = api
        .post(format!("{base}/api/auth/mfa/confirm"))
        .header(reqwest::header::COOKIE, &hdr)
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200, "first use of the code confirms");
    let cookie = session_from(&ok).unwrap();

    // The very same code, replayed at verify within its window, is refused
    // (mfa_last_step now covers this step).
    let login: serde_json::Value = api
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({ "email": email, "password": PW }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pending = login["pending"].as_str().unwrap().to_string();
    let replay = api
        .post(format!("{base}/api/auth/mfa/verify"))
        .header("x-forwarded-for", uuid::Uuid::now_v7().simple().to_string())
        .json(&serde_json::json!({ "pending": pending, "code": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), 401, "a used time-step code cannot be replayed");

    let _ = api
        .post(format!("{base}/api/auth/logout"))
        .header(reqwest::header::COOKIE, format!("pai_session={cookie}"))
        .send()
        .await;
    let _ = sqlx::query("DELETE FROM users WHERE email = $1").bind(&email).execute(&pg).await;
}
