// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
use serde_json::{Value,json};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,PlatformRole},research::jobs,groundedness::durable};
struct Auth {actor:AuthContext,foreign:Uuid}
#[async_trait::async_trait]
impl fosnie_backend::ext::AuthProvider for Auth {
    async fn authenticate(&self,parts:&mut axum::http::request::Parts,_:&AppState)->fosnie_backend::Result<AuthContext> {
        let mut actor=self.actor.clone();
        match parts.headers.get("x-test-actor").and_then(|v|v.to_str().ok()) {
            Some("admin")=>actor.role=PlatformRole::ClientAdmin,
            Some("foreign")=>actor.user_id=Some(self.foreign),
            Some("restricted")=>actor.mfa_enroll_only=true,_=>{},
        } Ok(actor)
    }
}
async fn request(client:&reqwest::Client,base:&str,method:&str,path:&str,actor:&str,body:Option<Value>)->anyhow::Result<(u16,Value)> {
    let mut req=client.request(method.parse()?,format!("{base}{path}")).header("x-test-actor",actor);
    if let Some(body)=body {req=req.json(&body);}
    let response=req.send().await?;let code=response.status().as_u16();let text=response.text().await?;
    Ok((code,serde_json::from_str(&text).unwrap_or(Value::String(text))))
}
pub async fn check_jobs(state:&AppState,_admin:&AuthContext,actor:&AuthContext,kb:Uuid,path:&std::path::Path,
    mode:&Arc<AtomicUsize>,calls:&Arc<AtomicUsize>,entered:&Arc<tokio::sync::Notify>)->anyhow::Result<()> {
    let pool=&state.pg;mode.store(0,Ordering::SeqCst);tokio::fs::write(path,b"Evidence.").await?;
    sqlx::query("UPDATE knowledge_bases SET archived_at=NULL WHERE id=$1").bind(kb).execute(pool).await?;
    let foreign=Uuid::now_v7();
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Other','user')").bind(foreign).bind(format!("{foreign}@research.test")).execute(pool).await?;
    let mut api=state.clone();api.auth=Arc::new(Auth {actor:actor.clone(),foreign});
    let router=fosnie_backend::http::research_jobs::routes().merge(fosnie_backend::http::document_verification_jobs::routes()).with_state(api);
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let base=format!("http://{}",listener.local_addr()?);
    let server=tokio::spawn(async move {axum::serve(listener,router).await.unwrap();});let client=reqwest::Client::new();
    let gate="/api/routing/research-jobs";
    let body=json!({"request":{"question":"Question","kb_ids":[kb],"template":"freeform"},"selection":{"mode":"default"}});
    let before=calls.load(Ordering::SeqCst);
    let (code,scope)=request(&client,&base,"GET","/api/research-jobs/scope","user",None).await?;
    assert_eq!(code,200);assert!(scope.as_array().unwrap().iter().any(|k|k["id"]==json!(kb)));
    for entry in scope.as_array().unwrap() {assert_eq!(entry.as_object().unwrap().len(),4);}
    let (code,scope)=request(&client,&base,"GET","/api/research-jobs/scope","foreign",None).await?;
    assert_eq!(code,200);assert!(!scope.as_array().unwrap().iter().any(|k|k["id"]==json!(kb)));
    assert_eq!(request(&client,&base,"GET","/api/research-jobs/scope","restricted",None).await?.0,403);
    assert_eq!(calls.load(Ordering::SeqCst),before,"scope lookup must not invoke inference");
    assert_eq!(request(&client,&base,"GET",gate,"user",None).await?.1["enabled"],false);
    assert_eq!(request(&client,&base,"POST","/api/research-jobs","user",Some(body.clone())).await?.0,400);
    assert_eq!(request(&client,&base,"PUT",gate,"user",Some(json!({"enabled":true}))).await?.0,403);
    assert_eq!(request(&client,&base,"PUT",gate,"restricted",Some(json!({"enabled":true}))).await?.0,403);
    assert_eq!(request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?.0,200);
    assert_eq!(request(&client,&base,"POST","/api/research-jobs","restricted",Some(body.clone())).await?.0,403);
    assert_eq!(request(&client,&base,"POST","/api/research-jobs","foreign",Some(body.clone())).await?.0,403);
    let mut bad=body.clone();bad["request"]["unsupported"]=json!("web");
    assert_eq!(request(&client,&base,"POST","/api/research-jobs","user",Some(bad)).await?.0,422);
    // A failed enqueue audit must roll back the newly-created chat and task too.
    let chats:i64=sqlx::query_scalar("SELECT count(*) FROM chats").fetch_one(pool).await?;
    sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT research_queue_test CHECK(action_type<>'routing.research.queued') NOT VALID").execute(pool).await?;
    assert_eq!(request(&client,&base,"POST","/api/research-jobs","user",Some(body.clone())).await?.0,500);
    sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT research_queue_test").execute(pool).await?;
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM chats").fetch_one(pool).await?,chats);
    for case in 0..17 {
        mode.store(if case==10 {7} else if case>=11 {6} else {0},Ordering::SeqCst);
        tokio::fs::write(path,b"Evidence.").await?;
        sqlx::query("UPDATE knowledge_bases SET archived_at=NULL WHERE id=$1").bind(kb).execute(pool).await?;
        request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?;
        let mut launch=body.clone();
        if case==1 {
            let provider:Uuid=sqlx::query_scalar("SELECT (policy->>'default_provider_id')::uuid FROM routing_policies WHERE is_default").fetch_one(pool).await?;
            launch["selection"]=json!({"mode":"manual","provider_id":provider});
        }
        if case==2 {launch["selection"]=json!({"mode":"auto","objective":"balanced"});}
        let before=calls.load(Ordering::SeqCst);
        let (code,started)=request(&client,&base,"POST","/api/research-jobs","user",Some(launch)).await?;
        assert_eq!(code,202,"research launch {case}: {started}");
        let id:Uuid=serde_json::from_value(started["job_id"].clone())?;let chat:Uuid=serde_json::from_value(started["chat_id"].clone())?;
        let url=format!("/api/research-jobs/{id}");
        let (code,status)=request(&client,&base,"GET",&url,"user",None).await?;
        assert_eq!(code,200);assert_eq!(status["status"],"queued");assert_eq!(status["usage"]["calls"],0);
        for secret in ["path","api_key","lease_until","email","sha256"] {assert!(!status.to_string().contains(secret));}
        assert_eq!(request(&client,&base,"GET",&url,"foreign",None).await?.0,403);
        assert_eq!(request(&client,&base,"POST",&format!("{url}/cancel"),"foreign",None).await?.0,403);
        assert_eq!(request(&client,&base,"GET",&format!("/api/document-verification-jobs/{id}"),"user",None).await?.0,404);
        let (task,deadline,plan):(Uuid,time::OffsetDateTime,Value)=sqlx::query_as("SELECT task_id,deadline,plan FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        let payload:Value=sqlx::query_scalar("SELECT payload FROM tasks WHERE id=$1").bind(task).fetch_one(pool).await?;
        assert_eq!(payload,json!({"routed_job_id":id}));assert!(!plan.to_string().contains("api_key"));
        assert!(durable::handle_task(state,id,task).await.is_err(),"document worker must not claim research");
        match case {
            3=>{jobs::cancel(state,actor,id).await?;},
            4=>{tokio::fs::write(path,b"changed").await?;},
            5=>{sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT research_publish_test CHECK(action_type<>'routing.research.succeeded') NOT VALID").execute(pool).await?;},
            6=>{request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":false}))).await?;},
            7=>{sqlx::query("UPDATE users SET role='power_user' WHERE id=$1").bind(actor.user_id).execute(pool).await?;},
            8=>{assert!(jobs::handle_task(state,id,Uuid::now_v7()).await.is_err());},
            9=>{sqlx::query("UPDATE routed_jobs SET deadline=now()-interval '1 second' WHERE id=$1").bind(id).execute(pool).await?;durable::recover(state).await?;},
            _=>{},
        }
        let work=Box::pin(jobs::handle_task(state,id,task));
        let result=if case>=11 {
            let stop=async {
                entered.notified().await;
                match case {
                    11=>jobs::cancel(state,actor,id).await?,
                    12=>{request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":false}))).await?;},
                    13=>{sqlx::query("UPDATE knowledge_bases SET archived_at=now() WHERE id=$1").bind(kb).execute(pool).await?;},
                    14=>{sqlx::query("UPDATE routed_jobs SET owner=$2 WHERE id=$1").bind(id).bind(Uuid::now_v7()).execute(pool).await?;},
                    15=>{sqlx::query("UPDATE chats SET archived_at=now() WHERE id=$1").bind(chat).execute(pool).await?;},
                    16=>{assert!(jobs::handle_task(state,id,task).await.is_err());},_=>{},
                } Ok::<_,anyhow::Error>(())
            };
            let (result,stopped)=tokio::time::timeout(std::time::Duration::from_secs(12),async {tokio::join!(work,stop)}).await?;
            stopped?;result
        } else {work.await};
        if case==5 {sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT research_publish_test").execute(pool).await?;}
        if case==7 {sqlx::query("UPDATE users SET role='user' WHERE id=$1").bind(actor.user_id).execute(pool).await?;}
        let success=matches!(case,0|1|2|8|16);
        assert_eq!(result.is_ok(),success,"research worker {case}: {result:?}");
        if case==14 {sqlx::query("UPDATE routed_jobs SET lease_until=now()-interval '1 second' WHERE id=$1").bind(id).execute(pool).await?;durable::recover(state).await?;}
        let (status,stored_deadline,stored_plan,usage):(String,time::OffsetDateTime,Value,Value)=sqlx::query_as("SELECT status,deadline,plan,usage FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        assert_eq!(status,if success {"succeeded"} else if matches!(case,3|11) {"cancelled"} else if case==14 {"interrupted"} else {"failed"},"research status {case}");
        if case!=9 {assert_eq!(stored_deadline,deadline);}
        assert_eq!(stored_plan,plan);
        assert_eq!(calls.load(Ordering::SeqCst)-before,usize::from(success || case==5 || case>=10),"research dispatch {case}");
        if case>=10 && case!=16 {assert_eq!(usage["calls"],22,"unknown streamed work stays reserved");}
        let counts:(i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM messages WHERE chat_id=$1),(SELECT count(*) FROM generated_artefacts WHERE chat_id=$1),(SELECT count(*) FROM citations c JOIN messages m ON m.id=c.message_id WHERE m.chat_id=$1),(SELECT count(*) FROM audit_events WHERE resource_id=$2 AND action_type='routing.research.succeeded')")
            .bind(chat).bind(id).fetch_one(pool).await?;
        assert_eq!(counts,if success {(1,1,1,1)} else {(0,0,0,0)},"atomic research publication {case}");
        if success {
            let before=calls.load(Ordering::SeqCst);jobs::handle_task(state,id,task).await?;assert_eq!(calls.load(Ordering::SeqCst),before);
            let (code,view)=request(&client,&base,"GET",&url,"user",None).await?;assert_eq!(code,200);assert!(!view["message_id"].is_null());
            let rel:String=sqlx::query_scalar("SELECT disk_path FROM generated_artefacts WHERE chat_id=$1").bind(chat).fetch_one(pool).await?;
            let file=tokio::fs::read_to_string(fosnie_backend::storage::resolve_file(&state.boot.storage.artefacts_dir,&rel)).await?;
            assert!(file.contains("Evidence [D1]."));
        }
        jobs::cancel(state,actor,id).await?;
    }
    // Web/hybrid use the same frozen job and fenced publication. No external
    // network calls occur: the ML service above is a local protocol fixture.
    for case in 0..9 {
        mode.store(if matches!(case,5|6|8) {26} else if case==3 {21} else if case==4 {22} else {20},Ordering::SeqCst);
        for (key,value,kind) in [("integration.web_search.enabled","true","bool"),("web_search.allowlist","weather.test","string"),
            ("web_search.allowlist_only","true","bool"),("web_search.blocklist","blocked.test","string"),("web_search.robots_policy","respect","string")] {
            sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,$2,$3::config_value_type) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,value_type=EXCLUDED.value_type")
                .bind(key).bind(value).bind(kind).execute(pool).await?;
        }
        request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?;
        let mut launch=body.clone();let hybrid=case==1;
        launch["request"]["source"]=json!(if hybrid {"hybrid"} else {"web"});
        if !hybrid {launch["request"]["kb_ids"]=json!([]);}
        if case==1 {launch["selection"]=json!({"mode":"auto","objective":"balanced"});}
        if case==7 {
            let provider:Uuid=sqlx::query_scalar("SELECT (policy->>'default_provider_id')::uuid FROM routing_policies WHERE is_default").fetch_one(pool).await?;
            launch["selection"]=json!({"mode":"manual","provider_id":provider});
        }
        let (code,started)=request(&client,&base,"POST","/api/research-jobs","user",Some(launch.clone())).await?;
        assert_eq!(code,202,"web research launch {case}: {started}");
        let id:Uuid=serde_json::from_value(started["job_id"].clone())?;let chat:Uuid=serde_json::from_value(started["chat_id"].clone())?;
        let (task,deadline,plan):(Uuid,time::OffsetDateTime,Value)=sqlx::query_as("SELECT task_id,deadline,plan FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        if case==2 {sqlx::query("UPDATE config_settings SET value='false' WHERE key='integration.web_search.enabled'").execute(pool).await?;}
        if case==7 {sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT web_publish_test CHECK(action_type<>'routing.research.succeeded') NOT VALID").execute(pool).await?;}
        let before=calls.load(Ordering::SeqCst);
        let work=Box::pin(jobs::handle_task(state,id,task));
        let result=if matches!(case,5|6|8) {
            let stop=async {
                entered.notified().await;
                if case==8 {jobs::cancel(state,actor,id).await?;} else {
                    sqlx::query("UPDATE config_settings SET value=$2 WHERE key=$1")
                        .bind(if case==5 {"integration.web_search.enabled"} else {"web_search.allowlist"})
                        .bind(if case==5 {"false"} else {"changed.test"}).execute(pool).await?;
                } Ok::<_,anyhow::Error>(())
            };
            let (result,stopped)=tokio::time::timeout(std::time::Duration::from_secs(12),async {tokio::join!(work,stop)}).await?;
            stopped?;result
        } else {work.await};
        if case==7 {sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT web_publish_test").execute(pool).await?;}
        assert_eq!(result.is_ok(),case<2,"web research worker {case}: {result:?}");
        assert_eq!(calls.load(Ordering::SeqCst)-before,usize::from(case!=2));
        let (stored_deadline,stored_plan):(time::OffsetDateTime,Value)=sqlx::query_as("SELECT deadline,plan FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        assert_eq!(stored_deadline,deadline);assert_eq!(stored_plan,plan);
        let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM messages WHERE chat_id=$1),(SELECT count(*) FROM generated_artefacts WHERE chat_id=$1),(SELECT count(*) FROM web_citations w JOIN messages m ON m.id=w.message_id WHERE m.chat_id=$1)")
            .bind(chat).fetch_one(pool).await?;
        assert_eq!(counts,if case<2 {(1,1,1)} else {(0,0,0)},"atomic web publication {case}");
        if hybrid {
            let text:String=sqlx::query_scalar("SELECT content FROM messages WHERE chat_id=$1").bind(chat).fetch_one(pool).await?;
            assert!(text.contains("Web evidence [D2], file evidence [D1]."));
        }
        if case==2 {assert_eq!(request(&client,&base,"POST","/api/research-jobs","user",Some(launch)).await?.0,403);}
    }
    for case in 0..9 {
        request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?;
        mode.store(if matches!(case,4|5|6) {6} else {0},Ordering::SeqCst);
        let template=Uuid::now_v7();
        let skeleton=if case==8 {json!([])} else {json!([{"heading":"Findings","brief":"Evidence","expandable":false,"exec_summary":false}])};
        sqlx::query("INSERT INTO research_templates(id,label,skeleton,writing_instructions,outline_mode,scope,created_by) VALUES($1,'Custom',$2,$3,$4,$5,$6)")
            .bind(template).bind(skeleton).bind(if case==2 {"x".repeat(4097)} else {"Write evidence-led prose.".into()})
            .bind(if case==8 {"free"} else {"constrained"}).bind(if case==5 {"global"} else {"personal"})
            .bind(if matches!(case,1|5) {Some(foreign)} else {actor.user_id}).execute(pool).await?;
        let mut launch=body.clone();launch["request"]["template"]=json!(template);
        let before=calls.load(Ordering::SeqCst);
        let (code,started)=request(&client,&base,"POST","/api/research-jobs","user",Some(launch)).await?;
        if matches!(case,1|2) {
            assert_eq!(code,if case==1 {404} else {400},"custom template preflight {case}: {started}");
            assert_eq!(calls.load(Ordering::SeqCst),before);continue;
        }
        assert_eq!(code,202,"custom template launch {case}: {started}");
        let id:Uuid=serde_json::from_value(started["job_id"].clone())?;let chat:Uuid=serde_json::from_value(started["chat_id"].clone())?;
        let (task,frozen):(Uuid,Value)=sqlx::query_as("SELECT task_id,input->'source'->'template' FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        assert_eq!(frozen["id"],json!(template));assert_eq!(frozen["writing_instructions"],"Write evidence-led prose.");
        if case==3 {sqlx::query("UPDATE research_templates SET writing_instructions='Changed' WHERE id=$1").bind(template).execute(pool).await?;}
        if case==7 {sqlx::query("UPDATE research_templates SET archived_at=now() WHERE id=$1").bind(template).execute(pool).await?;}
        let work=Box::pin(jobs::handle_task(state,id,task));
        let result=if matches!(case,4|5|6) {
            let edit=async {
                entered.notified().await;
                let sql=match case {4=>"UPDATE research_templates SET writing_instructions='Changed' WHERE id=$1",
                    5=>"UPDATE research_templates SET scope='personal' WHERE id=$1",_=>"DELETE FROM research_templates WHERE id=$1"};
                sqlx::query(sql).bind(template).execute(pool).await?;Ok::<_,anyhow::Error>(())
            };
            let (result,edited)=tokio::time::timeout(std::time::Duration::from_secs(12),async {tokio::join!(work,edit)}).await?;
            edited?;result
        } else {work.await};
        let success=matches!(case,0|7|8);
        assert_eq!(result.is_ok(),success,"custom template worker {case}: {result:?}");
        assert_eq!(calls.load(Ordering::SeqCst)-before,usize::from(case!=3));
        let saved:Value=sqlx::query_scalar("SELECT input->'source'->'template' FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        assert_eq!(saved,frozen,"template snapshot must never be rewritten");
        let messages:i64=sqlx::query_scalar("SELECT count(*) FROM messages WHERE chat_id=$1").bind(chat).fetch_one(pool).await?;
        assert_eq!(messages,i64::from(success));
    }
    // Clarification is one durable checkpoint, with one charged planning call.
    // It never changes source scope or renews the execution plan/deadline.
    for case in 0..12 {
        mode.store(match case {1=>41,2=>42,3=>43,11=>44,_=>40},Ordering::SeqCst);
        tokio::fs::write(path,b"Evidence.").await?;
        sqlx::query("UPDATE knowledge_bases SET archived_at=NULL WHERE id=$1").bind(kb).execute(pool).await?;
        request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?;
        let mut launch=body.clone();launch["clarify"]=json!(true);
        let (code,started)=request(&client,&base,"POST","/api/research-jobs","user",Some(launch)).await?;
        assert_eq!(code,202,"clarify launch {case}: {started}");
        let id:Uuid=serde_json::from_value(started["job_id"].clone())?;
        let chat:Uuid=serde_json::from_value(started["chat_id"].clone())?;
        let url=format!("/api/research-jobs/{id}");let answers_url=format!("{url}/answers");
        let (task,deadline,plan,limits):(Uuid,time::OffsetDateTime,Value,Value)=sqlx::query_as("SELECT task_id,deadline,plan,limits FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        let before=calls.load(Ordering::SeqCst);
        if case==10 {sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT research_pause_test CHECK(action_type<>'routing.research.awaiting_input') NOT VALID").execute(pool).await?;}
        let result=if case==11 {
            let (work,stopped)=tokio::join!(jobs::handle_task(state,id,task),async {entered.notified().await;jobs::cancel(state,actor,id).await});
            stopped?;work
        } else {jobs::handle_task(state,id,task).await};
        if case==10 {sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT research_pause_test").execute(pool).await?;}
        assert_eq!(result.is_ok(),!matches!(case,2|3|10|11),"triage worker {case}: {result:?}");
        let (code,status)=request(&client,&base,"GET",&url,"user",None).await?;
        assert_eq!(code,200);assert_eq!(calls.load(Ordering::SeqCst)-before,if case==1 {2} else {1});
        if matches!(case,1|2|3|10|11) {
            assert_eq!(status["status"],if case==1 {"succeeded"} else if case==11 {"cancelled"} else {"failed"});
            assert!(status["clarification"].is_null());
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM messages WHERE chat_id=$1").bind(chat).fetch_one(pool).await?,i64::from(case==1));
            continue;
        }
        assert_eq!(status["status"],"awaiting_input");assert_eq!(status["usage"]["calls"],1);
        let paused_limits:Value=sqlx::query_scalar("SELECT limits FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        assert!(paused_limits["max_duration_ms"].as_u64().unwrap()<=limits["max_duration_ms"].as_u64().unwrap_or(u64::MAX));
        assert_eq!(sqlx::query_scalar::<_,String>("SELECT status::text FROM tasks WHERE id=$1").bind(task).fetch_one(pool).await?,"succeeded");
        jobs::handle_task(state,id,task).await?;assert_eq!(calls.load(Ordering::SeqCst)-before,1,"duplicate paused delivery is inert");
        let answers=json!({"clarification_id":status["clarification"]["id"],"answers":["Last year"]});
        assert_eq!(request(&client,&base,"POST",&answers_url,"foreign",Some(answers.clone())).await?.0,403);
        assert_eq!(request(&client,&base,"POST",&answers_url,"restricted",Some(answers.clone())).await?.0,403);
        let mut invalid=answers.clone();invalid["answers"]=json!([]);
        assert_eq!(request(&client,&base,"POST",&answers_url,"user",Some(invalid)).await?.0,400);
        let mut invalid=answers.clone();invalid["clarification_id"]=json!(Uuid::now_v7());
        assert_eq!(request(&client,&base,"POST",&answers_url,"user",Some(invalid)).await?.0,400);
        let mut invalid=answers.clone();invalid["kb_ids"]=json!([Uuid::now_v7()]);
        assert_eq!(request(&client,&base,"POST",&answers_url,"user",Some(invalid)).await?.0,422);
        match case {
            4=>jobs::cancel(state,actor,id).await?,
            5=>{sqlx::query("UPDATE routed_jobs SET deadline=now()-interval '1 second' WHERE id=$1").bind(id).execute(pool).await?;durable::recover(state).await?;},
            6=>{tokio::fs::write(path,b"Changed evidence.").await?;},
            7=>{sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT research_resume_test CHECK(action_type<>'routing.research.resumed') NOT VALID").execute(pool).await?;},
            8=>{request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":false}))).await?;},
            9=>{sqlx::query("UPDATE users SET role='power_user' WHERE id=$1").bind(actor.user_id).execute(pool).await?;},_=>{},
        }
        let (code,response)=request(&client,&base,"POST",&answers_url,"user",Some(answers.clone())).await?;
        assert_eq!(code==200,case==0,"resume {case}: {code} {response}");
        if case==7 {sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT research_resume_test").execute(pool).await?;}
        if case==9 {sqlx::query("UPDATE users SET role='user' WHERE id=$1").bind(actor.user_id).execute(pool).await?;}
        let (new_task,new_deadline,new_plan,new_limits,usage):(Uuid,time::OffsetDateTime,Value,Value,Value)=sqlx::query_as("SELECT task_id,deadline,plan,limits,usage FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        assert_eq!(new_plan,plan);assert_eq!(new_limits,paused_limits);if case!=5 {assert_eq!(new_deadline,deadline);}
        assert_eq!(usage["calls"],1,"answer submission must not call a model");
        if case==0 {
            assert_ne!(new_task,task);
            assert_eq!(request(&client,&base,"POST",&answers_url,"user",Some(answers.clone())).await?.0,200);
            let mut changed=answers.clone();changed["answers"]=json!(["This year"]);
            assert_eq!(request(&client,&base,"POST",&answers_url,"user",Some(changed)).await?.0,400);
            assert_eq!(sqlx::query_scalar::<_,Uuid>("SELECT task_id FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?,new_task);
            assert!(jobs::handle_task(state,id,task).await.is_err(),"old delivery cannot resume");
            jobs::handle_task(state,id,new_task).await?;
            let (_,status)=request(&client,&base,"GET",&url,"user",None).await?;
            assert_eq!(status["status"],"succeeded");assert_eq!(status["usage"]["calls"],2);
            assert_eq!(calls.load(Ordering::SeqCst)-before,2,"triage must not repeat after answers");
            let saved:Value=sqlx::query_scalar("SELECT research_params FROM chats WHERE id=$1").bind(chat).fetch_one(pool).await?;
            assert_eq!(saved["refinements"],json!(["Which timeframe?: Last year"]));
        } else {
            assert_eq!(new_task,task,"rejected answers cannot enqueue work");
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM messages WHERE chat_id=$1").bind(chat).fetch_one(pool).await?,0);
            jobs::cancel(state,actor,id).await?;
        }
    }
    request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":false}))).await?;
    sqlx::query("UPDATE knowledge_bases SET archived_at=NULL WHERE id=$1").bind(kb).execute(pool).await?;
    for case in 0..11 {
        mode.store(50+case,Ordering::SeqCst);tokio::fs::write(path,b"Evidence.").await?;
        request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?;
        let mut launch=body.clone();launch["request"]["verify"]=json!(case!=10);
        launch["request"]["deepen"]=json!(matches!(case,1|7|8|10));
        let (code,started)=request(&client,&base,"POST","/api/research-jobs","user",Some(launch)).await?;
        assert_eq!(code,202,"quality launch {case}: {started}");
        let id:Uuid=serde_json::from_value(started["job_id"].clone())?;let chat:Uuid=serde_json::from_value(started["chat_id"].clone())?;
        let (task,deadline,plan):(Uuid,time::OffsetDateTime,Value)=sqlx::query_as("SELECT task_id,deadline,plan FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        if case!=10 {assert!(plan["fixed_assignments"]["verify"].is_object());}
        if case==6 {sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT research_verification_test CHECK(action_type<>'groundedness.verified') NOT VALID").execute(pool).await?;}
        let result=if case==9 {
            let (r,c)=tokio::join!(jobs::handle_task(state,id,task),async {entered.notified().await;jobs::cancel(state,actor,id).await});c?;r
        } else {jobs::handle_task(state,id,task).await};
        if case==6 {sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT research_verification_test").execute(pool).await?;}
        let success=matches!(case,0|1|10);
        assert_eq!(result.is_ok(),success,"quality worker {case}: {result:?}");
        let (new_deadline,new_plan):(time::OffsetDateTime,Value)=sqlx::query_as("SELECT deadline,plan FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        assert_eq!(new_deadline,deadline);assert_eq!(new_plan,plan);
        let messages:Vec<(Uuid,String,Option<Value>)>=sqlx::query_as("SELECT id,content,groundedness FROM messages WHERE chat_id=$1").bind(chat).fetch_all(pool).await?;
        assert_eq!(messages.len(),usize::from(success));
        let runs:i64=sqlx::query_scalar("SELECT count(*) FROM verification_runs WHERE target_id IN (SELECT id FROM messages WHERE chat_id=$1)").bind(chat).fetch_one(pool).await?;
        assert_eq!(runs,i64::from(success && case!=10));
        if success {
            let (_,status)=request(&client,&base,"GET",&format!("/api/research-jobs/{id}"),"user",None).await?;
            assert_eq!(status["options"]["verify"],case!=10);
            if case==10 {assert!(status["verification"].is_null());}
            else {
                let summary=messages[0].2.as_ref().unwrap();assert_eq!(summary["method"],"source_grounding_v1");
                assert_eq!(summary["flagged"],usize::from(case==1));
                assert_eq!(status["verification"]["flagged"],summary["flagged"]);assert!(status["verification"].get("spans").is_none());
                for span in summary["spans"].as_array().unwrap() {
                    let start=span["start"].as_u64().unwrap() as usize;let end=span["end"].as_u64().unwrap() as usize;
                    assert_eq!(messages[0].1.chars().skip(start).take(end-start).collect::<String>(),span["text"].as_str().unwrap());
                }
                assert!(status["usage"]["calls"].as_u64().unwrap()>=2);
            }
        }
    }
    request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":false}))).await?;
    // A durable launch receipt fences concurrent/replayed planning and survives
    // an enqueue rollback. History remains usable after gate/access revocation.
    mode.store(0,Ordering::SeqCst);tokio::fs::write(path,b"Evidence.").await?;
    request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?;
    let key=Uuid::now_v7();let receipt_url=format!("/api/research-launches/{key}");
    let mut keyed=body.clone();keyed["request_id"]=json!(key);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM routed_jobs").fetch_one(pool).await?;
    let (first,second)=tokio::join!(
        request(&client,&base,"POST","/api/research-jobs","user",Some(keyed.clone())),
        request(&client,&base,"POST","/api/research-jobs","user",Some(keyed.clone())));
    let first=first?;let second=second?;
    assert!(first.0==202 || second.0==202,"concurrent launches: {first:?} {second:?}");
    for response in [&first,&second] {assert!(matches!(response.0,202|409),"{response:?}");}
    let started=if first.0==202 {first.1} else {second.1};
    let id:Uuid=serde_json::from_value(started["job_id"].clone())?;
    let frozen:(Uuid,time::OffsetDateTime,Value)=sqlx::query_as("SELECT task_id,deadline,plan FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM routed_jobs").fetch_one(pool).await?,count+1);
    request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":false}))).await?;
    let replay=request(&client,&base,"POST","/api/research-jobs","user",Some(keyed.clone())).await?;
    assert_eq!(replay.0,202,"{replay:?}");assert_eq!(replay.1,started);
    assert_eq!(sqlx::query_as::<_,(Uuid,time::OffsetDateTime,Value)>("SELECT task_id,deadline,plan FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?,frozen);
    let (code,receipt)=request(&client,&base,"GET",&receipt_url,"user",None).await?;
    assert_eq!(code,200);assert_eq!(receipt["status"],"succeeded");assert_eq!(receipt["job_id"],json!(id));
    assert_eq!(request(&client,&base,"GET",&receipt_url,"foreign",None).await?.0,404);
    assert_eq!(request(&client,&base,"GET",&receipt_url,"restricted",None).await?.0,403);
    let mut changed=keyed.clone();changed["request"]["question"]=json!("Different question");
    assert_eq!(request(&client,&base,"POST","/api/research-jobs","user",Some(changed)).await?.0,409);
    let (code,page)=request(&client,&base,"GET","/api/research-jobs","user",None).await?;
    assert_eq!(code,200);assert_eq!(page["items"].as_array().unwrap().len(),20);
    assert_eq!(page["items"][0]["job_id"],json!(id));assert_eq!(page["items"][0]["accessible"],true);
    let cursor=page["next_cursor"].as_str().unwrap();
    let (code,older)=request(&client,&base,"GET",&format!("/api/research-jobs?before={cursor}"),"user",None).await?;
    assert_eq!(code,200);assert!(!older["items"].as_array().unwrap().is_empty());
    for entry in older["items"].as_array().unwrap() {
        assert!(entry["job_id"].as_str().unwrap()<cursor);
        assert!(!page["items"].as_array().unwrap().iter().any(|i|i["job_id"]==entry["job_id"]));
    }
    assert_eq!(request(&client,&base,"GET","/api/research-jobs","foreign",None).await?.1["items"],json!([]));
    assert_eq!(request(&client,&base,"GET","/api/research-jobs","restricted",None).await?.0,403);
    sqlx::query("UPDATE knowledge_bases SET archived_at=now() WHERE id=$1").bind(kb).execute(pool).await?;
    let (code,redacted)=request(&client,&base,"GET","/api/research-jobs","user",None).await?;
    assert_eq!(code,200);let entry=&redacted["items"][0];assert_eq!(entry["job_id"],json!(id));
    assert_eq!(entry["accessible"],false);assert_eq!(entry.as_object().unwrap().len(),4);
    assert_ne!(request(&client,&base,"GET",&receipt_url,"user",None).await?.0,200);
    assert_ne!(request(&client,&base,"POST","/api/research-jobs","user",Some(keyed.clone())).await?.0,202);
    assert_eq!(request(&client,&base,"POST",&format!("/api/research-jobs/{id}/cancel"),"user",None).await?.0,200);
    sqlx::query("UPDATE knowledge_bases SET archived_at=NULL WHERE id=$1").bind(kb).execute(pool).await?;
    sqlx::query("UPDATE users SET role='power_user' WHERE id=$1").bind(actor.user_id).execute(pool).await?;
    assert_eq!(request(&client,&base,"GET",&receipt_url,"user",None).await?.0,403);
    sqlx::query("UPDATE users SET role='user' WHERE id=$1").bind(actor.user_id).execute(pool).await?;
    // Audit rejection cannot leave a runnable job, nor make the paid-planning
    // key reusable. An expired planning tombstone models process interruption.
    request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?;
    let failed_key=Uuid::now_v7();keyed["request_id"]=json!(failed_key);
    let failed_url=format!("/api/research-launches/{failed_key}");
    sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT research_receipt_test CHECK(action_type<>'routing.research.queued') NOT VALID").execute(pool).await?;
    assert_eq!(request(&client,&base,"POST","/api/research-jobs","user",Some(keyed.clone())).await?.0,500);
    sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT research_receipt_test").execute(pool).await?;
    assert_eq!(request(&client,&base,"GET",&failed_url,"user",None).await?.1["status"],"failed");
    assert_eq!(request(&client,&base,"POST","/api/research-jobs","user",Some(keyed.clone())).await?.0,409);
    sqlx::query("UPDATE research_launch_receipts SET status='planning',deadline=now()-interval '1 second' WHERE actor_user_id=$1 AND request_id=$2")
        .bind(actor.user_id).bind(failed_key).execute(pool).await?;
    assert_eq!(request(&client,&base,"GET",&failed_url,"user",None).await?.1["status"],"interrupted");
    assert_eq!(request(&client,&base,"POST","/api/research-jobs","user",Some(keyed)).await?.0,409);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM routed_jobs").fetch_one(pool).await?,count+1);
    request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":false}))).await?;
    server.abort();Ok(())
}
