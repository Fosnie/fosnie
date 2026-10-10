// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}};
use serde_json::{Value,json};
use uuid::Uuid;
use fosnie_backend::{auth::{AuthContext,PlatformRole,rbac::{FlatRbacPolicy,ResourceType,Permission}},
    ext::RbacPolicy,model_routing::*,config::BootConfig,state::AppStateBuilder};

struct SourceAcl { denied:Arc<AtomicBool>,doc:Uuid }
#[async_trait::async_trait]
impl RbacPolicy for SourceAcl {
    async fn can(&self,pool:&sqlx::PgPool,ctx:&AuthContext,kind:ResourceType,id:Uuid,permission:Permission)->fosnie_backend::Result<bool> {
        FlatRbacPolicy.can(pool,ctx,kind,id,permission).await
    }
    async fn may_grant(&self,pool:&sqlx::PgPool,ctx:&AuthContext,kind:ResourceType,id:Uuid)->fosnie_backend::Result<bool> {
        FlatRbacPolicy.may_grant(pool,ctx,kind,id).await
    }
    async fn denied_kb_doc_ids(&self,_:&sqlx::PgPool,ctx:&AuthContext,_:&[Uuid])->fosnie_backend::Result<Vec<Uuid>> {
        assert_eq!(ctx.role,PlatformRole::User);
        Ok(if self.denied.load(Ordering::SeqCst) {vec![self.doc]} else {vec![]})
    }
}

fn reports(body:&Value,purposes:&[&str],roles:&[&str])->Value {
    let route=&body["overrides"]["routing"];
    let calls:Vec<Value>=purposes.iter().map(|purpose|{
        let mut one=body.clone();one["overrides"]["routing"]["purpose"]=json!(purpose);
        super::routing_mock_report(&one)["calls"][0].clone()
    }).collect();
    let fixed:Vec<Value>=roles.iter().map(|role| {
        let target=&route["fixed_assignments"][role];
        json!({"call_id":Uuid::now_v7(),"lease_id":route["fixed_budget"]["id"],"decision_id":route["decision_id"],"role":role,
            "provider_id":target["provider_id"],"provider_revision":target["provider_revision"],"profile_revision":target["profile_revision"],
            "active_index_revision":target["active_index_revision"],"status":"completed","latency_ms":1,"input_bytes":20,"output_bytes":10})
    }).collect();
    json!({"routing_report":{"lease_id":route["budget"]["id"],"complete":true,"calls":calls},
        "fixed_routing_report":{"lease_id":route["fixed_budget"]["id"],"complete":true,"calls":fixed}})
}

pub async fn check_document_chat(pool:&sqlx::PgPool,admin:&AuthContext,policy_id:Uuid)->anyhow::Result<()> {
    use fosnie_backend::{chat::{run_turn,origin::TurnContext},ws::protocol::ServerFrame};
    let mode=Arc::new(AtomicUsize::new(0));let count=Arc::new(AtomicUsize::new(0));let steps=Arc::new(AtomicUsize::new(0));
    let entered=Arc::new(tokio::sync::Notify::new());let denied=Arc::new(AtomicBool::new(false));
    let agent=Uuid::now_v7();let project=Uuid::now_v7();let kb=Uuid::now_v7();let doc=Uuid::now_v7();let outsider=Uuid::now_v7();
    let app=axum::Router::new().route("/retrieve",axum::routing::post({
        let count=count.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            assert_eq!(body["kb_ids"],json!([kb]));
            let mut result=reports(&body,&["retrieval_decompose","retrieval_grade"],&["embed","rerank"]);
            result["type"]=json!("done");result["citations"]=json!([]);
            result["context"]=json!(if body["max_rounds"]==1 {"Documents:\n[D1] Additional library evidence."} else {"Initial library evidence."});
            format!("{result}\n")
        }}
    })).route("/web_search",axum::routing::post({
        let count=count.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            let mut result=reports(&body,&["web_plan","web_grade"],&["rerank","ocr"]);
            // WebGrade allows at most 80 output tokens; mock usage is 40.
            result["type"]=json!("done");result["digest"]=json!("Web evidence.");
            result["citations"]=json!([{"url":"https://weather.test/report","title":"Weather","domain":"weather.test","quote_text":"Web evidence."}]);
            format!("{result}\n")
        }}
    })).route("/read-document",axum::routing::post({
        let count=count.clone();let mode=mode.clone();let entered=entered.clone();let pool=pool.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let mode=mode.clone();let entered=entered.clone();let pool=pool.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);let case=mode.load(Ordering::SeqCst);
            assert_eq!(body["overrides"]["routing"]["purpose"],"document");
            assert_eq!(body["document_limits"]["max_sections"],32);
            if matches!(case,26|27|28) {entered.notify_one();tokio::time::sleep(std::time::Duration::from_secs(5)).await;}
            if case==23 {sqlx::query("UPDATE knowledge_bases SET archived_at=now() WHERE id=$1").bind(kb).execute(&pool).await.unwrap();}
            let purposes=if case==36 {vec![]} else {vec!["document";if case==25 {32} else {1}]};
            let roles=if case==36 {vec![]} else {vec!["ocr"]};
            let mut result=reports(&body,&purposes,&roles);
            result["mode"]=json!(if case==36 {"stuff"} else {"map_reduce"});
            result["text"]=json!("Document evidence.");result["truncated"]=json!(case==21);
            if case==22 {result.as_object_mut().unwrap().remove("fixed_routing_report");}
            axum::Json(result)
        }}
    })).route("/chat-step",axum::routing::post({
        let count=count.clone();let mode=mode.clone();let steps=steps.clone();let pool=pool.clone();let denied=denied.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let mode=mode.clone();let steps=steps.clone();let pool=pool.clone();let denied=denied.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);let case=mode.load(Ordering::SeqCst);
            assert_eq!(body["overrides"]["routing"]["purpose"],"tool_loop");assert_eq!(body["sampling"]["max_tokens"],512);
            assert!(body["tools"].as_array().unwrap().iter().any(|d|d["function"]["name"]=="read_document"));
            let mut result=json!({"content":"Answer based on the document.","usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)});
            if steps.fetch_add(1,Ordering::SeqCst)==0 {
                if case==24 {denied.store(true,Ordering::SeqCst);}
                if case==30 {sqlx::query("INSERT INTO agent_tools(agent_id,tool_name) VALUES($1,'edit_document')").bind(agent).execute(&pool).await.unwrap();}
                if case==35 {sqlx::query("UPDATE knowledge_bases SET owner_id=$2 WHERE id=$1").bind(kb).bind(outsider).execute(&pool).await.unwrap();}
                let mut calls=vec![json!({"id":"read-1","name":"read_document","arguments":{"doc_id":if case==31 {"invalid".to_string()} else {doc.to_string()},"query":"task"}})];
                if case==25 {calls=(0..4).map(|i|json!({"id":format!("read-{i}"),"name":"read_document","arguments":{"doc_id":doc,"query":format!("task {i}")}})).collect();}
                if case==29 {calls.push(json!({"id":"edit","name":"edit_document","arguments":{}}));}
                if case==32 {
                    calls.insert(0,json!({"id":"web","name":"web_search","arguments":{"query":"weather?"}}));
                    calls.insert(1,json!({"id":"library","name":"search_library","arguments":{"query":"details"}}));
                }
                if case==33 {let mut duplicate=calls[0].clone();duplicate["id"]=json!("read-2");calls.push(duplicate);}
                if case==37 {calls.push(json!({"id":"bad","name":"read_document","arguments":{"doc_id":"invalid"}}));}
                result["tool_calls"]=json!(calls);
            } else {
                let messages=body["messages"].to_string();
                assert!(messages.contains("Document evidence.") && messages.contains("UNTRUSTED") && messages.contains(&doc.to_string()));
                if case==21 {assert!(messages.contains("partial; do not claim exhaustive coverage"));}
                if case==32 {assert!(messages.contains("Web evidence.") && messages.contains("Additional library evidence."));}
                if case==33 {assert!(messages.contains("already performed"));}
            }
            axum::Json(result)
        }}
    })).route("/generate",axum::routing::post({
        let count=count.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            assert_eq!(body["overrides"]["routing"]["purpose"],"final");
            assert!(body["messages"].to_string().contains("Document evidence."));
            format!("{}\n{}\n",json!({"type":"token","delta":"Document answer."}),json!({"type":"done","model":"catalog-model","finish_reason":"stop",
                "usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)}))
        }}
    }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let addr=listener.local_addr()?;
    let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
    let mut boot=BootConfig::default();boot.features.agents_enabled=true;boot.ml.base_url=format!("http://{addr}");
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
        .with_rbac(Arc::new(SourceAcl {denied:denied.clone(),doc})).build();
    let mut actor=admin.clone();actor.role=PlatformRole::User;
    sqlx::query("INSERT INTO users(id,display_name,email,role) VALUES($1,'Other document owner',$2,'user')").bind(outsider).bind(format!("{outsider}@routing.test")).execute(pool).await?;
    sqlx::query("INSERT INTO projects(id,name,owner_user_id) VALUES($1,'Document agent project',$2)").bind(project).bind(actor.user_id).execute(pool).await?;
    sqlx::query("INSERT INTO knowledge_bases(id,name,owner_id,embedding_model_id,embedding_dimension,status) VALUES($1,'Document chat library',$2,'routed-embed',384,'ready')").bind(kb).bind(actor.user_id).execute(pool).await?;
    sqlx::query("INSERT INTO project_kb_links(project_id,kb_id) VALUES($1,$2)").bind(project).bind(kb).execute(pool).await?;
    sqlx::query("INSERT INTO kb_documents(id,kb_id,original_filename,mime,bytes_path) VALUES($1,$2,'doc.pdf','application/pdf','routed-doc.pdf')").bind(doc).bind(kb).execute(pool).await?;
    sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'Routed document agent','Use document evidence','{}',$2,ARRAY['general'])").bind(agent).bind(actor.user_id).execute(pool).await?;
    sqlx::query("INSERT INTO agent_tools(agent_id,tool_name) VALUES($1,'read_document')").bind(agent).execute(pool).await?;
    sqlx::query("INSERT INTO embedding_index(id,embed_model,embed_base_url,dim,collection_name) VALUES(1,'routed-embed','http://embed.test/v1',384,'document_chat_index')").execute(pool).await?;
    let index:i64=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_one(pool).await?;
    let (embed,revision):(Uuid,i64)=sqlx::query_as("SELECT id,routing_revision FROM provider_configs WHERE role='embed' AND scope='deployment' LIMIT 1").fetch_one(pool).await?;
    let profile=fixed::save_profile(&state,admin,embed,fixed::SaveFixedProfile {expected_revision:0,profile:fixed::FixedProfile {
        provider_id:embed,provider_revision:revision,revision:1,role:fixed::FixedRole::Embed,model:"routed-embed".into(),active_index_revision:Some(index),
        desired_index_revision:None,locality:Locality::Local,locality_trusted:true,ocr_pdf_mode:None
    }}).await?;
    let mut policy=store::load_policy(&state,admin,Some(policy_id)).await?;
    for case in 20..38 {
        mode.store(case,Ordering::SeqCst);steps.store(0,Ordering::SeqCst);denied.store(false,Ordering::SeqCst);
        // Discard any unused notification from the timeout fixture.
        let _=tokio::time::timeout(std::time::Duration::from_millis(1),entered.notified()).await;
        sqlx::query("UPDATE knowledge_bases SET archived_at=NULL,owner_id=$2 WHERE id=$1").bind(kb).bind(actor.user_id).execute(pool).await?;
        sqlx::query("DELETE FROM agent_tools WHERE agent_id=$1 AND tool_name<>'read_document'").bind(agent).execute(pool).await?;
        if case==32 {sqlx::query("INSERT INTO agent_tools(agent_id,tool_name) VALUES($1,'web_search')").bind(agent).execute(pool).await?;}
        sqlx::query("UPDATE agents SET params=$2 WHERE id=$1").bind(agent).bind(json!({"max_tokens":512,"max_steps":if case==34 {1} else {8}})).execute(pool).await?;
        for (key,value,kind) in [("rag.model_search_mode",if case==32 {"always"} else {"off"},"string"),("rag.model_search_locus","loop","string"),
            ("rag.synthesis_mode","unified","string"),("rag.rerank_enabled","true","bool"),("integration.web_search.enabled","true","bool")] {
            sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,$2,$3::config_value_type) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value").bind(key).bind(value).bind(kind).execute(pool).await?;
        }
        policy.constraints.max_calls=Some(if case==25 {120} else {200});
        policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Document chat".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone()}).await?;
        let chat=Uuid::now_v7();let turn=Uuid::now_v7();let cancel=Arc::new(tokio::sync::Notify::new());
        sqlx::query("INSERT INTO chats(id,owner_user_id,project_id,agent_id,title) VALUES($1,$2,$3,$4,'Document chat')").bind(chat).bind(actor.user_id).bind(project).bind(agent).execute(pool).await?;
        let mut turn_state=state.clone();
        if case==28 {let mut boot=(*state.boot).clone();boot.tool_timeout_secs.insert("document_read".into(),1);turn_state.boot=Arc::new(boot);}
        let run=fosnie_backend::chat::routing::prepare(&turn_state,&actor,chat,turn,"Read the source",Some(policy_id),RoutingObjective::Balanced,
            WorkloadRequirements {output_tokens:1024,..Default::default()},&[],&cancel).await?;
        assert!(run.plan().fixed_assignments.contains_key(&fixed::FixedRole::Ocr));
        let before=count.load(Ordering::SeqCst);let (tx,mut rx)=tokio::sync::mpsc::channel(128);
        let work=Box::pin(run_turn(&turn_state,TurnContext::web(&actor).with_routing(&run),turn,Some(chat),None,None,"Read the source".into(),vec![],vec![],false,None,None,None,None,&tx,cancel.clone()));
        if matches!(case,26|27) {
            let stop=async {entered.notified().await;if case==26 {cancel.notify_one();} else {
                let id:Uuid=sqlx::query_scalar("SELECT id FROM agent_runs WHERE chat_id=$1 AND turn_id=$2").bind(chat).bind(turn).fetch_one(pool).await.unwrap();
                fosnie_backend::agent::kill(&state,id).await;
            }};
            tokio::time::timeout(std::time::Duration::from_secs(3),async {tokio::join!(work,stop);}).await?;
        } else {work.await;}
        let success=matches!(case,20|21|32|33|34|36);let mut completed=0;let mut errors=vec![];
        while let Ok(frame)=rx.try_recv() {match frame {ServerFrame::ChatCompleted {..}=>completed+=1,ServerFrame::ChatError {message,..}=>errors.push(message),_=>{}}}
        assert_eq!(completed,usize::from(success),"document chat {case}: {errors:?}");assert_eq!(errors.len(),usize::from(!success),"{case}: {errors:?}");
        let http=match case {20|21|33|34|36=>4,32=>6,25=>5,24|29|30|31|35=>2,_=>3};
        assert_eq!(count.load(Ordering::SeqCst)-before,http,"document chat {case}: {errors:?}");
        let calls=match case {20|21|33|34=>8,32=>16,36=>6,25=>104,22|26|27|28=>38,24|29|30|31|35=>5,_=>7};
        assert_eq!(run.usage().calls,calls,"document chat {case}");
        let (id,status,checkpoint):(Uuid,String,Value)=sqlx::query_as("SELECT id,status::text,checkpoint FROM agent_runs WHERE chat_id=$1 AND turn_id=$2").bind(chat).bind(turn).fetch_one(pool).await?;
        assert_eq!(status,if success {"completed"} else if case==27 {"cancelled"} else {"failed"});assert!(!fosnie_backend::agent::alive(&state,id).await);
        assert_eq!(checkpoint["routing_usage"]["calls"],json!(calls));assert_eq!(checkpoint["routing_usage"]["resumable"],false);
        if success || matches!(case,25|37) {
            let messages=checkpoint["messages"].to_string();
            assert!(messages.contains("Document evidence.") && messages.contains(&doc.to_string()) && messages.contains("UNTRUSTED"));
            if case==21 {assert!(messages.contains("partial; do not claim exhaustive coverage"));}
        } else {assert!(!checkpoint.to_string().contains("Document evidence."),"failed/denied reads must not be persisted");}
        if success {
            let actual:Value=sqlx::query_scalar("SELECT routing_decision FROM messages WHERE chat_id=$1 AND role='assistant'").bind(chat).fetch_one(pool).await?;
            assert_eq!(actual["actual"]["model"],"catalog-model");
            if case==32 {let citations:i64=sqlx::query_scalar("SELECT count(*) FROM web_citations c JOIN messages m ON m.id=c.message_id WHERE m.chat_id=$1").bind(chat).fetch_one(pool).await?;assert_eq!(citations,1);}
        } else {
            let closed:bool=sqlx::query_scalar("SELECT interrupted_at IS NOT NULL AND completed_at IS NULL FROM messages WHERE chat_id=$1 AND role='assistant'").bind(chat).fetch_one(pool).await?;assert!(closed);
        }
        sqlx::query("DELETE FROM agent_runs WHERE id=$1").bind(id).execute(pool).await?;
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    for no_project in [false,true] {
        let chat=Uuid::now_v7();let before=count.load(Ordering::SeqCst);
        sqlx::query("INSERT INTO chats(id,owner_user_id,project_id,agent_id,title) VALUES($1,$2,$3,$4,'Blocked document agent')").bind(chat).bind(actor.user_id).bind((!no_project).then_some(project)).bind(agent).execute(pool).await?;
        let mut blocked=state.clone();if !no_project {let mut boot=(*state.boot).clone();boot.features.agents_enabled=false;blocked.boot=Arc::new(boot);}
        assert!(fosnie_backend::chat::routing::prepare(&blocked,&actor,chat,Uuid::now_v7(),"Read",Some(policy_id),RoutingObjective::Balanced,
            WorkloadRequirements {output_tokens:1024,..Default::default()},&[],&tokio::sync::Notify::new()).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst),before);
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    fixed::delete_profile(&state,admin,embed,profile.revision).await?;
    sqlx::query("DELETE FROM embedding_index WHERE id=1").execute(pool).await?;
    sqlx::query("DELETE FROM agents WHERE id=$1").bind(agent).execute(pool).await?;
    sqlx::query("DELETE FROM knowledge_bases WHERE id=$1").bind(kb).execute(pool).await?;
    sqlx::query("DELETE FROM projects WHERE id=$1").bind(project).execute(pool).await?;
    sqlx::query("DELETE FROM users WHERE id=$1").bind(outsider).execute(pool).await?;
    server.abort();Ok(())
}
