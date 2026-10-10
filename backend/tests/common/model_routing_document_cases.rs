// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
use serde_json::{Value,json};
use uuid::Uuid;
use fosnie_backend::{auth::{AuthContext,PlatformRole,rbac::{FlatRbacPolicy,ResourceType,Permission}},
    ext::RbacPolicy,model_routing::*,config::BootConfig,state::AppStateBuilder};

struct SourceAcl { mode:Arc<AtomicUsize>,doc:Uuid }
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
        match self.mode.load(Ordering::SeqCst) {
            7=>Ok(vec![self.doc]),8=>Err(fosnie_backend::AppError::Unavailable("ACL unavailable".into())),_=>Ok(vec![]),
        }
    }
}

pub async fn check_document(pool:&sqlx::PgPool,admin:&AuthContext,policy_id:Uuid)->anyhow::Result<()> {
    let count=Arc::new(AtomicUsize::new(0));let mode=Arc::new(AtomicUsize::new(0));let entered=Arc::new(tokio::sync::Notify::new());
    let project=Uuid::now_v7();let kb=Uuid::now_v7();let doc=Uuid::now_v7();
    let app=axum::Router::new().route("/read-document",axum::routing::post({
        let count=count.clone();let mode=mode.clone();let entered=entered.clone();let pool=pool.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let mode=mode.clone();let entered=entered.clone();let pool=pool.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);let case=mode.load(Ordering::SeqCst);
            assert_eq!(body["document_limits"]["max_sections"],32);
            assert_eq!(body["document_limits"]["max_file_bytes"],4*1024*1024);
            assert!(body["path"].as_str().unwrap().ends_with("routed-doc.pdf"));
            let route=&body["overrides"]["routing"];
            assert_eq!(route["purpose"],"document");
            assert_eq!(route["fixed_budget"]["allowances"]["ocr"]["max_calls"],1);
            assert_eq!(route["budget"]["allowances"]["document"]["max_calls"],32);
            if matches!(case,12|13) {entered.notify_one();tokio::time::sleep(std::time::Duration::from_secs(5)).await;}
            if case==9 {sqlx::query("UPDATE knowledge_bases SET archived_at=now() WHERE id=$1").bind(kb).execute(&pool).await.unwrap();}
            let mut result=json!({"mode":"map_reduce","text":"document evidence","truncated":false,"routing_report":super::routing_mock_report(&body)});
            let target=&route["fixed_assignments"]["ocr"];
            result["fixed_routing_report"]=json!({"lease_id":route["fixed_budget"]["id"],"complete":true,"calls":[{
                "call_id":Uuid::now_v7(),"lease_id":route["fixed_budget"]["id"],"decision_id":route["decision_id"],"role":"ocr",
                "provider_id":target["provider_id"],"provider_revision":target["provider_revision"],"profile_revision":target["profile_revision"],
                "status":"completed","latency_ms":1,"input_bytes":20,"output_bytes":10}]});
            if case==0 {result["mode"]=json!("stuff");result["routing_report"]["calls"]=json!([]);result["fixed_routing_report"]["calls"]=json!([]);}
            if case==2 {result.as_object_mut().unwrap().remove("fixed_routing_report");}
            if case==3 {result.as_object_mut().unwrap().remove("truncated");}
            if case==15 {result["text"]=json!("x".repeat(16001));}
            axum::Json(result)
        }}
    }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let addr=listener.local_addr()?;
    let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
    let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{addr}");
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
        .with_rbac(Arc::new(SourceAcl {mode:mode.clone(),doc})).build();
    let mut actor=admin.clone();actor.role=PlatformRole::User;
    sqlx::query("INSERT INTO projects(id,name,owner_user_id) VALUES($1,'Routed documents',$2)").bind(project).bind(actor.user_id).execute(pool).await?;
    sqlx::query("INSERT INTO knowledge_bases(id,name,owner_id,embedding_model_id,embedding_dimension,status) VALUES($1,'Document library',$2,'routed-embed',384,'ready')")
        .bind(kb).bind(actor.user_id).execute(pool).await?;
    sqlx::query("INSERT INTO kb_documents(id,kb_id,original_filename,mime,bytes_path) VALUES($1,$2,'doc.pdf','application/pdf','routed-doc.pdf')")
        .bind(doc).bind(kb).execute(pool).await?;
    let mut policy=store::load_policy(&state,admin,Some(policy_id)).await?;
    for case in 0..16 {
        mode.store(case,Ordering::SeqCst);
        // Plan before linking the source: the executor must authorize live links.
        sqlx::query("DELETE FROM project_kb_links WHERE project_id=$1").bind(project).execute(pool).await?;
        sqlx::query("UPDATE knowledge_bases SET archived_at=NULL WHERE id=$1").bind(kb).execute(pool).await?;
        policy.constraints.max_calls=Some(if case==10 {32} else {200});
        policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Document test".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone()}).await?;
        let chat=Uuid::now_v7();let turn=Uuid::now_v7();
        sqlx::query("INSERT INTO chats(id,owner_user_id,project_id,title) VALUES($1,$2,$3,'Read document')")
            .bind(chat).bind(actor.user_id).bind(project).execute(pool).await?;
        let mut turn_state=state.clone();
        if case==13 {let mut boot=(*state.boot).clone();boot.tool_timeout_secs.insert("document_read".into(),1);turn_state.boot=Arc::new(boot);}
        let roles=if case==14 {vec![]} else {vec![fixed::FixedRole::Ocr]};
        let run=fosnie_backend::chat::routing::prepare(&turn_state,&actor,chat,turn,"read",Some(policy_id),RoutingObjective::Balanced,
            WorkloadRequirements {output_tokens:1024,..Default::default()},&roles,&tokio::sync::Notify::new()).await?;
        if case!=5 {sqlx::query("INSERT INTO project_kb_links(project_id,kb_id) VALUES($1,$2)").bind(project).bind(kb).execute(pool).await?;}
        if case==6 {sqlx::query("UPDATE knowledge_bases SET archived_at=now() WHERE id=$1").bind(kb).execute(pool).await?;}
        if case==11 {run.cancel();}
        let names=if case==4 {vec![]} else {vec!["read_document".into()]};
        let authorised=fosnie_backend::tools::AuthorisedTools::build(&names,&names,false,&Default::default());
        let before=count.load(Ordering::SeqCst);let args=json!({"doc_id":doc,"query":"task"});
        let work=Box::pin(fosnie_backend::tools::routed_document::read(&turn_state,&run,&authorised,&args));
        let result=if case==12 {
            let stop=async {entered.notified().await;run.cancel();};
            tokio::time::timeout(std::time::Duration::from_secs(3),async {let (result,_)=tokio::join!(work,stop);result}).await?
        } else {work.await};
        assert_eq!(result.is_ok(),case<=1,"document {case}: {:?}",result.as_ref().err());
        let sent=matches!(case,0|1|2|3|9|12|13|15);
        assert_eq!(count.load(Ordering::SeqCst)-before,usize::from(sent),"document {case}");
        assert_eq!(run.usage().calls,if matches!(case,2|12|13) {33} else if sent && case!=0 {2} else {0},"document {case}");
        if case==1 {assert_eq!(result?.text,"document evidence");assert_eq!(run.usage().tokens,140);}
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    sqlx::query("DELETE FROM knowledge_bases WHERE id=$1").bind(kb).execute(pool).await?;
    sqlx::query("DELETE FROM projects WHERE id=$1").bind(project).execute(pool).await?;
    server.abort();Ok(())
}
