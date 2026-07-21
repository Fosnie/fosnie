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

//! Power-user "lead" console. A `power_user` is a team-lead tier between `user`
//! and admin: they manage the RBAC groups they created (handled in `users_admin`,
//! owner-scoped) and see analytics for the teams they lead. This module adds the
//! two reads unique to that surface:
//!   - a full active-user directory (so a lead can build a team from anyone), a
//!     deliberate power-gated widening of the otherwise circle-scoped `/api/users`;
//!   - usage analytics scoped to the lead's `led_member_ids` (members of groups they
//!     created + grantees of projects they own), never the whole firm.
//! Everything here is gated to PowerUser|ClientAdmin|SuperAdmin; even an admin sees
//! only their *own* led teams (it's a lead view, not the global admin analytics).

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::auth::keycloak::AuthUser;
use crate::auth::rbac;
use crate::auth::{AuthContext, PlatformRole};
use crate::error::{AppError, Result};
use crate::http::users_admin::{AgentRollup, UserEntry, UserRollup};
use crate::state::AppState;

fn require_lead(ctx: &AuthContext) -> Result<()> {
    if matches!(
        ctx.role,
        PlatformRole::PowerUser | PlatformRole::ClientAdmin | PlatformRole::SuperAdmin
    ) {
        Ok(())
    } else {
        Err(AppError::Forbidden("requires power user or admin".into()))
    }
}

/// Full active-user directory for the group-builder picker (power-gated). Unlike
/// `/api/users` (circle-scoped for non-admins) this lists everyone, so a lead can
/// add a brand-new colleague who doesn't yet share a project/group with them.
pub async fn power_directory(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<Vec<UserEntry>>> {
    require_lead(&ctx)?;
    let rows = sqlx::query!(
        r#"SELECT id, display_name, email,
                  extract(epoch from avatar_updated_at)::bigint AS avatar_epoch
           FROM users
           WHERE deactivated_at IS NULL
           ORDER BY display_name"#
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

#[derive(Serialize)]
pub struct PowerAnalytics {
    /// Distinct people the lead oversees, excluding the lead themselves.
    pub team_size: i64,
    pub per_user: Vec<UserRollup>,
    pub per_agent: Vec<AgentRollup>,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_answers: i64,
}

/// Usage analytics scoped to the lead's teams — a filtered clone of
/// `users_admin::usage_analytics`. Token usage + agent traceability live on the
/// `chat.assistant.completed` audit rows; we restrict to actors in `led_member_ids`.
pub async fn power_analytics(
    State(state): State<AppState>,
    AuthUser(ctx): AuthUser,
) -> Result<Json<PowerAnalytics>> {
    require_lead(&ctx)?;
    let me = ctx
        .user_id
        .ok_or_else(|| AppError::Forbidden("a user is required".into()))?;
    let members = rbac::led_member_ids(&state.pg, me).await?;
    let team_size = members.iter().filter(|&&u| u != me).count() as i64;

    // Both metered completion actions, so a team's API usage is counted
    // alongside its chat usage rather than silently missing.
    let metered: Vec<String> =
        crate::audit::METERED_COMPLETION_ACTIONS.iter().map(|s| s.to_string()).collect();

    let per_user = sqlx::query!(
        r#"SELECT a.actor_user_id, u.email AS "email?",
                  COALESCE(SUM((a.token_usage->>'prompt_tokens')::bigint), 0)::bigint AS "prompt_tokens!: i64",
                  COALESCE(SUM((a.token_usage->>'completion_tokens')::bigint), 0)::bigint AS "completion_tokens!: i64",
                  COUNT(*) AS "count!: i64"
           FROM audit_events a
           LEFT JOIN users u ON u.id = a.actor_user_id
           WHERE a.action_type = ANY($1)
             AND a.actor_user_id = ANY($2)
           GROUP BY a.actor_user_id, u.email
           ORDER BY COUNT(*) DESC"#,
        &metered,
        &members,
    )
    .fetch_all(&state.pg)
    .await?;

    let per_agent = sqlx::query!(
        r#"SELECT a.model_agent_traceability->>'agent_id' AS "agent_id?: String",
                  ag.name AS "agent_name?",
                  COALESCE(SUM((a.token_usage->>'prompt_tokens')::bigint), 0)::bigint AS "prompt_tokens!: i64",
                  COALESCE(SUM((a.token_usage->>'completion_tokens')::bigint), 0)::bigint AS "completion_tokens!: i64",
                  COUNT(*) AS "count!: i64"
           FROM audit_events a
           LEFT JOIN agents ag ON ag.id = (a.model_agent_traceability->>'agent_id')::uuid
           WHERE a.action_type = ANY($1)
             AND a.actor_user_id = ANY($2)
           GROUP BY a.model_agent_traceability->>'agent_id', ag.name
           ORDER BY COUNT(*) DESC"#,
        &metered,
        &members,
    )
    .fetch_all(&state.pg)
    .await?;

    let total_prompt_tokens: i64 = per_user.iter().map(|r| r.prompt_tokens).sum();
    let total_completion_tokens: i64 = per_user.iter().map(|r| r.completion_tokens).sum();
    let total_answers: i64 = per_user.iter().map(|r| r.count).sum();

    Ok(Json(PowerAnalytics {
        team_size,
        per_user: per_user
            .into_iter()
            .map(|r| UserRollup {
                user_id: r.actor_user_id,
                email: r.email,
                prompt_tokens: r.prompt_tokens,
                completion_tokens: r.completion_tokens,
                count: r.count,
            })
            .collect(),
        per_agent: per_agent
            .into_iter()
            .map(|r| AgentRollup {
                agent_id: r.agent_id,
                agent_name: r.agent_name,
                prompt_tokens: r.prompt_tokens,
                completion_tokens: r.completion_tokens,
                count: r.count,
            })
            .collect(),
        total_prompt_tokens,
        total_completion_tokens,
        total_answers,
    }))
}
