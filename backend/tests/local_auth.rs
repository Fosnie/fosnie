//! Local email/password auth end-to-end. No Keycloak:
//! builds `http::router(state, None, None)` with the `LocalAuthProvider` and
//! drives it over HTTP. Needs Postgres (:5433) + Redis; skips if DATABASE_URL is
//! unset. Cookies are handled manually (reqwest is built without the cookie
//! feature here).

use std::sync::Arc;

use fosnie_backend::auth::local::LocalAuthProvider;
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
    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_auth(Arc::new(LocalAuthProvider))
        .build();
    let app = http::router(state, None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Some((pg, port))
}

/// Pull the `pai_session` value out of a response's `Set-Cookie` header.
fn session_from(resp: &reqwest::Response) -> Option<String> {
    let raw = resp.headers().get(reqwest::header::SET_COOKIE)?.to_str().ok()?;
    raw.split(';').next()?.trim().strip_prefix("pai_session=").map(String::from)
}

fn uniq_email() -> String {
    format!("u{}@local.test", uuid::Uuid::now_v7().simple())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_auth_end_to_end() {
    let Some((pg, port)) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let base = format!("http://127.0.0.1:{port}");
    let api = reqwest::Client::new();
    const PW: &str = "correct-horse-battery";

    // GET /api/auth/config is public and reports local mode.
    let cfg: serde_json::Value =
        api.get(format!("{base}/api/auth/config")).send().await.unwrap().json().await.unwrap();
    assert_eq!(cfg["mode"], "local");
    assert_eq!(cfg["local_enabled"], true);

    // Whether an admin already exists determines the first-registrant rule.
    let admin_before: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE role IN ('super_admin','client_admin') AND deactivated_at IS NULL)",
    )
    .fetch_one(&pg)
    .await
    .unwrap();

    // Register user A.
    let email_a = uniq_email();
    let ra = api
        .post(format!("{base}/api/auth/register"))
        .json(&serde_json::json!({ "email": email_a, "password": PW }))
        .send()
        .await
        .unwrap();
    assert_eq!(ra.status(), 200, "register A");
    let cookie_a = session_from(&ra).expect("register sets a session cookie");
    let body_a: serde_json::Value = ra.json().await.unwrap();
    if admin_before {
        assert_eq!(body_a["role"], "user", "an admin already exists → A is a user");
    } else {
        assert_eq!(body_a["role"], "client_admin", "first registrant becomes admin");
    }

    // whoami with the cookie returns A's identity.
    let who = api
        .get(format!("{base}/api/whoami"))
        .header(reqwest::header::COOKIE, format!("pai_session={cookie_a}"))
        .send()
        .await
        .unwrap();
    assert_eq!(who.status(), 200, "whoami via session cookie");
    let who_b: serde_json::Value = who.json().await.unwrap();
    assert_eq!(who_b["email"], email_a);

    // whoami with no cookie → 401.
    let anon = api.get(format!("{base}/api/whoami")).send().await.unwrap();
    assert_eq!(anon.status(), 401, "no session → unauthorized");

    // Self-registration past the bootstrap admin is closed by default; open it via
    // the runtime knob so B can self-register (cleaned up at the end).
    sqlx::query("INSERT INTO config_settings (key, value, value_type, scope) VALUES ('auth.allow_registration','true','bool','global') ON CONFLICT (key) DO UPDATE SET value='true'")
        .execute(&pg).await.unwrap();

    // Register user B → always a plain user (an admin now exists).
    let email_b = uniq_email();
    let rb = api
        .post(format!("{base}/api/auth/register"))
        .json(&serde_json::json!({ "email": email_b, "password": PW }))
        .send()
        .await
        .unwrap();
    assert_eq!(rb.status(), 200, "register B");
    assert_eq!(rb.json::<serde_json::Value>().await.unwrap()["role"], "user", "second registrant is a user");

    // Duplicate email → 409.
    let dup = api
        .post(format!("{base}/api/auth/register"))
        .json(&serde_json::json!({ "email": email_a, "password": PW }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup.status(), 409, "duplicate email rejected");

    // Login: wrong password → 401; correct → 200 + fresh cookie.
    let bad = api
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({ "email": email_a, "password": "nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401, "wrong password");

    let good = api
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({ "email": email_a, "password": PW }))
        .send()
        .await
        .unwrap();
    assert_eq!(good.status(), 200, "correct password");
    let cookie_login = session_from(&good).expect("login sets a session cookie");

    // Logout invalidates the session.
    let out = api
        .post(format!("{base}/api/auth/logout"))
        .header(reqwest::header::COOKIE, format!("pai_session={cookie_login}"))
        .send()
        .await
        .unwrap();
    assert_eq!(out.status(), 200, "logout");
    let after_logout = api
        .get(format!("{base}/api/whoami"))
        .header(reqwest::header::COOKIE, format!("pai_session={cookie_login}"))
        .send()
        .await
        .unwrap();
    assert_eq!(after_logout.status(), 401, "session revoked on logout");

    // Deactivation: a fresh session stops working once the user is deactivated.
    let relog = api
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({ "email": email_a, "password": PW }))
        .send()
        .await
        .unwrap();
    let cookie_live = session_from(&relog).expect("login cookie");
    let uid_a = uuid::Uuid::parse_str(body_a["user_id"].as_str().unwrap()).unwrap();
    sqlx::query("UPDATE users SET deactivated_at = now() WHERE id = $1")
        .bind(uid_a)
        .execute(&pg)
        .await
        .unwrap();
    let deact = api
        .get(format!("{base}/api/whoami"))
        .header(reqwest::header::COOKIE, format!("pai_session={cookie_live}"))
        .send()
        .await
        .unwrap();
    assert_eq!(deact.status(), 401, "deactivated account is rejected (load_context)");

    // Rate-limit: a burst of failed logins from one source trips 429.
    let mut saw_429 = false;
    for _ in 0..15 {
        let r = api
            .post(format!("{base}/api/auth/login"))
            .json(&serde_json::json!({ "email": email_b, "password": "wrong" }))
            .send()
            .await
            .unwrap();
        if r.status() == 429 {
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "repeated failed logins must trip the rate limit (429)");

    // Cleanup the rows and the runtime knob this test set.
    let _ = sqlx::query("DELETE FROM users WHERE email = ANY($1)")
        .bind(vec![email_a, email_b])
        .execute(&pg)
        .await;
    let _ = sqlx::query("DELETE FROM config_settings WHERE key = 'auth.allow_registration'")
        .execute(&pg)
        .await;
}
