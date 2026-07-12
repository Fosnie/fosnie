// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Extension surface — the seams a private `fosnie-enterprise` crate overrides
//! without editing Core.
//!
//! Each seam is an object-safe trait plus a Core default implementation. The
//! default lives here and carries the byte-for-byte host behaviour; Enterprise
//! supplies its own `impl` and injects it through [`crate::state::AppStateBuilder`].
//! Core never references an enterprise symbol — it only depends on these traits.
//!
//! First (reference) seam: [`FeatureResolver`]. The others (audit, auth, rbac,
//! moderation, provider, connector, keystore) follow the same pattern.

use async_trait::async_trait;
use axum::http::request::Parts;
use sqlx::PgPool;
use uuid::Uuid;

use crate::audit::{self, AppendResult, AuditEvent};
use crate::auth::rbac::{Permission, PrincipalType, ResourceType};
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::integrations::{dms, ConnectorKind};
use crate::state::AppState;

/// Resolves whether a feature is enabled for a caller. The Core default
/// ([`HostFeatureResolver`]) applies the host `features.*` ceiling restricted by
/// per-group flags; Enterprise can swap in licence-/entitlement-aware logic.
#[async_trait]
pub trait FeatureResolver: Send + Sync {
    /// Is `feature` enabled for `user_id`? (The WebSocket path holds a raw user
    /// id rather than a full [`AuthContext`].)
    async fn enabled_for_user(&self, state: &AppState, user_id: Option<Uuid>, feature: &str) -> bool;

    /// As [`enabled_for_user`](Self::enabled_for_user), keyed by an [`AuthContext`].
    /// Default delegates to `enabled_for_user(ctx.user_id, …)`.
    async fn enabled_for(&self, state: &AppState, ctx: &AuthContext, feature: &str) -> bool {
        self.enabled_for_user(state, ctx.user_id, feature).await
    }
}

/// A resolved provider configuration for one role (LLM/embed/rerank/ocr/stt/tts/
/// verify). Any `None` field means "no override for this field" — the ML service
/// keeps its own default. The decrypted `api_key` is plaintext (never persisted
/// or logged in clear).
#[derive(Debug, Clone)]
pub struct ResolvedProvider {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub enabled: bool,
    /// Operator override for the reasoning-control mode (`auto|none|toggle|levels|
    /// budget|always_on`); `None`/`"auto"` ⇒ auto-detect. Only meaningful for the
    /// `llm` role. See [`crate::reasoning`].
    pub reasoning_mode: Option<String>,
}

/// Resolves the provider configuration for a role. The Core default
/// ([`crate::providers::DbProviderRegistry`]) reads `provider_configs` with
/// precedence user-row → deployment-row → `None`; Enterprise can wrap it to apply
/// org policy (allowed providers / forced-local / egress ban). `None` ⇒ no
/// override ⇒ the ML service uses its own `.env` default.
#[async_trait]
pub trait ProviderRegistry: Send + Sync {
    async fn resolve(
        &self,
        pool: &sqlx::PgPool,
        role: &str,
        user_id: Option<Uuid>,
    ) -> Result<Option<ResolvedProvider>>;
}

/// Resolves the implementation of a data-source (DMS) connector for a
/// [`ConnectorKind`]. The Core default ([`crate::integrations::dms::DefaultConnectorRegistry`])
/// returns dormant `NotBuilt` adapters for the named v1 DMS kinds; a private
/// `fosnie-enterprise` crate registers real iManage/NetDocuments adapters. The
/// zero-egress gate ([`crate::integrations::guard_egress`]) runs *before* resolve
/// and is unaffected. `resolve` is sync — adapter construction does no I/O.
pub trait ConnectorRegistry: Send + Sync {
    /// The DMS connector adapter for `kind`. `None` = not a DMS kind.
    fn resolve_dms(&self, kind: ConnectorKind) -> Option<Box<dyn dms::DmsConnector>>;

    /// The mail connector adapter for `kind` (Outlook/Gmail). `None` = not a mail
    /// kind. The Core default returns `None` for every kind (no mail connector in a
    /// Core build); the [`crate::integrations::dms::DefaultConnectorRegistry`]
    /// overrides this to a dormant `NotBuilt`, and a private `fosnie-enterprise`
    /// crate injects the real adapters. Default keeps existing registry impls valid.
    fn resolve_mail(&self, _kind: ConnectorKind) -> Option<Box<dyn crate::integrations::mail::MailConnector>> {
        None
    }
}

/// Post-turn moderation hook. The Core default ([`crate::moderation::CoreModerationHook`])
/// runs the in-perimeter accountability classifier (OFF by default); a private
/// `fosnie-enterprise` crate can swap in its own accountability subsystem. Invoked
/// fire-and-forget off the
/// hot path — it must never block TTFT or panic.
#[async_trait]
pub trait ModerationHook: Send + Sync {
    async fn on_turn_completed(
        &self,
        state: &AppState,
        user_id: Uuid,
        chat_id: Uuid,
        message_id: Uuid,
        project_id: Option<Uuid>,
        prompt: String,
    );

    /// Is `user_id` a moderator (assignment-granted, orthogonal to the base role)?
    /// Surfaced in `whoami.is_moderator` to gate the SPA Moderation tab. Core default
    /// is `false` (no moderation subsystem in Core); a private `fosnie-enterprise` crate
    /// queries `moderator_assignments`. The Core default ([`NoopModerationHook`])
    /// returns `false`.
    async fn is_moderator(&self, _state: &AppState, _user_id: Uuid) -> bool {
        false
    }
}

/// Hot-path per-interaction evidence capture (FEATURE A2). The Core default
/// ([`crate::audit::evidence::CoreEvidenceSink`]) encrypts the PII fields, writes
/// the `interaction_evidence` row, and returns its `content_hash` for stamping
/// into the audit chain — byte-identical to the host today. A private
/// `fosnie-enterprise` crate can register its own sink via
/// [`crate::state::AppStateBuilder::with_evidence`]; a Core-only build can fall
/// back to a `NoopEvidenceSink` returning `None`. Off the TTFT
/// path; a failure must never fail the turn (returns `None`, chain unbound).
#[async_trait]
pub trait EvidenceSink: Send + Sync {
    /// The hex `content_hash` to stamp into the audit chain, or `None` if evidence
    /// is disabled or capture failed.
    async fn capture(&self, state: &AppState, input: crate::audit::EvidenceInput) -> Option<String>;
}

/// Populates the scheduler's [`JobRegistry`](crate::scheduler::JobRegistry) at boot.
/// The Core default ([`crate::scheduler::CoreJobs`]) registers the host's periodic
/// jobs + task handlers (byte-identical to the previously hard-wired set). A
/// private `fosnie-enterprise` crate can register additional jobs such as
/// checkpoint minting and evidence/hold retention; the Core default registers only
/// the genuinely-Core jobs. Registered through [`crate::state::AppStateBuilder::with_jobs`].
pub trait JobRegistrar: Send + Sync {
    fn register(&self, reg: &mut crate::scheduler::JobRegistry);
}

/// Retention policy for the PII-bearing, hold-gated records (FEATURE A2). The Core
/// default ([`crate::scheduler::CoreRetentionPolicy`]) reads `legal_holds` and
/// prunes `interaction_evidence` exactly as the host does today; those tables are an
/// Enterprise concern, so a Core-only build's default answers
/// `holds_active = false` / `prune_evidence = 0`, and a private `fosnie-enterprise`
/// crate registers the real policy via [`crate::state::AppStateBuilder::with_retention`]. Keeps the
/// audit-partition retention sweep ([`crate::scheduler::run_audit_retention`])
/// behaviour-identical while removing the enterprise-table references from Core.
#[async_trait]
pub trait RetentionPolicy: Send + Sync {
    /// Is any legal hold active? A `true` blocks retention sweeps entirely
    /// (conservative — never delete potentially-held evidence).
    async fn holds_active(&self, state: &AppState) -> bool;

    /// Prune evidence rows past their (shorter) retention window. Returns the count
    /// pruned; audits the expiry itself.
    async fn prune_evidence(&self, state: &AppState) -> u64;

    /// Is `doc_id` (or its `project_id`) under an active legal hold? A `true` blocks
    /// the document's deletion (audit §A.2.3). Core default: `false` (no holds in
    /// Core); a private `fosnie-enterprise` crate queries `legal_holds`.
    async fn is_held(&self, _state: &AppState, _project_id: Uuid, _doc_id: Uuid) -> Result<bool> {
        Ok(false)
    }
}

/// Outcome of evaluating a group-membership add against the data-owner gate
/// (FEATURE / Решение #6). The return type of [`GroupMembershipPolicy::gate_add`],
/// so it stays Core even though the owner-approval impl + endpoints live in
/// Enterprise.
pub enum AddOutcome {
    /// Add the member immediately (no approval required).
    Direct,
    /// Held pending approval — the (new or existing open) request's id.
    Pending(Uuid),
}

/// Decides how a member may be added to an access-bearing group. The Core default
/// ([`DirectAddPolicy`]) always adds directly; the owner-approval logic
/// ([`crate::http::group_requests::OwnerApprovalPolicy`]) is registered via
/// [`crate::state::AppStateBuilder::with_group_policy`]. `group_requests` is an
/// Enterprise concern; the Core default is `DirectAddPolicy`, and a private
/// `fosnie-enterprise` crate injects owner approval.
#[async_trait]
pub trait GroupMembershipPolicy: Send + Sync {
    async fn gate_add(
        &self,
        state: &AppState,
        ctx: &AuthContext,
        group_id: Uuid,
        target: Uuid,
    ) -> Result<AddOutcome>;
}

/// The Core [`GroupMembershipPolicy`]: a direct add, no data-owner approval. The
/// future Core default; Enterprise overrides with owner approval.
pub struct DirectAddPolicy;

#[async_trait]
impl GroupMembershipPolicy for DirectAddPolicy {
    async fn gate_add(
        &self,
        _state: &AppState,
        _ctx: &AuthContext,
        _group_id: Uuid,
        _target: Uuid,
    ) -> Result<AddOutcome> {
        Ok(AddOutcome::Direct)
    }
}

/// Gates the creation of a *new* user against a deployment seat limit. Checked at
/// the deliberate-creation paths (self-registration, admin manual create, SCIM) —
/// **not** on SSO JIT login, which would lock out an already-authorised directory
/// user. Soft-enforcement: block new creation past the limit, never deactivate or
/// lock out existing users.
///
/// The Core default ([`UnlimitedSeats`]) always answers `Ok` — Core is not seat-
/// licensed. A private `fosnie-enterprise` crate injects a licence-aware policy via
/// [`crate::state::AppStateBuilder::with_seats`] that counts active users against
/// the signed licence's `seats` claim.
#[async_trait]
pub trait SeatPolicy: Send + Sync {
    /// May one more user be created right now? `Err` (a validation error with a
    /// clear message) blocks the creation; `Ok(())` allows it.
    async fn allow_new_user(&self, pool: &PgPool) -> Result<()>;
}

/// The Core [`SeatPolicy`]: no seat cap (Core is not seat-licensed). The Core
/// default; Enterprise injects a licence-aware policy via
/// [`crate::state::AppStateBuilder::with_seats`].
pub struct UnlimitedSeats;

#[async_trait]
impl SeatPolicy for UnlimitedSeats {
    async fn allow_new_user(&self, _pool: &PgPool) -> Result<()> {
        Ok(())
    }
}

/// The Core [`ModerationHook`]: no-op accountability — no classifier runs and no
/// user is ever a moderator. The Core default; Enterprise injects its
/// accountability subsystem via [`crate::state::AppStateBuilder::with_moderation`].
pub struct NoopModerationHook;

#[async_trait]
impl ModerationHook for NoopModerationHook {
    async fn on_turn_completed(
        &self,
        _state: &AppState,
        _user_id: Uuid,
        _chat_id: Uuid,
        _message_id: Uuid,
        _project_id: Option<Uuid>,
        _prompt: String,
    ) {
    }
    // `is_moderator` uses the trait default (`false`).
}

/// The Core [`EvidenceSink`]: per-interaction evidence capture disabled (returns
/// `None`, chain unbound). The future Core default; Enterprise injects the real
/// capture via [`crate::state::AppStateBuilder::with_evidence`].
pub struct NoopEvidenceSink;

#[async_trait]
impl EvidenceSink for NoopEvidenceSink {
    async fn capture(&self, _state: &AppState, _input: crate::audit::EvidenceInput) -> Option<String> {
        None
    }
}

/// The Core [`RetentionPolicy`]: no legal holds, no evidence pruning (`legal_holds` /
/// `interaction_evidence` are an Enterprise concern). The future Core default;
/// Enterprise injects the real hold-gated policy via
/// [`crate::state::AppStateBuilder::with_retention`].
pub struct NoopRetentionPolicy;

#[async_trait]
impl RetentionPolicy for NoopRetentionPolicy {
    async fn holds_active(&self, _state: &AppState) -> bool {
        false
    }
    async fn prune_evidence(&self, _state: &AppState) -> u64 {
        0
    }
}

/// The audit append-path. The Core default ([`crate::audit::ChainAuditSink`])
/// writes the tamper-evident hash-chain (+ optional Ed25519 signature); a private
/// `fosnie-enterprise` crate can register a `TamperEvidentAuditSink`, with a light
/// `BasicAuditSink` as the Core-only alternative. Registered process-globally
/// via [`crate::audit::init_sink`] (like the signing key) — not an `AppState` slot,
/// because `append_with` is called from many `tx`/`pool`-only contexts.
#[async_trait]
pub trait AuditSink: Send + Sync {
    /// Append `event` using the caller-supplied connection, so the write stays
    /// atomic with the action it records.
    async fn append_with(
        &self,
        conn: &mut sqlx::PgConnection,
        event: &AuditEvent,
    ) -> Result<AppendResult, sqlx::Error>;
}

/// Turns an authenticated request into an [`AuthContext`]. The Core default
/// ([`crate::auth::keycloak::KeycloakAuthProvider`]) reads the Keycloak token the
/// middleware layer injected; a future `fosnie-enterprise` crate (or `LocalAuth`)
/// supplies its own — e.g. a SAML or session-cookie provider.
///
/// `parts` is the request's [`Parts`] *after* the transport/middleware layer has
/// run; `authenticate` only maps an already-present credential to identity.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, parts: &mut Parts, state: &AppState) -> Result<AuthContext, AppError>;
}

/// The outcome of a document-level access check ([`RbacPolicy::document_access`]).
/// Deliberately binary: a source ACL either lets the caller read the document in
/// full or not at all — there is no partial-read notion at this seam. Core always
/// answers `Full`; Enterprise's ACL-inheritance layer can answer `Denied` for a
/// document imported from a connected source whose ACL excludes the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocAccess {
    /// The caller may read the document (Core default; also the answer for any
    /// document with no enforced source ACL).
    Full,
    /// A source ACL under `enforce` excludes the caller. Read handlers surface this
    /// as 404 (existence is not leaked); list/export/tabular omit the document.
    Denied,
}

/// The access-control policy: the decision (`can`) and grant authorisation
/// (`may_grant`) seams, plus default guard/mutation methods built on them. The
/// Core default ([`crate::auth::rbac::FlatRbacPolicy`]) is flat AccessGrants with
/// an admin override and admin-only granting. Enterprise overrides `can`
/// (custom roles / ABAC / ACL inheritance) and/or `may_grant` (delegated admin)
/// and inherits the rest unchanged.
#[async_trait]
pub trait RbacPolicy: Send + Sync {
    /// Does `ctx` hold `permission` on this resource? (Override point.)
    async fn can(
        &self,
        pool: &PgPool,
        ctx: &AuthContext,
        resource_type: ResourceType,
        resource_id: Uuid,
        permission: Permission,
    ) -> Result<bool>;

    /// May `granter` create a grant on this resource? (Override point — Core =
    /// admin-only.)
    async fn may_grant(
        &self,
        pool: &PgPool,
        granter: &AuthContext,
        resource_type: ResourceType,
        resource_id: Uuid,
    ) -> Result<bool>;

    /// Does `ctx` hold the fine-grained admin `permission` (a catalogue string
    /// such as `users.manage` — see [`crate::auth::permissions`])? This is the
    /// seam behind every admin authorisation gate.
    ///
    /// The Core default answers `is_admin()` for **every** permission: an admin
    /// holds all of them, a non-admin none — byte-identical to the pre-catalogue
    /// `require_admin` gates. Enterprise overrides this to consult custom roles
    /// and delegated scopes (granting a subset), then inherits
    /// [`require_permission`](Self::require_permission) unchanged.
    ///
    /// Scope is not a parameter here: an *unscoped* holding answers `true`
    /// globally, while a purely *scoped* holding is resolved by the caller
    /// against the concrete resource (ТЗ §4). This method answers the global
    /// question "does the caller hold this permission at all?".
    async fn has_permission(&self, pool: &PgPool, ctx: &AuthContext, permission: &str) -> Result<bool> {
        let _ = (pool, permission);
        Ok(ctx.is_admin())
    }

    /// [`has_permission`](Self::has_permission) as a guard. 403 when denied.
    async fn require_permission(&self, pool: &PgPool, ctx: &AuthContext, permission: &str) -> Result<()> {
        if self.has_permission(pool, ctx, permission).await? {
            Ok(())
        } else {
            Err(AppError::Forbidden(format!("permission '{permission}' required")))
        }
    }

    /// At what scope does `ctx` hold `permission`? A scope-aware Core handler uses
    /// this to filter its lists and guard its mutations to the delegated set (ТЗ
    /// §4) — e.g. `users.view@group` returns the groups whose members are visible.
    ///
    /// The Core default has no delegation: an admin holds every permission
    /// [`Global`](crate::auth::permissions::PermissionScope::Global)ly, everyone
    /// else is [`Denied`](crate::auth::permissions::PermissionScope::Denied) —
    /// byte-identical to the pre-catalogue gates. Enterprise returns the narrowed
    /// `Groups`/`Projects` forms for a delegated admin.
    async fn permission_scope(
        &self,
        pool: &PgPool,
        ctx: &AuthContext,
        permission: &str,
    ) -> Result<crate::auth::permissions::PermissionScope> {
        use crate::auth::permissions::PermissionScope;
        let _ = (pool, permission);
        Ok(if ctx.is_admin() { PermissionScope::Global } else { PermissionScope::Denied })
    }

    /// Every fine-grained permission `ctx` effectively holds, for surfacing in
    /// `whoami` so the SPA can gate admin sections per-permission. A scoped-only
    /// holding is marked with a `:scoped` suffix (e.g. `users.manage:scoped`) so
    /// the SPA can show a section but signal the narrowing.
    ///
    /// The Core default returns an empty list (Core has no roles) — the SPA then
    /// falls back to its `is_admin` check, so a Core-only front end is unchanged.
    async fn resolved_permissions(&self, pool: &PgPool, ctx: &AuthContext) -> Result<Vec<String>> {
        let _ = (pool, ctx);
        Ok(Vec::new())
    }

    /// Document-level access, checked **after** the project gate (ТЗ #4). A
    /// document imported from a connected source (iManage/NetDocuments/mail)
    /// carries the source's own ACL; under an `enforce` sync-mapping that ACL
    /// restricts which project members may read the document, independently of the
    /// project grant. This is a *separate* seam from [`can`](Self::can) — plain
    /// project documents never reach an ACL, and `can(Document, …)` is deliberately
    /// not overloaded (nothing calls it; see the Enterprise ACL layer).
    ///
    /// The Core default answers [`DocAccess::Full`] for every document — byte-
    /// identical to today, where document access is purely project-derived.
    /// Enterprise overrides this to consult the materialised entitlements.
    async fn document_access(
        &self,
        pool: &PgPool,
        ctx: &AuthContext,
        project_id: Uuid,
        doc_id: Uuid,
    ) -> Result<DocAccess> {
        let _ = (pool, ctx, project_id, doc_id);
        Ok(DocAccess::Full)
    }

    /// The batch form of [`document_access`](Self::document_access): given the
    /// documents of a project, return the subset the caller may read. Used by list
    /// handlers (workspace documents, tabular review, export) to filter in one
    /// query rather than N per-row checks.
    ///
    /// The Core default returns **every** id unchanged — byte-identical to today's
    /// unfiltered project listings. Enterprise overrides this with a single batch
    /// query over the entitlement table.
    async fn filter_documents(
        &self,
        pool: &PgPool,
        ctx: &AuthContext,
        project_id: Uuid,
        doc_ids: &[Uuid],
    ) -> Result<std::collections::HashSet<Uuid>> {
        let _ = (pool, ctx, project_id);
        Ok(doc_ids.iter().copied().collect())
    }

    /// Retrieval-time deny-list for KB documents (ТЗ connector-kb-rag §2). A
    /// connector import can land a document into a KB (the RAG corpus); under an
    /// `enforce` sync-mapping the source ACL must also restrict *retrieval*, or RAG
    /// would leak across an ethical wall the workspace read-path already honours.
    /// Given the KBs a query is authorised to search (the intersection allow-list),
    /// returns the `kb_documents.id`s the caller is **not** entitled to — the RAG
    /// layer adds them as a Qdrant `must_not doc_id` filter. This is a *deny*-list
    /// (typically small: only enforce-mapped docs the user lacks) layered on top of
    /// the KB-level allow-list, never a replacement for it.
    ///
    /// The Core default returns an **empty** list — byte-identical to today, where
    /// KB access is purely KB-level. Enterprise overrides it to consult the
    /// materialised KB entitlements; on its own error it must fail **closed** (deny
    /// the query's enforce-KB docs), never silently return empty.
    async fn denied_kb_doc_ids(
        &self,
        pool: &PgPool,
        ctx: &AuthContext,
        kb_ids: &[Uuid],
    ) -> Result<Vec<Uuid>> {
        let _ = (pool, ctx, kb_ids);
        Ok(Vec::new())
    }

    /// [`can`](Self::can) as a guard. 403 when denied.
    async fn require(
        &self,
        pool: &PgPool,
        ctx: &AuthContext,
        resource_type: ResourceType,
        resource_id: Uuid,
        permission: Permission,
    ) -> Result<()> {
        if self.can(pool, ctx, resource_type, resource_id, permission).await? {
            Ok(())
        } else {
            Err(AppError::Forbidden(format!(
                "no {} grant on {} {resource_id}",
                permission.as_str(),
                resource_type.as_str()
            )))
        }
    }

    /// Project access with the owner short-circuit: the project's
    /// `owner_user_id` and admin levels always pass; otherwise a flat `permission`
    /// grant on the project is required.
    async fn project_can(
        &self,
        pool: &PgPool,
        ctx: &AuthContext,
        project_id: Uuid,
        permission: Permission,
    ) -> Result<bool> {
        if ctx.is_admin() {
            return Ok(true);
        }
        let owner: Option<Uuid> = sqlx::query_scalar!(
            "SELECT owner_user_id FROM projects WHERE id = $1 AND archived_at IS NULL",
            project_id
        )
        .fetch_optional(pool)
        .await?;
        let owner = owner.ok_or_else(|| AppError::Validation("project not found".into()))?;
        if ctx.user_id == Some(owner) {
            return Ok(true);
        }
        self.can(pool, ctx, ResourceType::Project, project_id, permission).await
    }

    /// [`project_can`](Self::project_can) as a guard. 403 when denied.
    async fn require_project(
        &self,
        pool: &PgPool,
        ctx: &AuthContext,
        project_id: Uuid,
        permission: Permission,
    ) -> Result<()> {
        if self.project_can(pool, ctx, project_id, permission).await? {
            Ok(())
        } else {
            Err(AppError::Forbidden(format!(
                "no {} grant on project {project_id}",
                permission.as_str()
            )))
        }
    }

    /// Create an AccessGrant if [`may_grant`](Self::may_grant) allows it. Audited.
    #[allow(clippy::too_many_arguments)]
    async fn grant(
        &self,
        pool: &PgPool,
        granter: &AuthContext,
        resource_type: ResourceType,
        resource_id: Uuid,
        principal_type: PrincipalType,
        principal_id: Uuid,
        permission: Permission,
    ) -> Result<Uuid> {
        if !self.may_grant(pool, granter, resource_type, resource_id).await? {
            return Err(AppError::Forbidden(
                "only an admin may grant access in this build".into(),
            ));
        }

        let id = Uuid::now_v7();
        let mut tx = pool.begin().await?;

        sqlx::query!(
            r#"
        INSERT INTO access_grants
            (id, resource_type, resource_id, principal_type, principal_id, permission, created_by)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (resource_type, resource_id, principal_type, principal_id, permission)
        DO NOTHING
        "#,
            id,
            resource_type as ResourceType,
            resource_id,
            principal_type as PrincipalType,
            principal_id,
            permission as Permission,
            granter.user_id,
        )
        .execute(&mut *tx)
        .await?;

        let mut event = AuditEvent::action("permission.change", granter.role.as_str());
        event.actor_user_id = granter.user_id;
        event.resource_type = Some(resource_type.as_str().into());
        event.resource_id = Some(resource_id);
        event.payload = Some(serde_json::json!({
            "op": "grant",
            "principal_type": format!("{principal_type:?}").to_lowercase(),
            "principal_id": principal_id,
            "permission": permission.as_str(),
        }));
        audit::append_with(&mut tx, &event).await?;

        tx.commit().await?;
        Ok(id)
    }

    /// Revoke an AccessGrant by id. Core default is admin-only (we cannot derive
    /// the resource from a bare `grant_id`, so `may_grant` is not consulted here);
    /// delegated revoke is Enterprise's concern — override this whole method.
    /// Audited.
    async fn revoke_grant(
        &self,
        pool: &PgPool,
        granter: &AuthContext,
        grant_id: Uuid,
    ) -> Result<()> {
        if !granter.is_admin() {
            return Err(AppError::Forbidden("only an admin may revoke access".into()));
        }

        let mut tx = pool.begin().await?;
        let deleted = sqlx::query!("DELETE FROM access_grants WHERE id = $1", grant_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();

        let mut event = AuditEvent::action("permission.change", granter.role.as_str());
        event.actor_user_id = granter.user_id;
        event.resource_type = Some("access_grant".into());
        event.resource_id = Some(grant_id);
        event.payload = Some(serde_json::json!({ "op": "revoke", "deleted": deleted }));
        audit::append_with(&mut tx, &event).await?;

        tx.commit().await?;
        Ok(())
    }
}

/// The host (Tier-2 #8) feature resolution. The deployment's global `features.*`
/// flag is the ceiling; a `group_feature_flags` row can only turn a feature OFF
/// for the members of a user group (restrict-only). Across a user's groups the
/// most-restrictive wins: if ANY of their groups disables a feature, it is off
/// for that user. A caller with no user id (e.g. break-glass) sees the global.
pub struct HostFeatureResolver;

impl HostFeatureResolver {
    /// The global (host) setting for a feature — the ceiling a group flag can lower.
    fn global(state: &AppState, feature: &str) -> bool {
        match feature {
            "voice" => state.boot.features.voice,
            "voice_live" => state.boot.features.voice_live,
            "groundedness" => state.boot.features.groundedness,
            "code_interpreter" => state.boot.features.code_interpreter,
            "messaging" => state.boot.features.messaging,
            "workflows" => state.boot.features.workflows,
            // Edition capabilities: gated by edition, default off in Core.
            "white_label" => state.boot.features.white_label,
            "compliance_audit" => state.boot.features.compliance_audit,
            "moderation" => state.boot.features.moderation,
            "message_review" => state.boot.features.message_review,
            "data_owner_approval" => state.boot.features.data_owner_approval,
            "federated_sso" => state.boot.features.federated_sso,
            "custom_rbac" => state.boot.features.custom_rbac,
            "enterprise_connectors" => state.boot.features.enterprise_connectors,
            _ => false,
        }
    }

    /// The host ceiling with a runtime override applied. `messaging`,
    /// `workflows`, `voice`, `voice_live` and `groundedness` are admin-toggleable
    /// at runtime (`config_settings["features.<name>"]`, bool) like BYOK; an
    /// absent row falls back to the boot flag. Other features keep the boot-only
    /// ceiling unchanged. When a runtime row is present it is authoritative — it
    /// can turn a feature ON even when the boot flag defaults off (so a
    /// self-hoster enables voice/verifier from the admin UI, no `.env` edit), or
    /// OFF as a kill-switch.
    async fn global_runtime(state: &AppState, feature: &str) -> bool {
        if matches!(
            feature,
            "messaging" | "workflows" | "voice" | "voice_live" | "groundedness"
        ) {
            if let Ok(Some(e)) =
                crate::config::runtime::get(&state.pg, &format!("features.{feature}")).await
            {
                return e.value == "true";
            }
        }
        Self::global(state, feature)
    }
}

#[async_trait]
impl FeatureResolver for HostFeatureResolver {
    async fn enabled_for_user(&self, state: &AppState, user_id: Option<Uuid>, feature: &str) -> bool {
        if !Self::global_runtime(state, feature).await {
            return false;
        }
        let Some(uid) = user_id else {
            return true; // global is on; no group context to restrict
        };
        let disabled: bool = sqlx::query_scalar!(
            r#"SELECT EXISTS(
            SELECT 1 FROM group_feature_flags f
            JOIN group_members m ON m.group_id = f.group_id
            WHERE m.user_id = $1 AND f.feature = $2 AND f.enabled = false
        ) AS "e!""#,
            uid,
            feature
        )
        .fetch_one(&state.pg)
        .await
        .unwrap_or(false);
        !disabled
    }
}

/// Resolves the deployment's at-rest data-encryption key(s) at boot (BYOK seam).
///
/// The Core default ([`EnvFileKeyProvider`]) reads the DEK from config
/// (`message_encryption_key`) and the Ed25519 audit seed from `audit_signing_key`
/// — byte-identical to the pre-BYOK behaviour. A private `fosnie-enterprise` crate
/// supplies a `Pkcs11KeyProvider` that unwraps the DEK from an HSM under a
/// customer-held KEK (envelope encryption): revoke the KEK and every ciphertext is
/// permanently inaccessible.
///
/// Called once at boot, before the datastores are touched, to build the process
/// global [`crate::crypto::Keyring`] via [`install_key_provider`]. `unwrap_*`
/// failures must be surfaced loudly and early — never boot with half the crypto.
pub trait KeyProvider: Send + Sync {
    /// The active DEK (used for new writes), unwrapped. `None` ⇒ at-rest encryption
    /// disabled (dev default — data stored in plaintext).
    fn active_dek(&self) -> Result<Option<crate::crypto::Dek>>;

    /// Retired DEKs still required to decrypt not-yet-re-encrypted rows (after a DEK
    /// rotation, until the background re-encrypt pass completes). Default: none.
    fn retired_deks(&self) -> Result<Vec<crate::crypto::Dek>> {
        Ok(Vec::new())
    }

    /// The Ed25519 audit signing seed (32 bytes), unwrapped; `None` ⇒ unsigned
    /// hash-chain only. Held under the same KeyProvider as the DEK so an HSM-backed
    /// deployment protects both secrets. Default: none.
    fn audit_seed(&self) -> Result<Option<[u8; 32]>> {
        Ok(None)
    }

    /// A stable identifier for the provider kind (`env-file`, `pkcs11`) — audited at
    /// boot (`key.provider_loaded`), never a secret.
    fn kind(&self) -> &'static str;
}

/// The Core default [`KeyProvider`]: DEK from config `message_encryption_key`
/// (base64 32-byte, empty ⇒ disabled), audit seed from `audit_signing_key`
/// (hex 32-byte). Reproduces the pre-BYOK boot exactly.
pub struct EnvFileKeyProvider {
    message_key_b64: String,
    audit_seed_hex: String,
}

impl EnvFileKeyProvider {
    /// Build from the two config strings (`boot.message_encryption_key`,
    /// `boot.audit_signing_key`).
    pub fn new(message_key_b64: impl Into<String>, audit_seed_hex: impl Into<String>) -> Self {
        Self { message_key_b64: message_key_b64.into(), audit_seed_hex: audit_seed_hex.into() }
    }
}

impl KeyProvider for EnvFileKeyProvider {
    fn active_dek(&self) -> Result<Option<crate::crypto::Dek>> {
        Ok(crate::crypto::parse_key(&self.message_key_b64).map(crate::crypto::Dek::legacy))
    }

    fn audit_seed(&self) -> Result<Option<[u8; 32]>> {
        Ok(crate::audit::parse_seed_bytes(&self.audit_seed_hex))
    }

    fn kind(&self) -> &'static str {
        "env-file"
    }
}

/// Resolve `provider` and install the process-global [`crate::crypto::Keyring`] +
/// audit signing seed. Returns the built keyring (its active key also seeds
/// `AppState.message_key`). Call once at boot in `main`, before building `AppState`.
pub fn install_key_provider(provider: &dyn KeyProvider) -> Result<crate::crypto::Keyring> {
    let active = provider.active_dek()?;
    let retired = provider.retired_deks()?;
    let keyring = crate::crypto::Keyring::new(active, retired);
    crate::crypto::init_keyring(keyring.clone());
    crate::audit::init_signing_bytes(provider.audit_seed()?);
    Ok(keyring)
}
