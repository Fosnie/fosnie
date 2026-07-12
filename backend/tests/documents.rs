//! Workspace documents + tracked changes over the real stack. Upload → version
//! chain → download/text/PDF → soft-delete; and the assistant `edit_document`
//! round-trip (propose tracked changes → accept → new version). Gated on
//! `PAI_E2E=1` (needs Postgres + Redis + Keycloak + ML + Ollama + LibreOffice).

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::{auth, cache, db, documents, http};

const KC: &str = "http://localhost:8081/realms/fosnie";

fn enabled() -> bool {
    std::env::var("PAI_E2E").as_deref() == Ok("1")
}

fn fixture() -> Vec<u8> {
    let path = format!("{}/tests/fixtures/sample.docx", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(path).expect("sample.docx fixture")
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

async fn setup() -> (sqlx::PgPool, AppState, u16, String) {
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let ml_url =
        std::env::var("PAI__ML__BASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());
    let tok = token("alice").await.expect("keycloak token");

    let pg = db::connect(&db_url, 5).await.unwrap();
    let redis = cache::create_pool(&redis_url).unwrap();
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.keycloak.url = "http://localhost:8081".into();
    boot.keycloak.realm = "fosnie".into();
    boot.keycloak.client_id = "fosnie".into();
    boot.ml.base_url = ml_url;
    boot.server.static_dir = "___no_spa___".into();
    boot.storage.workspace_dir =
        std::env::temp_dir().join("pai_test_workspace").to_string_lossy().into();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));

    let instance = Arc::new(auth::keycloak::build_instance(&state.boot.keycloak).unwrap());
    let kc = auth::keycloak::auth_layer(instance.clone(), "fosnie".into());
    let ws = auth::keycloak::auth_layer_passthrough(instance, "fosnie".into());
    let app = http::router(state.clone(), Some(kc), Some(ws), None, None);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (pg, state, port, tok)
}

async fn make_project(api: &reqwest::Client, base: &str, tok: &str) -> String {
    let p: serde_json::Value = api
        .post(format!("{base}/api/projects"))
        .bearer_auth(tok)
        .json(&serde_json::json!({ "name": "Matter A", "sector": "legal" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    p["id"].as_str().unwrap().to_string()
}

async fn upload_doc(api: &reqwest::Client, base: &str, tok: &str, project_id: &str) -> serde_json::Value {
    api.post(format!("{base}/api/projects/{project_id}/workspace/documents?filename=sample.docx"))
        .bearer_auth(tok)
        .header("content-type", "application/octet-stream")
        .body(fixture())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_document_lifecycle() {
    if !enabled() {
        eprintln!("skipping workspace lifecycle (set PAI_E2E=1 with full stack up)");
        return;
    }
    let (pg, _state, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let project_id = make_project(&api, &base, &tok).await;
    let up = upload_doc(&api, &base, &tok, &project_id).await;
    let doc_id = up["document_id"].as_str().unwrap().to_string();
    let ver_id = up["version_id"].as_str().unwrap().to_string();

    // v1 is current, source user_upload.
    let did = uuid::Uuid::parse_str(&doc_id).unwrap();
    let (cur, source): (Option<uuid::Uuid>, String) = {
        let r = sqlx::query!(
            r#"SELECT d.current_version_id, dv.source::text AS "source!"
               FROM documents d JOIN document_versions dv ON dv.id = d.current_version_id
               WHERE d.id = $1"#,
            did
        )
        .fetch_one(&pg)
        .await
        .unwrap();
        (r.current_version_id, r.source)
    };
    assert_eq!(cur.map(|u| u.to_string()), Some(ver_id.clone()));
    assert_eq!(source, "user_upload");

    // Listed.
    let list: Vec<serde_json::Value> = api
        .get(format!("{base}/api/projects/{project_id}/workspace/documents"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.iter().any(|d| d["id"] == up["document_id"]));

    // Download bytes are byte-identical to the upload.
    let dl = api
        .get(format!("{base}/api/documents/{doc_id}/versions/{ver_id}/download"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(dl.as_ref(), fixture().as_slice(), "downloaded bytes must match upload");

    // Extracted text.
    let text: serde_json::Value = api
        .get(format!("{base}/api/documents/{doc_id}/versions/{ver_id}/text"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(text["text"].as_str().unwrap().contains("consultancy fee"));

    // PDF rendition (LibreOffice).
    let pdf = api
        .post(format!("{base}/api/documents/{doc_id}/versions/{ver_id}/pdf"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(pdf.status().is_success(), "render should succeed (soffice installed)");
    let pdf_bytes = pdf.bytes().await.unwrap();
    assert!(pdf_bytes.starts_with(b"%PDF"), "expected a PDF");

    // Soft-delete removes it from the listing.
    let del = api.delete(format!("{base}/api/documents/{doc_id}")).bearer_auth(&tok).send().await.unwrap();
    assert!(del.status().is_success());
    let after: Vec<serde_json::Value> = api
        .get(format!("{base}/api/projects/{project_id}/workspace/documents"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!after.iter().any(|d| d["id"] == up["document_id"]));

    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tracked_changes_round_trip() {
    if !enabled() {
        eprintln!("skipping tracked-changes round-trip (set PAI_E2E=1 with full stack up)");
        return;
    }
    let (pg, _state, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let project_id = make_project(&api, &base, &tok).await;
    let up = upload_doc(&api, &base, &tok, &project_id).await;
    let doc_id = up["document_id"].as_str().unwrap().to_string();
    let did = uuid::Uuid::parse_str(&doc_id).unwrap();

    // Agent that can edit documents.
    let agent: serde_json::Value = api
        .post(format!("{base}/api/agents"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({
            "name": "Redliner",
            "system_prompt": "You edit legal documents. When asked to change wording, you MUST call the edit_document tool with the given doc_id and an edits array of {find, replace}, then confirm.",
            "tools": ["edit_document", "list_workspace_documents"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let agent_id = agent["id"].as_str().unwrap().to_string();

    // Chat in the project; give the doc_id explicitly so the turn is deterministic.
    let (mut socket, _) =
        connect_async(format!("ws://127.0.0.1:{port}/ws?token={tok}")).await.expect("ws connect");
    let _hello = next_json(&mut socket).await.expect("hello");
    socket
        .send(Message::Text(
            serde_json::json!({
                "version": 1, "type": "chat.send",
                "agent_id": agent_id,
                "project_id": project_id,
                "content": format!(
                    "Call edit_document with doc_id \"{doc_id}\" and edits [{{\"find\":\"30 pounds\",\"replace\":\"14 pounds\"}}]."
                )
            })
            .to_string(),
        ))
        .await
        .unwrap();

    let mut saw_tool = false;
    let mut saw_doc_edited = false;
    let mut completed = false;
    loop {
        let Ok(Some(frame)) = timeout(Duration::from_secs(120), next_json(&mut socket)).await else {
            break;
        };
        match frame["type"].as_str() {
            Some("chat.tool") if frame["name"] == "edit_document" => saw_tool = true,
            Some("doc.edited") => {
                saw_doc_edited = true;
                assert!(
                    frame["changes"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
                    "doc.edited carries the proposed changes"
                );
            }
            Some("chat.completed") => {
                completed = true;
                break;
            }
            Some("chat.error") => panic!("chat.error: {}", frame["message"]),
            _ => {}
        }
    }
    assert!(completed, "turn should complete");
    assert!(saw_tool, "expected an edit_document tool call");
    assert!(saw_doc_edited, "expected a doc.edited frame with the tracked changes");

    // A pending, assistant-authored edit was recorded against a new assistant_edit version.
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM document_edits WHERE document_id = $1 AND status = 'pending' AND author = 'assistant'",
    )
    .bind(did)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(pending >= 1, "edit_document should record a pending change");

    let assistant_versions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM document_versions WHERE document_id = $1 AND source = 'assistant_edit'",
    )
    .bind(did)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(assistant_versions, 1);

    // Accept the change via REST → a user_accept version, edit marked accepted.
    let edits: Vec<serde_json::Value> = api
        .get(format!("{base}/api/documents/{doc_id}/edits"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let w_id = edits[0]["w_id"].as_str().unwrap().to_string();

    let accepted = api
        .post(format!("{base}/api/documents/{doc_id}/edits/{w_id}/accept"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(accepted.status().is_success());

    let accept_versions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM document_versions WHERE document_id = $1 AND source = 'user_accept'",
    )
    .bind(did)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(accept_versions, 1, "accept is version-creating");

    let status: String =
        sqlx::query_scalar("SELECT status::text FROM document_edits WHERE document_id = $1 AND w_id = $2")
            .bind(did)
            .bind(&w_id)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(status, "accepted");

    // Accepting the same change again is refused (no longer pending) and does
    // NOT create a second user_accept version — the concurrency guard holds.
    let again = api
        .post(format!("{base}/api/documents/{doc_id}/edits/{w_id}/accept"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap();
    assert!(!again.status().is_success(), "double-accept must be refused");
    let still_one: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM document_versions WHERE document_id = $1 AND source = 'user_accept'",
    )
    .bind(did)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert_eq!(still_one, 1, "no duplicate version from a repeat accept");

    // Audit recorded both proposed + accepted; chain intact.
    let proposed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'document.edit.proposed' AND resource_id = $1",
    )
    .bind(did)
    .fetch_one(&pg)
    .await
    .unwrap();
    let accepted_ev: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'document.edit.accepted' AND resource_id = $1",
    )
    .bind(did)
    .fetch_one(&pg)
    .await
    .unwrap();
    assert!(proposed >= 1 && accepted_ev >= 1);
    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cas_rejects_stale_base() {
    if !enabled() {
        return;
    }
    let (pg, state, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let project_id = make_project(&api, &base, &tok).await;
    let up = upload_doc(&api, &base, &tok, &project_id).await;
    let did = uuid::Uuid::parse_str(up["document_id"].as_str().unwrap()).unwrap();

    let ctx = AuthContext {
        user_id: None,
        email: None,
        display_name: None,
        role: PlatformRole::ClientAdmin,
        break_glass: false, mfa_enroll_only: false,
    };
    let base_ver = documents::current_version(&pg, &state.boot.storage.workspace_dir, did).await.unwrap().version_id;

    // First CAS against the live base advances the pointer.
    documents::add_version_cas(&state, &ctx, did, "user_accept", &fixture(), None, base_ver)
        .await
        .expect("first CAS succeeds");

    // A second CAS against the now-stale base must conflict and create no row.
    let count_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM document_versions WHERE document_id = $1")
            .bind(did)
            .fetch_one(&pg)
            .await
            .unwrap();
    let stale =
        documents::add_version_cas(&state, &ctx, did, "user_accept", &fixture(), None, base_ver).await;
    assert!(matches!(stale, Err(fosnie_backend::AppError::Conflict(_))), "stale base must conflict");
    let count_after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM document_versions WHERE document_id = $1")
            .bind(did)
            .fetch_one(&pg)
            .await
            .unwrap();
    assert_eq!(count_before, count_after, "a conflicted CAS leaves no orphan version");
}

/// A legal hold on a document (or its project) beats deletion: DELETE returns
/// 409 while held, succeeds once the hold is cleared. The blocked attempt is
/// audited and the chain stays valid.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legal_hold_blocks_delete() {
    if !enabled() {
        return;
    }
    let (pg, _state, port, tok) = setup().await;
    let api = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    let project_id = make_project(&api, &base, &tok).await;
    let up = upload_doc(&api, &base, &tok, &project_id).await;
    let doc_id = up["document_id"].as_str().unwrap().to_string();

    // Place a document-level hold (alice is admin).
    let hold: serde_json::Value = api
        .post(format!("{base}/api/admin/holds"))
        .bearer_auth(&tok)
        .json(&serde_json::json!({ "resource_type": "document", "resource_id": doc_id, "reason": "litigation" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hold_id = hold["id"].as_str().unwrap();

    // Delete is refused with 409 while held.
    let blocked = api.delete(format!("{base}/api/documents/{doc_id}")).bearer_auth(&tok).send().await.unwrap();
    assert_eq!(blocked.status().as_u16(), 409, "held document must not delete");
    // Still listed (not soft-deleted).
    let still: Vec<serde_json::Value> = api
        .get(format!("{base}/api/projects/{project_id}/workspace/documents"))
        .bearer_auth(&tok)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(still.iter().any(|d| d["id"] == up["document_id"]), "held doc stays in listing");

    // Clear the hold → delete now succeeds.
    let cleared = api.delete(format!("{base}/api/admin/holds/{hold_id}")).bearer_auth(&tok).send().await.unwrap();
    assert!(cleared.status().is_success());
    let del = api.delete(format!("{base}/api/documents/{doc_id}")).bearer_auth(&tok).send().await.unwrap();
    assert!(del.status().is_success(), "delete succeeds once the hold is cleared");

    assert!(fosnie_backend::audit::verify::verify_chain(&pg).await.unwrap().ok);
}

async fn next_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<serde_json::Value> {
    while let Some(msg) = socket.next().await {
        match msg.ok()? {
            Message::Text(t) => return serde_json::from_str(&t).ok(),
            Message::Close(_) => return None,
            _ => continue,
        }
    }
    None
}
