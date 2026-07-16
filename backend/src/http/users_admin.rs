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

//! Client-admin console: user management (list +
//! activate/deactivate), group management, sharing (AccessGrants over
//! `rbac::grant`/`revoke_grant`), and usage analytics. Identity stays
//! Keycloak-owned — the `users` table is a cache and deactivation is enforced at
//! the auth boundary. Group *creation* is also a power-user right; the rest is
//! client-admin.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::keycloak::AuthUser;
use crate::auth::permissions;
use crate::auth::rbac::{Permission, PrincipalType, ResourceType};
use crate::auth::{AuthContext, PlatformRole};
use crate::db;
use crate::error::{AppError, Result};
use crate::state::AppState;

fn require_group_manager(ctx: &AuthContext) -> Result<()> {
    if matches!(
        ctx.role,
        PlatformRole::PowerUser | PlatformRole::ClientAdmin | PlatformRole::SuperAdmin
    ) {
        Ok(())
    } else {
        Err(AppError::Forbidden("requires power user or admin".into()))
    }
}

/// Owner-or-admin gate for a single group (mirrors `require_manage_agent`): admins
/// (and break-glass) manage any group; a power-user-lead manages only groups they
/// created. Group not found → 400 as the read handlers expect.
async fn require_manage_group(state: &AppState, ctx: &AuthContext, group_id: Uuid) -> Result<()> {
    if ctx.is_admin() || ctx.break_glass {
        return Ok(());
    }
    let me = ctx
        .user_id
        .ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let created_by = sqlx::query_scalar!("SELECT created_by FROM groups WHERE id = $1", group_id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("group not found".into()))?;
    if created_by == Some(me) {
        Ok(())
    } else {
        Err(AppError::Forbidden("that group isn't yours".into()))
    }
}

// --- Delegated-admin scope helpers -------------------------------------------
//
// A permission may be held globally (a full admin) or narrowed to a set of groups
// / projects (a delegated admin). These helpers resolve the scope through the
// `RbacPolicy` seam and enforce it on both lists and mutations. In a Core deploy
// the scope is only ever `Global` (admin) or `Denied`, so behaviour is unchanged.

use crate::auth::permissions::PermissionScope;

/// Resolve `permission`'s scope for `ctx`; 403 when the caller does not hold it.
async fn scope_or_forbid(state: &AppState, ctx: &AuthContext, permission: &str) -> Result<PermissionScope> {
    let scope = state.rbac.permission_scope(&state.pg, ctx, permission).await?;
    if scope.is_denied() {
        return Err(AppError::Forbidden(format!("permission '{permission}' required")));
    }
    Ok(scope)
}

/// Guard a user-targeting mutation against a `@group`-scoped holding: the target
/// must be a member of a scope group, and IdP-managed (`scim`/`idp`) users are
/// read-only to a delegated admin (the IdP is the source of truth). A global
/// holding imposes neither restriction.
async fn require_user_in_scope(state: &AppState, scope: &PermissionScope, user_id: Uuid) -> Result<()> {
    let Some(gids) = scope.group_ids() else { return Ok(()) };
    let in_scope = sqlx::query_scalar!(
        r#"SELECT EXISTS(
               SELECT 1 FROM group_members WHERE user_id = $1 AND group_id = ANY($2)
           ) AS "e!""#,
        user_id,
        gids,
    )
    .fetch_one(&state.pg)
    .await?;
    if !in_scope {
        return Err(AppError::Forbidden("that user is outside your delegated groups".into()));
    }
    let managed = sqlx::query_scalar!("SELECT managed_by FROM users WHERE id = $1", user_id)
        .fetch_optional(&state.pg)
        .await?;
    if matches!(managed.as_deref(), Some("scim") | Some("idp")) {
        return Err(AppError::Forbidden("this user is managed by your identity provider".into()));
    }
    Ok(())
}

/// Guard a group-targeting mutation against a `@group`-scoped holding: the group
/// must be one of the scope groups, and IdP-managed groups are read-only.
async fn require_group_in_scope(state: &AppState, scope: &PermissionScope, group_id: Uuid) -> Result<()> {
    let Some(gids) = scope.group_ids() else { return Ok(()) };
    if !gids.contains(&group_id) {
        return Err(AppError::Forbidden("that group is outside your delegated groups".into()));
    }
    let managed = sqlx::query_scalar!("SELECT managed_by FROM groups WHERE id = $1", group_id)
        .fetch_optional(&state.pg)
        .await?;
    if matches!(managed.as_deref(), Some("scim") | Some("idp")) {
        return Err(AppError::Forbidden("this group is managed by your identity provider".into()));
    }
    Ok(())
}

// --- Users -------------------------------------------------------------------

#[derive(Serialize)]
pub struct UserOut {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub role: String,
    pub deactivated: bool,
    /// `local` (created/edited here) or `scim` (owned by a directory/IdP —
    /// surfaced in the admin UI as a read-only "Managed by IdP" badge).
    pub managed_by: String,
    /// Whether the user has a confirmed second factor — shown as a
    /// column + the target of the admin "Reset MFA" action.
    pub mfa_enabled: bool,
}

#[derive(Serialize)]
pub struct UserEntry {
    pub id: Uuid,
    pub display_name: String,
    pub email: String,
    /// Epoch (s) of the last avatar change — `None` = no avatar; doubles as the
    /// `?v=` cache-buster the frontend appends to the avatar URL.
    pub avatar_updated_at: Option<i64>,
}

/// Lightweight user directory for member / grant pickers — available to any
/// authenticated user (active users only). Not the admin list (no role/status).
///
/// Scoped to the caller's **team circle** for non-admins: a regular employee sees only
/// people they co-belong to a project or group with (self included), so they cannot
/// discover / DM / group-chat the whole company. Admins see everyone.
pub async fn list_directory(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<UserEntry>>> {
    let is_admin = ctx.is_admin();
    let me = ctx.user_id;
    let rows = sqlx::query!(
        r#"SELECT id, display_name, email,
                  extract(epoch from avatar_updated_at)::bigint AS avatar_epoch
           FROM users u
           WHERE u.deactivated_at IS NULL
             AND ($1 OR u.id = $2 OR EXISTS (
                   SELECT 1 FROM projects p
                   WHERE p.archived_at IS NULL
                     AND (p.owner_user_id = $2 OR EXISTS (
                           SELECT 1 FROM access_grants g WHERE g.resource_type = 'project' AND g.resource_id = p.id
                             AND ((g.principal_type = 'user'  AND g.principal_id = $2)
                               OR (g.principal_type = 'group' AND g.principal_id IN (SELECT group_id FROM group_members WHERE user_id = $2)))))
                     AND (p.owner_user_id = u.id OR EXISTS (
                           SELECT 1 FROM access_grants g WHERE g.resource_type = 'project' AND g.resource_id = p.id
                             AND ((g.principal_type = 'user'  AND g.principal_id = u.id)
                               OR (g.principal_type = 'group' AND g.principal_id IN (SELECT group_id FROM group_members WHERE user_id = u.id)))))
                 ) OR EXISTS (
                   SELECT 1 FROM group_members a JOIN group_members b ON a.group_id = b.group_id
                   WHERE a.user_id = $2 AND b.user_id = u.id
                 ))
           ORDER BY u.display_name"#,
        is_admin,
        me,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| UserEntry {
                id: r.id,
                display_name: r.display_name,
                email: r.email,
                avatar_updated_at: r.avatar_epoch,
            })
            .collect(),
    ))
}

pub async fn list_users(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<UserOut>>> {
    // `users.view`, narrowed to the delegated groups when scoped.
    let scope = scope_or_forbid(&state, &ctx, permissions::USERS_VIEW).await?;
    let unrestricted = scope.is_global();
    let groups: Vec<Uuid> = scope.group_ids().map(<[Uuid]>::to_vec).unwrap_or_default();
    // Self-archived (GDPR self-delete) rows are tombstones — hidden here so they
    // can't be reactivated; admin-suspended rows (deactivated_at only) still show.
    let rows = sqlx::query!(
        r#"SELECT id, email, display_name, role::text AS "role!", deactivated_at, managed_by,
                  (mfa_enabled_at IS NOT NULL) AS "mfa_enabled!"
           FROM users
           WHERE self_archived_at IS NULL
             AND ($1 OR id IN (SELECT user_id FROM group_members WHERE group_id = ANY($2)))
           ORDER BY created_at"#,
        unrestricted,
        &groups,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| UserOut {
                id: r.id,
                email: r.email,
                display_name: r.display_name,
                role: r.role,
                deactivated: r.deactivated_at.is_some(),
                managed_by: r.managed_by,
                mfa_enabled: r.mfa_enabled,
            })
            .collect(),
    ))
}

pub async fn deactivate_user(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(user_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let scope = scope_or_forbid(&state, &ctx, permissions::USERS_MANAGE).await?;
    require_user_in_scope(&state, &scope, user_id).await?;
    if ctx.user_id == Some(user_id) {
        return Err(AppError::Validation("cannot deactivate your own account".into()));
    }
    // Deactivate and emit the `directory.user_deactivated` domain event atomically
    // (transactional outbox) — only when the user was actually active.
    let mut tx = state.pg.begin().await?;
    let res = sqlx::query!("UPDATE users SET deactivated_at = now() WHERE id = $1 AND deactivated_at IS NULL", user_id)
        .execute(&mut *tx)
        .await?;
    if res.rows_affected() > 0 {
        let ev = crate::events::NewEvent::new(
            crate::events::DIRECTORY_USER_DEACTIVATED,
            crate::events::ActorType::Human,
        )
        .actor(ctx.user_id)
        .resource("user", user_id);
        crate::events::emit_with(&mut tx, &ev).await?;
    }
    tx.commit().await?;
    // Kill any live WebSocket the user holds; reconnect is already denied by
    // load_context (which filters deactivated_at), so this is the missing piece.
    state.hub.close_user(user_id);
    audit_user(&state, &ctx, "user.deactivated", user_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn reactivate_user(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(user_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let scope = scope_or_forbid(&state, &ctx, permissions::USERS_MANAGE).await?;
    require_user_in_scope(&state, &scope, user_id).await?;
    sqlx::query!("UPDATE users SET deactivated_at = NULL WHERE id = $1", user_id)
        .execute(&state.pg)
        .await?;
    audit_user(&state, &ctx, "user.reactivated", user_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /api/admin/users/{id}/mfa/reset` — an admin clears a user's second factor
/// device lost with no recovery codes left. Removes the secret +
/// recovery codes and force-logs-out the user; if `auth.require_mfa` is on they
/// re-enrol at their next login. Gated by `users.manage`, audited.
pub async fn reset_user_mfa(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(user_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let scope = scope_or_forbid(&state, &ctx, permissions::USERS_MANAGE).await?;
    require_user_in_scope(&state, &scope, user_id).await?;
    crate::auth::mfa::clear(&state.pg, user_id).await?;
    // Force-logout: any live session (including a two-step one already past verify)
    // is dropped, so the reset takes effect immediately.
    crate::auth::local::revoke_all_for_user(&state, user_id).await?;
    state.hub.close_user(user_id);
    audit_user(&state, &ctx, "user.mfa_reset", user_id).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn audit_user(state: &AppState, ctx: &AuthContext, action: &str, target: Uuid) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("user".into());
    ev.resource_id = Some(target);
    ev.risk_anomaly_flag = true; // account-state changes are sensitive
    let _ = audit::append(&state.pg, &ev).await;
}

// --- Groups ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateGroup {
    pub name: String,
    #[serde(default)]
    pub member_user_ids: Vec<Uuid>,
}

#[derive(Serialize)]
pub struct CreatedId {
    pub id: Uuid,
}

pub async fn create_group(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateGroup>,
) -> Result<Json<CreatedId>> {
    require_group_manager(&ctx)?;
    let id = db::new_id();
    let mut tx = state.pg.begin().await?;
    sqlx::query!("INSERT INTO groups (id, name, created_by) VALUES ($1, $2, $3)", id, body.name, ctx.user_id)
        .execute(&mut *tx)
        .await?;
    for u in &body.member_user_ids {
        sqlx::query!(
            "INSERT INTO group_members (group_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            id, u,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    audit_group(&state, &ctx, "group.created", id, None).await;
    let extra: Vec<Uuid> = ctx.user_id.into_iter().collect();
    notify_group_change(&state, id, &extra).await;
    Ok(Json(CreatedId { id }))
}

#[derive(Serialize)]
pub struct GroupSummary {
    pub id: Uuid,
    pub name: String,
}

pub async fn list_groups(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<GroupSummary>>> {
    // Power-user leads see only groups they created; admins (break-glass) see all.
    require_group_manager(&ctx)?;
    let all = ctx.is_admin() || ctx.break_glass;
    let rows = sqlx::query!(
        "SELECT id, name FROM groups WHERE $1 OR created_by = $2 ORDER BY created_at DESC",
        all,
        ctx.user_id,
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(rows.into_iter().map(|r| GroupSummary { id: r.id, name: r.name }).collect()))
}

#[derive(Serialize)]
pub struct GroupDetail {
    pub id: Uuid,
    pub name: String,
    pub members: Vec<Uuid>,
}

pub async fn get_group(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(group_id): Path<Uuid>,
) -> Result<Json<GroupDetail>> {
    require_manage_group(&state, &ctx, group_id).await?;
    let g = sqlx::query!("SELECT name FROM groups WHERE id = $1", group_id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("group not found".into()))?;
    let members = sqlx::query_scalar!("SELECT user_id FROM group_members WHERE group_id = $1", group_id)
        .fetch_all(&state.pg)
        .await?;
    Ok(Json(GroupDetail { id: group_id, name: g.name, members }))
}

#[derive(Deserialize)]
pub struct AddGroupMember {
    pub user_id: Uuid,
}

pub async fn add_group_member(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(group_id): Path<Uuid>,
    Json(body): Json<AddGroupMember>,
) -> Result<Json<serde_json::Value>> {
    require_manage_group(&state, &ctx, group_id).await?;
    // Confidentiality gate: if the group grants matters the caller doesn't own, the
    // add is held for the matter owners' approval rather than applied immediately.
    match state.group_policy.gate_add(&state, &ctx, group_id, body.user_id).await? {
        crate::ext::AddOutcome::Direct => {
            // Add the member and emit the `project.member_added` domain event
            // atomically (transactional outbox).
            let mut tx = state.pg.begin().await?;
            sqlx::query!(
                "INSERT INTO group_members (group_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
                group_id, body.user_id,
            )
            .execute(&mut *tx)
            .await?;
            let ev = crate::events::NewEvent::new(
                crate::events::PROJECT_MEMBER_ADDED,
                crate::events::ActorType::Human,
            )
            .actor(ctx.user_id)
            .resource("group", group_id)
            .payload(serde_json::json!({ "group_id": group_id.to_string(), "user_id": body.user_id.to_string() }));
            crate::events::emit_with(&mut tx, &ev).await?;
            // Also the group-scoped name (catalogue expansion) — a stable trigger
            // distinct from the legacy `project.member_added`; both ride this tx.
            let gev = crate::events::NewEvent::new(
                crate::events::GROUP_MEMBER_ADDED,
                crate::events::ActorType::Human,
            )
            .actor(ctx.user_id)
            .resource("group", group_id)
            .payload(serde_json::json!({ "group_id": group_id.to_string(), "user_id": body.user_id.to_string() }));
            crate::events::emit_with(&mut tx, &gev).await?;
            tx.commit().await?;
            audit_group(&state, &ctx, "group.member.added", group_id, Some(serde_json::json!({ "user_id": body.user_id }))).await;
            let mut extra = vec![body.user_id];
            extra.extend(ctx.user_id);
            notify_group_change(&state, group_id, &extra).await;
            Ok(Json(serde_json::json!({ "ok": true, "pending": false })))
        }
        crate::ext::AddOutcome::Pending(req) => {
            Ok(Json(serde_json::json!({ "ok": true, "pending": true, "request_id": req })))
        }
    }
}

pub async fn remove_group_member(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((group_id, user_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>> {
    require_manage_group(&state, &ctx, group_id).await?;
    // Remove the member and emit the `group.member_removed` domain event atomically
    // (transactional outbox) — only when a row was actually removed.
    let mut tx = state.pg.begin().await?;
    let res = sqlx::query!("DELETE FROM group_members WHERE group_id = $1 AND user_id = $2", group_id, user_id)
        .execute(&mut *tx)
        .await?;
    if res.rows_affected() > 0 {
        let ev = crate::events::NewEvent::new(
            crate::events::GROUP_MEMBER_REMOVED,
            crate::events::ActorType::Human,
        )
        .actor(ctx.user_id)
        .resource("group", group_id)
        .payload(serde_json::json!({ "group_id": group_id.to_string(), "user_id": user_id.to_string() }));
        crate::events::emit_with(&mut tx, &ev).await?;
    }
    tx.commit().await?;
    audit_group(&state, &ctx, "group.member.removed", group_id, Some(serde_json::json!({ "user_id": user_id }))).await;
    // The removed user is no longer in group_members, so include them explicitly.
    let mut extra = vec![user_id];
    extra.extend(ctx.user_id);
    notify_group_change(&state, group_id, &extra).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn delete_group(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(group_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    require_manage_group(&state, &ctx, group_id).await?;
    // A lead must not silently revoke project access by deleting a group that's an
    // access-grant principal; admins keep the unconditional delete.
    if !(ctx.is_admin() || ctx.break_glass) {
        let referenced = sqlx::query_scalar!(
            r#"SELECT EXISTS(
                   SELECT 1 FROM access_grants
                   WHERE principal_type = 'group' AND principal_id = $1
               ) AS "e!""#,
            group_id,
        )
        .fetch_one(&state.pg)
        .await?;
        if referenced {
            return Err(AppError::Conflict(
                "this group grants project access; remove those shares first".into(),
            ));
        }
    }
    // Capture the members before the cascade so we can still tell them to refresh.
    let mut recipients = sqlx::query_scalar!("SELECT user_id FROM group_members WHERE group_id = $1", group_id)
        .fetch_all(&state.pg)
        .await
        .unwrap_or_default();
    recipients.extend(ctx.user_id);
    sqlx::query!("DELETE FROM groups WHERE id = $1", group_id)
        .execute(&state.pg)
        .await?;
    audit_group(&state, &ctx, "group.deleted", group_id, None).await;
    state.hub.send_invalidate(&recipients, group_change_keys(group_id));
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn audit_group(state: &AppState, ctx: &AuthContext, action: &str, group_id: Uuid, payload: Option<serde_json::Value>) {
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("group".into());
    ev.resource_id = Some(group_id);
    ev.payload = payload;
    let _ = audit::append(&state.pg, &ev).await;
}

/// React-Query keys touched by a group/membership change (groups list, the group
/// detail, the lead's analytics, and the access-derived project/chat lists).
pub(crate) fn group_change_keys(group_id: Uuid) -> Vec<Vec<String>> {
    vec![
        vec!["groups".to_string()],
        vec!["group".to_string(), group_id.to_string()],
        vec!["power-analytics".to_string()],
        vec!["projects".to_string()],
        vec!["group-chats".to_string()],
    ]
}

/// Live cache-invalidation hint after a group/membership change: pushed to the
/// group's current members plus `extra` (actor, added/removed target) so their open
/// views refresh without a reload. Best-effort.
pub async fn notify_group_change(state: &AppState, group_id: Uuid, extra: &[Uuid]) {
    let mut recipients = sqlx::query_scalar!("SELECT user_id FROM group_members WHERE group_id = $1", group_id)
        .fetch_all(&state.pg)
        .await
        .unwrap_or_default();
    recipients.extend_from_slice(extra);
    state.hub.send_invalidate(&recipients, group_change_keys(group_id));
}

// --- Per-group feature flags (Tier-2 #8) -------------------------------------

/// Host features a client-admin may gate per group (restrict-only — see
/// `crate::features`). Keep in sync with the resolver's `global()`.
const GATEABLE_FEATURES: [&str; 2] = ["voice", "code_interpreter"];

#[derive(Serialize)]
pub struct GroupFeatureFlag {
    pub feature: String,
    pub enabled: bool,
}

/// The group's explicit feature overrides (a missing feature inherits global).
pub async fn list_group_flags(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<GroupFeatureFlag>>> {
    let scope = scope_or_forbid(&state, &ctx, permissions::GROUPS_MANAGE).await?;
    require_group_in_scope(&state, &scope, id).await?;
    let rows = sqlx::query!(
        "SELECT feature, enabled FROM group_feature_flags WHERE group_id = $1 ORDER BY feature",
        id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| GroupFeatureFlag { feature: r.feature, enabled: r.enabled })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct SetGroupFlag {
    pub enabled: bool,
}

/// Set a per-group override for a gateable feature (restrict-only). Client-admin.
pub async fn set_group_flag(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((id, feature)): Path<(Uuid, String)>,
    Json(body): Json<SetGroupFlag>,
) -> Result<Json<serde_json::Value>> {
    let scope = scope_or_forbid(&state, &ctx, permissions::GROUPS_MANAGE).await?;
    require_group_in_scope(&state, &scope, id).await?;
    if !GATEABLE_FEATURES.contains(&feature.as_str()) {
        return Err(AppError::Validation("feature is not gateable per group".into()));
    }
    sqlx::query!(
        "INSERT INTO group_feature_flags (group_id, feature, enabled, updated_by) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (group_id, feature) DO UPDATE \
         SET enabled = EXCLUDED.enabled, updated_by = EXCLUDED.updated_by, updated_at = now()",
        id, feature, body.enabled, ctx.user_id,
    )
    .execute(&state.pg)
    .await?;
    audit_group(&state, &ctx, "group.feature_flag.set", id,
        Some(serde_json::json!({ "feature": feature, "enabled": body.enabled }))).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Clear a per-group override → the group inherits the global setting again.
pub async fn clear_group_flag(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path((id, feature)): Path<(Uuid, String)>,
) -> Result<Json<serde_json::Value>> {
    let scope = scope_or_forbid(&state, &ctx, permissions::GROUPS_MANAGE).await?;
    require_group_in_scope(&state, &scope, id).await?;
    sqlx::query!("DELETE FROM group_feature_flags WHERE group_id = $1 AND feature = $2", id, feature)
        .execute(&state.pg)
        .await?;
    audit_group(&state, &ctx, "group.feature_flag.cleared", id,
        Some(serde_json::json!({ "feature": feature }))).await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// --- Sharing (AccessGrants) --------------------------------------------------

#[derive(Deserialize)]
pub struct CreateGrant {
    pub resource_type: String,
    pub resource_id: Uuid,
    pub principal_type: String, // user | group
    pub principal_id: Uuid,
    pub permission: String, // read | write | share | delete
}

fn parse_resource_type(s: &str) -> Result<ResourceType> {
    Ok(match s {
        "project" => ResourceType::Project,
        "project_knowledge" => ResourceType::ProjectKnowledge,
        "agent" => ResourceType::Agent,
        "skill" => ResourceType::Skill,
        "prompt" => ResourceType::Prompt,
        "chat" => ResourceType::Chat,
        "group_chat" => ResourceType::GroupChat,
        "project_chat" => ResourceType::ProjectChat,
        "tabular_review" => ResourceType::TabularReview,
        "automation" => ResourceType::Automation,
        "document" => ResourceType::Document,
        other => return Err(AppError::Validation(format!("unknown resource_type: {other}"))),
    })
}

fn parse_permission(s: &str) -> Result<Permission> {
    Ok(match s {
        "read" => Permission::Read,
        "write" => Permission::Write,
        "share" => Permission::Share,
        "delete" => Permission::Delete,
        other => return Err(AppError::Validation(format!("unknown permission: {other}"))),
    })
}

/// React-Query keys touched by a grant change (project lists, team chats, the
/// lead's analytics, the admin grants view).
fn grant_keys() -> Vec<Vec<String>> {
    vec![
        vec!["projects".to_string()],
        vec!["group-chats".to_string()],
        vec!["power-analytics".to_string()],
        vec!["admin-grants".to_string()],
    ]
}

/// Users whose access a grant changes (the principal user, or the group's members)
/// plus the actor — recipients of the live invalidation hint.
async fn grant_audience(state: &AppState, ctx: &AuthContext, principal_type: &str, principal_id: Uuid) -> Vec<Uuid> {
    let mut v = if principal_type == "group" {
        sqlx::query_scalar!("SELECT user_id FROM group_members WHERE group_id = $1", principal_id)
            .fetch_all(&state.pg)
            .await
            .unwrap_or_default()
    } else {
        vec![principal_id]
    };
    v.extend(ctx.user_id);
    v
}

pub async fn create_grant(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Json(body): Json<CreateGrant>,
) -> Result<Json<CreatedId>> {
    // rbac::grant enforces admin; parse the typed inputs first.
    let rt = parse_resource_type(&body.resource_type)?;
    let perm = parse_permission(&body.permission)?;
    let pt = match body.principal_type.as_str() {
        "user" => PrincipalType::User,
        "group" => PrincipalType::Group,
        other => return Err(AppError::Validation(format!("unknown principal_type: {other}"))),
    };
    let id = state.rbac.grant(&state.pg, &ctx, rt, body.resource_id, pt, body.principal_id, perm).await?;
    // A project grant changes who's "on" the project → keep its team chat in step.
    if matches!(rt, ResourceType::Project) {
        if let Err(e) = crate::http::messaging::resync_project_chat_members(&state, body.resource_id).await {
            tracing::warn!(error = %e, project = %body.resource_id, "project chat resync after grant failed");
        }
    }
    let audience = grant_audience(&state, &ctx, &body.principal_type, body.principal_id).await;
    state.hub.send_invalidate(&audience, grant_keys());
    Ok(Json(CreatedId { id }))
}

pub async fn revoke_grant(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(grant_id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    // Capture the grant's target before deletion so we can resync the project
    // chat afterwards (revoke takes only the grant id).
    let target = sqlx::query!(
        r#"SELECT resource_type::text AS "resource_type!", resource_id,
                  principal_type::text AS "principal_type!", principal_id
           FROM access_grants WHERE id = $1"#,
        grant_id
    )
    .fetch_optional(&state.pg)
    .await?;
    state.rbac.revoke_grant(&state.pg, &ctx, grant_id).await?;
    if let Some(t) = target {
        if t.resource_type == "project" {
            if let Err(e) = crate::http::messaging::resync_project_chat_members(&state, t.resource_id).await {
                tracing::warn!(error = %e, project = %t.resource_id, "project chat resync after revoke failed");
            }
        }
        let audience = grant_audience(&state, &ctx, &t.principal_type, t.principal_id).await;
        state.hub.send_invalidate(&audience, grant_keys());
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct GrantsQuery {
    pub resource_type: String,
    pub resource_id: Uuid,
}

#[derive(Serialize)]
pub struct GrantOut {
    pub id: Uuid,
    pub principal_type: String,
    pub principal_id: Uuid,
    pub permission: String,
}

pub async fn list_grants(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Query(q): Query<GrantsQuery>,
) -> Result<Json<Vec<GrantOut>>> {
    let scope = scope_or_forbid(&state, &ctx, permissions::GRANTS_MANAGE).await?;
    let rt = parse_resource_type(&q.resource_type)?;
    // `grants.manage@project` may only inspect grants on its delegated projects
    // (and their project-knowledge / project-chat, whose grant resource_id is the
    // project id). A global holding sees any resource.
    if let Some(pids) = scope.project_ids() {
        let project = matches!(rt, ResourceType::Project | ResourceType::ProjectKnowledge | ResourceType::ProjectChat)
            .then_some(q.resource_id);
        if !project.is_some_and(|p| pids.contains(&p)) {
            return Err(AppError::Forbidden("that resource is outside your delegated projects".into()));
        }
    }
    let rows = sqlx::query!(
        r#"SELECT id, principal_type::text AS "principal_type!", principal_id, permission::text AS "permission!"
           FROM access_grants WHERE resource_type = $1 AND resource_id = $2
           ORDER BY created_at"#,
        rt as ResourceType,
        q.resource_id
    )
    .fetch_all(&state.pg)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|r| GrantOut { id: r.id, principal_type: r.principal_type, principal_id: r.principal_id, permission: r.permission })
            .collect(),
    ))
}

// --- Usage analytics ---------------------------------------------------------

#[derive(Serialize)]
pub struct ModelRollup {
    pub model: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub count: i64,
}

#[derive(Serialize)]
pub struct UserRollup {
    pub user_id: Option<Uuid>,
    pub email: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub count: i64,
}

#[derive(Serialize)]
pub struct AgentRollup {
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub count: i64,
}

/// One day of the 30-day usage series (contiguous; empty days are zero).
#[derive(Serialize)]
pub struct DayPoint {
    pub day: String,
    pub tokens: i64,
    pub messages: i64,
}

#[derive(Serialize)]
pub struct Analytics {
    pub per_model: Vec<ModelRollup>,
    pub per_user: Vec<UserRollup>,
    pub per_agent: Vec<AgentRollup>,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_answers: i64,
    /// 30-day daily tokens + messages (for the area chart).
    pub series: Vec<DayPoint>,
    pub total_users: i64,
    pub new_users_30: i64,
    pub active_7: i64,
    pub active_30: i64,
}

pub async fn usage_analytics(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Analytics>> {
    // Organisation-wide usage analytics needs an unscoped `analytics.view`; a
    // group-scoped holder uses the existing power-user (`led_member_ids`) view.
    let scope = scope_or_forbid(&state, &ctx, permissions::ANALYTICS_VIEW).await?;
    if !scope.is_global() {
        return Err(AppError::Forbidden("organisation-wide analytics requires unscoped analytics.view".into()));
    }
    // Token usage + model traceability live on the chat.assistant.completed
    // audit rows.
    let per_model = sqlx::query!(
        r#"SELECT model_agent_traceability->>'model' AS "model?: String",
                  COALESCE(SUM((token_usage->>'prompt_tokens')::bigint), 0)::bigint AS "prompt_tokens!: i64",
                  COALESCE(SUM((token_usage->>'completion_tokens')::bigint), 0)::bigint AS "completion_tokens!: i64",
                  COUNT(*) AS "count!: i64"
           FROM audit_events
           WHERE action_type = 'chat.assistant.completed'
           GROUP BY model_agent_traceability->>'model'
           ORDER BY COUNT(*) DESC"#
    )
    .fetch_all(&state.pg)
    .await?;
    let per_user = sqlx::query!(
        r#"SELECT a.actor_user_id, u.email AS "email?",
                  COALESCE(SUM((a.token_usage->>'prompt_tokens')::bigint), 0)::bigint AS "prompt_tokens!: i64",
                  COALESCE(SUM((a.token_usage->>'completion_tokens')::bigint), 0)::bigint AS "completion_tokens!: i64",
                  COUNT(*) AS "count!: i64"
           FROM audit_events a
           LEFT JOIN users u ON u.id = a.actor_user_id
           WHERE a.action_type = 'chat.assistant.completed'
           GROUP BY a.actor_user_id, u.email
           ORDER BY COUNT(*) DESC"#
    )
    .fetch_all(&state.pg)
    .await?;
    // Per-Agent: the chat-completion audit stamps the agent id into
    // model_agent_traceability; join agents for the display name (null = a chat
    // run without a named Agent).
    let per_agent = sqlx::query!(
        r#"SELECT a.model_agent_traceability->>'agent_id' AS "agent_id?: String",
                  ag.name AS "agent_name?",
                  COALESCE(SUM((a.token_usage->>'prompt_tokens')::bigint), 0)::bigint AS "prompt_tokens!: i64",
                  COALESCE(SUM((a.token_usage->>'completion_tokens')::bigint), 0)::bigint AS "completion_tokens!: i64",
                  COUNT(*) AS "count!: i64"
           FROM audit_events a
           LEFT JOIN agents ag ON ag.id = (a.model_agent_traceability->>'agent_id')::uuid
           WHERE a.action_type = 'chat.assistant.completed'
           GROUP BY a.model_agent_traceability->>'agent_id', ag.name
           ORDER BY COUNT(*) DESC"#
    )
    .fetch_all(&state.pg)
    .await?;

    let total_prompt_tokens: i64 = per_model.iter().map(|r| r.prompt_tokens).sum();
    let total_completion_tokens: i64 = per_model.iter().map(|r| r.completion_tokens).sum();
    let total_answers: i64 = per_model.iter().map(|r| r.count).sum();

    // 30-day contiguous daily series (generate_series fills empty days with 0).
    let series_rows = sqlx::query!(
        r#"SELECT to_char(d, 'YYYY-MM-DD') AS "day!",
                  COALESCE(SUM((a.token_usage->>'prompt_tokens')::bigint
                             + (a.token_usage->>'completion_tokens')::bigint), 0)::bigint AS "tokens!: i64",
                  COUNT(a.id) AS "messages!: i64"
           FROM generate_series((now() - interval '29 days')::date, now()::date, interval '1 day') d
           LEFT JOIN audit_events a
             ON a.action_type = 'chat.assistant.completed'
            AND date_trunc('day', a.occurred_at)::date = d::date
           GROUP BY d ORDER BY d"#
    )
    .fetch_all(&state.pg)
    .await?;

    let total_users = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM users WHERE deactivated_at IS NULL"#
    )
    .fetch_one(&state.pg)
    .await?;
    let new_users_30 = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "n!" FROM users WHERE created_at >= now() - interval '30 days'"#
    )
    .fetch_one(&state.pg)
    .await?;
    // Active = distinct actors with any audited activity in the window.
    let active_7 = sqlx::query_scalar!(
        r#"SELECT COUNT(DISTINCT actor_user_id) AS "n!" FROM audit_events
           WHERE actor_user_id IS NOT NULL AND occurred_at >= now() - interval '7 days'"#
    )
    .fetch_one(&state.pg)
    .await?;
    let active_30 = sqlx::query_scalar!(
        r#"SELECT COUNT(DISTINCT actor_user_id) AS "n!" FROM audit_events
           WHERE actor_user_id IS NOT NULL AND occurred_at >= now() - interval '30 days'"#
    )
    .fetch_one(&state.pg)
    .await?;

    Ok(Json(Analytics {
        per_model: per_model
            .into_iter()
            .map(|r| ModelRollup { model: r.model, prompt_tokens: r.prompt_tokens, completion_tokens: r.completion_tokens, count: r.count })
            .collect(),
        per_user: per_user
            .into_iter()
            .map(|r| UserRollup { user_id: r.actor_user_id, email: r.email, prompt_tokens: r.prompt_tokens, completion_tokens: r.completion_tokens, count: r.count })
            .collect(),
        per_agent: per_agent
            .into_iter()
            .map(|r| AgentRollup { agent_id: r.agent_id, agent_name: r.agent_name, prompt_tokens: r.prompt_tokens, completion_tokens: r.completion_tokens, count: r.count })
            .collect(),
        total_prompt_tokens,
        total_completion_tokens,
        total_answers,
        series: series_rows
            .into_iter()
            .map(|r| DayPoint { day: r.day, tokens: r.tokens, messages: r.messages })
            .collect(),
        total_users,
        new_users_30,
        active_7,
        active_30,
    }))
}
