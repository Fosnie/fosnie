// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Revision-bound cooldowns, never a replacement for access/capability checks.
use sha2::{Digest,Sha256};
use serde_json::Value;
use std::sync::OnceLock;
static CREDENTIAL_PEPPER:OnceLock<[u8;32]>=OnceLock::new();
use super::{*,failure::ProviderFailure,fallback::FailureKind};
fn purpose(p:CallPurpose)->String {serde_json::to_value(p).unwrap().as_str().unwrap().into()}
fn binding(target:&InferenceTarget)->Option<String> {
    if target.api_key.is_empty() {return None;}
    let url=reqwest::Url::parse(&target.base_url).ok()?;
    let origin=format!("{}://{}:{}",url.scheme(),url.host_str()?,url.port_or_known_default()?);
    // Opaque digest stays in the private database, never in a response or audit.
    let pepper=CREDENTIAL_PEPPER.get_or_init(||{let mut p=[0;32];p[..16].copy_from_slice(uuid::Uuid::now_v7().as_bytes());p[16..].copy_from_slice(uuid::Uuid::now_v7().as_bytes());p});
    let mut digest=Sha256::new();digest.update(pepper);digest.update(origin.as_bytes());digest.update([0]);digest.update(target.api_key.as_bytes());
    Some(digest.finalize().iter().map(|b|format!("{b:02x}")).collect())
}
pub async fn blocked(state:&crate::AppState,reference:&TargetReference,p:CallPurpose)->crate::Result<Option<ProviderFailure>> {
    let value:Option<(Value,i64)>=sqlx::query_as("SELECT failure, GREATEST(1,ceil(extract(epoch FROM expires_at-now()))::bigint) FROM routing_provider_health WHERE provider_id=$1 AND provider_revision=$2 AND profile_revision=$3 AND purpose IN ('all',$4) AND expires_at>now() ORDER BY expires_at DESC LIMIT 1")
        .bind(reference.provider_id).bind(reference.provider_revision).bind(reference.profile_revision).bind(purpose(p)).fetch_optional(&state.pg).await?;
    if let Some((value,remaining))=value {
        let mut failure:ProviderFailure=serde_json::from_value(value).map_err(|_|RoutingError::Invalid("stored provider health"))?;
        failure.before_output=true;failure.retry_after_seconds=Some(remaining.clamp(1,86400) as u32);return Ok(Some(failure));
    }
    let value:Option<(Value,i64)>=sqlx::query_as("SELECT h.failure, GREATEST(1,ceil(extract(epoch FROM h.expires_at-now()))::bigint) FROM routing_credential_bindings b JOIN routing_credential_health h ON h.binding_hash=b.binding_hash JOIN provider_configs source ON source.id=h.source_provider_id WHERE b.provider_id=$1 AND b.provider_revision=$2 AND source.routing_revision=h.source_revision AND h.expires_at>now()")
        .bind(reference.provider_id).bind(reference.provider_revision).fetch_optional(&state.pg).await?;
    value.map(|(value,remaining)|{
        let mut failure:ProviderFailure=serde_json::from_value(value).map_err(|_|RoutingError::Invalid("stored credential health"))?;
        failure.before_output=true;failure.retry_after_seconds=Some(remaining.clamp(1,86400) as u32);Ok::<_,RoutingError>(failure)
    }).transpose().map_err(Into::into)
}
pub async fn filter(state:&crate::AppState,profiles:Vec<ModelProfile>,p:CallPurpose)->crate::Result<Vec<ModelProfile>> {
    let mut result=Vec::new();
    for profile in profiles {
        let reference=TargetReference{provider_id:profile.provider_id,provider_revision:profile.provider_revision,profile_revision:profile.revision,model:profile.model.clone(),effort:None};
        if blocked(state,&reference,p).await?.is_none() {result.push(profile);}
    }
    Ok(result)
}
pub async fn register(state:&crate::AppState,target:&InferenceTarget,p:CallPurpose)->crate::Result<()> {
    if let Some(hash)=binding(target) {
        sqlx::query("INSERT INTO routing_credential_bindings(provider_id,provider_revision,binding_hash) VALUES($1,$2,$3) ON CONFLICT(provider_id) DO UPDATE SET provider_revision=EXCLUDED.provider_revision,binding_hash=EXCLUDED.binding_hash")
            .bind(target.provider_id).bind(target.provider_revision).bind(hash).execute(&state.pg).await?;
    }
    if let Some(failure)=blocked(state,&target.reference(),p).await? {return Err(failure.app());}
    Ok(())
}
pub async fn note(state:&crate::AppState,target:&InferenceTarget,_p:CallPurpose,failure:&ProviderFailure)->crate::Result<()> {
    if !failure.valid() || matches!(failure.kind,FailureKind::InvalidRequest) {return Ok(());}
    sqlx::query("DELETE FROM routing_provider_health WHERE expires_at<=now()").execute(&state.pg).await?;
    sqlx::query("DELETE FROM routing_credential_health WHERE expires_at<=now()").execute(&state.pg).await?;
    let seconds=failure.retry_after_seconds.unwrap_or(if matches!(failure.kind,FailureKind::QuotaExhausted|FailureKind::PaymentRequired|FailureKind::Unauthorized){300}else{30}).min(86400);
    let mut value=failure.clone();value.retry_after_seconds=Some(seconds);
    sqlx::query("INSERT INTO routing_provider_health(provider_id,provider_revision,profile_revision,purpose,failure,expires_at) VALUES($1,$2,$3,'all',$4,now()+($5::bigint * interval '1 second')) ON CONFLICT(provider_id,provider_revision,profile_revision,purpose) DO UPDATE SET failure=EXCLUDED.failure,expires_at=GREATEST(routing_provider_health.expires_at,EXCLUDED.expires_at)")
        .bind(target.provider_id).bind(target.provider_revision).bind(target.profile_revision).bind(serde_json::to_value(&value).unwrap()).bind(i64::from(seconds)).execute(&state.pg).await?;
    if matches!(failure.kind,FailureKind::QuotaExhausted|FailureKind::PaymentRequired|FailureKind::Unauthorized) {
        if let Some(hash)=binding(target) {
            sqlx::query("INSERT INTO routing_credential_health(binding_hash,source_provider_id,source_revision,failure,expires_at) VALUES($1,$2,$3,$4,now()+($5::bigint * interval '1 second')) ON CONFLICT(binding_hash) DO UPDATE SET source_provider_id=EXCLUDED.source_provider_id,source_revision=EXCLUDED.source_revision,failure=EXCLUDED.failure,expires_at=GREATEST(routing_credential_health.expires_at,EXCLUDED.expires_at)")
                .bind(hash).bind(target.provider_id).bind(target.provider_revision).bind(serde_json::to_value(&value).unwrap()).bind(i64::from(seconds)).execute(&state.pg).await?;
        }
    }
    Ok(())
}

pub async fn note_contract(state:&crate::AppState,target:&InferenceTarget,p:CallPurpose)->crate::Result<()> {
    let failure=ProviderFailure {schema_version:1,kind:FailureKind::InvalidRequest,before_output:true,retry_after_seconds:Some(30)};
    sqlx::query("INSERT INTO routing_provider_health(provider_id,provider_revision,profile_revision,purpose,failure,expires_at) VALUES($1,$2,$3,$4,$5,now()+interval '30 seconds') ON CONFLICT(provider_id,provider_revision,profile_revision,purpose) DO UPDATE SET failure=EXCLUDED.failure,expires_at=EXCLUDED.expires_at")
        .bind(target.provider_id).bind(target.provider_revision).bind(target.profile_revision).bind(purpose(p)).bind(serde_json::to_value(failure).unwrap()).execute(&state.pg).await?;
    Ok(())
}
