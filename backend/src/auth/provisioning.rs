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

//! User provisioning.
//!
//! Auto-create/refresh the local `users` row on first login and thereafter. The
//! canonical user is resolved through the `user_identities` linkage table, NOT by
//! assuming `users.id == Keycloak sub`: a directory (SCIM) may have created the
//! user before their first login, so the first login *links* the Keycloak subject
//! to that existing user (matched by verified email) instead of parking it.
//!
//! Role precedence honours `identity.role_source` (D5): the login writes `role`
//! from the token only when `role_source = idp_claims`; under `scim_groups` the
//! role is owned by SCIM membership events and under `manual` only an admin sets
//! it, so the login must not overwrite it.

use sqlx::PgPool;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::PlatformRole;
use crate::error::Result;

/// Claims distilled from a validated Keycloak token.
pub struct ProvisionClaims {
    pub sub: Uuid,
    pub email: String,
    pub display_name: String,
    pub role: PlatformRole,
    /// Brokered-IdP group names/ids from the token `groups` claim (empty when the
    /// IdP does not send them). Drives JIT group sync (D5, §4).
    pub groups: Vec<String>,
}

/// JIT group-sync mode (`identity.jit_group_sync`, D5 §4). Off ⇒ no group changes.
#[derive(Clone, Copy, PartialEq)]
enum JitMode {
    Off,
    Match,
    Create,
}

fn jit_mode(v: &str) -> JitMode {
    match v.trim().to_ascii_lowercase().as_str() {
        "match" => JitMode::Match,
        "create" => JitMode::Create,
        _ => JitMode::Off,
    }
}

/// Where the platform role comes from (`identity.role_source`, D5). Absent/unknown
/// ⇒ `IdpClaims` so a Core-only Keycloak deploy behaves exactly as before.
fn role_from_claims(source: &str) -> bool {
    source == "idp_claims"
}

/// Read `identity.role_source` (runtime config). Fail-soft to `idp_claims`.
async fn role_source(pool: &PgPool) -> String {
    crate::config::runtime::get(pool, "identity.role_source")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .unwrap_or_else(|| "idp_claims".into())
}

/// Upsert the user on every authenticated request and return the **canonical**
/// `users.id` (which may differ from `claims.sub` for a directory-created user the
/// login has linked). Inserts on first sight (audited `user.provisioned`), links a
/// pre-existing same-email user (audited `user.identity_linked`), otherwise
/// refreshes the row.
pub async fn upsert_from_claims(pool: &PgPool, claims: &ProvisionClaims) -> Result<Uuid> {
    let write_role = role_from_claims(&role_source(pool).await);
    let mut tx = pool.begin().await?;

    // 1. Resolve the canonical user via the identity linkage, deciding whether we
    //    refresh an existing row or create a fresh one (id = sub).
    let linked = sqlx::query_scalar!(
        "SELECT user_id FROM user_identities WHERE provider = 'keycloak' AND subject = $1",
        claims.sub.to_string(),
    )
    .fetch_optional(&mut *tx)
    .await?;

    let (canonical, create) = match linked {
        // Known Keycloak identity → its user is canonical; refresh it.
        Some(uid) => (uid, false),
        None => {
            // No identity yet. Try to link by verified email.
            let by_email = sqlx::query!(
                "SELECT id FROM users WHERE email = $1",
                claims.email,
            )
            .fetch_optional(&mut *tx)
            .await?;
            match by_email {
                Some(row) => {
                    // Does this user already hold a Keycloak identity under a DIFFERENT
                    // subject? That is the genuine stale realm-recreate case (the old
                    // subject is dead) → park the old row and create a fresh one.
                    let stale: bool = sqlx::query_scalar!(
                        r#"SELECT EXISTS(
                               SELECT 1 FROM user_identities
                                WHERE user_id = $1 AND provider = 'keycloak' AND subject <> $2
                           ) AS "e!""#,
                        row.id,
                        claims.sub.to_string(),
                    )
                    .fetch_one(&mut *tx)
                    .await?;
                    if stale {
                        (claims.sub, true) // fresh user; park handled below
                    } else {
                        // Adopt/link the existing (SCIM- or local-managed) user.
                        link_identity(&mut tx, row.id, claims).await?;
                        (row.id, false)
                    }
                }
                None => (claims.sub, true),
            }
        }
    };

    // 2. Free the email if any OTHER row holds it (narrowed parking: the link branch
    //    above already made a same-email user canonical, so it is never parked here —
    //    only a genuine collision / stale realm-recreate row is). Email is UNIQUE, so
    //    at most one row matches.
    let parked = sqlx::query_scalar!(
        r#"UPDATE users
              SET email = 'stale-' || replace(id::text, '-', '') || '@stale.invalid',
                  deactivated_at = COALESCE(deactivated_at, now())
            WHERE email = $1 AND id <> $2
            RETURNING id"#,
        claims.email,
        canonical,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(old_id) = parked {
        let mut ev = AuditEvent::action("user.parked_stale", claims.role.as_str());
        ev.actor_user_id = Some(canonical);
        ev.resource_type = Some("user".into());
        ev.resource_id = Some(old_id);
        ev.payload = Some(serde_json::json!({ "email": claims.email, "reason": "email_collision" }));
        audit::append_with(&mut tx, &ev).await?;
    }

    // 3. Create or refresh the canonical row.
    if create {
        // A fresh, login-provisioned user. Its role is 'user' unless the IdP claims
        // are authoritative for roles (role_source = idp_claims).
        let initial_role = if write_role { claims.role } else { PlatformRole::User };
        sqlx::query!(
            r#"INSERT INTO users (id, display_name, email, role, managed_by, last_seen_at)
               VALUES ($1, $2, $3, $4, 'local', now())"#,
            canonical,
            claims.display_name,
            claims.email,
            initial_role as PlatformRole,
        )
        .execute(&mut *tx)
        .await?;
        link_identity(&mut tx, canonical, claims).await?;

        let mut event = AuditEvent::action("user.provisioned", claims.role.as_str());
        event.actor_user_id = Some(canonical);
        event.resource_type = Some("user".into());
        event.resource_id = Some(canonical);
        event.payload = Some(serde_json::json!({
            "email": claims.email,
            "role": initial_role.as_str(),
        }));
        audit::append_with(&mut tx, &event).await?;
    } else {
        // Refresh. Keep a user-customised name; write role only when idp_claims owns it.
        sqlx::query!(
            r#"UPDATE users
                  SET display_name = CASE WHEN display_name_custom THEN display_name ELSE $2 END,
                      email = $3,
                      role = CASE WHEN $4 THEN $5 ELSE role END,
                      last_seen_at = now()
                WHERE id = $1"#,
            canonical,
            claims.display_name,
            claims.email,
            write_role,
            claims.role as PlatformRole,
        )
        .execute(&mut *tx)
        .await?;
    }

    // JIT group sync (D5 §4): reconcile the user's `idp`-sourced group memberships
    // to the token's `groups` claim. Only touches groups the directory owns
    // (`managed_by IN ('idp','scim')`) and memberships it created (`source='idp'`);
    // manual/SCIM grants are never disturbed. `off` (default) is a no-op.
    let mode = jit_mode(
        &crate::config::runtime::get(pool, "identity.jit_group_sync")
            .await
            .ok()
            .flatten()
            .map(|e| e.value)
            .unwrap_or_default(),
    );
    if mode != JitMode::Off {
        jit_group_sync(&mut tx, canonical, &claims.groups, mode, claims.role.as_str()).await?;
        // Under `role_source = scim_groups`, the login-time role reflects group
        // membership, so recompute after the JIT changes.
        if role_source(pool).await == "scim_groups" {
            recompute_role_from_groups(&mut tx, canonical).await?;
        }
    }

    tx.commit().await?;
    Ok(canonical)
}

/// Reconcile a user's `idp`-sourced group memberships to `claim_groups`. Adds
/// membership to matching directory groups (creating them under `managed_by='idp'`
/// in `Create` mode), and removes `idp`-sourced memberships whose group is no longer
/// present in the claim. Every add/remove/create is audited.
async fn jit_group_sync(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    claim_groups: &[String],
    mode: JitMode,
    actor_role: &str,
) -> Result<()> {
    // Resolve each claim group to a directory group id (match by external_id or name
    // among directory-managed groups), minting it in Create mode.
    let mut desired: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    for g in claim_groups {
        let name = g.trim();
        if name.is_empty() {
            continue;
        }
        let found = sqlx::query_scalar!(
            r#"SELECT id FROM groups
                WHERE (external_id = $1 OR name = $1) AND managed_by IN ('idp', 'scim')
                LIMIT 1"#,
            name,
        )
        .fetch_optional(&mut **tx)
        .await?;
        let gid = match found {
            Some(id) => id,
            None if mode == JitMode::Create => {
                let id = Uuid::now_v7();
                sqlx::query!(
                    "INSERT INTO groups (id, name, managed_by) VALUES ($1, $2, 'idp')",
                    id,
                    name,
                )
                .execute(&mut **tx)
                .await?;
                let mut ev = AuditEvent::action("identity.jit.group_created", actor_role);
                ev.actor_user_id = Some(user_id);
                ev.resource_type = Some("group".into());
                ev.resource_id = Some(id);
                ev.payload = Some(serde_json::json!({ "name": name }));
                audit::append_with(tx, &ev).await?;
                id
            }
            None => continue, // Match mode: unknown group is skipped.
        };
        desired.insert(gid);
    }

    // Current `idp`-sourced memberships.
    let existing: std::collections::HashSet<Uuid> = sqlx::query_scalar!(
        "SELECT group_id FROM group_members WHERE user_id = $1 AND source = 'idp'",
        user_id,
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();

    for gid in desired.difference(&existing) {
        // Insert only if not already a member (any source); never override a
        // manual/SCIM membership's provenance.
        let inserted = sqlx::query_scalar!(
            r#"INSERT INTO group_members (group_id, user_id, source)
               VALUES ($1, $2, 'idp')
               ON CONFLICT (group_id, user_id) DO NOTHING
               RETURNING group_id"#,
            gid,
            user_id,
        )
        .fetch_optional(&mut **tx)
        .await?;
        if inserted.is_some() {
            audit_membership(tx, "identity.jit.member_added", user_id, *gid, actor_role).await?;
            let ev = crate::events::NewEvent::new(
                crate::events::GROUP_MEMBER_ADDED,
                crate::events::ActorType::Human,
            )
            .actor(Some(user_id))
            .resource("group", *gid)
            .payload(serde_json::json!({ "group_id": gid.to_string(), "user_id": user_id.to_string(), "source": "idp" }));
            crate::events::emit_with(&mut **tx, &ev).await?;
        }
    }
    for gid in existing.difference(&desired) {
        sqlx::query!(
            "DELETE FROM group_members WHERE group_id = $1 AND user_id = $2 AND source = 'idp'",
            gid,
            user_id,
        )
        .execute(&mut **tx)
        .await?;
        audit_membership(tx, "identity.jit.member_removed", user_id, *gid, actor_role).await?;
        let ev = crate::events::NewEvent::new(
            crate::events::GROUP_MEMBER_REMOVED,
            crate::events::ActorType::Human,
        )
        .actor(Some(user_id))
        .resource("group", *gid)
        .payload(serde_json::json!({ "group_id": gid.to_string(), "user_id": user_id.to_string(), "source": "idp" }));
        crate::events::emit_with(&mut **tx, &ev).await?;
    }
    Ok(())
}

async fn audit_membership(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    action: &str,
    user_id: Uuid,
    group_id: Uuid,
    actor_role: &str,
) -> Result<()> {
    let mut ev = AuditEvent::action(action, actor_role);
    ev.actor_user_id = Some(user_id);
    ev.resource_type = Some("group".into());
    ev.resource_id = Some(group_id);
    ev.payload = Some(serde_json::json!({ "user_id": user_id }));
    audit::append_with(tx, &ev).await?;
    Ok(())
}

/// Recompute `users.role` from `identity.role_mapping` and the user's current
/// directory-group memberships (`role_source = scim_groups`). Highest mapped role
/// wins; `super_admin` is never assignable via a mapping. Mirrors the SCIM path so a
/// JIT-only deployment gets the same role behaviour.
async fn recompute_role_from_groups(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<()> {
    let raw = sqlx::query_scalar!("SELECT value FROM config_settings WHERE key = 'identity.role_mapping'")
        .fetch_optional(&mut **tx)
        .await?;
    let mapping: std::collections::HashMap<String, PlatformRole> = raw
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| v.as_str().and_then(parse_mapped_role).map(|r| (k, r)))
                .collect()
        })
        .unwrap_or_default();

    let rows = sqlx::query!(
        r#"SELECT g.external_id, g.name
             FROM group_members gm JOIN groups g ON g.id = gm.group_id
            WHERE gm.user_id = $1 AND g.managed_by IN ('idp', 'scim')"#,
        user_id,
    )
    .fetch_all(&mut **tx)
    .await?;
    let mut best = PlatformRole::User;
    for r in rows {
        for key in [r.external_id.as_deref(), Some(r.name.as_str())].into_iter().flatten() {
            if let Some(role) = mapping.get(key) {
                if role_rank(*role) > role_rank(best) {
                    best = *role;
                }
            }
        }
    }
    sqlx::query!(
        "UPDATE users SET role = $2 WHERE id = $1 AND role <> 'super_admin'",
        user_id,
        best as PlatformRole,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Parse a role name from a mapping value; `super_admin`/unknown ⇒ `None` (never
/// assignable via directory sync).
fn parse_mapped_role(s: &str) -> Option<PlatformRole> {
    match s.trim().to_ascii_lowercase().as_str() {
        "client_admin" | "admin" => Some(PlatformRole::ClientAdmin),
        "power_user" => Some(PlatformRole::PowerUser),
        "user" => Some(PlatformRole::User),
        _ => None,
    }
}

fn role_rank(r: PlatformRole) -> u8 {
    match r {
        PlatformRole::SuperAdmin => 3,
        PlatformRole::ClientAdmin => 2,
        PlatformRole::PowerUser => 1,
        PlatformRole::User => 0,
    }
}

/// Insert a `keycloak` identity row for `user_id` (idempotent) and audit the link.
async fn link_identity(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    claims: &ProvisionClaims,
) -> Result<()> {
    let linked = sqlx::query_scalar!(
        r#"INSERT INTO user_identities (user_id, provider, subject)
           VALUES ($1, 'keycloak', $2)
           ON CONFLICT (provider, subject) DO NOTHING
           RETURNING id"#,
        user_id,
        claims.sub.to_string(),
    )
    .fetch_optional(&mut **tx)
    .await?;
    if linked.is_some() {
        let mut ev = AuditEvent::action("user.identity_linked", claims.role.as_str());
        ev.actor_user_id = Some(user_id);
        ev.resource_type = Some("user".into());
        ev.resource_id = Some(user_id);
        ev.payload = Some(serde_json::json!({
            "provider": "keycloak", "subject": claims.sub, "email": claims.email,
        }));
        audit::append_with(tx, &ev).await?;
    }
    Ok(())
}

/// Manual creation by an admin.
pub async fn create_manual(
    pool: &PgPool,
    email: &str,
    display_name: &str,
    role: PlatformRole,
    actor_user_id: Option<Uuid>,
    actor_role: &str,
) -> Result<Uuid> {
    let id = Uuid::now_v7();
    let mut tx = pool.begin().await?;

    sqlx::query!(
        r#"INSERT INTO users (id, display_name, email, role) VALUES ($1, $2, $3, $4)"#,
        id,
        display_name,
        email,
        role as PlatformRole,
    )
    .execute(&mut *tx)
    .await?;

    let mut event = AuditEvent::action("user.created", actor_role);
    event.actor_user_id = actor_user_id;
    event.resource_type = Some("user".into());
    event.resource_id = Some(id);
    event.payload = Some(serde_json::json!({ "email": email, "role": role.as_str() }));
    audit::append_with(&mut tx, &event).await?;

    // Domain event (§4): a directory user was provisioned. Local manual create is a
    // Human action; the SCIM path emits the same name with a System actor.
    let ev = crate::events::NewEvent::new(
        crate::events::DIRECTORY_USER_PROVISIONED,
        crate::events::ActorType::Human,
    )
    .actor(actor_user_id)
    .resource("user", id)
    .payload(serde_json::json!({ "email": email, "source": "manual" }));
    crate::events::emit_with(&mut tx, &ev).await?;

    tx.commit().await?;
    Ok(id)
}
