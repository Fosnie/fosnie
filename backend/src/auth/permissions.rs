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

//! The fine-grained **permission catalogue** — the source of truth for every
//! admin authorisation gate, shared by the backend guards, the assignment
//! validator and the admin UI.
//!
//! A *permission* is a low-cardinality string (`users.manage`, `audit.view`, …)
//! naming one administrative capability. Every Core admin gate resolves through
//! [`crate::ext::RbacPolicy::require_permission`]; the Core default treats *any*
//! permission as `is_admin()`, so Core behaviour is unchanged (an admin holds
//! every permission, a non-admin none). An Enterprise `RbacPolicy` reads custom
//! roles + delegated scopes to grant a subset.
//!
//! **Terminology (do not conflate):** these fine-grained strings are
//! *permissions*. The separate `require_capability(...)` mechanism gates
//! Enterprise *licence features* (`federated_sso`, `custom_rbac`, …) and is
//! called a *capability*. A delegated admin endpoint may check both: the
//! capability (is the feature licensed?) and the permission (may this caller?).
//!
//! The permission strings themselves are Core (the catalogue must list every
//! Core gate); the roles/assignments that map principals to a subset of them are
//! Enterprise. In a Core-only deploy the catalogue is inert — the default policy
//! never consults it.

/// How a permission may be *scoped* when assigned through a delegated role.
///
/// Scope narrows an otherwise-global permission to a set of groups or projects
/// (delegated administration). A permission whose semantics have no
/// meaningful narrowing is [`ScopeKind::None`]; assigning it with a scope is a
/// validation error (400), never a silent global grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    /// Global only — cannot be narrowed (e.g. `providers.manage`).
    None,
    /// May be narrowed to a set of groups (e.g. `users.manage@group=HR`).
    Group,
    /// May be narrowed to a set of projects (e.g. `grants.manage@project`).
    Project,
}

impl ScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeKind::None => "none",
            ScopeKind::Group => "group",
            ScopeKind::Project => "project",
        }
    }
}

/// The resolved scope at which a caller holds a given permission — the answer a
/// scope-aware Core handler needs to filter its lists and guard its mutations
/// The Core default policy only ever returns [`Global`](Self::Global)
/// (an admin) or [`Denied`](Self::Denied); an Enterprise policy also returns the
/// narrowed [`Groups`](Self::Groups)/[`Projects`](Self::Projects) forms for a
/// delegated admin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionScope {
    /// The caller does not hold the permission at all.
    Denied,
    /// Held globally — no narrowing (the pre-catalogue admin case).
    Global,
    /// Held only for members of these groups.
    Groups(Vec<uuid::Uuid>),
    /// Held only for these projects.
    Projects(Vec<uuid::Uuid>),
}

impl PermissionScope {
    /// True when the caller holds the permission in no form.
    pub fn is_denied(&self) -> bool {
        matches!(self, PermissionScope::Denied)
    }

    /// True when the caller holds the permission with no narrowing.
    pub fn is_global(&self) -> bool {
        matches!(self, PermissionScope::Global)
    }

    /// The groups the permission is narrowed to, if scoped to groups.
    pub fn group_ids(&self) -> Option<&[uuid::Uuid]> {
        match self {
            PermissionScope::Groups(ids) => Some(ids),
            _ => None,
        }
    }

    /// The projects the permission is narrowed to, if scoped to projects.
    pub fn project_ids(&self) -> Option<&[uuid::Uuid]> {
        match self {
            PermissionScope::Projects(ids) => Some(ids),
            _ => None,
        }
    }
}

/// A broad grouping used purely to lay the catalogue out in the admin UI. It has
/// no authorisation meaning — the permission string is the unit of authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionArea {
    Users,
    Groups,
    Sharing,
    Providers,
    Identity,
    Content,
    Compliance,
    Observability,
    System,
}

impl PermissionArea {
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionArea::Users => "users",
            PermissionArea::Groups => "groups",
            PermissionArea::Sharing => "sharing",
            PermissionArea::Providers => "providers",
            PermissionArea::Identity => "identity",
            PermissionArea::Content => "content",
            PermissionArea::Compliance => "compliance",
            PermissionArea::Observability => "observability",
            PermissionArea::System => "system",
        }
    }
}

/// One catalogue entry: the permission string plus its UI/validation metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct PermissionDef {
    /// The canonical permission string used at every gate.
    pub name: &'static str,
    /// One-line human description (admin UI + generated docs).
    pub description: &'static str,
    /// Which layout group it belongs to in the UI.
    pub area: PermissionArea,
    /// Whether — and how — a delegated assignment of this permission may be
    /// narrowed.
    pub scope: ScopeKind,
}

// ---- Permission string constants (the canonical names). ---------------------

pub const USERS_MANAGE: &str = "users.manage";
pub const USERS_VIEW: &str = "users.view";
pub const GROUPS_MANAGE: &str = "groups.manage";
pub const GRANTS_MANAGE: &str = "grants.manage";
pub const PROVIDERS_MANAGE: &str = "providers.manage";
pub const CONFIG_MANAGE: &str = "config.manage";
pub const MCP_MANAGE: &str = "mcp.manage";
pub const VOICE_MANAGE: &str = "voice.manage";
pub const IDENTITY_MANAGE: &str = "identity.manage";
pub const ANNOUNCEMENTS_MANAGE: &str = "announcements.manage";
pub const EXPORT_RUN: &str = "export.run";
pub const AUDIT_VIEW: &str = "audit.view";
pub const BRANDING_MANAGE: &str = "branding.manage";
pub const HOLDS_MANAGE: &str = "holds.manage";
pub const MODERATION_SETTINGS: &str = "moderation.settings";
pub const AGENTS_MANAGE: &str = "agents.manage";
pub const SKILLS_MANAGE: &str = "skills.manage";
pub const TOOLS_MANAGE: &str = "tools.manage";
pub const WORKFLOWS_MANAGE: &str = "workflows.manage";
pub const ANALYTICS_VIEW: &str = "analytics.view";
pub const INTEGRATIONS_MANAGE: &str = "integrations.manage";
pub const GROUNDEDNESS_VIEW: &str = "groundedness.view";
pub const FEEDBACK_VIEW: &str = "feedback.view";
pub const POLICIES_MANAGE: &str = "policies.manage";

/// The full catalogue — the single source of truth. Order is the UI display
/// order (grouped by area). **Every Core admin gate migrated off `is_admin()`
/// must name one of these**; the assignment validator and the admin UI both
/// enumerate this slice, so nothing may gate on a permission absent from here.
pub const PERMISSION_CATALOG: &[PermissionDef] = &[
    // Users.
    PermissionDef {
        name: USERS_VIEW,
        description: "View the user directory.",
        area: PermissionArea::Users,
        scope: ScopeKind::Group,
    },
    PermissionDef {
        name: USERS_MANAGE,
        description: "Deactivate, reactivate and manually create users (never change platform role).",
        area: PermissionArea::Users,
        scope: ScopeKind::Group,
    },
    // Groups.
    PermissionDef {
        name: GROUPS_MANAGE,
        description: "Manage group membership, renaming and feature flags.",
        area: PermissionArea::Groups,
        scope: ScopeKind::Group,
    },
    // Sharing.
    PermissionDef {
        name: GRANTS_MANAGE,
        description: "Create, revoke and list access grants on resources.",
        area: PermissionArea::Sharing,
        scope: ScopeKind::Project,
    },
    // Providers / configuration.
    PermissionDef {
        name: PROVIDERS_MANAGE,
        description: "Configure inference providers, BYOK and the embedding index.",
        area: PermissionArea::Providers,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: CONFIG_MANAGE,
        description: "Change deployment configuration and theme settings.",
        area: PermissionArea::Providers,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: MCP_MANAGE,
        description: "Register, approve and remove MCP servers.",
        area: PermissionArea::Providers,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: VOICE_MANAGE,
        description: "Configure live-voice settings.",
        area: PermissionArea::Providers,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: INTEGRATIONS_MANAGE,
        description: "View integration connectors (activation remains break-glass).",
        area: PermissionArea::Providers,
        scope: ScopeKind::None,
    },
    // Identity.
    PermissionDef {
        name: IDENTITY_MANAGE,
        description: "Manage federated SSO, SCIM tokens and identity settings.",
        area: PermissionArea::Identity,
        scope: ScopeKind::None,
    },
    // Content.
    PermissionDef {
        name: AGENTS_MANAGE,
        description: "Manage shared agents.",
        area: PermissionArea::Content,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: SKILLS_MANAGE,
        description: "Manage shared skills.",
        area: PermissionArea::Content,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: TOOLS_MANAGE,
        description: "Manage the tool catalogue: switch native tools on/off and edit their descriptions.",
        area: PermissionArea::Content,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: ANNOUNCEMENTS_MANAGE,
        description: "Manage announcements and the welcome message.",
        area: PermissionArea::Content,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: WORKFLOWS_MANAGE,
        description: "Create and manage event-driven workflows.",
        area: PermissionArea::Content,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: BRANDING_MANAGE,
        description: "Manage white-label branding.",
        area: PermissionArea::Content,
        scope: ScopeKind::None,
    },
    // Compliance.
    PermissionDef {
        name: AUDIT_VIEW,
        description: "View the audit log, anomalies, evidence and checkpoints.",
        area: PermissionArea::Compliance,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: EXPORT_RUN,
        description: "Run project, chat and audit exports.",
        area: PermissionArea::Compliance,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: HOLDS_MANAGE,
        description: "Set and clear legal holds.",
        area: PermissionArea::Compliance,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: MODERATION_SETTINGS,
        description: "Manage moderation settings and reviewer assignments.",
        area: PermissionArea::Compliance,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: POLICIES_MANAGE,
        description: "Manage attribute-based (ABAC) policies.",
        area: PermissionArea::Compliance,
        scope: ScopeKind::None,
    },
    // Observability.
    PermissionDef {
        name: ANALYTICS_VIEW,
        description: "View usage analytics.",
        area: PermissionArea::Observability,
        scope: ScopeKind::Group,
    },
    PermissionDef {
        name: GROUNDEDNESS_VIEW,
        description: "View groundedness/verification analytics.",
        area: PermissionArea::Observability,
        scope: ScopeKind::None,
    },
    PermissionDef {
        name: FEEDBACK_VIEW,
        description: "View submitted message feedback.",
        area: PermissionArea::Observability,
        scope: ScopeKind::None,
    },
];

/// Look a permission up in the catalogue by its string.
pub fn lookup(name: &str) -> Option<&'static PermissionDef> {
    PERMISSION_CATALOG.iter().find(|p| p.name == name)
}

/// Is this a known permission string?
pub fn is_known(name: &str) -> bool {
    lookup(name).is_some()
}

/// The scope kind for a permission, or `None` for an unknown string.
pub fn scope_of(name: &str) -> Option<ScopeKind> {
    lookup(name).map(|p| p.scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for p in PERMISSION_CATALOG {
            assert!(seen.insert(p.name), "duplicate permission in catalogue: {}", p.name);
        }
    }

    #[test]
    fn constants_are_all_catalogued() {
        // Every exported constant must appear in the catalogue (guards against a
        // gate naming a permission the UI/validator cannot see).
        for name in [
            USERS_MANAGE,
            USERS_VIEW,
            GROUPS_MANAGE,
            GRANTS_MANAGE,
            PROVIDERS_MANAGE,
            CONFIG_MANAGE,
            MCP_MANAGE,
            VOICE_MANAGE,
            IDENTITY_MANAGE,
            ANNOUNCEMENTS_MANAGE,
            EXPORT_RUN,
            AUDIT_VIEW,
            BRANDING_MANAGE,
            HOLDS_MANAGE,
            MODERATION_SETTINGS,
            AGENTS_MANAGE,
            SKILLS_MANAGE,
            TOOLS_MANAGE,
            WORKFLOWS_MANAGE,
            ANALYTICS_VIEW,
            INTEGRATIONS_MANAGE,
            GROUNDEDNESS_VIEW,
            FEEDBACK_VIEW,
            POLICIES_MANAGE,
        ] {
            assert!(is_known(name), "constant {name} missing from PERMISSION_CATALOG");
        }
    }

    #[test]
    fn scope_kinds_match_spec() {
        assert_eq!(scope_of(USERS_MANAGE), Some(ScopeKind::Group));
        assert_eq!(scope_of(GRANTS_MANAGE), Some(ScopeKind::Project));
        assert_eq!(scope_of(PROVIDERS_MANAGE), Some(ScopeKind::None));
        assert_eq!(scope_of("nonexistent.perm"), None);
    }
}
