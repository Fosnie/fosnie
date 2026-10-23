// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Explicit opt-in, single-attempt workflow inference. No legacy fallback.
use serde_json::{Value,json};
use uuid::Uuid;
use crate::{AppState,Result,model_routing::{*,durable::{Context,Lease}}};

pub fn selection(cfg:&Value)->Result<Option<ModelSelection>> {
    if let Some(v)=intent(cfg)? {return Ok(Some(v.selection));}
    cfg.get("model_selection").map(|v|serde_json::from_value(v.clone())
        .map_err(|_|RoutingError::Invalid("invalid workflow model_selection").into())).transpose()
}
pub fn intent(cfg:&Value)->Result<Option<fosnie_protocol::routing_vnext::RoutingIntentV2>> {
    cfg.get("model_selection").filter(|v|v.get("schema_version").is_some()).map(|v| {
        let intent:fosnie_protocol::routing_vnext::RoutingIntentV2=serde_json::from_value(v.clone()).map_err(|_|RoutingError::Invalid("workflow v2 intent"))?;
        intent.require_support(&[1,2],&[1,2]).map_err(RoutingError::Invalid)?;Ok(intent)
    }).transpose()
}
pub fn validate(cfg:&Value)->Result<()> {
    intent(cfg)?;
    if selection(cfg)?.is_none() {return Ok(());}
    super::sources::libraries(cfg)?;
    super::delivery::target(cfg)?;
    let prompt=cfg["prompt"].as_str().ok_or(RoutingError::Invalid("workflow prompt"))?;
    if prompt.trim().is_empty() || prompt.len()>4096 {return Err(RoutingError::Invalid("workflow prompt must be 1..4096 bytes").into());}
    super::sources::validate_template(prompt)?;
    Ok(())
}
/// Capture intent and principal inside the same transaction as the firing/task.
pub(super) async fn capture(conn:&mut sqlx::PgConnection,id:Uuid)->Result<()> {
    let row:Option<(Value,String)>=sqlx::query_as("SELECT jsonb_build_object('config',w.action_config,'project_id',w.project_id,'agent_id',w.agent_id,'version',w.version,'event_ids',r.trigger_event_ids,'project_kb_ids',COALESCE((SELECT jsonb_agg(kb_id ORDER BY kb_id) FROM project_kb_links WHERE project_id=w.project_id),'[]'::jsonb)),u.role::text FROM workflow_runs r JOIN workflows w ON w.id=r.workflow_id JOIN users u ON u.id=r.run_as_user_id WHERE r.id=$1 AND w.action_type='agent_run' AND w.action_config ? 'model_selection' AND u.deactivated_at IS NULL")
        .bind(id).fetch_optional(&mut *conn).await?;
    if let Some((mut input,role))=row {
        input["delivery"]=super::delivery::capture(&mut *conn,&input["config"]).await?;
        if let Some(id)=input["agent_id"].as_str().and_then(|s|Uuid::parse_str(s).ok()) {
            input["agent_snapshot"]=super::agents::capture(&mut *conn,id).await?;
        }
        sqlx::query("INSERT INTO workflow_routing_inputs(run_id,input,actor) SELECT id,$2,jsonb_build_object('user_id',run_as_user_id,'role',$3::text) FROM workflow_runs WHERE id=$1")
            .bind(id).bind(input).bind(role).execute(conn).await?;
    }
    Ok(())
}
async fn allowed(state:&AppState,id:Uuid)->Result<()> {
    let owner:Option<Uuid>=sqlx::query_scalar("SELECT w.owner_id FROM workflow_runs r JOIN workflows w ON w.id=r.workflow_id WHERE r.id=$1 AND routing_workflow_active(r.id)")
        .bind(id).fetch_optional(&state.pg).await?;
    let uid=owner.ok_or(RoutingError::Forbidden)?;
    if !super::feature_enabled(state).await || super::paused(state).await
        || !state.features.enabled_for_user(state,Some(uid),"workflows").await {
        return Err(RoutingError::Forbidden.into());
    }
    Ok(())
}
pub async fn sync_task(state:&AppState,task:Uuid,id:Uuid)->Result<bool> {
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_routing_inputs WHERE run_id=$1)").bind(id).fetch_one(&state.pg).await?;
    if !exists {return Ok(false);}
    sqlx::query("UPDATE tasks t SET status=CASE WHEN r.status='succeeded' THEN 'succeeded'::task_status ELSE 'dead_letter'::task_status END,finished_at=now(),last_error=CASE WHEN r.status='succeeded' THEN NULL ELSE 'routed workflow stopped; automatic replay disabled' END FROM workflow_runs r WHERE r.id=$1 AND t.id=$2 AND t.payload->>'workflow_run_id'=$1::text AND r.status IN ('succeeded','failed','skipped')")
        .bind(id).bind(task).execute(&state.pg).await?;
    Ok(true)
}
pub async fn recover(state:&AppState)->Result<()> {
    // A task claimed before the firing CAS has not started planning yet.
    sqlx::query("UPDATE tasks t SET status='queued',started_at=NULL,next_attempt_at=now() FROM workflow_runs r JOIN workflow_routing_inputs i ON i.run_id=r.id WHERE t.payload->>'workflow_run_id'=r.id::text AND t.task_type='workflow_run' AND r.status='queued' AND t.status='running' AND t.started_at<clock_timestamp()-interval '30 seconds'")
        .execute(&state.pg).await?;
    // Planning may die before a routed_jobs row exists. The committed firing
    // remains a tombstone: never rebuild it with a fresh budget.
    sqlx::query("UPDATE workflow_runs r SET status='failed',finished_at=now(),error='routed workflow interrupted; automatic replay disabled' FROM workflow_routing_inputs i WHERE i.run_id=r.id AND r.status='running' AND ((r.started_at<clock_timestamp()-interval '3 minutes' AND NOT EXISTS(SELECT 1 FROM routed_jobs j WHERE j.id=r.id)) OR EXISTS(SELECT 1 FROM routed_jobs j WHERE j.id=r.id AND j.status IN ('failed','cancelled','interrupted')))")
        .execute(&state.pg).await?;
    sqlx::query("UPDATE tasks t SET status='dead_letter',finished_at=now(),last_error='routed workflow stopped; automatic replay disabled' FROM workflow_runs r JOIN workflow_routing_inputs i ON i.run_id=r.id WHERE t.payload->>'workflow_run_id'=r.id::text AND t.task_type='workflow_run' AND r.status IN ('failed','skipped') AND t.status IN ('queued','running')")
        .execute(&state.pg).await?;
    super::agents::cleanup(state,None).await?;
    Ok(())
}
pub async fn run(state:&AppState,id:Uuid)->Result<bool> {
    let saved:Option<(Value,Value)>=sqlx::query_as("SELECT input,actor FROM workflow_routing_inputs WHERE run_id=$1").bind(id).fetch_optional(&state.pg).await?;
    let Some((input,actor))=saved else {
        // An old queued firing cannot opt into a later configuration edit.
        let changed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workflow_runs r JOIN workflows w ON w.id=r.workflow_id WHERE r.id=$1 AND w.action_type='agent_run' AND w.action_config ? 'model_selection')").bind(id).fetch_one(&state.pg).await?;
        if changed {super::finish(state,id,"failed",None,Some("routing needs a new workflow firing with a captured configuration")).await?;return Ok(true);}
        return Ok(false);
    };
    let claimed=sqlx::query("UPDATE workflow_runs SET status='running',started_at=now() WHERE id=$1 AND status='queued'").bind(id).execute(&state.pg).await?.rows_affected();
    if claimed==0 {return Ok(true);}
    let outcome=tokio::time::timeout(std::time::Duration::from_secs(600),Box::pin(execute(state,id,input,actor))).await
        .unwrap_or_else(|_|Err(RoutingError::BudgetExhausted.into()));
    if let Err(error)=&outcome {
        let mut tx=state.pg.begin().await?;
        sqlx::query("UPDATE routed_jobs SET status='failed',owner=NULL,lease_until=NULL,updated_at=now() WHERE id=$1 AND status IN ('queued','running')").bind(id).execute(&mut *tx).await?;
        sqlx::query("UPDATE messages SET interrupted_at=now() WHERE turn_id=$1 AND role='assistant' AND completed_at IS NULL").bind(id).execute(&mut *tx).await?;
        sqlx::query("UPDATE workflow_runs SET status='failed',finished_at=now(),error=$2 WHERE id=$1 AND status='running'").bind(id).bind(error.to_string()).execute(&mut *tx).await?;
        tx.commit().await?;
        super::agents::cleanup(state,Some(id)).await?;
    }
    outcome.map(|_|true)
}
async fn execute(state:&AppState,id:Uuid,input:Value,principal:Value)->Result<()> {
    allowed(state,id).await?;validate(&input["config"])?;
    let uid:Uuid=serde_json::from_value(principal["user_id"].clone()).map_err(|_|RoutingError::Forbidden)?;
    let actor=crate::auth::load_context(&state.pg,uid).await?;
    if principal["role"]!=actor.role.as_str() {return Err(RoutingError::Forbidden.into());}
    let context=RoutingContext {actor:actor.clone(),chat_id:None,project_id:None,run_id:id,purpose:CallPurpose::Primary};
    Context::capture(&context)?.validate(&state.pg).await?;
    let scope=super::sources::snapshot(state,&actor,&input).await?;
    Box::pin(super::delivery::check(state,&actor,&input)).await?;
    Box::pin(super::delivery::check_readers(state,&input,&scope)).await?;
    let prompt=scope.prompt(input["config"]["prompt"].as_str().unwrap())?;
    let selection=selection(&input["config"])?.ok_or(RoutingError::Invalid("missing workflow selection"))?;
    let chat=Uuid::now_v7();
    let mut chat_tx=state.pg.begin().await?;
    sqlx::query("INSERT INTO chats(id,owner_user_id,title,model_selection,project_id,agent_id) VALUES($1,$2,'Workflow output',$3,$4,$5)")
        .bind(chat).bind(uid).bind(input["config"]["model_selection"].clone()).bind(scope.project()).bind(scope.agent()).execute(&mut *chat_tx).await?;
    scope.attach(&mut chat_tx,&actor,chat).await?;
    super::delivery::protect(&mut chat_tx,&input,&scope,id,chat).await?;chat_tx.commit().await?;
    scope.check(state,&actor,&input,chat).await?;
    crate::chat::routing::workflow_scope(state,&actor,chat,&prompt).await?;
    let cancel=std::sync::Arc::new(tokio::sync::Notify::new());
    let vnext=intent(&input["config"])?;
    let constrained=crate::chat::browser::constrain(state.clone(),&vnext.as_ref().and_then(|v|v.options.clone()).unwrap_or_default());
    let mut planned=tokio::time::timeout(std::time::Duration::from_secs(120),async {match vnext {
        Some(intent)=>crate::chat::routing::prepare_vnext(&constrained,&actor,chat,id,&prompt,intent,WorkloadRequirements {output_tokens:1024,..Default::default()},&[],&cancel).await,
        None=>crate::chat::routing::prepare_selected(state,&actor,chat,id,&prompt,selection,WorkloadRequirements {output_tokens:1024,..Default::default()},&[],&cancel).await}})
        .await.map_err(|_|RoutingError::BudgetExhausted)??;
    planned.add_handoff_reason("workflow_agent")?;
    let frozen=planned.freeze()?;allowed(state,id).await?;scope.check(state,&actor,&input,chat).await?;
    let task:Uuid=sqlx::query_scalar("SELECT id FROM tasks WHERE task_type='workflow_run' AND payload->>'workflow_run_id'=$1 ORDER BY created_at LIMIT 1").bind(id.to_string()).fetch_one(&state.pg).await?;
    let owner=Uuid::now_v7();let deadline=frozen.deadline.min(time::OffsetDateTime::now_utc()+time::Duration::minutes(8));
    let mut tx=state.pg.begin().await?;
    sqlx::query("INSERT INTO routed_jobs(id,task_id,actor_user_id,kind,context,plan,limits,usage,input,deadline,status,owner,lease_until) VALUES($1,$2,$3,'workflow_agent',$4,$5,$6,$7,$8,$9,'running',$10,clock_timestamp()+interval '30 seconds')")
        .bind(id).bind(task).bind(uid).bind(json!(frozen.context)).bind(json!(frozen.plan)).bind(json!(frozen.limits)).bind(json!(frozen.usage)).bind(json!({"workflow_run_id":id,"scope":scope,"captured":input})).bind(deadline).bind(owner).execute(&mut *tx).await?;
    let mut event=crate::audit::AuditEvent::action("routing.workflow.started",actor.role.as_str());event.actor_user_id=Some(uid);
    event.resource_type=Some("workflow_run".into());event.resource_id=Some(id);event.payload=Some(json!({"chat_id":chat,"decision_id":frozen.plan.decision.id}));
    crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;
    let remaining=(deadline-time::OffsetDateTime::now_utc()).whole_milliseconds().max(0) as u64;
    let run=crate::model_routing::durable::restore(state,frozen.context,frozen.plan,frozen.limits,frozen.usage,remaining,Lease {pool:state.pg.clone(),id,owner})?;
    let (frames,mut receiver)=tokio::sync::mpsc::channel(64);
    let monitor=Box::pin(async {
        loop {
            allowed(state,id).await?;scope.check(state,&actor,&input,chat).await?;
            crate::chat::routing::workflow_scope(state,&actor,chat,&prompt).await?;
            let n=sqlx::query("UPDATE routed_jobs SET lease_until=clock_timestamp()+interval '30 seconds' WHERE id=$1 AND owner=$2 AND status='running' AND deadline>clock_timestamp() AND lease_until>clock_timestamp()")
                .bind(id).bind(owner).execute(&state.pg).await?.rows_affected();
            if n!=1 {return Err::<(),crate::AppError>(RoutingError::Cancelled.into());}
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
    let consume=async {let mut completed=false;let mut failed=false;while let Some(frame)=receiver.recv().await {
        match frame {crate::ws::protocol::ServerFrame::ChatCompleted {..}=>completed=true,crate::ws::protocol::ServerFrame::ChatError {..}=>failed=true,_=>{}}
    } (completed,failed)};
    let drive=async {
        let result={
        let work=Box::pin(crate::chat::run_turn(state,crate::chat::origin::TurnContext::web(&actor).with_routing(&run),id,Some(chat),None,None,
            prompt.clone(),vec![],vec![],true,None,None,None,None,&frames,cancel.clone()));
        tokio::select! {biased; result=monitor=>{run.cancel();result},_=work=>Ok(())}
        };
        drop(frames);result
    };
    let (result,(completed,failed))=tokio::join!(drive,consume);
    result?;if !completed || failed {return Err(RoutingError::Incompatible("routed workflow turn did not complete").into());}
    allowed(state,id).await?;scope.check(state,&actor,&input,chat).await?;
    let mut tx=state.pg.begin().await?;run.fence(&mut tx).await?;
    let delivery=Box::pin(super::delivery::publish(state,&actor,&input,&scope,chat,&mut tx)).await?;
    // Recipient checks may take time; expiry or disablement must still prevent
    // the delivery transaction from committing as a successful run.
    run.fence(&mut tx).await?;
    let delivery_message_id=delivery.as_ref().map(|d|d.message);
    sqlx::query("UPDATE routed_jobs SET status='succeeded',owner=NULL,lease_until=NULL,result_id=$2,usage=$3,updated_at=now() WHERE id=$1")
        .bind(id).bind(chat).bind(json!(run.usage())).execute(&mut *tx).await?;
    sqlx::query("UPDATE workflow_runs SET status='succeeded',finished_at=now(),outcome=$2,error=NULL WHERE id=$1 AND status='running'")
        .bind(id).bind(json!({"kind":"agent_run","output_chat_id":chat,"routing":true,"selection":run.plan().selection,"usage":run.usage(),"delivery_message_id":delivery_message_id})).execute(&mut *tx).await?;
    let mut event=crate::audit::AuditEvent::action("routing.workflow.succeeded",actor.role.as_str());event.actor_user_id=Some(uid);event.resource_type=Some("workflow_run".into());event.resource_id=Some(id);
    event.payload=Some(json!({"chat_id":chat,"delivery_message_id":delivery_message_id}));
    crate::audit::append_with(&mut tx,&event).await?;tx.commit().await?;
    if let Some(delivery)=delivery {delivery.notify(state);} Ok(())
}
