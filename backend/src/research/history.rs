// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use uuid::Uuid;
use crate::{AppState,Result,AppError,auth::AuthContext,model_routing::{RoutingContext,CallPurpose,RoutingError,durable::Context}};
use super::jobs::{self,Launch,Started};

async fn actor(state:&AppState,auth:&AuthContext)->Result<Uuid> {
    let context=RoutingContext {actor:auth.clone(),chat_id:None,project_id:None,run_id:Uuid::nil(),purpose:CallPurpose::ResearchPlan};
    Context::capture(&context)?.validate(&state.pg).await?;
    auth.user_id.ok_or_else(||RoutingError::Forbidden.into())
}
pub(super) async fn launch(state:&AppState,auth:&AuthContext,body:Launch)->Result<Started> {
    let Some(key)=body.request_id else {return jobs::launch_once(state,auth,body).await;};
    let uid=actor(state,auth).await?;
    body.request.validate()?;
    let hash=hex::encode(Sha256::digest(serde_json::to_vec(&body).map_err(|_|RoutingError::Invalid("research request"))?));
    // A committed insert precedes classifier/model work. Cancellation, crash or
    // rolled-back enqueue cannot make this key reusable for another paid call.
    let claimed=sqlx::query("INSERT INTO research_launch_receipts(actor_user_id,request_id,actor_role,request_hash) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(uid).bind(key).bind(auth.role.as_str()).bind(&hash).execute(&state.pg).await?.rows_affected();
    if claimed==0 {
        let (saved,role):(String,String)=sqlx::query_as("SELECT request_hash,actor_role FROM research_launch_receipts WHERE actor_user_id=$1 AND request_id=$2")
            .bind(uid).bind(key).fetch_one(&state.pg).await?;
        if role!=auth.role.as_str() {return Err(RoutingError::Forbidden.into());}
        if saved!=hash {return Err(AppError::Conflict("Research request ID was already used with different parameters".into()));}
        let receipt=lookup(state,auth,key).await?;
        if receipt["status"]=="succeeded" {
            return Ok(Started {job_id:serde_json::from_value(receipt["job_id"].clone()).map_err(|_|RoutingError::Invalid("launch receipt"))?,
                chat_id:serde_json::from_value(receipt["chat_id"].clone()).map_err(|_|RoutingError::Invalid("launch receipt"))?,
                status_url:receipt["status_url"].as_str().unwrap().into()});
        }
        return Err(AppError::Conflict("Research launch is pending or stopped. Read its receipt; this key will not restart planning".into()));
    }
    let result=match tokio::time::timeout(std::time::Duration::from_secs(120),Box::pin(jobs::launch_once(state,auth,body))).await {
        Ok(result)=>result,Err(_)=>Err(AppError::Unavailable("Research planning timed out; this request will not restart automatically".into())),
    };
    if result.is_err() {
        // Lost commit acknowledgement must not overwrite a successful receipt.
        // A failed update still leaves the planning tombstone in place.
        let _=sqlx::query("UPDATE research_launch_receipts SET status='failed' WHERE actor_user_id=$1 AND request_id=$2 AND status='planning'")
            .bind(uid).bind(key).execute(&state.pg).await;
    }
    result
}
pub async fn lookup(state:&AppState,auth:&AuthContext,key:Uuid)->Result<Value> {
    let uid=actor(state,auth).await?;
    let row:Option<(String,String,Option<Uuid>)>=sqlx::query_as("SELECT actor_role,CASE WHEN status='planning' AND deadline<=clock_timestamp() THEN 'interrupted' ELSE status END,job_id FROM research_launch_receipts WHERE actor_user_id=$1 AND request_id=$2")
        .bind(uid).bind(key).fetch_optional(&state.pg).await?;
    let (role,status,job)=row.ok_or(AppError::NotFound("Research launch receipt not found".into()))?;
    if role!=auth.role.as_str() {return Err(RoutingError::Forbidden.into());}
    let mut result=json!({"request_id":key,"status":status});
    if let Some(id)=job {
        let status=jobs::status(state,auth,id).await?;
        result["job_id"]=json!(id);result["chat_id"]=status["chat_id"].clone();
        result["status_url"]=json!(format!("/api/research-jobs/{id}"));
    }
    Ok(result)
}

/// Keyset pages include redacted owner-only entries after source access loss,
/// preserving cancellation without revealing source/template/chat content.
pub async fn list(state:&AppState,auth:&AuthContext,before:Option<Uuid>)->Result<Value> {
    let uid=actor(state,auth).await?;
    let mut rows:Vec<(Uuid,String,time::OffsetDateTime)>=sqlx::query_as("SELECT id,status,created_at FROM routed_jobs WHERE actor_user_id=$1 AND kind='deep_research' AND ($2::uuid IS NULL OR id<$2) ORDER BY id DESC LIMIT 21")
        .bind(uid).bind(before).fetch_all(&state.pg).await?;
    let more=rows.len()>20;rows.truncate(20);
    let cursor=if more {rows.last().map(|r|r.0)} else {None};let mut items=Vec::new();
    for (id,status,created) in rows {
        let mut item=json!({"job_id":id,"status":status,"created_at":created.format(&time::format_description::well_known::Rfc3339)
            .map_err(|_|RoutingError::Invalid("research creation time"))?,"accessible":false});
        match jobs::status(state,auth,id).await {
            Ok(view)=>{
                let chat:Uuid=serde_json::from_value(view["chat_id"].clone()).map_err(|_|RoutingError::Invalid("research chat"))?;
                let title:Option<String>=sqlx::query_scalar("SELECT title FROM chats WHERE id=$1 AND owner_user_id=$2 AND archived_at IS NULL")
                    .bind(chat).bind(uid).fetch_optional(&state.pg).await?;
                if let Some(title)=title {
                    item["accessible"]=json!(true);item["title"]=json!(title);item["chat_id"]=json!(chat);
                    item["selection"]=view["selection"].clone();item["usage"]=view["usage"].clone();
                }
            },
            Err(AppError::Database(e))=>return Err(e.into()),
            Err(_)=>{},
        }
        items.push(item);
    }
    Ok(json!({"items":items,"next_cursor":cursor}))
}
