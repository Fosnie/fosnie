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

struct Edition;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Replace {doc_id:Uuid,bytes_base64:String}
#[async_trait::async_trait]
impl pilot::PilotEdition for Edition {
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
        let promotion=Router::new().route("/api/routing/pilot-migration/promote",post(move|State(state):State<AppState>,AuthUser(actor):AuthUser| {
            let desired=promoted.clone();async move {
                if actor.user_id!=Some(uid) {return Err::<Json<Value>,fosnie_backend::AppError>(RoutingError::Forbidden.into());}
                state.rbac.require_permission(&state.pg,&actor,fosnie_backend::auth::permissions::PROVIDERS_MANAGE).await?;
                fosnie_backend::kb::require_manage(&state.pg,&actor,kb).await?;
                let running:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE kind='ingest' AND status IN ('queued','running'))").fetch_one(&state.pg).await?;
                if running {return Err(fosnie_backend::AppError::Conflict("private ingestion is busy".into()));}
                let response=state.http.post(format!("{}/swap-embedding-alias",state.boot.ml.base_url.trim_end_matches('/')))
                    .header("x-fosnie-ml-key",&state.boot.ml.shared_secret).json(&json!({"new_collection":desired,"old_collection":null}))
                    .send().await.map_err(|_|RoutingError::Invalid("private alias swap"))?;
                if !response.status().is_success() {return Err(RoutingError::Invalid("private alias swap refused").into());}
                fosnie_backend::embedding_index::finish_reindex(&state.pg,&desired).await?;
                sqlx::query("UPDATE knowledge_bases SET embedding_model_id=$1,embedding_dimension=1024 WHERE id=$2")
                    .bind("qwen3-embedding:0.6b").bind(kb).execute(&state.pg).await?;
                let entry=fixed::catalog(&state,&actor).await?.into_iter().find(|e|e.role==fixed::FixedRole::Embed).ok_or(RoutingError::ProviderUnavailable)?;
                let profile=fixed::FixedProfile {provider_id:entry.id,provider_revision:entry.provider_revision,revision:1,role:fixed::FixedRole::Embed,
                    model:"qwen3-embedding:0.6b".into(),active_index_revision:entry.active_index_revision,desired_index_revision:None,
                    locality:Locality::Local,locality_trusted:true,ocr_pdf_mode:None};
                let profile=fixed::save_profile(&state,&actor,entry.id,fixed::SaveFixedProfile {expected_revision:entry.profile_revision.unwrap(),profile}).await?;
                Ok(Json(json!({"active_collection":desired,"active_model":profile.model,"active_revision":profile.active_index_revision,"quality_evidence":false})))
            }
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
            "same_embedding_model":false,"desired_model":"qwen3-embedding:0.6b","ocr_pdf_mode":"page_images","parent_child":true,"quality_evidence":false}),routes:routes.merge(promotion)})
    }
}
fn model_routing_limit()->usize {ingestion::MAX_DOCUMENT_BYTES}
#[tokio::main]
async fn main() {
    if pilot::run_with_edition(Arc::new(Edition),&sqlx::migrate!("./migrations")).await.is_err() {
        eprintln!("Private format fixture stopped; credentials/details withheld.");std::process::exit(1);
    }
}
