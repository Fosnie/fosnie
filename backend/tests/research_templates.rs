//! Deep Research report templates over the real stack, driving the REST handlers
//! themselves (not a re-typed copy of their SQL). Needs a reachable Postgres and
//! skips when `DATABASE_URL` is unset; never needs Keycloak/ML/Ollama up (the ML
//! service is stubbed by an in-process socket only where a test exercises it).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use fosnie_backend::auth::keycloak::AuthUser;
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::AppError;
use fosnie_backend::http::research_templates::{
    archive_template, create_template, get_template, list_templates, CreateTemplate, SectionInput,
    TemplateContent,
};
use fosnie_backend::state::AppState;
use fosnie_backend::{cache, db};

// ---- Harness ----------------------------------------------------------------

async fn state_with_ml(ml_base_url: &str) -> Option<(sqlx::PgPool, AppState)> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
    boot.ml.base_url = ml_base_url.to_string();
    let state = AppState::new(pg.clone(), redis, Arc::new(boot));
    Some((pg, state))
}

async fn state() -> Option<(sqlx::PgPool, AppState)> {
    // A base URL that never answers — no test using this touches the ML service.
    state_with_ml("http://127.0.0.1:9").await
}

fn ctx(id: Uuid, role: PlatformRole) -> AuthContext {
    AuthContext { user_id: Some(id), email: None, display_name: None, role, break_glass: false, mfa_enroll_only: false }
}

async fn seed_user(pg: &sqlx::PgPool, name: &str) -> Uuid {
    let id = db::new_id();
    sqlx::query("INSERT INTO users (id, display_name, email, role) VALUES ($1, $2, $3, 'user')")
        .bind(id)
        .bind(name)
        .bind(format!("{name}-{id}@example.test"))
        .execute(pg)
        .await
        .unwrap();
    id
}

fn section(heading: &str) -> SectionInput {
    SectionInput { heading: heading.into(), brief: "brief".into(), expandable: false, exec_summary: false }
}

fn from_scratch(label: &str, scope: &str) -> CreateTemplate {
    CreateTemplate {
        duplicate_of: None,
        content: TemplateContent {
            label: label.into(),
            description: "d".into(),
            skeleton: vec![section("One"), section("Two")],
            writing_instructions: "be terse".into(),
            outline_mode: "constrained".into(),
        },
        scope: scope.into(),
    }
}

fn duplicate_body(src: &str) -> CreateTemplate {
    CreateTemplate {
        duplicate_of: Some(src.into()),
        // Content is absent for a duplicate; all fields default (the P1 fix).
        content: TemplateContent {
            label: String::new(),
            description: String::new(),
            skeleton: Vec::new(),
            writing_instructions: String::new(),
            outline_mode: "constrained".into(),
        },
        scope: "personal".into(),
    }
}

async fn cleanup(pg: &sqlx::PgPool, users: &[Uuid]) {
    for u in users {
        sqlx::query("DELETE FROM research_templates WHERE created_by = $1").bind(u).execute(pg).await.ok();
        sqlx::query("DELETE FROM users WHERE id = $1").bind(u).execute(pg).await.ok();
    }
}

/// A one-endpoint stand-in for the research service's `GET /research/templates`:
/// answers every request with the same JSON body. Returns its base URL.
async fn spawn_mock_ml(body: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await; // drain the request line + headers
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

// ---- Tests ------------------------------------------------------------------

#[tokio::test]
async fn visibility_can_manage_and_403_and_404_via_handlers() {
    let Some((pg, st)) = state().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let alice = seed_user(&pg, "alice").await;
    let bob = seed_user(&pg, "bob").await;
    let admin = seed_user(&pg, "admin").await;

    // Alice creates a personal template.
    let mine = create_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Json(from_scratch("Alice personal", "personal")))
        .await
        .expect("alice creates personal")
        .0
        .id;

    // Alice's catalogue shows it and marks it manageable; Bob's does not show it.
    let alice_cat = list_templates(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User))).await.unwrap().0;
    assert!(alice_cat.custom.iter().any(|t| t.id == mine && t.can_manage), "owner sees + manages");
    assert_eq!(alice_cat.builtin.len(), 4, "four built-ins from the constant");
    let bob_cat = list_templates(State(st.clone()), AuthUser(ctx(bob, PlatformRole::User))).await.unwrap().0;
    assert!(!bob_cat.custom.iter().any(|t| t.id == mine), "bob cannot see alice's personal template");

    // Bob fetching it by id → 404 (no existence oracle).
    let err = get_template(State(st.clone()), AuthUser(ctx(bob, PlatformRole::User)), Path(mine)).await.unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)), "foreign personal → 404, got {err:?}");

    // A non-admin cannot publish globally (403); an admin can, and Bob then sees it.
    let g_denied = create_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Json(from_scratch("Nope", "global"))).await.unwrap_err();
    assert!(matches!(g_denied, AppError::Forbidden(_)), "global without permission → 403, got {g_denied:?}");
    let global = create_template(State(st.clone()), AuthUser(ctx(admin, PlatformRole::ClientAdmin)), Json(from_scratch("Shared", "global")))
        .await
        .expect("admin publishes global")
        .0
        .id;
    let bob_cat2 = list_templates(State(st.clone()), AuthUser(ctx(bob, PlatformRole::User))).await.unwrap().0;
    let seen = bob_cat2.custom.iter().find(|t| t.id == global).expect("bob sees the global template");
    assert!(!seen.can_manage, "a plain user cannot manage a global template");

    cleanup(&pg, &[alice, bob, admin]).await;
}

#[tokio::test]
async fn archive_hides_from_catalogue_but_detail_still_returns_it() {
    let Some((pg, st)) = state().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let alice = seed_user(&pg, "alice").await;
    let id = create_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Json(from_scratch("Doomed", "personal")))
        .await
        .unwrap()
        .0
        .id;

    archive_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Path(id)).await.expect("archive");

    let cat = list_templates(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User))).await.unwrap().0;
    assert!(!cat.custom.iter().any(|t| t.id == id), "archived leaves the catalogue");

    // The picker still resolves an archived template by id (for Refine).
    let detail = get_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Path(id)).await.unwrap().0;
    assert!(detail.archived, "detail reports archived");

    cleanup(&pg, &[alice]).await;
}

#[tokio::test]
async fn duplicate_custom_forks_from_the_database() {
    let Some((pg, st)) = state().await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let alice = seed_user(&pg, "alice").await;
    let source = create_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Json(from_scratch("My template", "personal")))
        .await
        .unwrap()
        .0
        .id;

    // Duplicate a CUSTOM template — this never touches the ML service (its body is
    // already in the store). This is the flow the review found broken (P2), and it
    // POSTs the duplicate-only body that used to die in the deserializer (P1).
    let copy = create_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Json(duplicate_body(&source.to_string())))
        .await
        .expect("duplicate custom must succeed")
        .0
        .id;
    assert_ne!(copy, source);

    let detail = get_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Path(copy)).await.unwrap().0;
    assert_eq!(detail.label, "My template (copy)");
    assert_eq!(detail.skeleton.len(), 2, "skeleton was copied");
    assert_eq!(detail.writing_instructions, "be terse", "writing style copied");
    assert_eq!(detail.scope, "personal");

    cleanup(&pg, &[alice]).await;
}

#[tokio::test]
async fn duplicate_builtin_forks_via_the_research_service() {
    // Duplicate of a BUILT-IN reaches the (here mocked) research service for its
    // full body, then forks a personal copy with non-empty writing instructions
    // and section briefs.
    let spec = r#"[{"id":"formal","label":"Formal report","skeleton":[{"heading":"Executive summary","brief":"one paragraph","expandable":false,"exec_summary":true},{"heading":"Findings","brief":"the evidence","expandable":true,"exec_summary":false}],"writing_instructions":"Measured, third person.","outline_mode":"constrained"}]"#;
    let ml = spawn_mock_ml(spec.to_string()).await;
    let Some((pg, st)) = state_with_ml(&ml).await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let alice = seed_user(&pg, "alice").await;

    let id = create_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Json(duplicate_body("formal")))
        .await
        .expect("duplicate built-in against the mock ML")
        .0
        .id;
    let detail = get_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Path(id)).await.unwrap().0;
    assert_eq!(detail.label, "Formal report (copy)");
    assert_eq!(detail.writing_instructions, "Measured, third person.", "built-in body was fetched");
    assert!(detail.skeleton.iter().any(|s| !s.brief.is_empty()), "section briefs copied");

    cleanup(&pg, &[alice]).await;
}

#[tokio::test]
async fn ml_down_fails_duplicate_but_catalogue_still_serves_builtins() {
    // The D5 compromise: the catalogue is served from the Rust constant and never
    // dials the research service, so it works even when the service is down; only
    // Duplicate-of-built-in reaches the service and so fails honestly.
    let Some((pg, st)) = state_with_ml("http://127.0.0.1:9").await else {
        eprintln!("skipping: DATABASE_URL unset");
        return;
    };
    let alice = seed_user(&pg, "alice").await;

    let cat = list_templates(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User))).await.expect("catalogue works with ML down").0;
    assert_eq!(cat.builtin.len(), 4, "the four built-ins still load");

    let err = create_template(State(st.clone()), AuthUser(ctx(alice, PlatformRole::User)), Json(duplicate_body("formal"))).await.unwrap_err();
    // It fails at the ML fetch, NOT at deserialization (which would be the P1 bug).
    assert!(matches!(err, AppError::Other(_)), "duplicate fails on the dead ML, got {err:?}");

    cleanup(&pg, &[alice]).await;
}
