//! Native tool authorisation seam. Every native tool call passes
//! `tools::authorize_native_call` before dispatch; these assert the three refusal
//! outcomes directly at the seam (no LLM, no dispatch), against the offered set,
//! the admin override, and the caller's permissions. DB-only: they read
//! `state.boot.features`, the RBAC tables, and write/read audit rows, so they skip
//! when `DATABASE_URL` is unset.
//!
//! Note on offered sets: these build the authorised set from an EXPLICIT list, NOT
//! from the name under test, so the grant check can genuinely fail. A helper that
//! seeds the set with the called name would authorise vacuously and prove nothing.

use std::collections::HashMap;
use std::sync::Arc;

use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::state::AppState;
use fosnie_backend::tools::{self, AuthorisedTools, NativeDecision, Override};
use fosnie_backend::{cache, db};
use uuid::Uuid;

async fn state() -> Option<AppState> {
    let db_url = std::env::var("DATABASE_URL").ok()?;
    let redis_url =
        std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
    let pg = db::connect(&db_url, 5).await.ok()?;
    let redis = cache::create_pool(&redis_url).ok()?;
    Some(AppState::new(pg, redis, Arc::new(BootConfig::default())))
}

/// A fresh ordinary user with no grants anywhere (so default-deny applies).
fn user_ctx() -> AuthContext {
    AuthContext {
        user_id: Some(Uuid::now_v7()),
        email: None,
        display_name: None,
        role: PlatformRole::User,
        break_glass: false,
        mfa_enroll_only: false,
    }
}

async fn count_denied(pg: &sqlx::PgPool, reason: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE action_type = 'tool.denied' AND payload->>'denied' = $1",
    )
    .bind(reason)
    .fetch_one(pg)
    .await
    .unwrap()
}

/// A native name the model emits that was NOT offered this turn is refused
/// recoverably and audited as a grant denial. On the pre-fix code the native
/// dispatch path carried no grant set at all (only the code-interpreter host flag),
/// so such a call routed straight to its handler.
#[tokio::test]
async fn n1_native_not_in_offered_set_is_refused_and_audited() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let ctx = user_ctx();
    // Offered this turn: read_document only. The model names web_search.
    let offered = AuthorisedTools::build(&["read_document".into()], &[], false, &HashMap::new());
    let overrides: HashMap<String, Override> = HashMap::new();

    let before = count_denied(&st.pg, "grant").await;
    let decision = tools::authorize_native_call(
        &st,
        &ctx,
        Uuid::now_v7(),
        &offered,
        &overrides,
        "web_search",
        None,
    )
    .await;

    assert!(
        matches!(decision, NativeDecision::Recoverable(_)),
        "a native tool that was not offered must be Recoverable, not Allowed"
    );
    assert!(
        count_denied(&st.pg, "grant").await > before,
        "the refusal must be audited tool.denied reason=grant"
    );
}

/// An admin-disabled native tool is refused at the seam even though it WAS offered
/// (i.e. the grant gate passed). On the pre-fix code the override was applied only
/// when building the advertised defs, so a name reaching dispatch any other way ran.
#[tokio::test]
async fn n2_admin_disabled_tool_refused_at_the_seam() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let ctx = user_ctx();
    // web_search IS in the offered set, so only the override can refuse it.
    let offered = AuthorisedTools::build(&["web_search".into()], &[], false, &HashMap::new());
    let mut overrides: HashMap<String, Override> = HashMap::new();
    overrides.insert(
        "web_search".to_string(),
        Override { enabled: false, description_override: None },
    );

    let before = count_denied(&st.pg, "override").await;
    let decision = tools::authorize_native_call(
        &st,
        &ctx,
        Uuid::now_v7(),
        &offered,
        &overrides,
        "web_search",
        None,
    )
    .await;

    assert!(
        matches!(decision, NativeDecision::Recoverable(_)),
        "an admin-disabled tool must be refused even when offered"
    );
    assert!(
        count_denied(&st.pg, "override").await > before,
        "the refusal must be audited tool.denied reason=override"
    );
}

/// The sharpest regression: `edit_document` on a non-agentic turn. It is a Proposal
/// tool, so `needs_agent_run()` is false, so the turn opens no agent run. On the
/// pre-fix code the one permission check (`tool_permitted` asserting project write)
/// sat inside `if let Some(run_id)` and was skipped entirely, so an edit proceeded
/// with only the chat-project-membership check. The seam now runs the check on
/// every call: an agent granted edit_document but lacking project write is refused.
#[tokio::test]
async fn n3_edit_document_asserts_write_permission_when_non_agentic() {
    let Some(st) = state().await else {
        eprintln!("skip: DATABASE_URL unset");
        return;
    };
    let ctx = user_ctx(); // no grants anywhere
    let project_id = Uuid::now_v7(); // a project the user has no write on (default-deny)

    // edit_document IS offered and not overridden, so the ONLY gate that can refuse
    // it here is the constrained-delegation permission check.
    let offered = AuthorisedTools::build(&["edit_document".into()], &[], false, &HashMap::new());
    let overrides: HashMap<String, Override> = HashMap::new();

    let before = count_denied(&st.pg, "rbac").await;
    let decision = tools::authorize_native_call(
        &st,
        &ctx,
        Uuid::now_v7(),
        &offered,
        &overrides,
        "edit_document",
        Some(project_id),
    )
    .await;

    assert!(
        matches!(decision, NativeDecision::Denied(_)),
        "edit_document without project write must be Denied (a hard error), not run"
    );
    assert!(
        count_denied(&st.pg, "rbac").await > before,
        "the refusal must be audited tool.denied reason=rbac"
    );
    // That an rbac denial (not grant/override/host) was recorded also proves the
    // offered, non-overridden, capability-free edit_document passed every earlier
    // gate and reached the permission check: the gates run in order, so a grant,
    // override, or host miss would have short-circuited before rbac.
}
