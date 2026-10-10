// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}};
use serde_json::{Value,json};
use uuid::Uuid;
use futures_util::FutureExt;
use fosnie_backend::{AppState,config::BootConfig,state::AppStateBuilder,workflows,events::{self,ActorType,NewEvent}};
use fosnie_backend::{auth::{AuthContext,rbac::{FlatRbacPolicy,ResourceType,Permission}},ext::RbacPolicy};
struct SourceAcl {denied:Arc<AtomicBool>,doc:Uuid}
#[async_trait::async_trait]
impl RbacPolicy for SourceAcl {
    async fn may_grant(&self,pool:&sqlx::PgPool,actor:&AuthContext,kind:ResourceType,id:Uuid)->fosnie_backend::Result<bool> {
        FlatRbacPolicy.may_grant(pool,actor,kind,id).await
    }
    async fn can(&self,pool:&sqlx::PgPool,actor:&AuthContext,kind:ResourceType,id:Uuid,permission:Permission)->fosnie_backend::Result<bool> {
        FlatRbacPolicy.can(pool,actor,kind,id,permission).await
    }
    async fn denied_kb_doc_ids(&self,_:&sqlx::PgPool,actor:&AuthContext,_:&[Uuid])->fosnie_backend::Result<Vec<Uuid>> {
        assert_eq!(actor.role,fosnie_backend::auth::PlatformRole::User);
        Ok(if self.denied.load(Ordering::SeqCst) {vec![self.doc]} else {vec![]})
    }
}
struct Features(Arc<AtomicBool>);
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Features {
    async fn enabled_for_user(&self,_:&AppState,user:Option<Uuid>,feature:&str)->bool {feature=="workflows" && !(user.is_some() && self.0.load(Ordering::SeqCst))}
}
pub async fn check(pool:&sqlx::PgPool,provider:Uuid,admin:&AuthContext)->anyhow::Result<()> {
    for cfg in [json!({"prompt":"x","model_selection":null}),json!({"prompt":"{{secret}}","model_selection":{"mode":"default"}}),
        json!({"prompt":"x","model_selection":{"mode":"invalid"}}),json!({"prompt":"x","kb_ids":["invalid"],"model_selection":{"mode":"default"}})] {
        assert!(workflows::routing::validate(&cfg).is_err());
    }
    let count=Arc::new(AtomicUsize::new(0));let slow=Arc::new(AtomicUsize::new(0));let entered=Arc::new(tokio::sync::Notify::new());
    let kb=Uuid::now_v7();let doc=Uuid::now_v7();let project=Uuid::now_v7();
    let source_mode=Arc::new(AtomicUsize::new(100));let doc_denied=Arc::new(AtomicBool::new(false));
    let app=axum::Router::new().route("/generate",axum::routing::post({
        let count=count.clone();let slow=slow.clone();let entered=entered.clone();let source_mode=source_mode.clone();let pool=pool.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let slow=slow.clone();let entered=entered.clone();let source_mode=source_mode.clone();let pool=pool.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);entered.notify_one();
            assert!(body["messages"].to_string().contains("Frozen workflow prompt"));
            assert!(!body["messages"].to_string().contains("Private event payload"));
            if source_mode.load(Ordering::SeqCst)<100 {
                assert!(body["messages"].to_string().contains("current-name.txt"));
                // Context allocation may trim the final few bytes to a token boundary.
                assert!(body["messages"].to_string().contains("Authorised source"));
            }
            if source_mode.load(Ordering::SeqCst)==6 {sqlx::query("UPDATE kb_documents SET original_filename='renamed.txt' WHERE id=$1").bind(doc).execute(&pool).await.unwrap();}
            if slow.load(Ordering::SeqCst)>0 {tokio::time::sleep(std::time::Duration::from_millis(1500)).await;}
            format!("{}\n{}\n",json!({"type":"token","delta":"Workflow answer."}),json!({"type":"done","model":"catalog-model","finish_reason":"stop",
                "usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)}))
        }}
    })).route("/retrieve",axum::routing::post({
        let count=count.clone();let mode=source_mode.clone();let denied=doc_denied.clone();let pool=pool.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let mode=mode.clone();let denied=denied.clone();let pool=pool.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            assert_eq!(body["kb_ids"],json!([kb]));assert_eq!(body["deny_doc_ids"],json!([]));
            assert!(body["prompt"].as_str().unwrap().contains("current-name.txt"));
            assert!(!body.to_string().contains("Private event payload"));
            let routing=&body["overrides"]["routing"];let target=&routing["fixed_assignments"]["embed"];
            let mut calls=vec![];
            for purpose in ["retrieval_decompose","retrieval_grade"] {let mut one=body.clone();one["overrides"]["routing"]["purpose"]=json!(purpose);calls.push(super::routing_mock_report(&one)["calls"][0].clone());}
            if mode.load(Ordering::SeqCst)==4 {denied.store(true,Ordering::SeqCst);}
            if mode.load(Ordering::SeqCst)==5 {sqlx::query("DELETE FROM project_kb_links WHERE project_id=$1").bind(project).execute(&pool).await.unwrap();}
            format!("{}\n",json!({"type":"done","context":"Authorised source evidence.","citations":[],
                "routing_report":{"lease_id":routing["budget"]["id"],"complete":true,"calls":calls},
                "fixed_routing_report":{"lease_id":routing["fixed_budget"]["id"],"complete":true,"calls":[{
                    "call_id":Uuid::now_v7(),"lease_id":routing["fixed_budget"]["id"],"decision_id":routing["decision_id"],
                    "role":"embed","provider_id":target["provider_id"],"provider_revision":target["provider_revision"],"profile_revision":target["profile_revision"],
                    "active_index_revision":target["active_index_revision"],"status":"completed","latency_ms":1,"input_bytes":20,"output_bytes":10}]}}))
        }}
    }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
    let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
    let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{address}");
    let denied=Arc::new(AtomicBool::new(false));
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
        .with_features(Arc::new(Features(denied.clone()))).with_rbac(Arc::new(SourceAcl {denied:doc_denied.clone(),doc})).build();
    let uid=Uuid::now_v7();
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Workflow user','user')").bind(uid).bind(format!("{uid}@workflow.test")).execute(pool).await?;
    for (key,value,kind) in [("routing.workflow_jobs_enabled","true","bool"),("workflows.pause_all","false","bool"),("rag.model_search_mode","off","string")] {
        sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,$2,$3::config_value_type) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,value_type=EXCLUDED.value_type")
            .bind(key).bind(value).bind(kind).execute(pool).await?;
    }
    for case in 0..10 {
        let _=entered.notified().now_or_never();
        let workflow=Uuid::now_v7();let before=count.load(Ordering::SeqCst);slow.store(usize::from(case==5),Ordering::SeqCst);
        let selection=match case {1=>json!({"mode":"manual","provider_id":provider}),2=>json!({"mode":"auto","objective":"balanced"}),_=>json!({"mode":"default"})};
        let config=json!({"prompt":"Frozen workflow prompt","model_selection":selection});
        sqlx::query("INSERT INTO workflows(id,name,owner_id,enabled,trigger_event_type,action_type,action_config) VALUES($1,'Routed test',$2,true,'routing.workflow.test','agent_run',$3)")
            .bind(workflow).bind(uid).bind(&config).execute(pool).await?;
        let mut tx=pool.begin().await?;
        events::emit_with(&mut tx,&NewEvent::new("routing.workflow.test",ActorType::Human).actor(Some(uid)).payload(json!({"secret":"Private event payload"}))).await?;
        tx.commit().await?;assert_eq!(workflows::dispatch_once(&state).await?,1);
        let id:Uuid=sqlx::query_scalar("SELECT id FROM workflow_runs WHERE workflow_id=$1").bind(workflow).fetch_one(pool).await?;
        let snapshot:Value=sqlx::query_scalar("SELECT input FROM workflow_routing_inputs WHERE run_id=$1").bind(id).fetch_one(pool).await?;
        assert_eq!(snapshot["config"],config);
        // Editing/removing routing from the workflow cannot change this firing.
        sqlx::query("UPDATE workflows SET action_config='{\"prompt\":\"Changed prompt\"}' WHERE id=$1").bind(workflow).execute(pool).await?;
        if case==3 {sqlx::query("UPDATE config_settings SET value='false' WHERE key='routing.workflow_jobs_enabled'").execute(pool).await?;}
        if case==4 {sqlx::query("UPDATE users SET role='power_user' WHERE id=$1").bind(uid).execute(pool).await?;}
        if case==6 {sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT workflow_start_test CHECK(action_type<>'routing.workflow.started') NOT VALID").execute(pool).await?;}
        if case==7 {sqlx::query("UPDATE workflow_runs SET status='running',started_at=now()-interval '4 minutes' WHERE id=$1").bind(id).execute(pool).await?;workflows::routing::recover(&state).await?;}
        if case==8 {sqlx::query("UPDATE workflows SET enabled=false WHERE id=$1").bind(workflow).execute(pool).await?;}
        if case==9 {denied.store(true,Ordering::SeqCst);}
        let results=if case==5 {
            // Cancel with the deployment gate while the response is still idle.
            tokio::join!(Box::pin(workflows::run(&state,id)),async {
                entered.notified().await;
                sqlx::query("UPDATE config_settings SET value='false' WHERE key='routing.workflow_jobs_enabled'").execute(pool).await
            }).0
        } else {let (a,b)=tokio::join!(Box::pin(workflows::run(&state,id)),Box::pin(workflows::run(&state,id)));if a.is_err(){a}else{b}};
        if case==6 {sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT workflow_start_test").execute(pool).await?;}
        if case==4 {sqlx::query("UPDATE users SET role='user' WHERE id=$1").bind(uid).execute(pool).await?;}
        sqlx::query("UPDATE config_settings SET value='true' WHERE key='routing.workflow_jobs_enabled'").execute(pool).await?;
        let (status,outcome,error):(String,Option<Value>,Option<String>)=sqlx::query_as("SELECT status::text,outcome,error FROM workflow_runs WHERE id=$1").bind(id).fetch_one(pool).await?;
        assert_eq!(status,if case<3 {"succeeded"} else {"failed"},"case {case}: {results:?} {error:?}");
        if case<3 {
            assert_eq!(count.load(Ordering::SeqCst)-before,1);
            let outcome=outcome.unwrap();
            for (key,value) in selection.as_object().unwrap() {assert_eq!(&outcome["selection"][key],value);}
            if case==2 {assert!(Uuid::parse_str(outcome["selection"]["policy_id"].as_str().unwrap()).is_ok());}
            assert_eq!(outcome["usage"]["calls"],1);
            let chat:Uuid=serde_json::from_value(outcome["output_chat_id"].clone())?;
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM messages WHERE chat_id=$1 AND role='assistant' AND completed_at IS NOT NULL").bind(chat).fetch_one(pool).await?,1);
        } else if case!=5 {assert_eq!(count.load(Ordering::SeqCst),before);}
        let calls=count.load(Ordering::SeqCst);workflows::run(&state,id).await?;assert_eq!(count.load(Ordering::SeqCst),calls);
        let task:Uuid=sqlx::query_scalar("SELECT id FROM tasks WHERE task_type='workflow_run' AND payload->>'workflow_run_id'=$1").bind(id.to_string()).fetch_one(pool).await?;
        assert!(workflows::routing::sync_task(&state,task,id).await?);
        assert_eq!(sqlx::query_scalar::<_,String>("SELECT status::text FROM tasks WHERE id=$1").bind(task).fetch_one(pool).await?,if case<3 {"succeeded"} else {"dead_letter"});
        denied.store(false,Ordering::SeqCst);
        sqlx::query("UPDATE workflows SET enabled=false WHERE id=$1").bind(workflow).execute(pool).await?;
    }
    slow.store(0,Ordering::SeqCst);
    Box::pin(super::workflow_source_cases::check(&state,admin,uid,kb,doc,project,&source_mode,&doc_denied,&count)).await?;
    super::workflow_agent_cases::check(pool,admin,uid,kb,doc,project).await?;
    super::workflow_delivery_cases::check(pool,admin,uid,kb,doc,project,provider).await?;
    sqlx::query("UPDATE config_settings SET value='false' WHERE key='routing.workflow_jobs_enabled'").execute(pool).await?;
    server.abort();Ok(())
}
