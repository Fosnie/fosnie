// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
use serde_json::{json,Value};
use anyhow::Context;
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,PlatformRole},config::BootConfig,state::AppStateBuilder,
    model_routing::{*,fixed::{self,FixedRole}},groundedness::routed_draft};
struct Features;
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Features {
    async fn enabled_for_user(&self,_:&AppState,_:Option<Uuid>,f:&str)->bool {f=="groundedness"}
}
fn reports(body:&Value,repair:bool,cut:bool)->Value {
    let r=&body["overrides"]["routing"];let roles=if cut {vec!["embed"]} else {vec!["embed","rerank","verify_claims"]};
    let calls:Vec<_>=roles.iter().map(|role|{let t=&r["fixed_assignments"][role];json!({
        "call_id":Uuid::now_v7(),"lease_id":r["fixed_budget"]["id"],"decision_id":r["decision_id"],"role":role,
        "provider_id":t["provider_id"],"provider_revision":t["provider_revision"],"profile_revision":t["profile_revision"],
        "active_index_revision":t["active_index_revision"],"status":"completed","latency_ms":1,"input_bytes":100,"output_bytes":100})}).collect();
    let mut llm=super::routing_mock_report(body);
    if repair && cut {llm["calls"]=json!([]);}
    json!({"routing_report":llm,"fixed_routing_report":{"lease_id":r["fixed_budget"]["id"],"complete":true,"calls":calls}})
}
pub async fn check_draft(pool:&sqlx::PgPool,admin:&AuthContext,policy_id:Uuid)->anyhow::Result<()> {
    let mode=Arc::new(AtomicUsize::new(0));let calls=Arc::new(AtomicUsize::new(0));let entered=Arc::new(tokio::sync::Notify::new());
    let root=tempfile::tempdir()?;let kb=Uuid::now_v7();let evidence=Uuid::now_v7();let project=Uuid::now_v7();
    let mut actor=admin.clone();actor.role=PlatformRole::User;
    sqlx::query("INSERT INTO projects(id,name,owner_user_id) VALUES($1,'Drafts',$2)").bind(project).bind(actor.user_id).execute(pool).await?;
    sqlx::query("INSERT INTO knowledge_bases(id,name,owner_id,embedding_model_id,embedding_dimension,status) VALUES($1,'Evidence',$2,'routed-embed',384,'ready')").bind(kb).bind(actor.user_id).execute(pool).await?;
    sqlx::query("INSERT INTO project_kb_links(project_id,kb_id) VALUES($1,$2)").bind(project).bind(kb).execute(pool).await?;
    sqlx::query("INSERT INTO kb_documents(id,kb_id,original_filename,bytes_path) VALUES($1,$2,'evidence.txt','evidence.txt')").bind(evidence).bind(kb).execute(pool).await?;
    let app=axum::Router::new().route("/verify-draft",axum::routing::post({
        let mode=mode.clone();let calls=calls.clone();let entered=entered.clone();let pool=pool.clone();
        move|axum::Json(body):axum::Json<Value>|{let mode=mode.clone();let calls=calls.clone();let entered=entered.clone();let pool=pool.clone();async move {
            calls.fetch_add(1,Ordering::SeqCst);let case=mode.load(Ordering::SeqCst);
            assert_eq!(body["kb_ids"],json!([kb]));assert_eq!(body["deny_doc_ids"],json!([]));
            if case==4 {tokio::fs::write(body["path"].as_str().unwrap(),b"edited").await.unwrap();}
            if case==10 {entered.notify_one();tokio::time::sleep(std::time::Duration::from_secs(5)).await;}
            if case>=20 {
                let reserved:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM routed_jobs WHERE status='running' AND (usage->>'calls')::bigint>0)").fetch_one(&pool).await.unwrap();
                assert!(reserved,"durable reservations must commit before HTTP");
            }
            if matches!(case,22|23) {entered.notify_one();tokio::time::sleep(std::time::Duration::from_millis(if case==22 {100} else {5000})).await;}
            let mut out=reports(&body,false,false);
            out.as_object_mut().unwrap().extend(json!({"claims":[{"text":"Wrong.","verdict":"contradicted","score":0.9,
                "evidence":"Right.","evidence_doc_ids":[evidence],"section":"section 1","had_citation":true,
                "source_span":{"start":0,"end":6,"text":"Wrong."}}],"document_text":"Wrong.",
                "model":body["overrides"]["routing"]["fixed_assignments"]["verify_claims"]["model"],
                "total":1,"supported":0,"contradicted":1,"not_mentioned":0,"score":0.0}).as_object().unwrap().clone());
            if case==1 {out.as_object_mut().unwrap().remove("fixed_routing_report");}
            if case==2 {out["score"]=json!(1);}
            axum::Json(out)
        }}
    })).route("/repair-draft",axum::routing::post({
        let mode=mode.clone();let calls=calls.clone();let pool=pool.clone();
        move|axum::Json(body):axum::Json<Value>|{let mode=mode.clone();let calls=calls.clone();let pool=pool.clone();async move {
            calls.fetch_add(1,Ordering::SeqCst);let case=mode.load(Ordering::SeqCst);
            let cut=case==12;let mut out=reports(&body,true,cut);
            out["results"]=json!([{"source_text":"Wrong.","claim_text":"Wrong.","action":if cut {"cut"} else {"regenerated"},
                "replacement":if cut {None} else {Some("Right.")},"evidence":if cut {""} else {"Right."},
                "evidence_doc_ids":if cut {vec![]} else {vec![evidence]},"reverify_verdict":if cut {"not_mentioned"} else {"supported"},"reverify_score":0.9}]);
            if case==6 {out.as_object_mut().unwrap().remove("routing_report");}
            if case==7 {out["results"][0]["reverify_verdict"]=json!("contradicted");}
            if case==11 {out["fixed_routing_report"]["calls"]=json!([]);}
            if case==13 {sqlx::query("UPDATE config_settings SET value='false' WHERE key='groundedness.repair'").execute(&pool).await.unwrap();}
            axum::Json(out)
        }}
    })).route("/apply-tracked-changes",axum::routing::post({let mode=mode.clone();let pool=pool.clone();
        move|axum::Json(body):axum::Json<Value>|{let mode=mode.clone();let pool=pool.clone();async move {
            tokio::fs::write(body["out_path"].as_str().unwrap(),b"tracked DOCX fixture").await.unwrap();
            if mode.load(Ordering::SeqCst)==8 {tokio::fs::write(body["path"].as_str().unwrap(),b"edited").await.unwrap();}
            if mode.load(Ordering::SeqCst)==30 {sqlx::query("UPDATE routed_jobs SET status='cancelled',owner=NULL,lease_until=NULL WHERE status='running'").execute(&pool).await.unwrap();}
            if mode.load(Ordering::SeqCst)==41 {sqlx::query("UPDATE config_settings SET value='false' WHERE key='routing.document_jobs_enabled'").execute(&pool).await.unwrap();}
            axum::Json(json!({"changes":[{"w_id":"1","find":body["edits"][0]["find"],"replace":body["edits"][0]["replace"]}],
                "errors":if mode.load(Ordering::SeqCst)==15 {json!([{"index":0,"reason":"ambiguous"}])} else {json!([])}}))
        }}
    }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
    let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
    let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{address}");boot.storage.workspace_dir=root.path().to_string_lossy().into();
    boot.storage.artefacts_dir=root.path().to_string_lossy().into();
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot)).with_features(Arc::new(Features)).build();
    let catalog=fixed::catalog(&state,admin).await?;
    // The chat fixture deliberately removes its index and attestation on exit.
    sqlx::query("INSERT INTO embedding_index(id,embed_model,embed_base_url,dim,collection_name) VALUES(1,'routed-embed','http://embed.test/v1',384,'draft_index')").execute(pool).await?;
    let index:i64=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_one(pool).await?;
    let embed=catalog.iter().find(|p|p.role==FixedRole::Embed).context("draft embedding provider")?;
    fixed::save_profile(&state,admin,embed.id,fixed::SaveFixedProfile {expected_revision:embed.profile_revision.unwrap_or(0),profile:fixed::FixedProfile {
        provider_id:embed.id,provider_revision:embed.provider_revision,revision:1,role:FixedRole::Embed,model:"routed-embed".into(),
        active_index_revision:Some(index),desired_index_revision:None,locality:Locality::Local,locality_trusted:true,ocr_pdf_mode:None
    }}).await.context("draft embedding attestation")?;
    let mut claims:fixed::FixedProfile=serde_json::from_value(catalog.iter().find(|p|p.role==FixedRole::Verify).unwrap().profile.clone().unwrap())?;
    claims.role=FixedRole::VerifyClaims;
    let revision=catalog.iter().find(|p|p.role==FixedRole::VerifyClaims && p.id==claims.provider_id)
        .and_then(|p|p.profile.as_ref()).and_then(|p|p["revision"].as_i64()).unwrap_or(0);
    fixed::save_profile(&state,admin,claims.provider_id,fixed::SaveFixedProfile {expected_revision:revision,profile:claims}).await?;
    let mut policy=store::load_policy(&state,admin,Some(policy_id)).await?;policy.strategy=RoutingStrategy::Rules;policy.rules.clear();
    policy.constraints=RoutingConstraints {max_calls:Some(200),..Default::default()};
    let policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {expected_revision:policy.revision,label:"Draft".into(),scope:"deployment".into(),is_default:true,policy}).await?;
    for case in 0..16 {
        mode.store(case,Ordering::SeqCst);
        for (k,v) in [("groundedness.strict","true"),("groundedness.threshold","0"),("groundedness.hhem_filter","false"),("groundedness.repair","true")] {
            sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,$2,'string') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value").bind(k).bind(v).execute(pool).await?;
        }
        let (doc,version)=fosnie_backend::documents::create_document(&state,&actor,project,"draft.docx",None,b"Wrong.").await?;
        let context=RoutingContext {actor:actor.clone(),chat_id:None,project_id:Some(project),run_id:Uuid::now_v7(),purpose:CallPurpose::Document};
        let run=run::RoutedRun::auto(&state,context,Some(policy.id),RoutingObjective::Balanced,WorkloadRequirements {output_tokens:1024,..Default::default()},"Verify draft",
            &[FixedRole::Embed,FixedRole::Rerank,FixedRole::Ocr,FixedRole::VerifyClaims],&tokio::sync::Notify::new()).await.with_context(||format!("draft plan case {case}"))?;
        if case==3 {run.cancel();}
        if case==5 {sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT draft_test_audit CHECK(action_type<>'groundedness.verified') NOT VALID").execute(pool).await?;}
        let work=Box::pin(routed_draft::verify_document(&state,&run,doc));
        let checked=if case==10 {let (r,_)=tokio::join!(work,async {entered.notified().await;run.cancel();});r} else {work.await};
        let success=!matches!(case,1|2|3|4|5|10);
        assert_eq!(checked.is_ok(),success,"verify {case}: {:?}",checked.as_ref().err());
        let stored:i64=sqlx::query_scalar("SELECT count(*) FROM verification_runs WHERE target_id=$1").bind(doc).fetch_one(pool).await?;
        assert_eq!(stored,i64::from(success),"atomic verification {case}");
        if case==5 {sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT draft_test_audit").execute(pool).await?;}
        if let Ok(verified)=checked {
            assert!(routed_draft::reject_legacy_resume(&state,verified.id).await.is_err());
            if case==9 {sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT draft_test_audit CHECK(action_type<>'groundedness.repaired') NOT VALID").execute(pool).await?;}
            let result=routed_draft::repair_document(&state,&run,&verified).await;
            let repaired=matches!(case,0|12|14);
            assert_eq!(result.is_ok(),repaired,"repair {case}: {result:?}");
            let current:Uuid=sqlx::query_scalar("SELECT current_version_id FROM documents WHERE id=$1").bind(doc).fetch_one(pool).await?;
            let edits:i64=sqlx::query_scalar("SELECT count(*) FROM document_edits WHERE document_id=$1 AND status='pending'").bind(doc).fetch_one(pool).await?;
            let actions:i64=sqlx::query_scalar("SELECT count(*) FROM claim_verdicts WHERE run_id=$1 AND repair_action IS NOT NULL").bind(verified.id).fetch_one(pool).await?;
            let audits:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE resource_id=$1 AND action_type='groundedness.repaired'").bind(verified.id).fetch_one(pool).await?;
            assert_eq!((current!=version,edits,actions,audits),(repaired,i64::from(repaired),i64::from(repaired),i64::from(repaired)),"atomic repair {case}");
            if case==14 {let before=calls.load(Ordering::SeqCst);assert!(routed_draft::repair_document(&state,&run,&verified).await.is_err());assert_eq!(calls.load(Ordering::SeqCst),before);}
            if case==9 {sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT draft_test_audit").execute(pool).await?;}
        }
    }
    // Real durable handoff/worker: one stored ledger, no repeat on duplicate,
    // uncertain crash, cancellation, changed authority or commit fencing.
    use fosnie_backend::groundedness::durable;
    sqlx::query("UPDATE config_settings SET value='true' WHERE key='groundedness.repair'").execute(pool).await?;
    for case in 20..=31 {
        mode.store(0,Ordering::SeqCst);
        let (doc,version)=fosnie_backend::documents::create_document(&state,&actor,project,"queued.docx",None,b"Wrong.").await?;
        let context=RoutingContext {actor:actor.clone(),chat_id:None,project_id:Some(project),run_id:Uuid::now_v7(),purpose:CallPurpose::Document};
        let run=run::RoutedRun::auto(&state,context,Some(policy.id),RoutingObjective::Balanced,WorkloadRequirements {output_tokens:1024,..Default::default()},"Verify draft",
            &[FixedRole::Embed,FixedRole::Rerank,FixedRole::Ocr,FixedRole::VerifyClaims],&tokio::sync::Notify::new()).await?;
        if case==20 {
            assert!(durable::enqueue(&state,run.clone(),doc,true).await.is_err(),"clones cannot retain a second budget owner");
            routed_draft::verify_document(&state,&run,doc).await?;
            assert_eq!(run.usage().calls,4);
        }
        if case==28 {sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT draft_queue_audit CHECK(action_type<>'routing.job.queued') NOT VALID").execute(pool).await?;}
        let id=run.plan().id;
        let enqueued=durable::enqueue(&state,run,doc,true).await;
        if case==28 {
            assert!(enqueued.is_err());
            let n:i64=sqlx::query_scalar("SELECT count(*) FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
            let tasks:i64=sqlx::query_scalar("SELECT count(*) FROM tasks WHERE payload->>'routed_job_id'=$1").bind(id.to_string()).fetch_one(pool).await?;
            assert_eq!((n,tasks),(0,0));
            sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT draft_queue_audit").execute(pool).await?;
            continue;
        }
        enqueued.with_context(||format!("durable enqueue {case}"))?;
        let payload:Value=sqlx::query_scalar("SELECT t.payload FROM tasks t JOIN routed_jobs j ON j.task_id=t.id WHERE j.id=$1").bind(id).fetch_one(pool).await?;
        assert_eq!(payload,json!({"routed_job_id":id}));
        mode.store(case,Ordering::SeqCst);let before=calls.load(Ordering::SeqCst);
        if case==21 {durable::cancel(&state,&actor,id).await?;}
        if case==24 {
            let current=fosnie_backend::documents::current_version(pool,&state.boot.storage.workspace_dir,doc).await?;
            tokio::fs::write(current.bytes_path,b"edited before execution").await?;
        }
        if case==25 {sqlx::query("UPDATE users SET role='power_user' WHERE id=$1").bind(actor.user_id).execute(pool).await?;}
        if case==26 {sqlx::query("UPDATE routed_jobs SET deadline=now()-interval '1 second' WHERE id=$1").bind(id).execute(pool).await?;}
        if case==27 {
            sqlx::query("UPDATE routed_jobs SET status='running',owner=$2,lease_until=now()-interval '1 second',usage=jsonb_set(usage,'{calls}','38') WHERE id=$1")
                .bind(id).bind(Uuid::now_v7()).execute(pool).await?;
        }
        if case==31 {
            sqlx::query("UPDATE tasks SET status='running',started_at=now()-interval '1 minute' WHERE id=(SELECT task_id FROM routed_jobs WHERE id=$1)").bind(id).execute(pool).await?;
            durable::recover(&state).await?;
            let status:String=sqlx::query_scalar("SELECT status::text FROM tasks WHERE id=(SELECT task_id FROM routed_jobs WHERE id=$1)").bind(id).fetch_one(pool).await?;
            assert_eq!(status,"queued","unstarted work can resume without a new run");
        }
        if matches!(case,26|27) {assert_eq!(durable::recover(&state).await?,1);}
        if case==29 {sqlx::query("ALTER TABLE routed_jobs ADD CONSTRAINT draft_checkpoint CHECK((usage->>'calls')::bigint=0) NOT VALID").execute(pool).await?;}
        let result=if matches!(case,22|23) {
            let (result,_)=tokio::join!(Box::pin(durable::handle(&state,id)),async {
                entered.notified().await;
                if case==22 {assert!(durable::handle(&state,id).await.is_err());}
                else {durable::cancel(&state,&actor,id).await.unwrap();}
            });result
        } else {durable::handle(&state,id).await};
        let success=matches!(case,20|22|31);
        if result.is_ok()!=success {
            let diagnostic:Value=sqlx::query_scalar("SELECT jsonb_build_object('status',status,'usage',usage,'owner',owner,'lease_until',lease_until,'deadline',deadline,'now',clock_timestamp(),'actor_role',context->>'role') FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
            panic!("durable {case}: {result:?}; calls={}, checkpoint={diagnostic}",calls.load(Ordering::SeqCst)-before);
        }
        if case==25 {sqlx::query("UPDATE users SET role='user' WHERE id=$1").bind(actor.user_id).execute(pool).await?;}
        if case==29 {sqlx::query("ALTER TABLE routed_jobs DROP CONSTRAINT draft_checkpoint").execute(pool).await?;}
        let (status,usage,result_id):(String,Value,Option<Uuid>)=sqlx::query_as("SELECT status,usage,result_id FROM routed_jobs WHERE id=$1").bind(id).fetch_one(pool).await?;
        let expected=match case {20|22|31=>"succeeded",21|23|30=>"cancelled",27=>"interrupted",_=>"failed"};
        assert_eq!(status,expected,"durable status {case}");
        if success {
            assert_eq!(result_id,Some(id));assert_eq!(usage["calls"],json!(if case==20 {12} else {8}));
            let count=calls.load(Ordering::SeqCst);durable::handle(&state,id).await?;assert_eq!(calls.load(Ordering::SeqCst),count);
        } else if case==23 || case==27 {assert_eq!(usage["calls"],json!(38),"uncertain attempts keep their reservation");}
        if matches!(case,21|24|25|26|27|29) {assert_eq!(calls.load(Ordering::SeqCst),before,"no HTTP {case}");}
        let current:Uuid=sqlx::query_scalar("SELECT current_version_id FROM documents WHERE id=$1").bind(doc).fetch_one(pool).await?;
        assert_eq!(current!=version,success,"durable document commit {case}");
    }
    Box::pin(super::job_http_cases::check_http(&state,admin,&actor,project,kb,policy.id,&mode,&calls)).await?;
    server.abort();Ok(())
}
