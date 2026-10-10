// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
use serde_json::{Value,json};
use uuid::Uuid;
use fosnie_backend::{auth::{AuthContext,PlatformRole},model_routing::{*,fixed::FixedRole},
    config::BootConfig,state::AppStateBuilder,research::routed};

pub async fn check_research(pool:&sqlx::PgPool,admin:&AuthContext,policy_id:Uuid)->anyhow::Result<()> {
    let mode=Arc::new(AtomicUsize::new(0));let calls=Arc::new(AtomicUsize::new(0));
    let entered=Arc::new(tokio::sync::Notify::new());
    let root=tempfile::tempdir()?;let path=root.path().join("research.txt");
    tokio::fs::write(&path,b"Evidence.").await?;
    let kb=Uuid::now_v7();let doc=Uuid::now_v7();let chat=Uuid::now_v7();
    let app=axum::Router::new().route("/deep_research",axum::routing::post({
        let mode=mode.clone();let calls=calls.clone();let entered=entered.clone();let pool=pool.clone();let path=path.clone();
        move|axum::Json(body):axum::Json<Value>|{let mode=mode.clone();let calls=calls.clone();let entered=entered.clone();let pool=pool.clone();let path=path.clone();async move {
            calls.fetch_add(1,Ordering::SeqCst);let case=mode.load(Ordering::SeqCst);
            let web=body["source"]!="files";let documents=if body["source"]=="web" {0} else {1};
            assert_eq!(body["research_limits"]["schema_version"],1);
            if documents==1 {assert_eq!(body["docs"][0]["doc_id"],doc.to_string());assert_eq!(body["docs"][0]["sha256"].as_str().unwrap().len(),64);}
            let route=&body["overrides"]["routing"];
            assert_eq!(route["purpose"],"research_plan");
            let sections=body["template_spec"]["skeleton"].as_array().map(|s|s.len()).filter(|n|*n>0).unwrap_or(8);
            let deepen=body["research_limits"]["deepen"]==true;
            assert_eq!(route["budget"]["allowances"]["research_plan"]["max_calls"],documents+1+if web {4} else {0}+if deepen {sections} else {0});
            assert_eq!(route["budget"]["allowances"]["research_write"]["max_calls"],sections+1);
            assert_eq!(route["fixed_budget"]["allowances"]["ocr"]["max_calls"],documents+if web {4} else {0});
            if web {
                assert_eq!(body["max_fetches"],4);assert_eq!(body["domain_allowlist"],"weather.test");
                assert_eq!(route["fixed_budget"]["allowances"]["rerank"]["max_calls"],1);
            }
            if case==4 {tokio::fs::write(&path,b"changed").await.unwrap();}
            if case==5 {sqlx::query("UPDATE knowledge_bases SET archived_at=now() WHERE id=$1").bind(kb).execute(&pool).await.unwrap();}
            if matches!(case,6|26|59) {entered.notify_one();tokio::time::sleep(std::time::Duration::from_secs(5)).await;}
            let mut value=json!({"type":"done","title":"Research","report_md":"## Findings\nEvidence [D1].","citations":[],
                "verification":null,"coverage":{"selected":1,"read":1,"sampled":false},
                "doc_citations":[{"doc_id":doc,"reference":"D1","quote_text":"Evidence."}],
                "routing_report":super::routing_mock_report(&body),
                "fixed_routing_report":{"lease_id":route["fixed_budget"]["id"],"complete":true,"calls":[]}});
            if web {
                let source=json!({"reference":format!("D{}",documents+1),"url":"https://weather.test/report","domain":"weather.test",
                    "title":"Weather","quote_text":"Web evidence.","fetched_at":"2026-09-23T12:00:00Z","published_date":null,"snippet_only":false});
                value["web_sources"]=json!([source.clone()]);value["citations"]=json!([source]);
                value["coverage"]=json!({"selected":documents,"read":documents,"sampled":false});
                value["report_md"]=json!(if documents==0 {"Web evidence [D1]."} else {"Web evidence [D2], file evidence [D1]."});
                if documents==0 {value["doc_citations"]=json!([]);}
                if case==21 {value.as_object_mut().unwrap().remove("fixed_routing_report");}
                if case==22 {value["citations"][0]["url"]=json!("https://forged.test/");}
            }
            if case==1 {value.as_object_mut().unwrap().remove("routing_report");}
            if case==2 {value["doc_citations"][0]["doc_id"]=json!(Uuid::now_v7());}
            if case==3 {value["coverage"]["sampled"]=json!(true);}
            if case==7 {return "{\"type\":\"progress\"}\n".to_string();}
            if body["verify"]==true {
                assert_eq!(route["fixed_budget"]["allowances"]["verify"]["max_calls"],1);
                let target=&route["fixed_assignments"]["verify"];
                let flagged=case==51;
                if flagged {value["title"]=json!("Обзор 🧭");}
                let text=value["report_md"].as_str().unwrap();
                value["verification"]=json!({"model":target["model"],"score":if flagged {0.0} else {1.0},"total":1,"flagged":usize::from(flagged),
                    "spans":if flagged {json!([{"start":0,"end":text.chars().count(),"text":text,"label":"not_mentioned","score":0.9}])} else {json!([])}});
                value["fixed_routing_report"]["calls"]=json!([{"call_id":Uuid::now_v7(),"lease_id":route["fixed_budget"]["id"],
                    "decision_id":route["decision_id"],"role":"verify","provider_id":target["provider_id"],"provider_revision":target["provider_revision"],
                    "profile_revision":target["profile_revision"],"active_index_revision":null,"status":"completed","latency_ms":1,"input_bytes":100,"output_bytes":100}]);
                if case==52 {value["verification"]=Value::Null;}
                if case==53 {value["verification"]["spans"]=json!([{"start":0,"end":99999,"text":"bad","label":"not_mentioned","score":0.5}]);value["verification"]["flagged"]=json!(1);}
                if case==54 {value["verification"]["model"]=json!("forged-verifier");}
                if case==55 {value["fixed_routing_report"]["calls"]=json!([]);}
            }
            if deepen {
                value["deepening"]=json!({"method":"existing_sources_v1","sections":1,"sections_with_gaps":1});
                let original=value["routing_report"]["calls"][0].clone();
                for _ in 0..documents+usize::from(web)+1 {
                    let mut call=original.clone();call["call_id"]=json!(Uuid::now_v7());
                    value["routing_report"]["calls"].as_array_mut().unwrap().push(call);
                }
                if case==57 {value.as_object_mut().unwrap().remove("deepening");}
                if case==58 {value["deepening"]["sections"]=json!(2);}
            }
            format!("{value}\n")
        }}
    })).route("/research/routed-triage",axum::routing::post({
        let mode=mode.clone();let calls=calls.clone();let entered=entered.clone();
        move|axum::Json(body):axum::Json<Value>| {let mode=mode.clone();let calls=calls.clone();let entered=entered.clone();async move {
            calls.fetch_add(1,Ordering::SeqCst);let case=mode.load(Ordering::SeqCst);
            assert_eq!(body["overrides"]["routing"]["purpose"],"research_plan");
            assert_eq!(body["overrides"]["routing"]["budget"]["allowances"].as_object().unwrap().len(),1);
            assert_eq!(body["overrides"]["routing"]["budget"]["allowances"]["research_plan"]["max_calls"],1);
            assert!(body.get("docs").is_none());assert!(body.get("scope").is_none());
            if case==44 {entered.notify_one();tokio::time::sleep(std::time::Duration::from_secs(5)).await;}
            let mut response=json!({"questions":[{"prompt":"Which timeframe?","options":["This year","Last year"]}],
                "routing_report":super::routing_mock_report(&body)});
            if case==41 {response["questions"]=json!([]);}
            if case==42 {response["questions"][0]["scope_indices"]=json!([99]);}
            if case==43 {response.as_object_mut().unwrap().remove("routing_report");}
            axum::Json(response)
        }}
    }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let addr=listener.local_addr()?;
    let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
    let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{addr}");boot.storage.documents_dir=root.path().to_string_lossy().into();
    boot.storage.artefacts_dir=root.path().join("artefacts").to_string_lossy().into();
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot)).build();
    let mut actor=admin.clone();actor.role=PlatformRole::User;
    sqlx::query("INSERT INTO knowledge_bases(id,name,owner_id,embedding_model_id,embedding_dimension,status) VALUES($1,'Research',$2,'routed-embed',384,'ready')")
        .bind(kb).bind(actor.user_id).execute(pool).await?;
    sqlx::query("INSERT INTO kb_documents(id,kb_id,original_filename,mime,bytes_path,ingest_status) VALUES($1,$2,'research.txt','text/plain','research.txt','ready')")
        .bind(doc).bind(kb).execute(pool).await?;
    sqlx::query("INSERT INTO chats(id,owner_user_id,title,mode) VALUES($1,$2,'Research','research')").bind(chat).bind(actor.user_id).execute(pool).await?;
    let mut policy=store::load_policy(&state,admin,Some(policy_id)).await?;
    policy.constraints=RoutingConstraints {max_calls:Some(100),..Default::default()};
    policy=store::save_policy(&state,admin,policy_id,store::SavePolicy {label:"Research".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy}).await?;
    for case in 0..11 {
        mode.store(case,Ordering::SeqCst);tokio::fs::write(&path,b"Evidence.").await?;
        sqlx::query("UPDATE knowledge_bases SET archived_at=NULL WHERE id=$1").bind(kb).execute(pool).await?;
        let context=RoutingContext {actor:actor.clone(),chat_id:Some(chat),project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::ResearchPlan};
        let run=run::RoutedRun::auto(&state,context,Some(policy.id),RoutingObjective::Balanced,
            WorkloadRequirements {output_tokens:1024,..Default::default()},"Files research",&[FixedRole::Ocr],&tokio::sync::Notify::new()).await?;
        let req=routed::Request {question:"Question".into(),source:"files".into(),kb_ids:vec![if case==8 {Uuid::now_v7()} else {kb}],
            template:if case==9 {"unsupported"} else {"freeform"}.into(),refinements:vec![],verify:false,deepen:false};
        if case==10 {run.cancel();}
        let before=calls.load(Ordering::SeqCst);
        let work=Box::pin(routed::execute(&state,&run,&req));
        let result=if case==6 {let (r,_)=tokio::join!(work,async {entered.notified().await;run.cancel();});r} else {work.await};
        assert_eq!(result.is_ok(),case==0,"research case {case}: {:?}",result.as_ref().err());
        assert_eq!(calls.load(Ordering::SeqCst)-before,usize::from(case<8),"research dispatch {case}");
        if case==0 {assert_eq!(run.usage().calls,1);}
        if matches!(case,1|6|7) {assert_eq!(run.usage().calls,22,"uncertain research must retain its full reservation");}
    }
    Box::pin(super::research_job_cases::check_jobs(&state,admin,&actor,kb,&path,&mode,&calls,&entered)).await?;
    sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    sqlx::query("DELETE FROM knowledge_bases WHERE id=$1").bind(kb).execute(pool).await?;
    server.abort();Ok(())
}
