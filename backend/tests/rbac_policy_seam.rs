//! Extension-surface proof for the `RbacPolicy` seam.
//!
//! A `FakeRbacPolicy` (defined in this external test crate) whose `can` always
//! denies is injected via `AppStateBuilder::with_rbac`. The guard path through
//! `state.rbac` then denies even an admin — whereas the Core `FlatRbacPolicy`
//! short-circuits admins to `true`. Proves the slot is consumed and the `pub`
//! surface suffices for a future `fosnie-enterprise` `CustomRolesRbacPolicy`.
//!
//! No database: admin decisions short-circuit before any SQL, and the fake never
//! touches the pool, so the pg pool is built lazily and never connects.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

use fosnie_backend::auth::rbac::{Permission, PrincipalType, ResourceType};
use fosnie_backend::auth::{AuthContext, PlatformRole};
use fosnie_backend::config::BootConfig;
use fosnie_backend::error::Result;
use fosnie_backend::ext::{DocAccess, RbacPolicy};
use fosnie_backend::state::{AppState, AppStateBuilder};

/// Denies everything, regardless of grants — stands in for a custom Enterprise
/// policy. Inherits the default guard/mutation methods from the trait.
struct DenyAllRbacPolicy;

#[async_trait]
impl RbacPolicy for DenyAllRbacPolicy {
    async fn can(&self, _pool: &PgPool, _ctx: &AuthContext, _rt: ResourceType, _id: Uuid, _perm: Permission) -> Result<bool> {
        Ok(false)
    }
    async fn may_grant(&self, _pool: &PgPool, _granter: &AuthContext, _rt: ResourceType, _id: Uuid) -> Result<bool> {
        Ok(false)
    }
    // Deny every fine-grained permission too — stands in for a delegated policy
    // that has granted this caller nothing.
    async fn has_permission(&self, _pool: &PgPool, _ctx: &AuthContext, _perm: &str) -> Result<bool> {
        Ok(false)
    }
}

/// Grants exactly one permission and nothing else — proves an admin gate reads
/// the *specific* permission string, not a blanket admin flag.
struct OnlyPolicy(&'static str);

#[async_trait]
impl RbacPolicy for OnlyPolicy {
    async fn can(&self, _pool: &PgPool, _ctx: &AuthContext, _rt: ResourceType, _id: Uuid, _perm: Permission) -> Result<bool> {
        Ok(false)
    }
    async fn may_grant(&self, _pool: &PgPool, _granter: &AuthContext, _rt: ResourceType, _id: Uuid) -> Result<bool> {
        Ok(false)
    }
    async fn has_permission(&self, _pool: &PgPool, _ctx: &AuthContext, perm: &str) -> Result<bool> {
        Ok(perm == self.0)
    }
}

fn state_with(rbac: Option<Arc<dyn RbacPolicy>>) -> AppState {
    let pg = PgPoolOptions::new().connect_lazy("postgres://localhost/pai_test").expect("lazy pg pool");
    let redis = fosnie_backend::cache::create_pool("redis://localhost:6379").expect("redis pool");
    let boot = Arc::new(BootConfig::default());
    let mut b = AppStateBuilder::new(pg, redis, boot);
    if let Some(p) = rbac {
        b = b.with_rbac(p);
    }
    b.build()
}

fn admin() -> AuthContext {
    AuthContext {
        user_id: Some(Uuid::now_v7()),
        email: None,
        display_name: None,
        role: PlatformRole::ClientAdmin,
        break_glass: false, mfa_enroll_only: false,
    }
}

#[tokio::test]
async fn injected_policy_overrides_admin_short_circuit() {
    let id = Uuid::now_v7();
    let state = state_with(Some(Arc::new(DenyAllRbacPolicy)));

    // The Core FlatRbacPolicy would pass an admin (short-circuit); the fake denies.
    assert!(
        !state.rbac.can(&state.pg, &admin(), ResourceType::Project, id, Permission::Read).await.unwrap(),
        "gate path must see the injected policy, not FlatRbacPolicy"
    );
    assert!(
        state.rbac.require(&state.pg, &admin(), ResourceType::Project, id, Permission::Write).await.is_err(),
        "require() (default method) must use the injected can()"
    );
    // may_grant override flows through the default grant() guard.
    assert!(
        state.rbac
            .grant(&state.pg, &admin(), ResourceType::Project, id, PrincipalType::User, Uuid::now_v7(), Permission::Read)
            .await
            .is_err(),
        "grant() must be refused when the injected may_grant() denies"
    );
}

#[tokio::test]
async fn default_policy_admits_admin() {
    // No override → Core FlatRbacPolicy. An admin passes via the short-circuit,
    // confirming the deny above came from the slot (admin path touches no DB).
    let state = state_with(None);
    assert!(
        state.rbac.can(&state.pg, &admin(), ResourceType::Project, Uuid::now_v7(), Permission::Read).await.unwrap(),
        "default FlatRbacPolicy admits an admin"
    );
}

fn plain_user() -> AuthContext {
    AuthContext {
        user_id: Some(Uuid::now_v7()),
        email: None,
        display_name: None,
        role: PlatformRole::User,
        break_glass: false, mfa_enroll_only: false,
    }
}

#[tokio::test]
async fn permission_gate_reads_the_injected_policy() {
    use fosnie_backend::auth::permissions;

    // The injected policy denies every permission → an admin gate that now
    // resolves through `require_permission` must refuse even a client-admin,
    // where the pre-catalogue `is_admin()` gate would have passed. This is the
    // swap-test for the fine-grained seam every migrated Core gate flows through.
    let state = state_with(Some(Arc::new(DenyAllRbacPolicy)));
    assert!(
        state.rbac.require_permission(&state.pg, &admin(), permissions::USERS_MANAGE).await.is_err(),
        "gate path must consult the injected has_permission(), not the admin short-circuit"
    );

    // A policy granting exactly one permission passes that one and no other.
    let state = state_with(Some(Arc::new(OnlyPolicy(permissions::AUDIT_VIEW))));
    assert!(
        state.rbac.has_permission(&state.pg, &plain_user(), permissions::AUDIT_VIEW).await.unwrap(),
        "the single granted permission is honoured"
    );
    assert!(
        state.rbac.require_permission(&state.pg, &plain_user(), permissions::PROVIDERS_MANAGE).await.is_err(),
        "an ungranted permission is refused"
    );
}

/// Hides exactly one document (as an enforced source ACL would), granting every
/// other decision. Proves the document seam is consumed independently of `can`
/// (which stays permissive here) — the doc-level layer added after the project gate.
struct HidesOneDocPolicy(Uuid);

#[async_trait]
impl RbacPolicy for HidesOneDocPolicy {
    async fn can(&self, _pool: &PgPool, _ctx: &AuthContext, _rt: ResourceType, _id: Uuid, _perm: Permission) -> Result<bool> {
        Ok(true)
    }
    async fn may_grant(&self, _pool: &PgPool, _granter: &AuthContext, _rt: ResourceType, _id: Uuid) -> Result<bool> {
        Ok(true)
    }
    async fn document_access(&self, _pool: &PgPool, _ctx: &AuthContext, _project: Uuid, doc: Uuid) -> Result<DocAccess> {
        Ok(if doc == self.0 { DocAccess::Denied } else { DocAccess::Full })
    }
    async fn filter_documents(&self, _pool: &PgPool, _ctx: &AuthContext, _project: Uuid, doc_ids: &[Uuid]) -> Result<std::collections::HashSet<Uuid>> {
        Ok(doc_ids.iter().copied().filter(|d| *d != self.0).collect())
    }
}

#[tokio::test]
async fn injected_policy_hides_one_document() {
    let hidden = Uuid::now_v7();
    let visible = Uuid::now_v7();
    let project = Uuid::now_v7();
    let state = state_with(Some(Arc::new(HidesOneDocPolicy(hidden))));

    // Point check: the hidden doc is Denied, the other Full.
    assert_eq!(
        state.rbac.document_access(&state.pg, &plain_user(), project, hidden).await.unwrap(),
        DocAccess::Denied
    );
    assert_eq!(
        state.rbac.document_access(&state.pg, &plain_user(), project, visible).await.unwrap(),
        DocAccess::Full
    );
    // Batch filter drops only the hidden id.
    let allowed = state
        .rbac
        .filter_documents(&state.pg, &plain_user(), project, &[hidden, visible])
        .await
        .unwrap();
    assert!(!allowed.contains(&hidden), "enforced source ACL hides the document");
    assert!(allowed.contains(&visible), "an unrestricted document stays visible");
}

#[tokio::test]
async fn default_document_seam_admits_everything() {
    // No override → Core default: every document is Full and no id is filtered,
    // so document access stays purely project-derived (byte-identical to today).
    let state = state_with(None);
    let project = Uuid::now_v7();
    let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
    assert_eq!(
        state.rbac.document_access(&state.pg, &plain_user(), project, a).await.unwrap(),
        DocAccess::Full
    );
    let allowed = state.rbac.filter_documents(&state.pg, &plain_user(), project, &[a, b]).await.unwrap();
    assert_eq!(allowed.len(), 2, "Core default returns every document id unchanged");
}

/// Returns a fixed KB-document deny-list regardless of the KBs in scope — stands
/// in for the Enterprise source-ACL retrieval filter (connector-kb-rag §2).
struct DeniesOneKbDocPolicy(Uuid);

#[async_trait]
impl RbacPolicy for DeniesOneKbDocPolicy {
    async fn can(&self, _pool: &PgPool, _ctx: &AuthContext, _rt: ResourceType, _id: Uuid, _perm: Permission) -> Result<bool> {
        Ok(true)
    }
    async fn may_grant(&self, _pool: &PgPool, _granter: &AuthContext, _rt: ResourceType, _id: Uuid) -> Result<bool> {
        Ok(true)
    }
    async fn denied_kb_doc_ids(&self, _pool: &PgPool, _ctx: &AuthContext, _kb_ids: &[Uuid]) -> Result<Vec<Uuid>> {
        Ok(vec![self.0])
    }
}

#[tokio::test]
async fn injected_policy_supplies_retrieval_deny_list() {
    let denied = Uuid::now_v7();
    let state = state_with(Some(Arc::new(DeniesOneKbDocPolicy(denied))));
    let got = state.rbac.denied_kb_doc_ids(&state.pg, &plain_user(), &[Uuid::now_v7()]).await.unwrap();
    assert_eq!(got, vec![denied], "the retrieval seam surfaces the injected deny-list");
}

#[tokio::test]
async fn default_retrieval_deny_list_is_empty() {
    // No override → Core default: an empty deny-list, so the ML request serialises
    // byte-identically to before the feature (no `must_not` filter).
    let state = state_with(None);
    let got = state.rbac.denied_kb_doc_ids(&state.pg, &plain_user(), &[Uuid::now_v7()]).await.unwrap();
    assert!(got.is_empty(), "Core default denies nothing at retrieval");
}

#[tokio::test]
async fn default_permission_gate_tracks_is_admin() {
    // No override → Core default: every permission is `is_admin()`, so behaviour
    // is byte-identical to the pre-catalogue gates (admin holds all, user none).
    use fosnie_backend::auth::permissions;
    let state = state_with(None);
    assert!(
        state.rbac.has_permission(&state.pg, &admin(), permissions::PROVIDERS_MANAGE).await.unwrap(),
        "Core default admits an admin for any permission"
    );
    assert!(
        !state.rbac.has_permission(&state.pg, &plain_user(), permissions::USERS_VIEW).await.unwrap(),
        "Core default refuses a non-admin for any permission"
    );
}
