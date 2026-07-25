//! The desktop installer an instance serves to its own users.
//!
//! Two things here are worth more than the rest of the file. First, that the
//! download and the update manifest answer a device token and not only a browser
//! session: the updater in an installed client has no cookie, so a fence meant
//! for write routes would silently strand every paired machine on the version it
//! has. Second, that an installer with no update signature yields a 404 rather
//! than a manifest — a client that receives a manifest it cannot verify stops,
//! where one that receives nothing goes on to ask the public channel.
//!
//! The same header-driven fake `AuthProvider` as the pairing suite: it fails
//! without `X-Test-User`, which is exactly the condition under which the device
//! fallback runs.
//!
//! Needs Postgres + Redis; skips if DATABASE_URL is unset.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::request::Parts;
use uuid::Uuid;

use fosnie_backend::auth::{self, AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::AppError;
use fosnie_backend::ext::AuthProvider;
use fosnie_backend::state::{AppState, AppStateBuilder};
use fosnie_backend::{cache, db, http};

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
        Ok(AuthContext {
            user_id: Some(uid),
            email: None,
            display_name: None,
            role: PlatformRole::User,
            break_glass: false,
            mfa_enroll_only: false,
        })
    }
}

/// The published default, which the meta route falls back to when no override row
/// exists. Kept in step with the knob registry by the first case below.
const DEFAULT_DOWNLOAD_URL: &str = "https://get.fosnie.dev/desktop/";

async fn mk_user(pg: &sqlx::PgPool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, 'installer', $2, 'user')")
        .bind(id)
        .bind(format!("installer-{}@local.test", id.simple()))
        .execute(pg)
        .await
        .unwrap();
    id
}

/// Each case gets its own installer directory: they are a deployment-wide
/// singleton by design, so sharing one would make the cases order-dependent.
struct Harness {
    state: AppState,
    pg: sqlx::PgPool,
    base: String,
    api: reqwest::Client,
    dir: std::path::PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn setup() -> Option<Harness> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;

    let dir = std::env::temp_dir().join(format!("fosnie-installer-{}", Uuid::now_v7().simple()));
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.server.static_dir = "___no_spa___".into();
    boot.storage.desktop_installer_dir = dir.to_string_lossy().into_owned();

    let state = AppStateBuilder::new(pg.clone(), redis, Arc::new(boot))
        .with_auth(Arc::new(HeaderAuthProvider))
        .build();
    let app = http::router(state.clone(), None, None, None, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    Some(Harness {
        state,
        pg,
        base: format!("http://127.0.0.1:{port}"),
        api: reqwest::Client::new(),
        dir,
    })
}

async fn grant(state: &AppState) -> String {
    auth::breakglass::issue(state, 300, "test", "desktop installer")
        .await
        .unwrap()
        .to_string()
}

/// Look for this upload's own audit row rather than counting them. The cases run
/// against one database at the same time, so a count is a race; a digest is not.
async fn upload_audited(pg: &sqlx::PgPool, sha256: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM audit_events \
         WHERE action_type = 'desktop.installer_uploaded' AND payload->>'sha256' = $1",
    )
    .bind(sha256)
    .fetch_one(pg)
    .await
    .unwrap()
        > 0
}

/// Put bytes in, as the super-admin panel does: raw body, everything else in the
/// query string.
async fn put(
    h: &Harness,
    g: &str,
    kind: &str,
    filename: &str,
    version: &str,
    body: &[u8],
) -> reqwest::Response {
    h.api
        .post(format!(
            "{}/api/admin/desktop-installer?filename={}&kind={}&version={}",
            h.base,
            urlencode(filename),
            kind,
            version
        ))
        .header("x-break-glass", g)
        .header("content-type", "application/octet-stream")
        .body(body.to_vec())
        .send()
        .await
        .unwrap()
}

fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            other => format!("%{:02X}", other as u32),
        })
        .collect()
}

const MSI: &str = "Fosnie_0.1.1_x64_en-US.msi";
const SIG: &str = "Fosnie_0.1.1_x64_en-US.msi.sig";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_instance_with_nothing_uploaded_says_so_rather_than_failing() {
    let Some(h) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&h.pg).await;
    // The knob is a deployment-wide row; clear any override so the default is
    // what is actually under test.
    sqlx::query("DELETE FROM config_settings WHERE key = 'desktop.download_url'")
        .execute(&h.pg)
        .await
        .unwrap();

    let installer = h
        .api
        .get(format!("{}/api/desktop/installer", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(installer.status(), 404, "nothing to download");

    let manifest = h
        .api
        .get(format!("{}/api/desktop/latest.json", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(manifest.status(), 404, "the updater is told to look elsewhere");

    let meta: serde_json::Value = h
        .api
        .get(format!("{}/api/desktop/installer/meta", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(meta["available"], false);
    assert_eq!(meta["has_signature"], false);
    assert_eq!(
        meta["download_url"], DEFAULT_DOWNLOAD_URL,
        "people are sent to the published location instead"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_super_admin_may_publish_an_installer() {
    let Some(h) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&h.pg).await;

    let no_grant = h
        .api
        .post(format!(
            "{}/api/admin/desktop-installer?filename={MSI}&kind=installer&version=0.1.1",
            h.base
        ))
        .body(b"MZ fake".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(no_grant.status(), 401, "no break-glass, no upload");

    // A signed-in ordinary session is not a super-admin either: the panel is the
    // only way in, and it holds a grant rather than a session.
    let session_only = h
        .api
        .post(format!(
            "{}/api/admin/desktop-installer?filename={MSI}&kind=installer&version=0.1.1",
            h.base
        ))
        .header("x-test-user", user.to_string())
        .body(b"MZ fake".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(session_only.status(), 401, "a session is not a grant");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_is_uploaded_is_what_is_served_and_its_digest_is_reported() {
    let Some(h) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&h.pg).await;
    let g = grant(&h.state).await;
    // Distinct bytes per run, so this case's audit row is its own.
    let bytes = format!("MZ this stands in for an installer {}", Uuid::now_v7()).into_bytes();

    let res = put(&h, &g, "installer", MSI, "0.1.1", &bytes).await;
    assert_eq!(res.status(), 200);
    let meta: serde_json::Value = res.json().await.unwrap();
    assert_eq!(meta["available"], true);
    assert_eq!(meta["version"], "0.1.1");
    assert_eq!(meta["filename"], MSI);
    assert_eq!(meta["size"], bytes.len() as u64);

    // The digest is shown because vetting pipelines check it, so it had better be
    // the digest of the bytes and not of something adjacent.
    let expected = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex::encode(hasher.finalize())
    };
    assert_eq!(meta["sha256"], expected, "the digest is of the bytes uploaded");

    assert!(
        upload_audited(&h.pg, &expected).await,
        "publishing software to an estate is audited, with the digest of what was published"
    );

    let got = h
        .api
        .get(format!("{}/api/desktop/installer", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(got.status(), 200);
    assert_eq!(
        got.headers().get("content-disposition").unwrap().to_str().unwrap(),
        format!("attachment; filename=\"{MSI}\""),
        "offered as a download, not rendered"
    );
    assert_eq!(got.bytes().await.unwrap().as_ref(), bytes.as_slice());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_that_is_not_an_installer_is_refused_and_so_is_a_path() {
    let Some(h) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let g = grant(&h.state).await;

    let wrong_kind = put(&h, &g, "installer", "payload.exe", "0.1.1", b"MZ").await;
    assert_eq!(wrong_kind.status(), 400, "only a .msi is an installer");

    let escape = put(&h, &g, "installer", "../../escape.msi", "0.1.1", b"MZ").await;
    assert_eq!(escape.status(), 400, "a filename is a name, not a path");

    // Nothing was written anywhere, inside the directory or above it.
    assert!(
        !h.dir.parent().unwrap().join("escape.msi").exists(),
        "nothing escaped the installer directory"
    );
    assert!(
        std::fs::read_dir(&h.dir).map(|mut d| d.next().is_none()).unwrap_or(true),
        "the directory is still empty"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manifest_is_offered_only_once_the_update_can_be_verified() {
    let Some(h) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&h.pg).await;
    let g = grant(&h.state).await;

    put(&h, &g, "installer", MSI, "0.1.1", b"MZ installer").await;

    // An installer with no signature is a download, not an update. The client
    // must be told there is nothing here so that it asks the public channel.
    let unsigned = h
        .api
        .get(format!("{}/api/desktop/latest.json", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), 404, "an unverifiable update is not offered");

    let signature = "untrusted comment: signature\nSIGNATUREBYTES";
    put(&h, &g, "signature", SIG, "0.1.1", signature.as_bytes()).await;

    let res = h
        .api
        .get(format!("{}/api/desktop/latest.json", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let m: serde_json::Value = res.json().await.unwrap();
    assert_eq!(m["version"], "0.1.1");
    assert_eq!(m["platforms"]["windows-x86_64"]["signature"], signature);

    // The location is this instance's own. A manifest pointing anywhere else
    // would send a machine off a network it is not allowed to leave.
    let url = m["platforms"]["windows-x86_64"]["url"].as_str().unwrap();
    assert!(url.ends_with("/api/desktop/installer"), "served from here: {url}");
    assert!(!url.contains("get.fosnie.dev"), "not the public channel: {url}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_uploaded_manifest_gives_up_its_notes_and_signature_but_not_its_location() {
    let Some(h) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&h.pg).await;
    let g = grant(&h.state).await;

    put(&h, &g, "installer", MSI, "0.1.1", b"MZ installer").await;
    let published = serde_json::json!({
        "version": "0.1.1",
        "notes": "Two fixes and a faster start.",
        "pub_date": "2026-07-24T09:00:00Z",
        "platforms": { "windows-x86_64": {
            "signature": "PUBLISHED-SIGNATURE",
            "url": "https://get.fosnie.dev/desktop/Fosnie_0.1.1_x64_en-US.msi"
        }}
    });
    let res = put(
        &h,
        &g,
        "manifest",
        "latest.json",
        "0.1.1",
        published.to_string().as_bytes(),
    )
    .await;
    assert_eq!(res.status(), 200);

    let m: serde_json::Value = h
        .api
        .get(format!("{}/api/desktop/latest.json", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(m["notes"], "Two fixes and a faster start.");
    assert_eq!(m["pub_date"], "2026-07-24T09:00:00Z");
    assert_eq!(m["platforms"]["windows-x86_64"]["signature"], "PUBLISHED-SIGNATURE");
    assert!(
        !m["platforms"]["windows-x86_64"]["url"].as_str().unwrap().contains("get.fosnie.dev"),
        "the published location is discarded"
    );

    let rubbish = put(&h, &g, "manifest", "latest.json", "0.1.1", b"not json at all").await;
    assert_eq!(rubbish.status(), 400);
}

/// The case this whole file exists for.
///
/// The updater in an installed client carries a device token and no cookie. If
/// these two routes ever grow the fence that write routes have, every paired
/// machine stops updating and nothing else in the suite notices.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paired_client_reads_both_routes_with_its_device_token_alone() {
    let Some(h) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&h.pg).await;
    let g = grant(&h.state).await;
    put(&h, &g, "installer", MSI, "0.1.1", b"MZ installer").await;
    put(&h, &g, "signature", SIG, "0.1.1", b"SIGNATURE").await;

    // Pair a device the way one really pairs: a code from the session, redeemed
    // by the machine for a token of its own.
    let code: serde_json::Value = h
        .api
        .post(format!("{}/api/me/devices/pairing-code", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let paired: serde_json::Value = h
        .api
        .post(format!("{}/api/device/pair", h.base))
        .header("x-forwarded-for", Uuid::now_v7().to_string())
        .json(&serde_json::json!({
            "code": code["code"].as_str().unwrap(),
            "name": "Work laptop",
            "platform": "windows"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = paired["token"].as_str().expect("token once");

    // No X-Test-User anywhere below: the token is the only credential.
    let manifest = h
        .api
        .get(format!("{}/api/desktop/latest.json", h.base))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(manifest.status(), 200, "the updater can read the manifest");

    let installer = h
        .api
        .get(format!("{}/api/desktop/installer", h.base))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(installer.status(), 200, "and then download the update");
    assert_eq!(installer.bytes().await.unwrap().as_ref(), b"MZ installer");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_version_replaces_the_old_one_rather_than_joining_it() {
    let Some(h) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&h.pg).await;
    let g = grant(&h.state).await;

    put(&h, &g, "installer", MSI, "0.1.1", b"MZ old").await;
    put(&h, &g, "signature", SIG, "0.1.1", b"OLD-SIGNATURE").await;

    let next = "Fosnie_0.1.2_x64_en-US.msi";
    let res = put(&h, &g, "installer", next, "0.1.2", b"MZ new").await;
    assert_eq!(res.status(), 200);

    let names: Vec<String> = std::fs::read_dir(&h.dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.contains(&next.to_string()), "the new installer is there: {names:?}");
    assert!(!names.contains(&MSI.to_string()), "the old one is gone: {names:?}");
    assert!(!names.contains(&SIG.to_string()), "and so is its signature: {names:?}");

    // The old signature went with the old bytes, so there is no manifest to
    // offer until the new one is uploaded — never a new version signed with an
    // old signature.
    let manifest = h
        .api
        .get(format!("{}/api/desktop/latest.json", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(manifest.status(), 404, "no stale signature is carried forward");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_the_installer_puts_the_instance_back_as_it_was() {
    let Some(h) = setup().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let user = mk_user(&h.pg).await;
    let g = grant(&h.state).await;
    put(&h, &g, "installer", MSI, "0.1.1", b"MZ installer").await;

    let gone = h
        .api
        .delete(format!("{}/api/admin/desktop-installer", h.base))
        .header("x-break-glass", &g)
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), 204);

    let installer = h
        .api
        .get(format!("{}/api/desktop/installer", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(installer.status(), 404);

    let meta: serde_json::Value = h
        .api
        .get(format!("{}/api/desktop/installer/meta", h.base))
        .header("x-test-user", user.to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(meta["available"], false);
}
