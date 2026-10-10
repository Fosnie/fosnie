// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Opt-in private worker control/receipts; not part of the deployed router.
use std::sync::{Arc,Mutex};
use axum::{Router,routing::{get,post},extract::{State,Query},Json};
use serde::Deserialize;
use serde_json::{json,Value};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::keycloak::AuthUser};

pub struct EmptyJobs;
impl fosnie_backend::ext::JobRegistrar for EmptyJobs {fn register(&self,_:&mut fosnie_backend::scheduler::JobRegistry) {}}
pub struct Worker {stop:tokio::sync::watch::Sender<bool>,handle:Mutex<Option<std::thread::JoinHandle<()>>>}
impl Worker {
    pub fn new()->Arc<Self> {let(stop,_)=tokio::sync::watch::channel(false);Arc::new(Self {stop,handle:Mutex::new(None)})}
    fn start(&self,state:AppState)->bool {
        let mut slot=self.handle.lock().unwrap();if slot.is_some() {return false;}
        *slot=Some(fosnie_backend::scheduler::spawn(state.clone(),state.boot.scheduler.clone(),self.stop.subscribe()));true
    }
    pub async fn close(&self)->anyhow::Result<()> {
        let _=self.stop.send(true);let handle=self.handle.lock().unwrap().take();
        if let Some(handle)=handle {tokio::time::timeout(std::time::Duration::from_secs(15),tokio::task::spawn_blocking(move||handle.join())).await??.map_err(|_|anyhow::anyhow!("private scheduler panicked"))?;}
        Ok(())
    }
}
#[derive(Deserialize)]
pub struct Scope {kb_id:Uuid}
async fn receipt(State(state):State<AppState>,AuthUser(actor):AuthUser,Query(scope):Query<Scope>)->fosnie_backend::Result<Json<Value>> {
    fosnie_backend::kb::require_manage(&state.pg,&actor,scope.kb_id).await?;
    let documents:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'filename',original_filename,'status',ingest_status::text,'effective_date',effective_date,'source',source) FROM kb_documents WHERE kb_id=$1 ORDER BY created_at,id")
        .bind(scope.kb_id).fetch_all(&state.pg).await?;
    let jobs:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',j.id,'task_id',j.task_id,'status',j.status,'task_status',t.status::text,'usage',j.usage,'document_id',j.input->'source'->>'doc','target',j.plan->'assignments'->'utility_summary','fixed',j.plan->'fixed_assignments','snapshot',j.input->>'snapshot','snapshot_removed',j.ingestion_snapshot_removed) FROM routed_jobs j JOIN tasks t ON t.id=j.task_id WHERE j.kind='ingest' AND j.actor_user_id=$1 AND j.input->'source'->>'kb'=$2 ORDER BY j.created_at,j.id")
        .bind(actor.user_id).bind(scope.kb_id.to_string()).fetch_all(&state.pg).await?;
    Ok(Json(json!({"documents":documents,"jobs":jobs,"quality_evidence":false})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {job_id:Uuid,task_id:Uuid}
async fn duplicate(State(state):State<AppState>,AuthUser(actor):AuthUser,Json(body):Json<Delivery>)->fosnie_backend::Result<Json<Value>> {
    state.rbac.require_permission(&state.pg,&actor,fosnie_backend::auth::permissions::PROVIDERS_MANAGE).await?;
    let owned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE id=$1 AND actor_user_id=$2 AND kind='ingest' AND status='succeeded')")
        .bind(body.job_id).bind(actor.user_id).fetch_one(&state.pg).await?;
    if !owned {return Err(fosnie_backend::model_routing::RoutingError::Forbidden.into());}
    let accepted=fosnie_backend::model_routing::ingestion::handle_task(&state,body.job_id,body.task_id).await.is_ok();
    Ok(Json(json!({"accepted":accepted,"quality_evidence":false})))
}
pub fn routes(worker:Arc<Worker>)->Router<AppState> {
    Router::new().route("/api/routing/pilot-ingestion/receipt",get(receipt))
        .route("/api/routing/pilot-ingestion/duplicate",post(duplicate))
        .route("/api/routing/pilot-ingestion/start",post(move|State(state):State<AppState>,AuthUser(actor):AuthUser| {
            let worker=worker.clone();async move {
                state.rbac.require_permission(&state.pg,&actor,fosnie_backend::auth::permissions::PROVIDERS_MANAGE).await?;
                Ok::<_,fosnie_backend::AppError>(Json(json!({"started":worker.start(state)})))
            }
        }))
}
