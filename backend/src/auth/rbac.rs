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

//! RBAC — flat AccessGrants, no inheritance.
//!
//! `(resource_type, resource_id, principal_type, principal_id, permission)`.
//! A check passes if the caller is an admin level (client-admin or ephemeral
//! super-admin), or a matching grant exists for the user **or any group they
//! belong to**. There is no cascade from any parent — Projects are flat.

use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::ext::{self, RbacPolicy as _};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "grant_resource_type", rename_all = "snake_case")]
pub enum ResourceType {
    Project,
    ProjectKnowledge,
    Agent,
    Skill,
    Prompt,
    Chat,
    GroupChat,
    ProjectChat,
    TabularReview,
    Automation,
    Document,
    McpServer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "principal_type", rename_all = "snake_case")]
pub enum PrincipalType {
    User,
    Group,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "permission", rename_all = "snake_case")]
pub enum Permission {
    Read,
    Write,
    Share,
    Delete,
}

impl ResourceType {
    pub fn as_str(self) -> &'static str {
        // matches the enum labels; used in audit payloads
        match self {
            ResourceType::Project => "project",
            ResourceType::ProjectKnowledge => "project_knowledge",
            ResourceType::Agent => "agent",
            ResourceType::Skill => "skill",
            ResourceType::Prompt => "prompt",
            ResourceType::Chat => "chat",
            ResourceType::GroupChat => "group_chat",
            ResourceType::ProjectChat => "project_chat",
            ResourceType::TabularReview => "tabular_review",
            ResourceType::Automation => "automation",
            ResourceType::Document => "document",
            ResourceType::McpServer => "mcp_server",
        }
    }
}

impl Permission {
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Read => "read",
            Permission::Write => "write",
            Permission::Share => "share",
            Permission::Delete => "delete",
        }
    }
}

/// The Core [`ext::RbacPolicy`]: flat AccessGrants with an admin override and
/// admin-only granting. Enterprise injects its own (custom roles / delegated
/// admin / ABAC) via [`crate::state::AppStateBuilder::with_rbac`]; the default
/// guard/mutation methods live on the trait and are inherited unchanged.
pub struct FlatRbacPolicy;

#[async_trait::async_trait]
impl ext::RbacPolicy for FlatRbacPolicy {
    async fn can(
        &self,
        pool: &PgPool,
        ctx: &AuthContext,
        resource_type: ResourceType,
        resource_id: Uuid,
        permission: Permission,
    ) -> Result<bool> {
        if ctx.is_admin() {
            return Ok(true);
        }
        let Some(user_id) = ctx.user_id else {
            return Ok(false);
        };

        let allowed = sqlx::query_scalar!(
            r#"
        SELECT EXISTS (
            SELECT 1 FROM access_grants g
            WHERE g.resource_type = $1
              AND g.resource_id = $2
              AND g.permission = $3
              AND (
                    (g.principal_type = 'user'  AND g.principal_id = $4)
                 OR (g.principal_type = 'group' AND g.principal_id IN
                        (SELECT group_id FROM group_members WHERE user_id = $4))
              )
        ) AS "allowed!"
        "#,
            resource_type as ResourceType,
            resource_id,
            permission as Permission,
            user_id,
        )
        .fetch_one(pool)
        .await?;

        Ok(allowed)
    }

    /// Core grant authorisation: admin levels only. (Enterprise's delegated admin
    /// would let a resource owner grant on their own resources.)
    async fn may_grant(
        &self,
        _pool: &PgPool,
        granter: &AuthContext,
        _resource_type: ResourceType,
        _resource_id: Uuid,
    ) -> Result<bool> {
        Ok(granter.is_admin())
    }
}

/// Does `ctx` hold `permission` on this resource? Thin delegator to the Core
/// [`FlatRbacPolicy`] — for bare-pool helpers/tests that have no `AppState`.
/// Stateful call-sites go through the `state.rbac` slot instead.
pub async fn can(
    pool: &PgPool,
    ctx: &AuthContext,
    resource_type: ResourceType,
    resource_id: Uuid,
    permission: Permission,
) -> Result<bool> {
    FlatRbacPolicy.can(pool, ctx, resource_type, resource_id, permission).await
}

/// Project access *with the owner short-circuit*: the project's
/// `owner_user_id` and admin levels always pass; otherwise a flat `permission`
/// grant on the project is required. Mirrors the local checks in
/// `http/documents.rs` and `http/tabular.rs` — use this for any project-scoped
/// resource (memory facts, project prompts) so an owner is never locked out of
/// their own project.
pub async fn project_can(
    pool: &PgPool,
    ctx: &AuthContext,
    project_id: Uuid,
    permission: Permission,
) -> Result<bool> {
    FlatRbacPolicy.project_can(pool, ctx, project_id, permission).await
}

/// `project_can` as a guard. 403 when denied. Delegates to [`FlatRbacPolicy`].
pub async fn require_project(
    pool: &PgPool,
    ctx: &AuthContext,
    project_id: Uuid,
    permission: Permission,
) -> Result<()> {
    FlatRbacPolicy.require_project(pool, ctx, project_id, permission).await
}

/// `can` as a guard. 403 when denied. Delegates to [`FlatRbacPolicy`].
pub async fn require(
    pool: &PgPool,
    ctx: &AuthContext,
    resource_type: ResourceType,
    resource_id: Uuid,
    permission: Permission,
) -> Result<()> {
    FlatRbacPolicy.require(pool, ctx, resource_type, resource_id, permission).await
}

/// Do two users belong to the same "circle" — i.e. co-belong to ≥1 project (as owner
/// or via any access-grant, direct or through a group) OR ≥1 RBAC group? This is the
/// boundary a non-admin's messaging is scoped to (a regular employee may only DM /
/// group-chat people they share a matter or group with). Admins bypass this entirely.
pub async fn shares_circle(pool: &PgPool, me: Uuid, other: Uuid) -> Result<bool> {
    if me == other {
        return Ok(true);
    }
    let yes: bool = sqlx::query_scalar!(
        r#"SELECT EXISTS (
               SELECT 1 FROM projects p
               WHERE p.archived_at IS NULL
                 AND (p.owner_user_id = $1 OR EXISTS (
                       SELECT 1 FROM access_grants g WHERE g.resource_type = 'project' AND g.resource_id = p.id
                         AND ((g.principal_type = 'user'  AND g.principal_id = $1)
                           OR (g.principal_type = 'group' AND g.principal_id IN (SELECT group_id FROM group_members WHERE user_id = $1)))))
                 AND (p.owner_user_id = $2 OR EXISTS (
                       SELECT 1 FROM access_grants g WHERE g.resource_type = 'project' AND g.resource_id = p.id
                         AND ((g.principal_type = 'user'  AND g.principal_id = $2)
                           OR (g.principal_type = 'group' AND g.principal_id IN (SELECT group_id FROM group_members WHERE user_id = $2)))))
               UNION ALL
               SELECT 1 FROM group_members a JOIN group_members b ON a.group_id = b.group_id
               WHERE a.user_id = $1 AND b.user_id = $2
           ) AS "e!""#,
        me,
        other
    )
    .fetch_one(pool)
    .await?;
    Ok(yes)
}

/// Guard form of [`shares_circle`] — admin bypasses; otherwise 403 unless in-circle.
pub async fn require_circle(ctx: &AuthContext, pool: &PgPool, other: Uuid) -> Result<()> {
    if ctx.is_admin() {
        return Ok(());
    }
    let me = ctx.user_id.ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    if shares_circle(pool, me, other).await? {
        Ok(())
    } else {
        Err(AppError::Forbidden("that user is outside your team circle".into()))
    }
}

/// The distinct user ids a power-user *lead* oversees — members of groups they
/// created, plus the user/group grantees (and owner = self) of projects they own,
/// plus the lead themselves. This is the audience for the lead's "Power" analytics:
/// the teams they actually lead, not the wider circle. Active projects only.
pub async fn led_member_ids(pool: &PgPool, me: Uuid) -> Result<Vec<Uuid>> {
    let ids = sqlx::query_scalar!(
        r#"SELECT DISTINCT uid AS "uid!" FROM (
               SELECT user_id AS uid FROM group_members
                WHERE group_id IN (SELECT id FROM groups WHERE created_by = $1)
               UNION
               SELECT g.principal_id AS uid FROM access_grants g
                 JOIN projects p ON p.id = g.resource_id
                  AND p.owner_user_id = $1 AND p.archived_at IS NULL
                WHERE g.resource_type = 'project' AND g.principal_type = 'user'
               UNION
               SELECT gm.user_id AS uid FROM access_grants g
                 JOIN projects p ON p.id = g.resource_id
                  AND p.owner_user_id = $1 AND p.archived_at IS NULL
                 JOIN group_members gm ON gm.group_id = g.principal_id
                WHERE g.resource_type = 'project' AND g.principal_type = 'group'
               UNION
               SELECT $1 AS uid
           ) m"#,
        me,
    )
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// Create an AccessGrant. Delegates to [`FlatRbacPolicy`] (admin-only, audited).
#[allow(clippy::too_many_arguments)]
pub async fn grant(
    pool: &PgPool,
    granter: &AuthContext,
    resource_type: ResourceType,
    resource_id: Uuid,
    principal_type: PrincipalType,
    principal_id: Uuid,
    permission: Permission,
) -> Result<Uuid> {
    FlatRbacPolicy
        .grant(pool, granter, resource_type, resource_id, principal_type, principal_id, permission)
        .await
}

/// Revoke an AccessGrant by id. Delegates to [`FlatRbacPolicy`] (admin-only, audited).
pub async fn revoke_grant(pool: &PgPool, granter: &AuthContext, grant_id: Uuid) -> Result<()> {
    FlatRbacPolicy.revoke_grant(pool, granter, grant_id).await
}
