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

//! `GET /v1/models` — what this caller may address in the `model` field.
//!
//! Two kinds of entry, both usable verbatim as `model` in a completion request:
//! configured LLM providers (by their human-readable label) and the caller's
//! agents (as `agent/<uuid>`).

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::AuthContext;
use crate::auth::api_key::ApiKeyAuth;
use crate::state::AppState;

use super::error::ApiError;

/// The prefix that selects the agent pipeline rather than a raw model.
///
/// A provider whose label happens to start with this string is unreachable
/// through the API: the prefix is tested first. Not worth a uniqueness
/// constraint on provider labels, which would change a write path used by every
/// user's own provider settings for a collision nobody has hit.
pub const AGENT_PREFIX: &str = "agent/";

pub async fn list_models(
    State(state): State<AppState>,
    ApiKeyAuth(ctx, key_id): ApiKeyAuth,
) -> Result<Json<Value>, ApiError> {
    super::require_enabled_for(&state, &ctx).await?;
    super::rate_limit(&state, key_id).await?;
    let uid = user_id(&ctx)?;

    let mut data: Vec<Value> = Vec::new();

    for p in visible_llm_providers(&state, uid).await? {
        data.push(json!({
            "id": p.label,
            "object": "model",
            "created": p.created,
            "owned_by": "fosnie",
        }));
    }

    // Agent visibility mirrors the application's own rule: an administrator sees
    // every agent, everyone else sees the shared pool plus the ones they made.
    let agents = sqlx::query!(
        "SELECT id, name, description, created_at FROM agents \
         WHERE archived_at IS NULL AND ($1 OR created_by IS NULL OR created_by = $2) \
         ORDER BY created_at DESC",
        ctx.is_admin(),
        uid,
    )
    .fetch_all(&state.pg)
    .await?;
    for a in agents {
        data.push(json!({
            "id": format!("{AGENT_PREFIX}{}", a.id),
            "object": "model",
            "created": a.created_at.unix_timestamp(),
            "owned_by": "fosnie-agent",
            // Non-standard, and safe: clients ignore fields they do not know,
            // while a Fosnie-aware one can show the agent's purpose.
            "name": a.name,
            "description": a.description,
        }));
    }

    Ok(Json(json!({ "object": "list", "data": data })))
}

pub(crate) struct VisibleProvider {
    pub id: Uuid,
    pub label: String,
    pub created: i64,
}

/// The LLM providers this user may address, collapsed to one entry per label.
///
/// Labels are unique per scope, not globally, so a user's own provider may share
/// a label with a deployment one. The user's wins: that is the precedence the
/// platform already applies when resolving a provider for a chat, and the
/// alternative (two identical ids in the list) has no meaning over this API.
pub(crate) async fn visible_llm_providers(
    state: &AppState,
    uid: Uuid,
) -> Result<Vec<VisibleProvider>, ApiError> {
    let rows = sqlx::query!(
        // A provider row need not carry a label. It still has to be addressable,
        // so fall back to its model name and finally to its id: something stable
        // that a caller can copy out of the model list and send straight back.
        r#"SELECT id, scope, COALESCE(label, model, id::text) AS "label!", is_default, updated_at
           FROM provider_configs
           WHERE role = 'llm' AND enabled
             AND ( scope = 'deployment' OR (scope = 'user' AND scope_id = $1) )
           ORDER BY (scope = 'user') DESC, is_default DESC, COALESCE(label, model, id::text)"#,
        uid,
    )
    .fetch_all(&state.pg)
    .await?;

    // The ordering above puts user-scope rows first, so the first sighting of a
    // label is the one that wins.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for r in rows {
        if seen.insert(r.label.clone()) {
            out.push(VisibleProvider {
                id: r.id,
                label: r.label,
                created: r.updated_at.unix_timestamp(),
            });
        }
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(out)
}

/// Resolve a `model` label to a provider id, using exactly the collapse rule
/// above so anything listed by `/v1/models` can be sent straight back.
pub(crate) async fn resolve_label(
    state: &AppState,
    uid: Uuid,
    label: &str,
) -> Result<Uuid, ApiError> {
    visible_llm_providers(state, uid)
        .await?
        .into_iter()
        .find(|p| p.label == label)
        .map(|p| p.id)
        .ok_or_else(|| ApiError::model_not_found(label))
}

pub(crate) fn user_id(ctx: &AuthContext) -> Result<Uuid, ApiError> {
    ctx.user_id
        .ok_or_else(|| ApiError::forbidden("this key is not attached to a user account"))
}
