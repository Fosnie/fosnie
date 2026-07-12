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

//! Integrations admin REST: list connectors and activate /
//! deactivate them. Connectors are dormant by default (zero-egress); enabling
//! one is an explicit admin action, audited via [`crate::integrations`].

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;

use crate::auth::keycloak::AuthUser;
use crate::auth::permissions;
use crate::error::{AppError, Result};
use crate::integrations::{self, ConnectorKind, Descriptor};
use crate::state::AppState;

fn parse_kind(kind: &str) -> Result<ConnectorKind> {
    ConnectorKind::from_str(kind)
        .ok_or_else(|| AppError::Validation(format!("unknown connector '{kind}'")))
}

/// User-facing: the connectors that are currently enabled (for UI). Dormancy is
/// still enforced at the egress gate regardless of this list.
pub async fn list_enabled(
    State(state): State<AppState>,
    AuthUser(_ctx): AuthUser,
) -> Result<Json<Vec<Descriptor>>> {
    Ok(Json(integrations::list_descriptors(&state.pg, true).await?))
}

/// Admin: every connector with its activation state.
pub async fn list_all(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<Descriptor>>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::INTEGRATIONS_MANAGE).await?;
    Ok(Json(integrations::list_descriptors(&state.pg, false).await?))
}

/// Admin: one connector's descriptor + state.
pub async fn get_one(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
    Path(kind): Path<String>,
) -> Result<Json<Descriptor>> {
    state.rbac.require_permission(&state.pg, &ctx, permissions::INTEGRATIONS_MANAGE).await?;
    let kind = parse_kind(&kind)?;
    Ok(Json(integrations::descriptor(&state.pg, kind).await?))
}

#[derive(Deserialize)]
pub struct SetEnabled {
    pub enabled: bool,
}

/// Super-admin: activate / deactivate a connector. Enabling a connector lifts
/// zero-egress and may carry secrets — a sensitive/infrastructure action, so it
/// is gated to the *ephemeral super-admin* (active break-glass) rather than the
/// persistent client-admin. Viewing the
/// connector list stays client-admin (see `list_all`/`get_one`). The break-glass
/// extractor audits the grant use; `integrations::set_enabled` audits the change.
pub async fn set_enabled(
    State(state): State<AppState>,
    crate::auth::breakglass::SuperAdmin(ctx): crate::auth::breakglass::SuperAdmin,
    Path(kind): Path<String>,
    Json(body): Json<SetEnabled>,
) -> Result<Json<Descriptor>> {
    let kind = parse_kind(&kind)?;
    integrations::set_enabled(&state, &ctx, kind, body.enabled).await?;
    Ok(Json(integrations::descriptor(&state.pg, kind).await?))
}


/// Super-admin (break-glass): every connector with its activation state, for the
/// super-admin panel's Integrations section. Same data as `list_all`, but gated by
/// the break-glass session rather than the Keycloak admin Bearer, since the panel
/// runs outside Keycloak.
pub async fn list_all_super(
    State(state): State<AppState>,
    crate::auth::breakglass::SuperAdmin(_ctx): crate::auth::breakglass::SuperAdmin,
) -> Result<Json<Vec<Descriptor>>> {
    Ok(Json(integrations::list_descriptors(&state.pg, false).await?))
}
