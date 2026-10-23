// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::super::{AdapterKind, CallPurpose, Locality, RoutingContext, RoutingError};
use super::catalogue::CatalogueScope;
use crate::{
    auth::{permissions, AuthContext},
    AppError, AppState, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub(crate) async fn check_actor(state: &AppState, actor: &AuthContext) -> Result<()> {
    let context = RoutingContext {
        actor: actor.clone(),
        chat_id: None,
        project_id: None,
        run_id: Uuid::now_v7(),
        purpose: CallPurpose::RouterClassify,
    };
    super::super::durable::Context::capture(&context)?
        .validate(&state.pg)
        .await?;
    let active: bool = sqlx::query_scalar("SELECT active FROM routing_catalogue_state WHERE id")
        .fetch_one(&state.pg)
        .await?;
    if !active {
        return Err(AppError::Unavailable(
            "connection catalogue is in legacy maintenance mode".into(),
        ));
    }
    Ok(())
}
pub(crate) fn scope(kind: &str, owner: Option<Uuid>) -> Result<CatalogueScope> {
    match (kind, owner) {
        ("deployment", None) => Ok(CatalogueScope::Deployment),
        ("user", Some(owner_id)) if !owner_id.is_nil() => Ok(CatalogueScope::User { owner_id }),
        _ => Err(RoutingError::Invalid("catalogue scope").into()),
    }
}
pub(crate) async fn owner_for(
    state: &AppState,
    actor: &AuthContext,
    kind: &str,
) -> Result<Option<Uuid>> {
    match kind {
        "deployment" => {
            state
                .rbac
                .require_permission(&state.pg, actor, permissions::PROVIDERS_MANAGE)
                .await?;
            Ok(None)
        }
        "user" if super::super::store::byok_enabled(state).await? => {
            Ok(Some(actor.user_id.ok_or(RoutingError::Forbidden)?))
        }
        _ => Err(RoutingError::Forbidden.into()),
    }
}
pub(crate) async fn mutation_lock(
    conn: &mut sqlx::PgConnection,
    owner: Option<Uuid>,
) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("fosnie.catalogue.{}", owner.unwrap_or(Uuid::nil())))
        .execute(conn)
        .await?;
    Ok(())
}
pub(crate) fn version(version: u32, expected: i64) -> Result<()> {
    if version != super::ROUTING_VNEXT_VERSION || expected < 0 {
        return Err(RoutingError::Invalid("catalogue version or expected revision").into());
    }
    Ok(())
}
pub(crate) fn label(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 120 {
        return Err(RoutingError::Invalid("catalogue label").into());
    }
    Ok(value)
}
pub(crate) fn endpoint(value: &str) -> Result<String> {
    let url =
        reqwest::Url::parse(value).map_err(|_| RoutingError::Invalid("connection endpoint"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || value.len() > 2048
    {
        return Err(RoutingError::Invalid(
            "endpoint must not contain credentials, query or fragment",
        )
        .into());
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
fn adapter(value: AdapterKind) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectionMetadata {
    pub locality: Locality,
    pub locality_trusted: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ConnectionEntry {
    pub id: Uuid,
    pub label: Option<String>,
    pub scope: String,
    pub scope_id: Option<Uuid>,
    pub revision: i64,
    pub auth_revision: i64,
    pub base_url: Option<String>,
    pub adapter: Option<String>,
    pub enabled: bool,
    pub api_key_set: bool,
    pub quota_group_id: Option<Uuid>,
    pub metadata: Value,
}
pub(crate) async fn row(
    conn: &mut sqlx::PgConnection,
    id: Uuid,
    lock: bool,
) -> Result<Option<ConnectionEntry>> {
    let sql = if lock {
        "SELECT id,label,scope,scope_id,routing_revision AS revision,auth_revision,base_url,connection_adapter AS adapter,enabled,api_key_encrypted IS NOT NULL AS api_key_set,quota_group_id,model_metadata AS metadata FROM provider_configs WHERE id=$1 AND role='connection' FOR UPDATE"
    } else {
        "SELECT id,label,scope,scope_id,routing_revision AS revision,auth_revision,base_url,connection_adapter AS adapter,enabled,api_key_encrypted IS NOT NULL AS api_key_set,quota_group_id,model_metadata AS metadata FROM provider_configs WHERE id=$1 AND role='connection'"
    };
    Ok(sqlx::query_as(sql).bind(id).fetch_optional(conn).await?)
}

pub async fn list(state: &AppState, actor: &AuthContext) -> Result<Vec<ConnectionEntry>> {
    check_actor(state, actor).await?;
    let admin = state
        .rbac
        .has_permission(&state.pg, actor, permissions::PROVIDERS_MANAGE)
        .await?;
    let user = if super::super::store::byok_enabled(state).await? {
        actor.user_id
    } else {
        None
    };
    Ok(sqlx::query_as("SELECT id,label,scope,scope_id,routing_revision AS revision,auth_revision,base_url,connection_adapter AS adapter,enabled,api_key_encrypted IS NOT NULL AS api_key_set,quota_group_id,model_metadata AS metadata FROM provider_configs WHERE role='connection' AND ((scope='deployment' AND $1) OR (scope='user' AND scope_id=$2)) ORDER BY scope,label,id")
        .bind(admin).bind(user).fetch_all(&state.pg).await?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveConnection {
    pub schema_version: u32,
    pub expected_revision: i64,
    pub scope: String,
    pub label: String,
    pub adapter: AdapterKind,
    pub base_url: String,
    pub enabled: bool,
    pub quota_group_id: Option<Uuid>,
    pub metadata: ConnectionMetadata,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub clear_api_key: bool,
}
pub async fn save(
    state: &AppState,
    actor: &AuthContext,
    id: Uuid,
    body: SaveConnection,
) -> Result<ConnectionEntry> {
    check_actor(state, actor).await?;
    version(body.schema_version, body.expected_revision)?;
    if id.is_nil()
        || body.quota_group_id.is_some_and(|v| v.is_nil())
        || (body.clear_api_key && body.api_key.is_some())
    {
        return Err(RoutingError::Invalid("connection identity or key action").into());
    }
    let owner = owner_for(state, actor, &body.scope).await?;
    let label = label(&body.label)?;
    let url = endpoint(&body.base_url)?;
    if body.metadata.locality_trusted {
        state
            .rbac
            .require_permission(&state.pg, actor, permissions::PROVIDERS_MANAGE)
            .await?;
    }
    let key = match body.api_key.as_deref() {
        Some(value) if value.trim().is_empty() || value.len() > 8192 => {
            return Err(RoutingError::Invalid("connection credential length").into())
        }
        Some(value) => {
            if state.message_key.is_none() {
                return Err(RoutingError::CredentialsUnavailable.into());
            }
            Some(crate::crypto::encrypt_at_rest(value)?)
        }
        None => None,
    };
    let mut tx = state.pg.begin().await?;
    mutation_lock(&mut tx, owner).await?;
    let existing = row(&mut tx, id, true).await?;
    if let Some(old) = &existing {
        if old.scope != body.scope || old.scope_id != owner {
            return Err(RoutingError::Forbidden.into());
        }
        if old.revision != body.expected_revision {
            return Err(RoutingError::NeedsReplan.into());
        }
        sqlx::query("UPDATE provider_configs SET label=$2,base_url=$3,connection_adapter=$4,enabled=$5,quota_group_id=$6,model_metadata=$7,api_key_encrypted=CASE WHEN $8 THEN NULL ELSE COALESCE($9,api_key_encrypted) END,updated_by=$10,updated_at=now() WHERE id=$1 AND role='connection'")
            .bind(id).bind(label).bind(url).bind(adapter(body.adapter)).bind(body.enabled).bind(body.quota_group_id)
            .bind(serde_json::to_value(&body.metadata).unwrap()).bind(body.clear_api_key).bind(key).bind(actor.user_id).execute(&mut *tx).await?;
    } else {
        if body.expected_revision != 0 {
            return Err(RoutingError::NeedsReplan.into());
        }
        sqlx::query("INSERT INTO provider_configs(id,role,scope,scope_id,label,base_url,connection_adapter,enabled,quota_group_id,model_metadata,api_key_encrypted,updated_by,routing_revision) VALUES($1,'connection',$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,nextval('routing_revision_generation'))")
            .bind(id).bind(&body.scope).bind(owner).bind(label).bind(url).bind(adapter(body.adapter)).bind(body.enabled).bind(body.quota_group_id)
            .bind(serde_json::to_value(&body.metadata).unwrap()).bind(key).bind(actor.user_id).execute(&mut *tx).await.map_err(conflict)?;
    }
    let saved = row(&mut tx, id, false)
        .await?
        .ok_or(RoutingError::ProviderUnavailable)?;
    audit(
        &mut tx,
        actor,
        "routing.connection_saved",
        id,
        saved.revision,
    )
    .await?;
    tx.commit().await?;
    Ok(saved)
}
pub async fn delete(state: &AppState, actor: &AuthContext, id: Uuid, expected: i64) -> Result<()> {
    check_actor(state, actor).await?;
    version(2, expected)?;
    let known = list(state, actor)
        .await?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or(RoutingError::Forbidden)?;
    let owner = owner_for(state, actor, &known.scope).await?;
    let mut tx = state.pg.begin().await?;
    mutation_lock(&mut tx, owner).await?;
    let current = row(&mut tx, id, true)
        .await?
        .ok_or(RoutingError::NeedsReplan)?;
    if current.revision != expected {
        return Err(RoutingError::NeedsReplan.into());
    }
    let used: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM provider_configs WHERE connection_id=$1)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if used {
        return Err(AppError::Conflict(
            "connection still has model deployments".into(),
        ));
    }
    sqlx::query("DELETE FROM provider_configs WHERE id=$1 AND role='connection'")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit(&mut tx, actor, "routing.connection_deleted", id, expected).await?;
    tx.commit().await?;
    Ok(())
}
pub(crate) fn conflict(error: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(e) = &error {
        if e.is_unique_violation()
            || e.is_foreign_key_violation()
            || matches!(e.code().as_deref(), Some("40001" | "40P01"))
        {
            return AppError::Conflict(
                "catalogue identity, configuration or revision changed".into(),
            );
        }
    }
    crate::providers::connection_error(error)
}
pub(crate) async fn audit(
    conn: &mut sqlx::PgConnection,
    actor: &AuthContext,
    action: &str,
    id: Uuid,
    revision: i64,
) -> Result<()> {
    let mut event = crate::audit::AuditEvent::action(action, actor.role.as_str());
    event.actor_user_id = actor.user_id;
    event.payload = Some(serde_json::json!({"id":id,"revision":revision,"schema_version":2}));
    crate::audit::append_with(conn, &event).await?;
    Ok(())
}
