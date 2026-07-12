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

//! Integrations — the dual-mode external-connector framework.
//!
//! Zero-egress is the platform default. Every connector (web-search, the DMS
//! connectors, mail, MCP) ships **present-but-dormant**: the code is here, the
//! switch is off. Activation is an explicit operator (admin) action and is
//! audited. Until a connector is enabled, no outbound call leaves the perimeter.
//!
//! There is **no `connectors` table** in v1 (binding data-model): a connector's
//! enabled flag is a typed `config_settings` row keyed `integration.<kind>.enabled`
//! — absence of the key means dormant. Credential storage and per-connector
//! per-user grants are Pass-2.
//!
//! The single choke-point is [`guard_egress`]: every would-be external call must
//! pass it first. Dormant ⇒ the attempt is audited (`integration.blocked`) and
//! refused; enabled ⇒ the call is audited (`integration.call`) and proceeds.

pub mod dms;
pub mod mail;

use crate::audit::{self, AuditEvent, AuditOutcome};
use crate::auth::AuthContext;
use crate::config::runtime::{self, ConfigValueType};
use crate::error::{AppError, Result};
use crate::state::AppState;

/// The closed set of connector kinds the platform knows. New kinds are added by
/// code change (like the tool registry), never by config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ConnectorKind {
    WebSearch,
    IManage,
    NetDocuments,
    Outlook,
    Gmail,
    Mcp,
    /// A deployment-defined custom HTTP tool. Its per-tool
    /// `requires_egress` flag drives the SSRF mode; this single global flag
    /// (`integration.custom_tool.enabled`) is the connector-level kill-switch.
    CustomTool,
}

/// Broad grouping of a connector (drives where it routes / how it is presented).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ConnectorCategory {
    Web,
    Dms,
    Mail,
    Mcp,
    Tool,
}

impl ConnectorCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectorCategory::Web => "web",
            ConnectorCategory::Dms => "dms",
            ConnectorCategory::Mail => "mail",
            ConnectorCategory::Mcp => "mcp",
            ConnectorCategory::Tool => "tool",
        }
    }
}

impl ConnectorKind {
    /// Every known connector kind.
    pub fn all() -> &'static [ConnectorKind] {
        use ConnectorKind::*;
        &[WebSearch, IManage, NetDocuments, Outlook, Gmail, Mcp, CustomTool]
    }

    /// Stable wire/config form.
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectorKind::WebSearch => "web_search",
            ConnectorKind::IManage => "imanage",
            ConnectorKind::NetDocuments => "netdocuments",
            ConnectorKind::Outlook => "outlook",
            ConnectorKind::Gmail => "gmail",
            ConnectorKind::Mcp => "mcp",
            ConnectorKind::CustomTool => "custom_tool",
        }
    }

    #[allow(clippy::should_implement_trait)] // intentional: a fallible enum lookup, not std::str::FromStr
    pub fn from_str(s: &str) -> Option<ConnectorKind> {
        Some(match s {
            "web_search" => ConnectorKind::WebSearch,
            "imanage" => ConnectorKind::IManage,
            "netdocuments" => ConnectorKind::NetDocuments,
            "outlook" => ConnectorKind::Outlook,
            "gmail" => ConnectorKind::Gmail,
            "mcp" => ConnectorKind::Mcp,
            "custom_tool" => ConnectorKind::CustomTool,
            _ => return None,
        })
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ConnectorKind::WebSearch => "Web search",
            ConnectorKind::IManage => "iManage",
            ConnectorKind::NetDocuments => "NetDocuments",
            ConnectorKind::Outlook => "Outlook",
            ConnectorKind::Gmail => "Gmail",
            ConnectorKind::Mcp => "MCP client",
            ConnectorKind::CustomTool => "Custom tool",
        }
    }

    pub fn category(self) -> ConnectorCategory {
        match self {
            ConnectorKind::WebSearch => ConnectorCategory::Web,
            ConnectorKind::IManage | ConnectorKind::NetDocuments => ConnectorCategory::Dms,
            ConnectorKind::Outlook | ConnectorKind::Gmail => ConnectorCategory::Mail,
            ConnectorKind::Mcp => ConnectorCategory::Mcp,
            ConnectorKind::CustomTool => ConnectorCategory::Tool,
        }
    }

    /// Every connector is an egress surface — that is the whole point of the
    /// dual-mode gate. Kept as a method so callers read intent, not a constant.
    pub fn requires_egress(self) -> bool {
        true
    }

    /// The `config_settings` key holding this connector's enabled flag.
    fn enabled_key(self) -> String {
        format!("integration.{}.enabled", self.as_str())
    }
}

/// A connector and its current activation state, for listing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Descriptor {
    pub kind: &'static str,
    pub display_name: &'static str,
    pub category: &'static str,
    pub requires_egress: bool,
    pub enabled: bool,
}

/// Is this connector activated? Missing config key ⇒ dormant (false).
pub async fn is_enabled(pg: &sqlx::PgPool, kind: ConnectorKind) -> Result<bool> {
    let entry = runtime::get(pg, &kind.enabled_key()).await?;
    Ok(entry.map(|e| e.value == "true").unwrap_or(false))
}

/// Activate or deactivate a connector. The caller must already be authorised
/// (the HTTP layer gates on admin). Persists the flag via `config::runtime::set`
/// (which audits `config.changed` atomically) and appends the domain event
/// `integration.activated` / `integration.deactivated`.
pub async fn set_enabled(
    state: &AppState,
    ctx: &AuthContext,
    kind: ConnectorKind,
    enabled: bool,
) -> Result<()> {
    runtime::set(
        &state.pg,
        &kind.enabled_key(),
        if enabled { "true" } else { "false" },
        ConfigValueType::Bool,
        "global",
        ctx.user_id,
        ctx.role.as_str(),
    )
    .await?;

    let action = if enabled { "integration.activated" } else { "integration.deactivated" };
    let mut ev = AuditEvent::action(action, ctx.role.as_str());
    ev.actor_user_id = ctx.user_id;
    ev.resource_type = Some("integration".into());
    ev.payload = Some(serde_json::json!({ "kind": kind.as_str() }));
    let _ = audit::append(&state.pg, &ev).await;
    Ok(())
}

/// One connector's descriptor + live state.
pub async fn descriptor(pg: &sqlx::PgPool, kind: ConnectorKind) -> Result<Descriptor> {
    Ok(Descriptor {
        kind: kind.as_str(),
        display_name: kind.display_name(),
        category: kind.category().as_str(),
        requires_egress: kind.requires_egress(),
        enabled: is_enabled(pg, kind).await?,
    })
}

/// All connectors with their state. `enabled_only` filters to the activated set
/// (the user-facing view); the full list is the admin view.
pub async fn list_descriptors(pg: &sqlx::PgPool, enabled_only: bool) -> Result<Vec<Descriptor>> {
    let mut out = Vec::new();
    for &kind in ConnectorKind::all() {
        let d = descriptor(pg, kind).await?;
        if !enabled_only || d.enabled {
            out.push(d);
        }
    }
    Ok(out)
}

/// **The zero-egress gate.** Every outbound path calls this first. Dormant ⇒
/// audit the blocked attempt and refuse (403). Enabled ⇒ audit the call and
/// allow it to proceed. This is what guarantees nothing leaves the perimeter in
/// a default (no connector enabled) deployment.
pub async fn guard_egress(state: &AppState, ctx: &AuthContext, kind: ConnectorKind) -> Result<()> {
    if is_enabled(&state.pg, kind).await? {
        let mut ev = AuditEvent::action("integration.call", ctx.role.as_str());
        ev.actor_user_id = ctx.user_id;
        ev.resource_type = Some("integration".into());
        ev.payload = Some(serde_json::json!({ "kind": kind.as_str() }));
        let _ = audit::append(&state.pg, &ev).await;
        Ok(())
    } else {
        let mut ev = AuditEvent::action("integration.blocked", ctx.role.as_str());
        ev.actor_user_id = ctx.user_id;
        ev.resource_type = Some("integration".into());
        ev.outcome = AuditOutcome::Failure;
        ev.outcome_reason = Some("connector dormant (zero-egress default)".into());
        ev.payload = Some(serde_json::json!({ "kind": kind.as_str() }));
        let _ = audit::append(&state.pg, &ev).await;
        Err(AppError::Forbidden(format!(
            "connector '{}' is dormant (zero-egress default); an admin must enable it",
            kind.as_str()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_str_round_trips() {
        for &k in ConnectorKind::all() {
            assert_eq!(ConnectorKind::from_str(k.as_str()), Some(k), "round-trip {k:?}");
        }
        assert_eq!(ConnectorKind::from_str("nope"), None);
    }

    #[test]
    fn closed_set_is_complete() {
        assert_eq!(ConnectorKind::all().len(), 7);
    }

    #[test]
    fn all_kinds_are_egress_surfaces() {
        assert!(ConnectorKind::all().iter().all(|k| k.requires_egress()));
    }

    #[test]
    fn categories_map_as_specified() {
        use ConnectorCategory::*;
        assert_eq!(ConnectorKind::WebSearch.category(), Web);
        assert_eq!(ConnectorKind::IManage.category(), Dms);
        assert_eq!(ConnectorKind::NetDocuments.category(), Dms);
        assert_eq!(ConnectorKind::Outlook.category(), Mail);
        assert_eq!(ConnectorKind::Gmail.category(), Mail);
        assert_eq!(ConnectorKind::Mcp.category(), Mcp);
        assert_eq!(ConnectorKind::CustomTool.category(), Tool);
    }

    #[test]
    fn enabled_key_convention() {
        assert_eq!(ConnectorKind::IManage.enabled_key(), "integration.imanage.enabled");
    }
}
