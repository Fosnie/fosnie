// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! One shared health store behind the existing health API, not a retry owner.
use super::{failure::ProviderFailure, fallback::FailureKind, *};
use crate::{AppState, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, time::Duration};
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct Metadata {
    provider_revision: i64,
    scope: String,
    scope_id: Option<Uuid>,
    model_config_revision: i64,
    connection_id: Option<Uuid>,
    auth_revision: Option<i64>,
    quota_group_id: Option<Uuid>,
}
fn digest(value: Value) -> String {
    hex::encode(Sha256::digest(serde_json::to_vec(&value).unwrap()))
}
pub(super) fn credential_hash(
    target: &InferenceTarget,
    scope: &str,
    owner: Option<Uuid>,
) -> Option<String> {
    if target.api_key.is_empty() {
        return None;
    }
    let url = reqwest::Url::parse(&target.base_url).ok()?;
    let origin = format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str()?,
        url.port_or_known_default()?
    );
    // Stable HMAC keyed by the existing high-entropy credential, scoped by its
    // authorised owner. No random process salt, new secret or credential store.
    let input = serde_json::to_vec(&json!([
        "fosnie.health.credential.v2",
        scope,
        owner,
        origin
    ]))
    .ok()?;
    use hmac::{Hmac, KeyInit, Mac};
    let mut h = <Hmac<Sha256> as KeyInit>::new_from_slice(target.api_key.as_bytes()).ok()?;
    h.update(&input);
    Some(hex::encode(h.finalize().into_bytes()))
}
async fn metadata(state: &AppState, id: Uuid) -> Result<Metadata> {
    sqlx::query_as("SELECT p.routing_revision AS provider_revision,p.scope,p.scope_id,p.model_config_revision,p.connection_id,c.auth_revision,c.quota_group_id FROM provider_configs p LEFT JOIN provider_configs c ON c.id=p.connection_id AND c.role='connection' WHERE p.id=$1 AND p.role='llm'").bind(id).fetch_optional(&state.pg).await?.ok_or_else(||RoutingError::NeedsReplan.into())
}
fn roots(m: &Metadata, id: Uuid) -> Vec<(String, String)> {
    let owner = json!([m.scope, m.scope_id]);
    let mut values = vec![(
        "model".into(),
        digest(json!([
            "fosnie.health.model.v2",
            owner,
            id,
            m.model_config_revision
        ])),
    )];
    if let (Some(connection), Some(revision)) = (m.connection_id, m.auth_revision) {
        values.push((
            "connection".into(),
            digest(json!([
                "fosnie.health.connection.v2",
                owner,
                connection,
                revision
            ])),
        ));
    }
    if let Some(group) = m.quota_group_id {
        values.push((
            "quota_group".into(),
            digest(json!(["fosnie.health.quota.v2", owner, group])),
        ));
    }
    values
}
async fn all_roots(state: &AppState, r: &TargetReference) -> Result<Vec<(String, String)>> {
    if let Some(keys) = captured_roots(state, r.provider_id, r.provider_revision).await? {
        return Ok(keys);
    }
    let mut values = roots(&metadata(state, r.provider_id).await?, r.provider_id);
    if let Some(hash)=sqlx::query_scalar::<_,String>("SELECT binding_hash FROM routing_credential_bindings WHERE provider_id=$1 AND provider_revision=$2").bind(r.provider_id).bind(r.provider_revision).fetch_optional(&state.pg).await? {values.push(("credential".into(),hash));}
    Ok(values)
}
async fn captured_roots(
    state: &AppState,
    id: Uuid,
    revision: i64,
) -> Result<Option<Vec<(String, String)>>> {
    let raw:Option<Value>=sqlx::query_scalar("SELECT scope_keys FROM routing_health_bindings WHERE provider_id=$1 AND provider_revision=$2").bind(id).bind(revision).fetch_optional(&state.pg).await?;
    raw.map(|raw| {
        let keys: Vec<(String, String)> = serde_json::from_value(raw)
            .map_err(|_| RoutingError::Invalid("health dispatch binding"))?;
        if keys.is_empty()
            || keys.len() > 4
            || keys.iter().any(|(kind, key)| {
                !matches!(
                    kind.as_str(),
                    "model" | "connection" | "credential" | "quota_group"
                ) || !vnext::sha256(key)
            })
        {
            return Err(RoutingError::Invalid("health dispatch binding").into());
        }
        Ok(keys)
    })
    .transpose()
}
pub(super) async fn has_quota_group(state: &AppState, r: &TargetReference) -> Result<bool> {
    Ok(all_roots(state, r)
        .await?
        .iter()
        .any(|(kind, _)| kind == "quota_group"))
}
pub(super) async fn register(state: &AppState, target: &InferenceTarget) -> Result<()> {
    // The longest routed deadline is one day. Cleanup precedes capture, so a
    // current long-lived binding is refreshed without resetting active probes.
    sqlx::query("DELETE FROM routing_health_bindings WHERE created_at<now()-interval '2 days'")
        .execute(&state.pg)
        .await?;
    let m = metadata(state, target.provider_id).await?;
    if m.provider_revision != target.provider_revision {
        return Err(RoutingError::NeedsReplan.into());
    }
    let mut keys = roots(&m, target.provider_id);
    let credential = credential_hash(target, &m.scope, m.scope_id);
    if let Some(hash) = &credential {
        keys.push(("credential".into(), hash.clone()));
    }
    sqlx::query("INSERT INTO routing_health_bindings(provider_id,provider_revision,scope_keys) VALUES($1,$2,$3) ON CONFLICT(provider_id,provider_revision) DO NOTHING").bind(target.provider_id).bind(target.provider_revision).bind(json!(keys)).execute(&state.pg).await?;
    if captured_roots(state, target.provider_id, target.provider_revision)
        .await?
        .as_ref()
        != Some(&keys)
    {
        return Err(RoutingError::NeedsReplan.into());
    }
    if let Some(hash) = credential {
        sqlx::query("INSERT INTO routing_credential_bindings(provider_id,provider_revision,binding_hash) VALUES($1,$2,$3) ON CONFLICT(provider_id) DO UPDATE SET provider_revision=EXCLUDED.provider_revision,binding_hash=EXCLUDED.binding_hash WHERE routing_credential_bindings.provider_revision<=EXCLUDED.provider_revision").bind(target.provider_id).bind(target.provider_revision).bind(hash).execute(&state.pg).await?;
    }
    Ok(())
}
pub(super) async fn blocked(
    state: &AppState,
    r: &TargetReference,
) -> Result<Option<ProviderFailure>> {
    let keys: Vec<_> = all_roots(state, r)
        .await?
        .into_iter()
        .map(|(_, key)| key)
        .collect();
    let row:Option<(Value,i64)>=sqlx::query_as("SELECT failure,GREATEST(1,ceil(extract(epoch FROM GREATEST(expires_at,COALESCE(probe_until,expires_at))-clock_timestamp()))::bigint) FROM routing_health_scopes WHERE scope_hash=ANY($1) AND (expires_at>clock_timestamp() OR probe_until>clock_timestamp()) ORDER BY GREATEST(expires_at,COALESCE(probe_until,expires_at)) DESC LIMIT 1").bind(keys).fetch_optional(&state.pg).await?;
    row.map(|(v, t)| {
        let mut f: ProviderFailure =
            serde_json::from_value(v).map_err(|_| RoutingError::Invalid("health scope failure"))?;
        f.before_output = true;
        f.retry_after_seconds = Some(t.clamp(1, 86400) as u32);
        Ok::<_, RoutingError>(f)
    })
    .transpose()
    .map_err(Into::into)
}
pub(super) async fn note(
    state: &AppState,
    target: &InferenceTarget,
    failure: &ProviderFailure,
) -> Result<()> {
    let values = if let Some(keys) =
        captured_roots(state, target.provider_id, target.provider_revision).await?
    {
        keys
    } else {
        let m = metadata(state, target.provider_id).await?;
        // An uncaptured old callback cannot infer scopes from a new binding.
        if m.provider_revision != target.provider_revision {
            return Ok(());
        }
        let mut keys = roots(&m, target.provider_id);
        if let Some(hash) = credential_hash(target, &m.scope, m.scope_id) {
            keys.push(("credential".into(), hash));
        }
        keys
    };
    let grouped = values.iter().any(|(kind, _)| kind == "quota_group");
    let scope = match failure.kind {
        FailureKind::Unauthorized => vec!["connection", "credential"],
        FailureKind::QuotaExhausted | FailureKind::PaymentRequired if grouped => {
            vec!["quota_group"]
        }
        FailureKind::QuotaExhausted | FailureKind::PaymentRequired => {
            vec!["quota_group", "connection", "credential"]
        }
        FailureKind::RateLimited if grouped => vec!["quota_group"],
        FailureKind::Connection => vec!["connection"],
        _ => vec!["model"],
    };
    let mut chosen: Vec<_> = values
        .iter()
        .filter(|(kind, _)| scope.contains(&kind.as_str()))
        .cloned()
        .collect();
    if chosen.is_empty() {
        chosen.extend(values.into_iter().filter(|(kind, _)| kind == "model"));
    }
    let seconds = failure
        .retry_after_seconds
        .unwrap_or(
            if matches!(
                failure.kind,
                FailureKind::Unauthorized
                    | FailureKind::QuotaExhausted
                    | FailureKind::PaymentRequired
                    | FailureKind::AccessDenied
            ) {
                300
            } else {
                30
            },
        )
        .min(86400);
    // Match probe lock order when failures update several shared scopes.
    chosen.sort_by(|a, b| a.1.cmp(&b.1));
    let mut tx = state.pg.begin().await?;
    for (kind, key) in chosen {
        sqlx::query("INSERT INTO routing_health_scopes(scope_hash,kind,failure,expires_at) VALUES($1,$2,$3,clock_timestamp()+($4::bigint*interval '1 second')) ON CONFLICT(scope_hash) DO UPDATE SET failure=EXCLUDED.failure,expires_at=GREATEST(routing_health_scopes.expires_at,EXCLUDED.expires_at),probe_token=NULL,probe_until=NULL,updated_at=now()").bind(key).bind(kind).bind(json!(failure)).bind(seconds as i64).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
pub struct Probe {
    pool: sqlx::PgPool,
    key: String,
    token: Uuid,
    sent: bool,
}
impl Probe {
    pub(super) fn arm(&mut self) {
        self.sent = true;
    }
    pub(super) async fn success(self) -> Result<()> {
        sqlx::query("DELETE FROM routing_health_scopes WHERE scope_hash=$1 AND probe_token=$2 AND expires_at<=clock_timestamp()").bind(&self.key).bind(self.token).execute(&self.pool).await?;
        Ok(())
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        if self.sent {
            return;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let pool = self.pool.clone();
            let key = self.key.clone();
            let token = self.token;
            handle.spawn(async move {let _=sqlx::query("UPDATE routing_health_scopes SET probe_token=NULL,probe_until=NULL WHERE scope_hash=$1 AND probe_token=$2").bind(key).bind(token).execute(&pool).await;});
        }
    }
}
pub(super) async fn claim(
    state: &AppState,
    targets: &[InferenceTarget],
    calls: u32,
    wait: Duration,
) -> Result<Vec<Probe>> {
    let mut keys = BTreeSet::new();
    for t in targets {
        for (_, k) in all_roots(state, &t.reference()).await? {
            keys.insert(k);
        }
    }
    let mut tx = state.pg.begin().await?;
    let mut probes = Vec::new();
    for key in keys {
        let row:Option<(Value,bool,bool)>=sqlx::query_as("SELECT failure,expires_at>clock_timestamp(),COALESCE(probe_until>clock_timestamp(),false) FROM routing_health_scopes WHERE scope_hash=$1 FOR UPDATE").bind(&key).fetch_optional(&mut *tx).await?;
        if let Some((value, cooling, probing)) = row {
            if cooling || probing || calls != 1 {
                let f: ProviderFailure = serde_json::from_value(value)
                    .map_err(|_| RoutingError::Invalid("health probe failure"))?;
                return Err(f.app());
            }
            let token = Uuid::now_v7();
            sqlx::query("UPDATE routing_health_scopes SET probe_token=$2,probe_until=clock_timestamp()+($3::bigint*interval '1 millisecond'),updated_at=now() WHERE scope_hash=$1").bind(&key).bind(token).bind((wait.as_millis()+10000).clamp(1000,86410000) as i64).execute(&mut *tx).await?;
            probes.push(Probe {
                pool: state.pg.clone(),
                key,
                token,
                sent: false,
            });
        }
    }
    tx.commit().await?;
    Ok(probes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target() -> InferenceTarget {
        InferenceTarget {
            schema_version: 1,
            provider_id: Uuid::from_u128(1),
            provider_revision: 1,
            profile_revision: 1,
            adapter: AdapterKind::OpenaiChat,
            base_url: "https://fixture.invalid/v1".into(),
            model: "fixture".into(),
            api_key: "synthetic-private-key".into(),
            effort: None,
            generation: None,
            variant: None,
        }
    }
    #[test]
    fn stable_credential_health_is_shared_between_workers_but_not_private_owners() {
        let a = target();
        let mut b = a.clone();
        b.provider_id = Uuid::from_u128(2);
        b.model = "other-model".into();
        assert_eq!(
            credential_hash(&a, "deployment", None),
            credential_hash(&b, "deployment", None)
        );
        assert_ne!(
            credential_hash(&a, "user", Some(Uuid::from_u128(1))),
            credential_hash(&b, "user", Some(Uuid::from_u128(2)))
        );
        b.api_key = "rotated-synthetic-key".into();
        assert_ne!(
            credential_hash(&a, "deployment", None),
            credential_hash(&b, "deployment", None)
        );
    }
}
