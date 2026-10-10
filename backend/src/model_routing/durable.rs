// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Server-owned, fenced JSON execution. Interrupted requests are never replayed.
use serde::{Deserialize,Serialize};
use uuid::Uuid;
use super::{*,budget::{RunBudget,Usage}};
use crate::{AppState,Result,auth::{AuthContext,PlatformRole}};

#[derive(Clone,Serialize,Deserialize)]
pub(crate) struct Context {
    user_id:Uuid,#[serde(with="stored_role")] role:PlatformRole,email:Option<String>,display_name:Option<String>,
    chat_id:Option<Uuid>,project_id:Option<Uuid>,run_id:Uuid,purpose:CallPurpose,
}
// PlatformRole's serde names are Rust variant names; database authority uses
// snake_case. Keep this durable format explicit without changing public auth JSON.
mod stored_role {
    use super::*;
    pub fn serialize<S:serde::Serializer>(role:&PlatformRole,serializer:S)->std::result::Result<S::Ok,S::Error> {
        serializer.serialize_str(role.as_str())
    }
    pub fn deserialize<'de,D:serde::Deserializer<'de>>(deserializer:D)->std::result::Result<PlatformRole,D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "user"=>Ok(PlatformRole::User),"power_user"=>Ok(PlatformRole::PowerUser),"client_admin"=>Ok(PlatformRole::ClientAdmin),
            _=>Err(serde::de::Error::custom("invalid durable actor role")),
        }
    }
}
impl Context {
    pub fn capture(c:&RoutingContext)->Result<Self> {
        if c.actor.break_glass || c.actor.mfa_enroll_only || c.actor.role==PlatformRole::SuperAdmin {
            return Err(RoutingError::Forbidden.into());
        }
        Ok(Self {user_id:c.actor.user_id.ok_or(RoutingError::Forbidden)?,role:c.actor.role,
            email:c.actor.email.clone(),display_name:c.actor.display_name.clone(),chat_id:c.chat_id,
            project_id:c.project_id,run_id:c.run_id,purpose:c.purpose})
    }
    pub fn actor(&self)->AuthContext {AuthContext {user_id:Some(self.user_id),role:self.role,email:self.email.clone(),
        display_name:self.display_name.clone(),break_glass:false,mfa_enroll_only:false}}
    pub async fn validate(&self,pool:&sqlx::PgPool)->Result<()> {
        let role:Option<String>=sqlx::query_scalar("SELECT role::text FROM users WHERE id=$1 AND deactivated_at IS NULL")
            .bind(self.user_id).fetch_optional(pool).await?;
        // Neither promotion nor demotion silently changes a queued principal.
        if role.as_deref()!=Some(self.role.as_str()) {return Err(RoutingError::Forbidden.into());}
        Ok(())
    }
    pub fn routing(&self)->RoutingContext {RoutingContext {actor:self.actor(),chat_id:self.chat_id,
        project_id:self.project_id,run_id:self.run_id,purpose:self.purpose}}
}
pub(crate) struct Frozen {pub context:Context,pub plan:ExecutionPlan,pub limits:RoutingConstraints,pub usage:Usage,pub deadline:time::OffsetDateTime}

/// The worker owns one fenced lease. Never returned to a client or task payload.
pub(crate) struct Lease {pub pool:sqlx::PgPool,pub id:Uuid,pub owner:Uuid}
impl Lease {
    pub async fn checkpoint(&self,budget:&RunBudget)->Result<()> {
        let n=sqlx::query("UPDATE routed_jobs SET limits=$3,usage=$4,updated_at=now() WHERE id=$1 AND owner=$2 AND status='running' AND lease_until>clock_timestamp() AND deadline>clock_timestamp() AND (kind<>'workflow_agent' OR routing_workflow_active(id)) AND (input->>'public_launch' IS DISTINCT FROM 'true' OR EXISTS(SELECT 1 FROM config_settings WHERE key=CASE routed_jobs.kind WHEN 'verify_draft' THEN 'routing.document_jobs_enabled' WHEN 'deep_research' THEN 'routing.research_jobs_enabled' WHEN 'ingest' THEN CASE WHEN input->'source'->>'origin'='connector_import' THEN 'routing.connector_ingestion_enabled' ELSE 'routing.ingestion_enabled' END END AND value_type='bool' AND value='true'))")
            .bind(self.id).bind(self.owner).bind(serde_json::to_value(budget.limits()).map_err(|_|RoutingError::Invalid("job limits"))?)
            .bind(serde_json::to_value(budget.snapshot()).map_err(|_|RoutingError::Invalid("job usage"))?).execute(&self.pool).await?.rows_affected();
        if n!=1 {return Err(crate::AppError::Unavailable("durable inference checkpoint lost its lease".into()));} Ok(())
    }
    /// Hold the same row until the document/audit transaction commits. Expiry,
    /// cancellation and a replacement worker cannot race through this fence.
    pub async fn fence(&self,tx:&mut sqlx::Transaction<'_,sqlx::Postgres>)->Result<()> {
        let allowed:Option<bool>=sqlx::query_scalar("SELECT owner=$2 AND status='running' AND lease_until>clock_timestamp() AND deadline>clock_timestamp() AND (kind<>'workflow_agent' OR routing_workflow_active(id)) AND EXISTS(SELECT 1 FROM users u WHERE u.id=routed_jobs.actor_user_id AND u.deactivated_at IS NULL AND u.role::text=routed_jobs.context->>'role') AND (input->>'public_launch' IS DISTINCT FROM 'true' OR EXISTS(SELECT 1 FROM config_settings WHERE key=CASE routed_jobs.kind WHEN 'verify_draft' THEN 'routing.document_jobs_enabled' WHEN 'deep_research' THEN 'routing.research_jobs_enabled' WHEN 'ingest' THEN CASE WHEN input->'source'->>'origin'='connector_import' THEN 'routing.connector_ingestion_enabled' ELSE 'routing.ingestion_enabled' END END AND value_type='bool' AND value='true')) FROM routed_jobs WHERE id=$1 FOR UPDATE")
            .bind(self.id).bind(self.owner).fetch_optional(&mut **tx).await?;
        if allowed!=Some(true) {return Err(crate::AppError::Unavailable("durable commit lost its lease or actor".into()));} Ok(())
    }
}
pub(crate) fn restore(state:&AppState,context:Context,plan:ExecutionPlan,limits:RoutingConstraints,usage:Usage,remaining_ms:u64,lease:Lease)->Result<run::RoutedRun> {
    let budget=RunBudget::restore(limits,usage,remaining_ms)?;
    run::RoutedRun::from_durable(context.routing(),plan,budget,transport::PipelineClient::from_state(state)?,lease)
}
