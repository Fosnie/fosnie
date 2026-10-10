// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit bounded cleanup of confirmed retired physical collections only.
use crate::{AppState,Result,auth::AuthContext};
use serde_json::json;
pub async fn cleanup(state:&AppState,actor:&AuthContext)->Result<usize> {
    state.rbac.require_permission(&state.pg,actor,crate::auth::permissions::PROVIDERS_MANAGE).await?;
    crate::embedding_promotion::require_ready(&state.pg).await?;
    let candidates:Vec<String>=sqlx::query_scalar("SELECT collection_name FROM embedding_retired_indices WHERE status='retained' AND retired_at<now()-interval '24 hours' ORDER BY retired_at DESC OFFSET 1 LIMIT 16")
        .fetch_all(&state.pg).await?;
    let mut cleaned=0;
    for name in candidates {
        let mut tx=state.pg.begin().await?;
        let index:Option<(String,Option<String>,Option<String>,Option<i32>)>=sqlx::query_as("SELECT collection_name,desired_collection_name,desired_model,desired_dim FROM embedding_index WHERE id=1 FOR UPDATE")
            .fetch_optional(&mut *tx).await?;
        let Some((active,desired,model,dim))=index else {continue;};
        let desired=desired.or_else(||model.zip(dim).map(|(m,d)|crate::model_routing::fixed::desired_collection(d,&m)));
        if active==name || desired.as_deref()==Some(&name) {continue;}
        let owned:Option<bool>=sqlx::query_scalar("SELECT status='retained' AND retired_at<now()-interval '24 hours' FROM embedding_retired_indices WHERE collection_name=$1 FOR UPDATE SKIP LOCKED")
            .bind(&name).fetch_optional(&mut *tx).await?;
        if owned!=Some(true) {continue;}
        let busy:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM embedding_promotions WHERE status='pending' AND (old_collection=$1 OR new_collection=$1)) OR EXISTS(SELECT 1 FROM routed_jobs WHERE kind='reindex' AND status IN ('queued','running') AND (input->'source'->>'old'=$1 OR input->'source'->>'new'=$1))")
            .bind(&name).fetch_one(&mut *tx).await?;
        if busy {continue;}
        let response=state.http.post(format!("{}/cleanup-embedding-index",state.boot.ml.base_url.trim_end_matches('/')))
            .header("x-fosnie-ml-key",&state.boot.ml.shared_secret).json(&json!({"collection":name,"expected_active":active}))
            .timeout(std::time::Duration::from_secs(15)).send().await.map_err(|_|crate::AppError::Unavailable("retired index cleanup unavailable".into()))?;
        if !response.status().is_success() {return Err(crate::AppError::Unavailable("retired index cleanup refused".into()));}
        sqlx::query("UPDATE embedding_retired_indices SET status='deleted',deleted_at=now() WHERE collection_name=$1")
            .bind(&name).execute(&mut *tx).await?;
        let mut event=crate::audit::AuditEvent::action("embedding.retired_index.deleted",actor.role.as_str());event.actor_user_id=actor.user_id;
        event.payload=Some(json!({"retention_hours":24,"newest_retired_preserved":true}));crate::audit::append_with(&mut tx,&event).await?;
        tx.commit().await?;cleaned+=1;
    }
    Ok(cleaned)
}
