// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Private actual Core consumer; only isolated public/synthetic format inputs.
use std::sync::Arc;
use axum::{Router,routing::post,extract::State,Json};
use serde::Deserialize;
use serde_json::{json,Value};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,keycloak::AuthUser},model_routing::{*,fixed}};
#[allow(dead_code)]
#[path="support/routing_pilot_shared.rs"]
mod pilot;

use std::sync::atomic::{AtomicBool,Ordering};
use fosnie_backend::{ext::RbacPolicy,auth::rbac::{ResourceType,Permission,FlatRbacPolicy},state::AppStateBuilder};
struct Policy(Arc<AtomicBool>);
#[async_trait::async_trait]
impl RbacPolicy for Policy {
    async fn can(&self,p:&sqlx::PgPool,a:&AuthContext,r:ResourceType,id:Uuid,v:Permission)->fosnie_backend::Result<bool> {
        FlatRbacPolicy.can(p,a,r,id,v).await
    }
    async fn may_grant(&self,p:&sqlx::PgPool,a:&AuthContext,r:ResourceType,id:Uuid)->fosnie_backend::Result<bool> {
        FlatRbacPolicy.may_grant(p,a,r,id).await
    }
    async fn denied_kb_doc_ids(&self,p:&sqlx::PgPool,_:&AuthContext,kbs:&[Uuid])->fosnie_backend::Result<Vec<Uuid>> {
        if self.0.load(Ordering::SeqCst) {Ok(sqlx::query_scalar("SELECT id FROM kb_documents WHERE kb_id=ANY($1)").bind(kbs).fetch_all(p).await?)} else {Ok(vec![])}
    }
}
struct Edition {denied:Arc<AtomicBool>}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Replace {doc_id:Uuid,bytes_base64:String}
#[async_trait::async_trait]
impl pilot::PilotEdition for Edition {
    fn configure(&self,builder:AppStateBuilder)->AppStateBuilder {builder.with_rbac(Arc::new(Policy(self.denied.clone())))}
    async fn prepare(&self,state:&AppState,actor:&AuthContext,flow:Option<&Value>,config:Option<&Value>)->anyhow::Result<pilot::EditionFixture> {
        let flow=flow.ok_or_else(||anyhow::anyhow!("format flow required"))?;
        let cfg=config.ok_or_else(||anyhow::anyhow!("format fixture required"))?;
        let desired=cfg["desired_collection"].as_str().ok_or_else(||anyhow::anyhow!("desired collection"))?;
        let endpoint=cfg["desired_embed_url"].as_str().ok_or_else(||anyhow::anyhow!("desired endpoint"))?;
        let url=reqwest::Url::parse(endpoint)?;
        anyhow::ensure!(url.scheme()=="http" && url.host_str()==Some("127.0.0.1") && url.port().is_some()
            && url.username().is_empty() && url.password().is_none() && url.query().is_none(),"private local desired endpoint only");
        anyhow::ensure!(desired.starts_with("routing_v19_") && desired.len()==44 && desired[12..].bytes().all(|b|b.is_ascii_hexdigit())
            && flow["collection"]!=desired,"distinct private desired collection");
        let kb=Uuid::parse_str(flow["kb_id"].as_str().unwrap())?;
        let provider=Uuid::parse_str(flow["fixed_providers"]["embed"].as_str().unwrap())?;
        let model="qwen3-embedding:0.6b";
        fosnie_backend::embedding_index::set_desired(&state.pg,state.message_key,model,Some(endpoint),Some(""),1024,actor.user_id).await?;
        sqlx::query("UPDATE embedding_index SET desired_collection_name=$1 WHERE id=1").bind(desired).execute(&state.pg).await?;
        fosnie_backend::embedding_index::begin_reindex(&state.pg,0,actor.user_id).await?;
        let revision:i64=sqlx::query_scalar("SELECT desired_routing_revision FROM embedding_index WHERE id=1").fetch_one(&state.pg).await?;
        let provider_revision:i64=sqlx::query_scalar("SELECT routing_revision FROM provider_configs WHERE id=$1").bind(provider).fetch_one(&state.pg).await?;
        let profile=fixed::FixedProfile {provider_id:provider,provider_revision,revision:1,role:fixed::FixedRole::EmbedDesired,
            model:model.into(),active_index_revision:None,desired_index_revision:Some(revision),locality:Locality::Local,locality_trusted:true,ocr_pdf_mode:None};
        fixed::save_profile(state,actor,provider,fixed::SaveFixedProfile {expected_revision:0,profile}).await?;
        sqlx::query("UPDATE knowledge_bases SET parent_child=true WHERE id=$1").bind(kb).execute(&state.pg).await?;
        let entry=fixed::catalog(state,actor).await?.into_iter().find(|e|e.role==fixed::FixedRole::Ocr).ok_or_else(||anyhow::anyhow!("OCR fixture profile"))?;
        let mut ocr:fixed::FixedProfile=serde_json::from_value(entry.profile.ok_or_else(||anyhow::anyhow!("OCR profile capture"))?)?;
        ocr.ocr_pdf_mode=Some(fixed::OcrPdfMode::PageImages);
        fixed::save_profile(state,actor,entry.id,fixed::SaveFixedProfile {expected_revision:ocr.revision,profile:ocr}).await?;
        let uid=actor.user_id.unwrap();
        let promoted=desired.to_string();
        let old_collection=flow["collection"].as_str().unwrap().to_string();
        let promotion=Router::new().route("/api/routing/pilot-migration/promote",post(move|State(state):State<AppState>,AuthUser(actor):AuthUser| {
            let desired=promoted.clone();let old=old_collection.clone();async move {
                if actor.user_id!=Some(uid) {return Err::<Json<Value>,fosnie_backend::AppError>(RoutingError::Forbidden.into());}
                state.rbac.require_permission(&state.pg,&actor,fosnie_backend::auth::permissions::PROVIDERS_MANAGE).await?;
                fosnie_backend::kb::require_manage(&state.pg,&actor,kb).await?;
                let running:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE kind='ingest' AND status IN ('queued','running'))").fetch_one(&state.pg).await?;
                if running {return Err(fosnie_backend::AppError::Conflict("private ingestion is busy".into()));}
                let revision:i64=sqlx::query_scalar("SELECT desired_routing_revision FROM embedding_index WHERE id=1").fetch_one(&state.pg).await?;
                fosnie_backend::embedding_promotion::promote(&state,&desired,&old,revision).await?;
                let entry=fixed::catalog(&state,&actor).await?.into_iter().find(|e|e.role==fixed::FixedRole::Embed).ok_or(RoutingError::ProviderUnavailable)?;
                let profile:fixed::FixedProfile=serde_json::from_value(entry.profile.ok_or(RoutingError::NeedsReplan)?).map_err(|_|RoutingError::Invalid("active profile"))?;
                Ok(Json(json!({"active_collection":desired,"active_model":profile.model,"active_revision":profile.active_index_revision,"quality_evidence":false})))
            }
        }));
        let status=Router::new().route("/api/routing/pilot-migration/receipt",axum::routing::get(move|State(state):State<AppState>,AuthUser(actor):AuthUser|async move {
            if actor.user_id!=Some(uid) {return Err::<Json<Value>,fosnie_backend::AppError>(RoutingError::Forbidden.into());}
            let intents:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'old_collection',old_collection,'new_collection',new_collection,'status',status) FROM embedding_promotions ORDER BY created_at").fetch_all(&state.pg).await?;
            let index:Value=sqlx::query_scalar("SELECT jsonb_build_object('model',embed_model,'collection',collection_name,'status',status,'desired_model',desired_model) FROM embedding_index WHERE id=1").fetch_one(&state.pg).await?;
            let blocked=fosnie_backend::embedding_promotion::pending(&state.pg).await?;
            Ok(Json(json!({"intents":intents,"index":index,"inference_blocked":blocked,"quality_evidence":false})))
        })).route("/api/routing/pilot-migration/recover",post(move|State(state):State<AppState>,AuthUser(actor):AuthUser|async move {
            if actor.user_id!=Some(uid) {return Err::<Json<Value>,fosnie_backend::AppError>(RoutingError::Forbidden.into());}
            fosnie_backend::embedding_promotion::recover(&state).await?;Ok(Json(json!({"recovered":true})))
        }));
        let receipts=Router::new().route("/api/routing/pilot-reindex/receipt",axum::routing::get(move|State(state):State<AppState>,AuthUser(actor):AuthUser|async move {
            if actor.user_id!=Some(uid) {return Err::<Json<Value>,fosnie_backend::AppError>(RoutingError::Forbidden.into());}
            let jobs:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',j.id,'task_id',j.task_id,'status',j.status,'task_status',t.status::text,'usage',j.usage,'plan',j.plan) FROM routed_jobs j JOIN tasks t ON t.id=j.task_id WHERE j.kind='reindex' ORDER BY j.created_at").fetch_all(&state.pg).await?;
            let retired:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('collection',collection_name,'status',status) FROM embedding_retired_indices ORDER BY retired_at").fetch_all(&state.pg).await?;
            Ok(Json(json!({"jobs":jobs,"retired":retired})))
        })).route("/api/routing/pilot-reindex/retired-fixtures",post(move|State(state):State<AppState>,AuthUser(actor):AuthUser,Json(names):Json<Vec<String>>|async move {
            if actor.user_id!=Some(uid) || names.len()!=2 || names.iter().any(|n|!n.starts_with("v29_retired_") || n.len()!=44) {return Err::<Json<Value>,fosnie_backend::AppError>(RoutingError::Forbidden.into());}
            let publication:Uuid=sqlx::query_scalar("SELECT id FROM embedding_promotions WHERE status='completed' ORDER BY completed_at DESC LIMIT 1").fetch_one(&state.pg).await?;
            for (i,name) in names.iter().enumerate() {
                sqlx::query("INSERT INTO embedding_retired_indices(collection_name,promotion_id,retired_at) VALUES($1,$2,now()-($3*interval '1 hour'))")
                    .bind(name).bind(publication).bind(48+i as i32).execute(&state.pg).await?;
            }
            Ok(Json(json!({"seeded":true})))
        }));
        let denied=self.denied.clone();
        let controls=Router::new().route("/api/routing/pilot-reindex/access",post(move|State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<Value>| {
            let denied=denied.clone();async move {
                if actor.user_id!=Some(uid) {return Err::<Json<Value>,fosnie_backend::AppError>(RoutingError::Forbidden.into());}
                match body["mode"].as_str() {
                    Some("deny_doc")=>denied.store(true,Ordering::SeqCst),
                    Some("allow")=>denied.store(false,Ordering::SeqCst),
                    Some("deactivate")=>{sqlx::query("UPDATE users SET deactivated_at=now() WHERE id=$1").bind(uid).execute(&state.pg).await?;},
                    Some("restore")=>{sqlx::query("UPDATE users SET deactivated_at=NULL WHERE id=$1").bind(uid).execute(&state.pg).await?;},
                    _=>return Err(RoutingError::Invalid("fixture access mode").into()),
                }
                Ok(Json(json!({"changed":true})))
            }
        })).route("/api/routing/pilot-reindex/state",axum::routing::get(move|State(state):State<AppState>,AuthUser(actor):AuthUser|async move {
            if actor.user_id!=Some(uid) {return Err::<Json<Value>,fosnie_backend::AppError>(RoutingError::Forbidden.into());}
            let attempts:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('task_id',task_id,'status',status,'source_sha256',source_sha256) FROM embedding_rebuild_attempts ORDER BY created_at").fetch_all(&state.pg).await?;
            let index:Value=sqlx::query_scalar("SELECT jsonb_build_object('status',status,'done',reindex_done,'desired_model',desired_model,'collection',collection_name) FROM embedding_index WHERE id=1").fetch_one(&state.pg).await?;
            let intents:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('status',status,'source_sha256',source_sha256) FROM embedding_promotions ORDER BY created_at").fetch_all(&state.pg).await?;
            Ok(Json(json!({"attempts":attempts,"index":index,"intents":intents})))
        }));
        let duplicate=Router::new().route("/api/routing/pilot-reindex/delivery",post(move|State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<Value>|async move {
            if actor.user_id!=Some(uid) {return Err::<Json<Value>,fosnie_backend::AppError>(RoutingError::Forbidden.into());}
            let job=Uuid::parse_str(body["job_id"].as_str().ok_or(RoutingError::Invalid("fixture job"))?).map_err(|_|RoutingError::Invalid("fixture job"))?;
            let task=Uuid::parse_str(body["task_id"].as_str().ok_or(RoutingError::Invalid("fixture task"))?).map_err(|_|RoutingError::Invalid("fixture task"))?;
            let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1 AND actor_user_id=$2 AND kind='reindex')").bind(job).bind(uid).fetch_one(&state.pg).await?;
            if !owned {return Err(RoutingError::Forbidden.into());}
            let accepted=fosnie_backend::model_routing::reindex::handle_task(&state,job,task).await.is_ok();
            Ok(Json(json!({"accepted":accepted})))
        }));
        let routes=Router::new().route("/api/routing/pilot-formats/replace",post(move|State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<Replace>|async move {
            if actor.user_id!=Some(uid) {return Err::<Json<Value>,fosnie_backend::AppError>(RoutingError::Forbidden.into());}
            fosnie_backend::kb::require_manage(&state.pg,&actor,kb).await?;
            let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM kb_documents WHERE id=$1 AND kb_id=$2 AND created_by=$3 AND source='upload')")
                .bind(body.doc_id).bind(kb).bind(uid).fetch_one(&state.pg).await?;
            if !owned || body.bytes_base64.len()>5*1024*1024 {return Err(RoutingError::Forbidden.into());}
            let busy:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE kind='ingest' AND input->'source'->>'doc'=$1 AND status IN ('queued','running'))")
                .bind(body.doc_id.to_string()).fetch_one(&state.pg).await?;
            if busy {return Err(fosnie_backend::AppError::Conflict("private document is busy".into()));}
            use base64::Engine;
            let bytes=base64::engine::general_purpose::STANDARD.decode(body.bytes_base64).map_err(|_|RoutingError::Invalid("private fixture bytes"))?;
            if bytes.is_empty() || bytes.len()>model_routing_limit() {return Err(RoutingError::Forbidden.into());}
            fosnie_backend::kb::replace_bytes_staged(&state,body.doc_id,&bytes).await?;
            let task=ingestion::queue(&state,&actor,body.doc_id).await?;
            Ok(Json(json!({"task_id":task})))
        }));
        Ok(pilot::EditionFixture {info:json!({"desired_collection":desired,"desired_index_revision":revision,
            "same_embedding_model":false,"desired_model":"qwen3-embedding:0.6b","ocr_pdf_mode":"page_images","parent_child":true,"quality_evidence":false}),routes:routes.merge(promotion).merge(status).merge(receipts).merge(duplicate).merge(controls)})
    }
}
fn model_routing_limit()->usize {ingestion::MAX_DOCUMENT_BYTES}
#[tokio::main]
async fn main() {
    if pilot::run_with_edition(Arc::new(Edition {denied:Arc::new(AtomicBool::new(false))}),&sqlx::migrate!("./migrations")).await.is_err() {
        eprintln!("Private format fixture stopped; credentials/details withheld.");std::process::exit(1);
    }
}
