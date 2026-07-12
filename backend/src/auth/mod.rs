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

//! Auth & RBAC.
//!
//! In-house wrapper over the pinned single-maintainer crate: the rest of the
//! codebase sees only [`AuthContext`], [`PlatformRole`], the [`keycloak`]
//! extractor, [`rbac`] checks and [`breakglass`] — never `axum-keycloak-auth`
//! directly. Only [`keycloak`] touches that crate, so a future swap to
//! `jsonwebtoken` + `openid` stays a wrapper-internal change (§A.1). Browser
//! login is keycloak-js (PKCE) + Bearer JWT — there is no server-side OIDC flow.
//! Crate versions are pinned in `Cargo.toml`, updates gated by the auth test.

pub mod breakglass;
pub mod keycloak;
pub mod local;
pub mod mfa;
pub mod permissions;
pub mod provisioning;
pub mod rbac;

use uuid::Uuid;

/// The four principals the architecture distinguishes (schema §3.1). The three
/// persistent roles are normalised from Keycloak; `SuperAdmin` is the ephemeral
/// break-glass principal and is **never** sourced from Keycloak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "platform_role", rename_all = "snake_case")]
pub enum PlatformRole {
    SuperAdmin,
    ClientAdmin,
    PowerUser,
    User,
}

impl PlatformRole {
    pub fn as_str(self) -> &'static str {
        match self {
            PlatformRole::SuperAdmin => "super_admin",
            PlatformRole::ClientAdmin => "client_admin",
            PlatformRole::PowerUser => "power_user",
            PlatformRole::User => "user",
        }
    }

    /// Admin levels that override AccessGrants (client-admin and the ephemeral
    /// super-admin).
    pub fn is_admin(self) -> bool {
        matches!(self, PlatformRole::SuperAdmin | PlatformRole::ClientAdmin)
    }
}

/// The authenticated caller, as the platform sees it — independent of how they
/// proved identity (Keycloak token or break-glass grant).
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// Keycloak `sub` / local user id. `None` for a pure break-glass principal
    /// (which has no Keycloak account).
    pub user_id: Option<Uuid>,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub role: PlatformRole,
    /// True when acting under an active ephemeral super-admin grant.
    pub break_glass: bool,
    /// True when this is a restricted *enrolment-only* local session:
    /// `auth.require_mfa` is on and the user has not yet enrolled a factor, so
    /// the session may only reach the MFA setup/confirm + logout/whoami surface
    /// until enrolment completes. Always `false` on the Keycloak path.
    pub mfa_enroll_only: bool,
}

impl AuthContext {
    pub fn is_admin(&self) -> bool {
        self.role.is_admin()
    }

    /// True only under an *active* ephemeral super-admin grant:
    /// the role is `SuperAdmin` AND it was proven via break-glass. A persistent
    /// client-admin is deliberately excluded — the super-admin surface is for
    /// sensitive/infrastructure actions (integration connectors/secrets, and the
    /// reserved boot-config / vector-snapshot operations).
    pub fn is_super_admin(&self) -> bool {
        self.role == PlatformRole::SuperAdmin && self.break_glass
    }
}

/// Gate a sensitive super-admin-only operation. Reserved surface: integration
/// connector activation (which carries secrets/egress), break-glass grant
/// administration, and — when wired — boot config, the deployment layer, and
/// vector snapshots taken outside the platform.
pub fn require_super_admin(ctx: &AuthContext) -> crate::error::Result<()> {
    if ctx.is_super_admin() {
        Ok(())
    } else {
        Err(crate::error::AppError::Forbidden("super-admin (active break-glass) required".into()))
    }
}

/// Load an [`AuthContext`] from the local user cache by id — used by the
/// WebSocket resume path (reconnect within the resume window, no fresh token).
pub async fn load_context(
    pg: &sqlx::PgPool,
    user_id: Uuid,
) -> crate::error::Result<AuthContext> {
    let row = sqlx::query!(
        r#"SELECT email, display_name, role AS "role: PlatformRole"
           FROM users WHERE id = $1 AND deactivated_at IS NULL"#,
        user_id
    )
    .fetch_optional(pg)
    .await?
    .ok_or_else(|| crate::error::AppError::Unauthorized("user not found or deactivated".into()))?;

    Ok(AuthContext {
        user_id: Some(user_id),
        email: Some(row.email),
        display_name: Some(row.display_name),
        role: row.role,
        break_glass: false,
        mfa_enroll_only: false,
    })
}

/// Normalise raw Keycloak realm/client role strings onto the three platform
/// roles. Highest privilege wins. `SuperAdmin`
/// is never produced here — it only ever comes from break-glass.
pub fn normalise_roles<S: AsRef<str>>(roles: &[S]) -> PlatformRole {
    let mut result = PlatformRole::User;
    for r in roles {
        match r.as_ref() {
            "admin" => return PlatformRole::ClientAdmin, // highest Keycloak-sourced role
            "power_user" if result == PlatformRole::User => result = PlatformRole::PowerUser,
            _ => {}
        }
    }
    result
}
