// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Durable model-free embedding publication. Never delete the old index here.
use crate::{AppState,Result,AppError};
use serde_json::{json,Value};
use uuid::Uuid;

pub async fn pending(pg:&sqlx::PgPool)->Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM embedding_promotions WHERE status='pending')").fetch_one(pg).await?)
}
pub async fn require_ready(pg:&sqlx::PgPool)->Result<()> {
    if pending(pg).await? {return Err(AppError::Unavailable("embedding promotion recovery is pending".into()));} Ok(())
}

/// Persist intent before any alias mutation. `expected` is captured before the
/// build, so a changed desired target cannot adopt vectors from an older build.
pub async fn promote(state:&AppState,new:&str,old:&str,expected:i64)->Result<()> {
    promote_inner(state,new,old,expected,None).await
}
pub(crate) async fn promote_rebuild(state:&AppState,new:&str,old:&str,expected:i64,task:Uuid)->Result<()> {
    promote_inner(state,new,old,expected,Some(task)).await
}
async fn promote_inner(state:&AppState,new:&str,old:&str,expected:i64,task:Option<Uuid>)->Result<()> {
    let mut tx=state.pg.begin().await?;
    let row:Option<(String,Option<String>,Option<i32>,Option<String>,i64)>=sqlx::query_as(
        "SELECT collection_name,desired_model,desired_dim,desired_collection_name,desired_routing_revision FROM embedding_index WHERE id=1 FOR UPDATE")
        .fetch_optional(&mut *tx).await?;
    let (active,model,dim,desired,revision)=row.ok_or_else(||AppError::Conflict("embedding index unavailable".into()))?;
    let target=desired.or_else(||model.as_ref().zip(dim).map(|(m,d)|crate::model_routing::fixed::desired_collection(d,m)));
    if active!=old || target.as_deref()!=Some(new) || expected!=revision || old==new {
        return Err(AppError::Conflict("embedding publication target changed".into()));
    }
    let (digest,job)=if let Some(task)=task {
        let built:Option<(Option<String>,Option<Uuid>)>=sqlx::query_as("SELECT source_sha256,routed_job_id FROM embedding_rebuild_attempts WHERE task_id=$1 AND status='built' AND desired_revision=$2 AND new_collection=$3 AND old_collection=$4")
            .bind(task).bind(expected).bind(new).bind(old).fetch_optional(&mut *tx).await?;
        let (digest,job)=built.ok_or_else(||AppError::Conflict("embedding build checkpoint no longer owns publication".into()))?;
        if job.is_none() {return Err(AppError::Conflict("routed embedding build has no original job".into()));}
        (digest,job)
    } else {(None,None)};
    let existing:Option<(String,String,i64,Option<Uuid>)>=sqlx::query_as("SELECT old_collection,new_collection,desired_revision,routed_job_id FROM embedding_promotions WHERE status='pending'")
        .fetch_optional(&mut *tx).await?;
    if let Some((a,b,r,j))=existing {
        if a!=old || b!=new || r!=expected || j!=job {return Err(AppError::Conflict("another embedding promotion is pending".into()));}
    } else {
        sqlx::query("INSERT INTO embedding_promotions(id,old_collection,new_collection,desired_revision,source_sha256,routed_job_id) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(Uuid::now_v7()).bind(old).bind(new).bind(expected).bind(&digest).bind(job).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    recover(state).await
}

pub async fn recover(state:&AppState)->Result<()> {
    let mut tx=state.pg.begin().await?;
    let row:Option<(Uuid,String,String,i64,Option<String>,Option<Uuid>)>=sqlx::query_as(
        "SELECT id,old_collection,new_collection,desired_revision,source_sha256,routed_job_id FROM embedding_promotions WHERE status='pending' FOR UPDATE SKIP LOCKED")
        .fetch_optional(&mut *tx).await?;
    let Some((id,old,new,revision,digest,job))=row else {return Ok(());};
    let current:Option<(String,String,i32,i64)>=sqlx::query_as(
        "SELECT collection_name,desired_model,desired_dim,desired_routing_revision FROM embedding_index WHERE id=1 AND desired_model IS NOT NULL AND desired_dim>0 FOR UPDATE")
        .fetch_optional(&mut *tx).await?;
    let Some((active,model,dim,actual))=current else {return Err(AppError::Conflict("embedding recovery requires the captured desired target".into()));};
    if active!=old || actual!=revision {return Err(AppError::Conflict("embedding recovery provenance changed".into()));}
    let authorised=crate::model_routing::reindex::publication_authorised(state,job,&new,revision,digest.as_deref()).await.unwrap_or(false);
    let response=state.http.post(format!("{}/swap-embedding-alias",state.boot.ml.base_url.trim_end_matches('/')))
        .header("x-fosnie-ml-key",&state.boot.ml.shared_secret).json(&json!({"new_collection":new,"old_collection":old,"expected_old_collection":old,"expected_source_sha256":digest,"publication_authorised":authorised}))
        .timeout(std::time::Duration::from_secs(15)).send().await.map_err(|_|AppError::Unavailable("embedding alias confirmation unavailable".into()))?;
    if response.status()==reqwest::StatusCode::CONFLICT {
        let refusal:Value=response.json().await.unwrap_or(Value::Null);
        if refusal["detail"]=="embedding_publication_refused" {
            // ML proved the alias still points to the source. Release this
            // failed intent; a fresh explicit attempt may capture current data.
            sqlx::query("UPDATE embedding_promotions SET status='failed',completed_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await?;
            sqlx::query("UPDATE routed_jobs j SET status='failed',owner=NULL,lease_until=NULL,updated_at=now() FROM embedding_rebuild_attempts a WHERE a.routed_job_id=j.id AND a.status='built' AND a.desired_revision=$1 AND a.new_collection=$2 AND a.routed_job_id IS NOT DISTINCT FROM $3 AND j.kind='reindex' AND j.status IN ('running','interrupted')")
                .bind(revision).bind(&new).bind(job).execute(&mut *tx).await?;
            sqlx::query("UPDATE tasks t SET status='dead_letter',finished_at=now(),last_error='Embedding publication refused; explicitly start a new attempt' FROM embedding_rebuild_attempts a WHERE a.task_id=t.id AND a.status='built' AND a.desired_revision=$1 AND a.new_collection=$2 AND a.routed_job_id IS NOT DISTINCT FROM $3 AND t.status IN ('queued','running')")
                .bind(revision).bind(&new).bind(job).execute(&mut *tx).await?;
            sqlx::query("UPDATE embedding_rebuild_attempts SET status='failed' WHERE status='built' AND desired_revision=$1 AND new_collection=$2 AND routed_job_id IS NOT DISTINCT FROM $3")
                .bind(revision).bind(&new).bind(job).execute(&mut *tx).await?;
            sqlx::query("UPDATE embedding_index SET status='failed',error='Source or authority changed before publication; explicitly start a new attempt' WHERE id=1 AND desired_routing_revision=$1")
                .bind(revision).execute(&mut *tx).await?;
            let mut event=crate::audit::AuditEvent::action("embedding.promotion.refused","system");event.resource_type=Some("embedding_promotion".into());event.resource_id=Some(id);
            crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;
            return Err(AppError::Conflict("embedding publication source or authority changed".into()));
        }
        return Err(AppError::Unavailable("embedding alias refusal was not confirmed".into()));
    }
    if !response.status().is_success() {return Err(AppError::Unavailable("embedding alias confirmation refused; old index retained".into()));}
    let value:Value=response.json().await.map_err(|_|AppError::Unavailable("embedding alias confirmation invalid".into()))?;
    if value["active_collection"]!=new {return Err(AppError::Conflict("embedding alias confirmation mismatch".into()));}
    // Alias confirmation may be repeated, never the embedding build. All SQL
    // promotion state, KB dimensions and adopted active profiles commit together.
    sqlx::query("UPDATE embedding_index SET embed_model=desired_model,embed_base_url=desired_base_url,embed_api_key_encrypted=desired_api_key_encrypted,dim=desired_dim,collection_name=$1,status='active',error=NULL,reindex_done=reindex_total,desired_model=NULL,desired_base_url=NULL,desired_api_key_encrypted=NULL,desired_dim=NULL,desired_collection_name=NULL,built_at=now(),updated_at=now() WHERE id=1 AND desired_routing_revision=$2")
        .bind(&new).bind(revision).execute(&mut *tx).await?;
    sqlx::query("UPDATE knowledge_bases SET embedding_model_id=$1,embedding_dimension=$2 WHERE archived_at IS NULL")
        .bind(&model).bind(dim).execute(&mut *tx).await?;
    let active_revision:i64=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_one(&mut *tx).await?;
    let profiles:Vec<(Uuid,Value)>=sqlx::query_as("SELECT f.provider_id,f.profile FROM fixed_inference_profiles f JOIN provider_configs p ON p.id=f.provider_id WHERE f.slot='embed_desired' AND p.scope='deployment' AND p.enabled")
        .fetch_all(&mut *tx).await?;
    for (provider,raw) in profiles {
        let mut profile:crate::model_routing::fixed::FixedProfile=serde_json::from_value(raw).map_err(|_|AppError::Validation("stored desired embedding profile".into()))?;
        let provider_revision:i64=sqlx::query_scalar("SELECT routing_revision FROM provider_configs WHERE id=$1").bind(provider).fetch_one(&mut *tx).await?;
        if profile.model!=model || profile.desired_index_revision!=Some(revision) || profile.provider_revision!=provider_revision {continue;}
        profile.role=crate::model_routing::fixed::FixedRole::Embed;profile.active_index_revision=Some(active_revision);profile.desired_index_revision=None;
        profile.revision=sqlx::query_scalar("SELECT nextval('routing_revision_generation')").fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO fixed_inference_profiles(provider_id,slot,revision,profile) VALUES($1,'embed',$2,$3) ON CONFLICT(provider_id,slot) DO UPDATE SET revision=$2,profile=$3,updated_at=now()")
            .bind(provider).bind(profile.revision).bind(serde_json::to_value(&profile).map_err(|_|AppError::Validation("embedding profile".into()))?).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE embedding_promotions SET status='completed',completed_at=now() WHERE id=$1").bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE embedding_rebuild_attempts SET status='completed' WHERE status='built' AND desired_revision=$1 AND new_collection=$2 AND routed_job_id IS NOT DISTINCT FROM $3")
        .bind(revision).bind(&new).bind(job).execute(&mut *tx).await?;
    sqlx::query("UPDATE routed_jobs j SET status='succeeded',owner=NULL,lease_until=NULL,result_id=j.id,updated_at=now() FROM embedding_rebuild_attempts a WHERE a.routed_job_id=j.id AND a.status='completed' AND a.desired_revision=$1 AND a.new_collection=$2 AND a.routed_job_id IS NOT DISTINCT FROM $3 AND j.kind='reindex' AND j.status IN ('running','interrupted')")
        .bind(revision).bind(&new).bind(job).execute(&mut *tx).await?;
    let retained=if old==crate::embedding_index::DEFAULT_COLLECTION {
        use sha2::{Digest,Sha256};let digest=hex::encode(Sha256::digest(format!("{old}|{new}").as_bytes()));
        format!("pai_kb__literal_backup_{}",&digest[..24])
    } else {old.clone()};
    sqlx::query("INSERT INTO embedding_retired_indices(collection_name,promotion_id) VALUES($1,$2) ON CONFLICT DO NOTHING")
        .bind(retained).bind(id).execute(&mut *tx).await?;
    let mut event=crate::audit::AuditEvent::action("embedding.promotion.completed","system");
    event.resource_type=Some("embedding_promotion".into());event.resource_id=Some(id);event.payload=Some(json!({"old_collection_retained":true,"desired_revision":revision}));
    crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;Ok(())
}
