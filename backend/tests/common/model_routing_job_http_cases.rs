// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
use serde_json::{json,Value};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,PlatformRole},groundedness::durable,model_routing::*};
struct Auth {actor:AuthContext,foreign:Uuid}
#[async_trait::async_trait]
impl fosnie_backend::ext::AuthProvider for Auth {
    async fn authenticate(&self,parts:&mut axum::http::request::Parts,_:&AppState)->fosnie_backend::Result<AuthContext> {
        let mut actor=self.actor.clone();
        match parts.headers.get("x-test-actor").and_then(|v|v.to_str().ok()) {
            Some("admin")=>actor.role=PlatformRole::ClientAdmin,
            Some("foreign")=>actor.user_id=Some(self.foreign),
            Some("restricted")=>actor.mfa_enroll_only=true,
            _=>{},
        } Ok(actor)
    }
}
async fn request(client:&reqwest::Client,base:&str,method:&str,path:&str,actor:&str,body:Option<Value>)->anyhow::Result<(u16,Value)> {
    let mut req=client.request(method.parse()?,format!("{base}{path}")).header("x-test-actor",actor);
    if let Some(body)=body {req=req.json(&body);}
    let response=req.send().await?;let status=response.status().as_u16();let text=response.text().await?;
    Ok((status,serde_json::from_str(&text).unwrap_or(Value::String(text))))
}
pub async fn check_http(state:&AppState,admin:&AuthContext,actor:&AuthContext,project:Uuid,kb:Uuid,policy_id:Uuid,
    mode:&Arc<AtomicUsize>,calls:&Arc<AtomicUsize>)->anyhow::Result<()> {
    use fosnie_backend::http::{document_verification_jobs as http,groundedness};
    let pool=&state.pg;let foreign=Uuid::now_v7();
    sqlx::query("INSERT INTO users(id,display_name,email,role) VALUES($1,'Other',$2,'user')").bind(foreign).bind(format!("{foreign}@job.test")).execute(pool).await?;
    let mut api=state.clone();api.auth=Arc::new(Auth {actor:actor.clone(),foreign});
    let router=http::routes()
        .route("/api/verification-runs/{id}",axum::routing::get(groundedness::get_run))
        .route("/api/verification-runs/{id}/report",axum::routing::get(groundedness::export_report))
        .route("/api/verification-runs/{id}/repair",axum::routing::post(groundedness::repair)).with_state(api);
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let base=format!("http://{}",listener.local_addr()?);
    let server=tokio::spawn(async move {axum::serve(listener,router).await.unwrap();});let client=reqwest::Client::new();
    let gate="/api/routing/document-jobs";let endpoint="/api/document-verification-jobs";
    let (doc,_)=fosnie_backend::documents::create_document(state,actor,project,"public.docx",None,b"Wrong.").await?;
    let body=json!({"document_id":doc,"selection":{"mode":"default"},"repair":false});
    assert_eq!(request(&client,&base,"GET",gate,"user",None).await?.1["enabled"],false);
    let before=calls.load(Ordering::SeqCst);
    assert_eq!(request(&client,&base,"POST",endpoint,"user",Some(body.clone())).await?.0,400);
    assert_eq!(request(&client,&base,"PUT",gate,"user",Some(json!({"enabled":true}))).await?.0,403);
    assert_eq!(request(&client,&base,"PUT",gate,"restricted",Some(json!({"enabled":true}))).await?.0,403);
    assert_eq!(request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?.0,200);
    assert_eq!(request(&client,&base,"POST",endpoint,"restricted",Some(body.clone())).await?.0,403);
    assert_eq!(request(&client,&base,"POST",endpoint,"foreign",Some(body.clone())).await?.0,403);
    assert_eq!(request(&client,&base,"POST",endpoint,"user",Some(json!({"document_id":doc,"selection":{"mode":"magic"}}))).await?.0,422);
    assert_eq!(request(&client,&base,"POST",endpoint,"user",Some(json!({"document_id":doc,"selection":{"mode":"default"},"target_type":"draft"}))).await?.0,422);
    let (status,queued)=request(&client,&base,"POST",endpoint,"user",Some(body.clone())).await?;
    assert_eq!(status,202,"{queued}");let id:Uuid=serde_json::from_value(queued["job_id"].clone())?;
    let path=format!("{endpoint}/{id}");
    let (status,view)=request(&client,&base,"GET",&path,"user",None).await?;
    assert_eq!(status,200);assert_eq!(view["status"],"queued");assert_eq!(view["selection"]["mode"],"default");
    assert_eq!(view["usage"]["calls"],0);assert!(view["result_id"].is_null());
    let text=view.to_string();for secret in ["api_key","bytes_path","workspace_dir","lease_until","email"] {assert!(!text.contains(secret));}
    assert_eq!(request(&client,&base,"GET",&path,"foreign",None).await?.0,404);
    assert_eq!(request(&client,&base,"POST",&format!("{path}/cancel"),"foreign",None).await?.0,403);
    assert_eq!(request(&client,&base,"POST",&format!("{path}/cancel"),"restricted",None).await?.0,403);
    // Losing project access blocks reads, but cannot prevent stopping own spend.
    sqlx::query("UPDATE projects SET owner_user_id=$2 WHERE id=$1").bind(project).bind(foreign).execute(pool).await?;
    assert_eq!(request(&client,&base,"GET",&path,"user",None).await?.0,403);
    assert_eq!(request(&client,&base,"POST",&format!("{path}/cancel"),"user",None).await?.1["status"],"cancelled");
    assert_eq!(request(&client,&base,"POST",&format!("{path}/cancel"),"user",None).await?.1["status"],"cancelled");
    sqlx::query("UPDATE projects SET owner_user_id=$2 WHERE id=$1").bind(project).bind(actor.user_id).execute(pool).await?;
    assert_eq!(calls.load(Ordering::SeqCst),before,"rejected/prepared/cancelled launches never invoke models");

    let mut policy=store::load_policy(state,admin,Some(policy_id)).await?;
    let provider=policy.default_provider_id.unwrap();
    mode.store(40,Ordering::SeqCst);
    for scenario in 0..5 {
        policy.rollout=match scenario {2=>RolloutMode::Off,3=>RolloutMode::Shadow,_=>RolloutMode::Active};
        policy.strategy=RoutingStrategy::Rules;policy.rules.clear();
        policy=store::save_policy(state,admin,policy.id,store::SavePolicy {expected_revision:policy.revision,label:"HTTP documents".into(),scope:"deployment".into(),is_default:true,policy}).await?;
        let selection=match scenario {0=>json!({"mode":"default"}),1=>json!({"mode":"manual","provider_id":provider}),
            _=>json!({"mode":"auto","policy_id":policy.id,"objective":"balanced"})};
        let (doc,_)=fosnie_backend::documents::create_document(state,actor,project,"api.docx",None,b"Wrong.").await?;
        let count=calls.load(Ordering::SeqCst);
        let (code,reply)=request(&client,&base,"POST",endpoint,"user",Some(json!({"document_id":doc,"selection":selection,"repair":scenario==1}))).await?;
        assert_eq!(code,202,"scenario {scenario}: {reply}");assert_eq!(calls.load(Ordering::SeqCst),count,"rules/manual/default make no judge call");
        let id:Uuid=serde_json::from_value(reply["job_id"].clone())?;
        durable::handle(state,id).await?;
        let (code,view)=request(&client,&base,"GET",&format!("{endpoint}/{id}"),"user",None).await?;
        assert_eq!(code,200);assert_eq!(view["status"],"succeeded");assert_eq!(view["selection"],selection);
        assert_eq!(view["result_id"],json!(id));assert_eq!(view["planned_document_target"]["provider_id"],json!(provider));
        assert!(view["reason_codes"].as_array().unwrap().iter().any(|v|v=="document_metadata_only"));
        if scenario==2 {assert!(view["reason_codes"].to_string().contains("routing_off"));}
        if scenario==3 {assert!(view["reason_codes"].to_string().contains("shadow_default_planned"));}
        let result=format!("/api/verification-runs/{id}");
        assert_eq!(request(&client,&base,"GET",&result,"user",None).await?.0,200);
        assert_eq!(request(&client,&base,"GET",&format!("{result}/report"),"user",None).await?.0,200);
        sqlx::query("UPDATE knowledge_bases SET owner_id=$2 WHERE id=$1").bind(kb).bind(foreign).execute(pool).await?;
        assert_eq!(request(&client,&base,"GET",&result,"user",None).await?.0,403,"creator cannot retain revoked evidence");
        assert_eq!(request(&client,&base,"GET",&format!("{result}/report"),"user",None).await?.0,403);
        assert_eq!(request(&client,&base,"GET",&format!("{endpoint}/{id}"),"user",None).await?.0,403);
        sqlx::query("UPDATE knowledge_bases SET owner_id=$2 WHERE id=$1").bind(kb).bind(actor.user_id).execute(pool).await?;
    }
    // Gate revocation before execution and after tracked-edit application both
    // prevent publication; an already committed verification remains readable.
    for after_apply in [false,true] {
        mode.store(if after_apply {41} else {40},Ordering::SeqCst);
        let (doc,version)=fosnie_backend::documents::create_document(state,actor,project,"gate.docx",None,b"Wrong.").await?;
        let (_,reply)=request(&client,&base,"POST",endpoint,"user",Some(json!({"document_id":doc,"selection":{"mode":"manual","provider_id":provider},"repair":true}))).await?;
        let id:Uuid=serde_json::from_value(reply["job_id"].clone())?;
        let count=calls.load(Ordering::SeqCst);
        if !after_apply {assert_eq!(request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":false}))).await?.0,200);}
        assert!(durable::handle(state,id).await.is_err());
        if !after_apply {assert_eq!(calls.load(Ordering::SeqCst),count);}
        let current:Uuid=sqlx::query_scalar("SELECT current_version_id FROM documents WHERE id=$1").bind(doc).fetch_one(pool).await?;
        assert_eq!(current,version);
        let (status,view)=request(&client,&base,"GET",&format!("{endpoint}/{id}"),"user",None).await?;
        assert_eq!(status,200);assert_eq!(view["status"],"failed");assert_eq!(!view["result_id"].is_null(),after_apply);
        request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?;
    }
    // Delayed repair reconstructs everything from the database. No verifier
    // runtime survives this handoff, and no new classifier/verification is run.
    mode.store(40,Ordering::SeqCst);
    assert_eq!(request(&client,&base,"GET",gate,"user",None).await?.1["delayed_repair"],true);
    for scenario in 0..10 {
        let (doc,version)=fosnie_backend::documents::create_document(state,actor,project,"delayed.docx",None,b"Wrong.").await?;
        let (code,reply)=request(&client,&base,"POST",endpoint,"user",Some(json!({"document_id":doc,"selection":{"mode":"default"}}))).await?;
        assert_eq!(code,202,"{reply}");let id:Uuid=serde_json::from_value(reply["job_id"].clone())?;
        durable::handle(state,id).await?;
        let path=format!("{endpoint}/{id}/repair");
        let saved:Value=sqlx::query_scalar("SELECT jsonb_build_object('plan',plan,'context',context,'limits',limits,'usage',usage,'deadline',deadline,'task',task_id) FROM routed_jobs WHERE id=$1")
            .bind(id).fetch_one(pool).await?;
        let old_task:Uuid=serde_json::from_value(saved["task"].clone())?;
        let before=calls.load(Ordering::SeqCst);
        assert_eq!(saved["usage"]["calls"],4);
        assert_eq!(request(&client,&base,"POST",&path,"foreign",None).await?.0,403);
        assert_eq!(request(&client,&base,"POST",&path,"restricted",None).await?.0,403);
        match scenario {
            1=>{sqlx::query("UPDATE routed_jobs SET deadline=now()-interval '1 second' WHERE id=$1").bind(id).execute(pool).await?;}
            2=>{let current=fosnie_backend::documents::current_version(pool,&state.boot.storage.workspace_dir,doc).await?;
                tokio::fs::write(current.bytes_path,b"Changed.").await?;}
            3=>{sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT delayed_audit CHECK(action_type<>'routing.job.repair_queued') NOT VALID").execute(pool).await?;}
            4=>{sqlx::query("UPDATE routed_jobs SET verified_checkpoint=jsonb_set(verified_checkpoint,'{claim_ids}',jsonb_build_array($2::text)) WHERE id=$1")
                .bind(id).bind(Uuid::now_v7().to_string()).execute(pool).await?;}
            5=>{sqlx::query("UPDATE routed_jobs SET limits=jsonb_set(limits,'{max_calls}','4') WHERE id=$1").bind(id).execute(pool).await?;}
            6=>{sqlx::query("UPDATE knowledge_bases SET owner_id=$2 WHERE id=$1").bind(kb).bind(foreign).execute(pool).await?;}
            9=>{sqlx::query("UPDATE users SET role='power_user' WHERE id=$1").bind(actor.user_id).execute(pool).await?;}
            _=>{},
        }
        let (code,reply)=request(&client,&base,"POST",&path,"user",None).await?;
        if matches!(scenario,1|2|3|6|9) {
            assert!(code>=400,"delayed {scenario}: {reply}");
            assert_eq!(calls.load(Ordering::SeqCst),before);
            let current:Uuid=sqlx::query_scalar("SELECT task_id FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
            assert_eq!(current,old_task,"rejected continuation must not rotate the task");
            let count:i64=sqlx::query_scalar("SELECT count(*) FROM tasks WHERE payload->>'routed_job_id'=$1").bind(id.to_string()).fetch_one(pool).await?;
            assert_eq!(count,1,"queue/audit rollback is atomic");
            if scenario==3 {sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT delayed_audit").execute(pool).await?;}
            if scenario==6 {sqlx::query("UPDATE knowledge_bases SET owner_id=$2 WHERE id=$1").bind(kb).bind(actor.user_id).execute(pool).await?;}
            if scenario==9 {sqlx::query("UPDATE users SET role='user' WHERE id=$1").bind(actor.user_id).execute(pool).await?;}
            continue;
        }
        assert_eq!(code,202,"delayed {scenario}: {reply}");
        assert!(durable::handle_task(state,id,old_task).await.is_err(),"old delivery cannot claim repair");
        assert_eq!(calls.load(Ordering::SeqCst),before);
        if scenario==0 {
            let (a,b)=tokio::join!(request(&client,&base,"POST",&path,"user",None),request(&client,&base,"POST",&path,"user",None));
            assert_eq!(a?.0,202);assert_eq!(b?.0,202);
            let current:Value=sqlx::query_scalar("SELECT jsonb_build_object('plan',plan,'context',context,'limits',limits,'usage',usage,'deadline',deadline) FROM routed_jobs WHERE id=$1")
                .bind(id).fetch_one(pool).await?;
            for key in ["plan","context","limits","usage","deadline"] {assert_eq!(current[key],saved[key],"unchanged {key}");}
        }
        if scenario==7 {durable::cancel(state,actor,id).await?;}
        if scenario==8 {request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":false}))).await?;}
        let restarted=state.clone();let result=durable::handle(&restarted,id).await;
        if scenario==0 {
            result?;
            assert_eq!(calls.load(Ordering::SeqCst),before+1,"only repair, no new verification");
            let (_,view)=request(&client,&base,"GET",&format!("{endpoint}/{id}"),"user",None).await?;
            assert_eq!(view["phase"],"repair");assert_eq!(view["status"],"succeeded");assert_eq!(view["usage"]["calls"],8);
            assert_eq!(view["decision_id"],saved["plan"]["decision"]["id"]);
            assert_eq!(request(&client,&base,"POST",&path,"user",None).await?.0,202);
            assert_eq!(request(&client,&base,"POST",&format!("/api/verification-runs/{id}/repair"),"user",None).await?.0,200);
            durable::handle(state,id).await?;assert_eq!(calls.load(Ordering::SeqCst),before+1);
        } else {
            assert!(result.is_err(),"delayed {scenario} must stop");
            assert_eq!(calls.load(Ordering::SeqCst),before,"reject before HTTP {scenario}");
            let current:Uuid=sqlx::query_scalar("SELECT current_version_id FROM documents WHERE id=$1").bind(doc).fetch_one(pool).await?;
            assert_eq!(current,version);
            if scenario==8 {request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":true}))).await?;}
            assert!(request(&client,&base,"POST",&path,"user",None).await?.0>=400,"stopped phase cannot replay");
        }
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM tasks WHERE payload->>'routed_job_id'=$1").bind(id.to_string()).fetch_one(pool).await?;
        assert_eq!(count,2,"one verify and one repair task across duplicate requests");
    }
    // Generated artefacts share the queue/ledger but retain chat scope, their
    // immutable file fingerprint and the draft result type. No repair route.
    let mut outside=tempfile::NamedTempFile::new()?;
    std::io::Write::write_all(&mut outside,b"Outside storage.")?;
    for scenario in 0..9 {
        mode.store(40,Ordering::SeqCst);
        let chat=Uuid::now_v7();let artefact=Uuid::now_v7();
        sqlx::query("INSERT INTO chats(id,owner_user_id,project_id) VALUES($1,$2,$3)").bind(chat).bind(actor.user_id).bind(project).execute(pool).await?;
        let format=match scenario {1=>"docx",2=>"pdf",8=>"html",_=>"md"};
        let rel=format!("{artefact}.{format}");
        let path=fosnie_backend::storage::resolve_file(&state.boot.storage.artefacts_dir,&rel);
        tokio::fs::write(&path,b"Wrong.").await?;
        sqlx::query("INSERT INTO generated_artefacts(id,chat_id,kind,title,disk_path,mime,created_by) VALUES($1,$2,$3::artefact_kind,'Routing draft',$4,'text/plain',$5)")
            .bind(artefact).bind(chat).bind(format).bind(&rel).bind(actor.user_id).execute(pool).await?;
        if scenario==7 {sqlx::query("UPDATE generated_artefacts SET disk_path=$2 WHERE id=$1").bind(artefact).bind(outside.path().to_string_lossy().as_ref()).execute(pool).await?;}
        let endpoint_artefact="/api/artefact-verification-jobs";
        let body=json!({"artefact_id":artefact,"selection":{"mode":"default"}});
        let before=calls.load(Ordering::SeqCst);
        assert_eq!(request(&client,&base,"POST",endpoint_artefact,"foreign",Some(body.clone())).await?.0,403);
        assert_eq!(request(&client,&base,"POST",endpoint_artefact,"user",Some(json!({"artefact_id":artefact,"selection":{"mode":"default"},"repair":true}))).await?.0,422);
        let (code,reply)=request(&client,&base,"POST",endpoint_artefact,"user",Some(body)).await?;
        if scenario>=7 {assert_eq!(code,400,"artefact preflight {scenario}: {reply}");assert_eq!(calls.load(Ordering::SeqCst),before);continue;}
        assert_eq!(code,202,"artefact {scenario}: {reply}");
        let id:Uuid=serde_json::from_value(reply["job_id"].clone())?;
        let job_path=format!("{endpoint}/{id}");
        let (_,view)=request(&client,&base,"GET",&job_path,"user",None).await?;
        assert_eq!(view["target_type"],"draft");assert_eq!(view["target_id"],json!(artefact));assert!(view["document_id"].is_null());
        match scenario {
            3=>{tokio::fs::write(&path,b"Changed.").await?;}
            4=>mode.store(4,Ordering::SeqCst),
            5=>{sqlx::query("UPDATE chats SET owner_user_id=$2,project_id=NULL WHERE id=$1").bind(chat).bind(foreign).execute(pool).await?;}
            6=>{sqlx::query("DELETE FROM generated_artefacts WHERE id=$1").bind(artefact).execute(pool).await?;}
            _=>{},
        }
        let result=durable::handle(state,id).await;
        if scenario<3 {
            result?;assert_eq!(calls.load(Ordering::SeqCst),before+1);
            let (code,view)=request(&client,&base,"GET",&job_path,"user",None).await?;
            assert_eq!(code,200);assert_eq!(view["status"],"succeeded");assert_eq!(view["usage"]["calls"],4);
            let report=format!("/api/verification-runs/{id}");
            let (code,view)=request(&client,&base,"GET",&report,"user",None).await?;
            assert_eq!(code,200);assert_eq!(view["target_type"],"draft");assert_eq!(view["target_id"],json!(artefact));
            assert_eq!(request(&client,&base,"GET",&format!("{report}/report"),"user",None).await?.0,200);
            assert_eq!(request(&client,&base,"POST",&format!("{job_path}/repair"),"user",None).await?.0,400);
            assert_eq!(request(&client,&base,"POST",&format!("{report}/repair"),"user",None).await?.0,400);
            assert_eq!(calls.load(Ordering::SeqCst),before+1);
            sqlx::query("UPDATE knowledge_bases SET owner_id=$2 WHERE id=$1").bind(kb).bind(foreign).execute(pool).await?;
            assert_eq!(request(&client,&base,"GET",&report,"user",None).await?.0,403);
            assert_eq!(request(&client,&base,"GET",&format!("{report}/report"),"user",None).await?.0,403);
            assert_eq!(request(&client,&base,"GET",&job_path,"user",None).await?.0,403);
            sqlx::query("UPDATE knowledge_bases SET owner_id=$2 WHERE id=$1").bind(kb).bind(actor.user_id).execute(pool).await?;
            sqlx::query("UPDATE chats SET owner_user_id=$2,project_id=NULL WHERE id=$1").bind(chat).bind(foreign).execute(pool).await?;
            assert_eq!(request(&client,&base,"GET",&report,"user",None).await?.0,403,"creator cannot bypass chat revocation");
        } else {
            assert!(result.is_err(),"artefact {scenario} must stop");
            assert_eq!(calls.load(Ordering::SeqCst),before+usize::from(scenario==4));
            let committed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM verification_runs WHERE id=$1)").bind(id).fetch_one(pool).await?;
            assert!(!committed,"changed/deleted/revoked artefacts cannot publish a report");
            durable::cancel(state,actor,id).await?;
        }
    }
    sqlx::query("UPDATE config_settings SET value='invalid' WHERE key='routing.document_jobs_enabled'").execute(pool).await?;
    assert_eq!(request(&client,&base,"GET",gate,"user",None).await?.0,400);
    request(&client,&base,"PUT",gate,"admin",Some(json!({"enabled":false}))).await?;
    server.abort();Ok(())
}
