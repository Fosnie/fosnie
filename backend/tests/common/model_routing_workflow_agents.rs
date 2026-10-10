// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
use serde_json::{Value,json};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::AuthContext,config::BootConfig,state::AppStateBuilder,
    workflows,events::{self,ActorType,NewEvent},model_routing::{*,fixed::*}};
struct Features;
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Features {
    async fn enabled_for_user(&self,_:&AppState,_:Option<Uuid>,feature:&str)->bool {feature=="workflows"}
}
pub(super) fn reports(body:&Value,purposes:&[&str],roles:&[&str])->Value {
    let routing=&body["overrides"]["routing"];
    let calls:Vec<_>=purposes.iter().map(|purpose|{let mut one=body.clone();one["overrides"]["routing"]["purpose"]=json!(purpose);super::routing_mock_report(&one)["calls"][0].clone()}).collect();
    let fixed:Vec<_>=roles.iter().map(|role| {let target=&routing["fixed_assignments"][role];
        json!({"call_id":Uuid::now_v7(),"lease_id":routing["fixed_budget"]["id"],"decision_id":routing["decision_id"],"role":role,
            "provider_id":target["provider_id"],"provider_revision":target["provider_revision"],"profile_revision":target["profile_revision"],
            "active_index_revision":target["active_index_revision"],"status":"completed","latency_ms":1,"input_bytes":20,"output_bytes":10})}).collect();
    json!({"routing_report":{"lease_id":routing["budget"]["id"],"complete":true,"calls":calls},
        "fixed_routing_report":{"lease_id":routing["fixed_budget"]["id"],"complete":true,"calls":fixed}})
}
// Return an already boxed fixture: constructing this multi-route future in the
// parent's poll frame inflates every earlier workflow call on Windows.
pub fn check<'a>(pool:&'a sqlx::PgPool,admin:&'a AuthContext,uid:Uuid,kb:Uuid,doc:Uuid,project:Uuid)
    ->std::pin::Pin<Box<dyn std::future::Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
    let mode=Arc::new(AtomicUsize::new(0));let count=Arc::new(AtomicUsize::new(0));let steps=Arc::new(AtomicUsize::new(0));
    let agent=Uuid::now_v7();
    let app=axum::Router::new().route("/generate",axum::routing::post({
        let count=count.clone();let mode=mode.clone();let pool=pool.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let mode=mode.clone();let pool=pool.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);assert!(body["messages"].to_string().contains("Queued agent instruction"));
            if mode.load(Ordering::SeqCst)==3 {assert!(body["messages"].to_string().contains("Workflow library"));}
            if mode.load(Ordering::SeqCst)==7 {sqlx::query("UPDATE agents SET system_prompt='Changed' WHERE id=$1").bind(agent).execute(&pool).await.unwrap();}
            format!("{}\n{}\n",json!({"type":"token","delta":"Agent answer."}),json!({"type":"done","model":"catalog-model","finish_reason":"stop","usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)}))
        }}
    })).route("/chat-step",axum::routing::post({
        let count=count.clone();let steps=steps.clone();let mode=mode.clone();let pool=pool.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let steps=steps.clone();let mode=mode.clone();let pool=pool.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);assert!(body["messages"].to_string().contains("Queued agent instruction"));
            let document=mode.load(Ordering::SeqCst)==2;let tool=if document {"read_document"} else {"web_search"};
            assert!(body["tools"].as_array().unwrap().iter().any(|d|d["function"]["name"]==tool));
            let mut result=json!({"content":"Completed from authorised evidence.","usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)});
            if steps.fetch_add(1,Ordering::SeqCst)==0 {
                if mode.load(Ordering::SeqCst)==8 {sqlx::query("DELETE FROM agent_tools WHERE agent_id=$1").bind(agent).execute(&pool).await.unwrap();}
                result["tool_calls"]=json!([{"id":"read-1","name":tool,"arguments":if document {json!({"doc_id":doc})} else {json!({"query":"Glasgow weather","depth":"standard"})}}]);
            } else {assert!(body["messages"].to_string().contains("Workflow evidence"));}
            axum::Json(result)
        }}
    })).route("/web_search",axum::routing::post({
        let count=count.clone();let mode=mode.clone();let pool=pool.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let mode=mode.clone();let pool=pool.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);assert_eq!(body["max_fetches"],2);
            if mode.load(Ordering::SeqCst)==9 {
                sqlx::query("UPDATE agents SET system_prompt='Changed during web' WHERE id=$1").bind(agent).execute(&pool).await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            }
            let mut result=reports(&body,&["web_plan","web_grade"],&["rerank","ocr"]);
            result["type"]=json!("done");result["digest"]=json!("Workflow evidence from web.");result["citations"]=json!([]);format!("{result}\n")
        }}
    })).route("/retrieve",axum::routing::post({
        let count=count.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);assert_eq!(body["kb_ids"],json!([kb]));
            let mut result=reports(&body,&["retrieval_decompose","retrieval_grade"],&["embed"]);
            result["type"]=json!("done");result["context"]=json!("Workflow library evidence for this question.");result["citations"]=json!([]);format!("{result}\n")
        }}
    })).route("/read-document",axum::routing::post({
        let count=count.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            let mut result=reports(&body,&["document"],&["ocr"]);
            result["mode"]=json!("stuff");result["text"]=json!("Workflow evidence from document.");result["truncated"]=json!(false);axum::Json(result)
        }}
    }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let addr=listener.local_addr()?;
    let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
    let mut boot=BootConfig::default();boot.features.agents_enabled=true;boot.ml.base_url=format!("http://{addr}");
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot)).with_features(Arc::new(Features)).build();
    for role in [FixedRole::Rerank,FixedRole::Ocr] {
        let (id,rev,model):(Uuid,i64,String)=sqlx::query_as("SELECT id,routing_revision,model FROM provider_configs WHERE scope='deployment' AND role=$1 LIMIT 1").bind(role.name()).fetch_one(pool).await?;
        let expected=sqlx::query_scalar::<_,i64>("SELECT revision FROM fixed_inference_profiles WHERE provider_id=$1 AND slot=$2").bind(id).bind(role.slot()).fetch_optional(pool).await?.unwrap_or(0);
        fixed::save_profile(&state,admin,id,SaveFixedProfile {expected_revision:expected,profile:FixedProfile {provider_id:id,provider_revision:rev,revision:1,role,model,active_index_revision:None,desired_index_revision:None,locality:Locality::Local,locality_trusted:true,ocr_pdf_mode:None}}).await?;
    }
    sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES('integration.web_search.enabled','true','bool') ON CONFLICT(key) DO UPDATE SET value='true'").execute(pool).await?;
    sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'Workflow routed agent','Queued agent instruction',$2,$3,ARRAY['general'])").bind(agent).bind(json!({"web_max_fetches":2,"max_tokens":512})).bind(uid).execute(pool).await?;
    for case in 0..16 {
        mode.store(case,Ordering::SeqCst);steps.store(0,Ordering::SeqCst);let before=count.load(Ordering::SeqCst);
        sqlx::query("UPDATE agents SET system_prompt='Queued agent instruction',params=$2,created_by=$3,archived_at=NULL,modes=ARRAY['general'] WHERE id=$1").bind(agent).bind(json!({"web_max_fetches":2,"max_tokens":512})).bind(uid).execute(pool).await?;
        sqlx::query("DELETE FROM agent_tools WHERE agent_id=$1").bind(agent).execute(pool).await?;
        sqlx::query("DELETE FROM agent_project_knowledge WHERE agent_id=$1").bind(agent).execute(pool).await?;
        sqlx::query("DELETE FROM project_kb_links WHERE project_id=$1").bind(project).execute(pool).await?;
        let tool=match case {1|8|9|10|11|15=>Some("web_search"),2=>Some("read_document"),12=>Some("code_interpreter"),_=>None};
        if let Some(tool)=tool {sqlx::query("INSERT INTO agent_tools(agent_id,tool_name) VALUES($1,$2)").bind(agent).bind(tool).execute(pool).await?;}
        if case==3 {sqlx::query("INSERT INTO agent_project_knowledge(agent_id,project_knowledge_id) VALUES($1,$2)").bind(agent).bind(kb).execute(pool).await?;}
        if case==2 {sqlx::query("INSERT INTO project_kb_links(project_id,kb_id,attached_by) VALUES($1,$2,$3)").bind(project).bind(kb).bind(uid).execute(pool).await?;}
        if case==6 {sqlx::query("UPDATE agents SET created_by=$2 WHERE id=$1").bind(agent).bind(admin.user_id).execute(pool).await?;}
        if case==14 {sqlx::query("UPDATE agents SET modes=ARRAY['research'] WHERE id=$1").bind(agent).execute(pool).await?;}
        let workflow=Uuid::now_v7();let policy=store::load_policy(&state,admin,None).await?;
        let selection=match case {10=>json!({"mode":"manual","provider_id":policy.default_provider_id.unwrap()}),11=>json!({"mode":"auto","objective":"balanced"}),_=>json!({"mode":"default"})};
        sqlx::query("INSERT INTO workflows(id,name,owner_id,project_id,agent_id,enabled,trigger_event_type,action_type,action_config) VALUES($1,'Agent workflow',$2,$3,$4,true,'routing.agent.test','agent_run',$5)")
            .bind(workflow).bind(uid).bind((case==2).then_some(project)).bind(agent).bind(json!({"prompt":"Frozen workflow prompt","model_selection":selection})).execute(pool).await?;
        let mut tx=pool.begin().await?;events::emit_with(&mut tx,&NewEvent::new("routing.agent.test",ActorType::Human).actor(Some(uid)).project((case==2).then_some(project))).await?;tx.commit().await?;
        assert_eq!(workflows::dispatch_once(&state).await?,1);
        let id:Uuid=sqlx::query_scalar("SELECT id FROM workflow_runs WHERE workflow_id=$1").bind(workflow).fetch_one(pool).await?;
        if case==4 {sqlx::query("UPDATE agents SET params='{}' WHERE id=$1").bind(agent).execute(pool).await?;}
        if case==5 {sqlx::query("UPDATE agents SET archived_at=now() WHERE id=$1").bind(agent).execute(pool).await?;}
        if case==13 {sqlx::query("INSERT INTO agent_project_knowledge(agent_id,project_knowledge_id) VALUES($1,$2)").bind(agent).bind(kb).execute(pool).await?;}
        if case==15 {
            // Interrupted after creating the child row, before its routed checkpoint.
            sqlx::query("UPDATE workflow_runs SET status='running',started_at=now()-interval '4 minutes' WHERE id=$1").bind(id).execute(pool).await?;
            fosnie_backend::agent::start_run(&state,Some(agent),Some(uid),"user",None,id,None,None,300).await?;
            workflows::routing::recover(&state).await?;
        }
        // Mirror the scheduler task boundary; don't put the five-route fixture's
        // poll frame underneath the full workflow/chat/tool stack on Windows.
        let task_state=state.clone();
        let result=tokio::spawn(async move {Box::pin(workflows::run(&task_state,id)).await}).await?;
        let (status,outcome,error):(String,Option<Value>,Option<String>)=sqlx::query_as("SELECT status::text,outcome,error FROM workflow_runs WHERE id=$1").bind(id).fetch_one(pool).await?;
        let success=matches!(case,0|1|2|3|10|11);
        assert_eq!(status,if success {"succeeded"} else {"failed"},"agent case {case}: {result:?} {error:?}");
        let calls=match case {0|7|8=>1,1|10|11=>3,2=>4,3|9=>2,_=>0};
        assert_eq!(count.load(Ordering::SeqCst)-before,calls,"agent case {case}: {error:?}");
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM messages WHERE turn_id=$1 AND role='assistant' AND completed_at IS NOT NULL").bind(id).fetch_one(pool).await?,i64::from(success));
        if success {assert_eq!(outcome.unwrap()["usage"]["calls"],match case {0=>1,3=>4,2=>7,_=>6});}
        let children:Vec<(Uuid,String)>=sqlx::query_as("SELECT id,status::text FROM agent_runs WHERE turn_id=$1").bind(id).fetch_all(pool).await?;
        for (child,status) in children {assert_eq!(status,if success {"completed"} else {"failed"},"agent case {case}");assert!(!fosnie_backend::agent::alive(&state,child).await);}
        let before=count.load(Ordering::SeqCst);Box::pin(workflows::run(&state,id)).await?;assert_eq!(count.load(Ordering::SeqCst),before);
        sqlx::query("UPDATE workflows SET enabled=false WHERE id=$1").bind(workflow).execute(pool).await?;
    }
    server.abort();Ok(())
    })
}
