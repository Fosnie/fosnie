// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}};
use serde_json::{Value,json};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{self,AuthContext,rbac::{FlatRbacPolicy,ResourceType,Permission}},
    ext::RbacPolicy,config::BootConfig,state::AppStateBuilder,workflows,events::{self,ActorType,NewEvent},http};
struct Features {denied:Arc<AtomicUsize>,owner:Uuid,recipient:Uuid}
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Features {
    async fn enabled_for_user(&self,_:&AppState,user:Option<Uuid>,feature:&str)->bool {
        feature=="workflows" || (feature=="messaging" && match self.denied.load(Ordering::SeqCst) {
            1=>user!=Some(self.owner),2=>user!=Some(self.recipient),_=>true,
        })
    }
}
struct DocAcl {denied:Arc<AtomicBool>,recipient:Uuid,doc:Uuid}
#[async_trait::async_trait]
impl RbacPolicy for DocAcl {
    async fn may_grant(&self,pool:&sqlx::PgPool,actor:&AuthContext,kind:ResourceType,id:Uuid)->fosnie_backend::Result<bool> {
        FlatRbacPolicy.may_grant(pool,actor,kind,id).await
    }
    async fn can(&self,pool:&sqlx::PgPool,actor:&AuthContext,kind:ResourceType,id:Uuid,permission:Permission)->fosnie_backend::Result<bool> {
        FlatRbacPolicy.can(pool,actor,kind,id,permission).await
    }
    async fn denied_kb_doc_ids(&self,_:&sqlx::PgPool,actor:&AuthContext,_:&[Uuid])->fosnie_backend::Result<Vec<Uuid>> {
        Ok(if actor.user_id==Some(self.recipient) && self.denied.load(Ordering::SeqCst) {vec![self.doc]} else {vec![]})
    }
}
async fn grants(pool:&sqlx::PgPool,recipient:Uuid,kb:Uuid,project:Uuid)->anyhow::Result<()> {
    sqlx::query("INSERT INTO access_grants(id,resource_type,resource_id,principal_type,principal_id,permission) VALUES($1,'project',$2,'user',$3,'read') ON CONFLICT DO NOTHING")
        .bind(Uuid::now_v7()).bind(project).bind(recipient).execute(pool).await?;
    sqlx::query("INSERT INTO kb_access_grants(id,kb_id,principal_type,principal_id,permission) VALUES($1,$2,'user',$3,'read') ON CONFLICT DO NOTHING")
        .bind(Uuid::now_v7()).bind(kb).bind(recipient).execute(pool).await?;Ok(())
}
// Synthetic loopback inference and isolated DB only. No live group or recipient.
pub fn check<'a>(pool:&'a sqlx::PgPool,admin:&'a AuthContext,uid:Uuid,kb:Uuid,doc:Uuid,project:Uuid,provider:Uuid)
    ->std::pin::Pin<Box<dyn std::future::Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
    let recipient=Uuid::now_v7();
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Synthetic delivery reader','user')")
        .bind(recipient).bind(format!("{recipient}@delivery.test")).execute(pool).await?;
    let reader=auth::load_context(pool,recipient).await?;
    let mode=Arc::new(AtomicUsize::new(0));let count=Arc::new(AtomicUsize::new(0));
    let denied=Arc::new(AtomicUsize::new(0));let doc_denied=Arc::new(AtomicBool::new(false));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let addr=listener.local_addr()?;
    let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{addr}");
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
        .with_features(Arc::new(Features {denied:denied.clone(),owner:uid,recipient}))
        .with_rbac(Arc::new(DocAcl {denied:doc_denied.clone(),recipient,doc})).build();
    let app=axum::Router::new().route("/generate",axum::routing::post({
        let state=state.clone();let reader=reader.clone();let mode=mode.clone();let count=count.clone();let doc_denied=doc_denied.clone();
        move |axum::Json(body):axum::Json<Value>| {let state=state.clone();let reader=reader.clone();let mode=mode.clone();let count=count.clone();let doc_denied=doc_denied.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            assert!(body["messages"].to_string().contains("Private delivery prompt"));
            let (chat,group):(Uuid,Uuid)=sqlx::query_as("SELECT s.chat_id,s.group_chat_id FROM workflow_output_scopes s JOIN chats c ON c.id=s.chat_id WHERE c.owner_user_id=$1 AND s.delivered_at IS NULL ORDER BY s.chat_id DESC LIMIT 1")
                .bind(uid).fetch_one(&state.pg).await.unwrap();
            // Even a project reader cannot read the chat before delivery.
            assert!(Box::pin(http::export::require_chat_read(&state,&reader,chat)).await.is_err());
            if mode.load(Ordering::SeqCst)==4 {
                sqlx::query("DELETE FROM group_chat_members WHERE group_chat_id=$1 AND user_id=$2").bind(group).bind(recipient).execute(&state.pg).await.unwrap();
            }
            if mode.load(Ordering::SeqCst)==12 {doc_denied.store(true,Ordering::SeqCst);}
            format!("{}\n{}\n",json!({"type":"token","delta":"Private delivery answer."}),json!({"type":"done","model":"catalog-model","finish_reason":"stop",
                "usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)}))
        }}
    })).route("/retrieve",axum::routing::post({
        let count=count.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);assert_eq!(body["kb_ids"],json!([kb]));
            let mut result=super::workflow_agent_cases::reports(&body,&["retrieval_decompose","retrieval_grade"],&["embed"]);
            result["type"]=json!("done");result["context"]=json!("Private delivery evidence.");result["citations"]=json!([]);format!("{result}\n")
        }}
    }));
    let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
    sqlx::query("INSERT INTO project_kb_links(project_id,kb_id,attached_by) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(project).bind(kb).bind(uid).execute(pool).await?;
    for case in 0..16 {
        mode.store(case,Ordering::SeqCst);denied.store(0,Ordering::SeqCst);doc_denied.store(false,Ordering::SeqCst);
        grants(pool,recipient,kb,project).await?;
        let group=Uuid::now_v7();let workflow=Uuid::now_v7();let before=count.load(Ordering::SeqCst);
        sqlx::query("INSERT INTO group_chats(id,kind,name,created_by) VALUES($1,'group','Synthetic workflow delivery',$2)").bind(group).bind(uid).execute(pool).await?;
        sqlx::query("INSERT INTO group_chat_members(group_chat_id,user_id,role) VALUES($1,$2,'owner'),($1,$3,'member')").bind(group).bind(uid).bind(recipient).execute(pool).await?;
        if case==14 {sqlx::query("DELETE FROM group_chat_members WHERE group_chat_id=$1 AND user_id=$2").bind(group).bind(uid).execute(pool).await?;}
        if case==15 {sqlx::query("DELETE FROM group_chat_members WHERE group_chat_id=$1").bind(group).execute(pool).await?;}
        let selection=match case {1=>json!({"mode":"manual","provider_id":provider}),2=>json!({"mode":"auto","objective":"balanced"}),_=>json!({"mode":"default"})};
        sqlx::query("INSERT INTO workflows(id,name,owner_id,project_id,enabled,trigger_event_type,action_type,action_config) VALUES($1,'Delivery workflow',$2,$3,true,'routing.delivery.test','agent_run',$4)")
            .bind(workflow).bind(uid).bind(project).bind(json!({"prompt":"Private delivery prompt","model_selection":selection,"deliver_group_chat_id":group})).execute(pool).await?;
        let mut tx=pool.begin().await?;events::emit_with(&mut tx,&NewEvent::new("routing.delivery.test",ActorType::Human).actor(Some(uid)).project(Some(project))).await?;tx.commit().await?;
        assert_eq!(workflows::dispatch_once(&state).await?,1,"delivery capture {case}");
        let id:Uuid=sqlx::query_scalar("SELECT id FROM workflow_runs WHERE workflow_id=$1").bind(workflow).fetch_one(pool).await?;
        if case==3 {sqlx::query("DELETE FROM group_chat_members WHERE group_chat_id=$1 AND user_id=$2").bind(group).bind(recipient).execute(pool).await?;}
        if case==5 {denied.store(1,Ordering::SeqCst);}
        if case==6 {denied.store(2,Ordering::SeqCst);}
        if case==7 {sqlx::query("UPDATE users SET deactivated_at=now() WHERE id=$1").bind(recipient).execute(pool).await?;}
        if case==8 {sqlx::query("DELETE FROM access_grants WHERE resource_id=$1 AND principal_id=$2").bind(project).bind(recipient).execute(pool).await?;}
        if case==9 {sqlx::query("DELETE FROM kb_access_grants WHERE kb_id=$1 AND principal_id=$2").bind(kb).bind(recipient).execute(pool).await?;}
        if case==10 {doc_denied.store(true,Ordering::SeqCst);}
        if case==11 {sqlx::query("ALTER TABLE audit_events ADD CONSTRAINT delivery_audit_test CHECK(action_type<>'routing.workflow.succeeded') NOT VALID").execute(pool).await?;}
        if case==13 {sqlx::query("DELETE FROM group_chats WHERE id=$1").bind(group).execute(pool).await?;}
        let task_state=state.clone();let result=tokio::spawn(async move {Box::pin(workflows::run(&task_state,id)).await}).await?;
        if case==11 {sqlx::query("ALTER TABLE audit_events DROP CONSTRAINT delivery_audit_test").execute(pool).await?;}
        if case==7 {sqlx::query("UPDATE users SET deactivated_at=NULL WHERE id=$1").bind(recipient).execute(pool).await?;}
        let (status,outcome,error):(String,Option<Value>,Option<String>)=sqlx::query_as("SELECT status::text,outcome,error FROM workflow_runs WHERE id=$1").bind(id).fetch_one(pool).await?;
        let success=case<3;
        assert_eq!(status,if success {"succeeded"} else {"failed"},"delivery {case}: {result:?} {error:?}");
        assert_eq!(count.load(Ordering::SeqCst)-before,if success || matches!(case,4|11|12) {2} else {0},"delivery {case}: {error:?}");
        let messages:i64=sqlx::query_scalar("SELECT count(*) FROM group_chat_messages WHERE group_chat_id=$1").bind(group).fetch_one(pool).await?;
        assert_eq!(messages,i64::from(success),"delivery {case}");
        let shares:i64=sqlx::query_scalar("SELECT count(*) FROM chat_shares WHERE group_chat_id=$1").bind(group).fetch_one(pool).await?;
        assert_eq!(shares,i64::from(success),"delivery {case}");
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM workflow_output_scopes WHERE run_id=$1 AND delivered_at IS NOT NULL").bind(id).fetch_one(pool).await?,i64::from(success));
        if success {
            let output=outcome.unwrap();assert_eq!(output["usage"]["calls"],4);
            let chat:Uuid=serde_json::from_value(output["output_chat_id"].clone())?;
            let (message,content,shared):(Uuid,String,Value)=sqlx::query_as("SELECT id,content,shared_resources FROM group_chat_messages WHERE group_chat_id=$1").bind(group).fetch_one(pool).await?;
            assert_eq!(output["delivery_message_id"],json!(message));assert_eq!(content,"Workflow completed — open the output.");assert_eq!(shared,json!({"chat_id":chat}));
            Box::pin(http::export::require_chat_read(&state,&reader,chat)).await?;
            if case==0 {check_reads(&state,admin,&reader,uid,workflow,group,chat,kb,project,&denied,&doc_denied).await?;}
        } else if matches!(case,11|12) {
            let chat:Uuid=sqlx::query_scalar("SELECT chat_id FROM workflow_output_scopes WHERE run_id=$1").bind(id).fetch_one(pool).await?;
            assert!(Box::pin(http::export::require_chat_read(&state,&reader,chat)).await.is_err());
            let owner=auth::load_context(pool,uid).await?;Box::pin(http::export::require_chat_read(&state,&owner,chat)).await?;
            assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM messages WHERE chat_id=$1 AND role='assistant' AND completed_at IS NOT NULL").bind(chat).fetch_one(pool).await?,1);
        }
        let calls=count.load(Ordering::SeqCst);
        if case!=0 {Box::pin(workflows::run(&state,id)).await?;}
        assert_eq!(count.load(Ordering::SeqCst),calls);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM group_chat_messages WHERE group_chat_id=$1").bind(group).fetch_one(pool).await?,if case==0 {0} else {messages});
        sqlx::query("UPDATE workflows SET enabled=false WHERE id=$1").bind(workflow).execute(pool).await?;
    }
    server.abort();Ok(())
    })
}
fn check_reads<'a>(state:&'a AppState,admin:&'a AuthContext,reader:&'a AuthContext,uid:Uuid,workflow:Uuid,group:Uuid,chat:Uuid,kb:Uuid,project:Uuid,
    denied:&'a AtomicUsize,doc_denied:&'a AtomicBool)->std::pin::Pin<Box<dyn std::future::Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
    let pool=&state.pg;let recipient=reader.user_id.unwrap();
    let export=http::export::create_export(axum::extract::State(state.clone()),auth::keycloak::AuthUser(reader.clone()),
        axum::Json(http::export::CreateExport {kind:"chat".into(),target_id:Some(chat),format:Some("json".into())})).await?.0;
    let export_id:Uuid=serde_json::from_value(export["id"].clone())?;
    doc_denied.store(true,Ordering::SeqCst);
    assert!(Box::pin(http::export::run_export(state,export_id)).await.is_err());
    assert_eq!(sqlx::query_scalar::<_,String>("SELECT status::text FROM exports WHERE id=$1").bind(export_id).fetch_one(pool).await?,"failed");
    // Simulate an already built export. Its saved file must not bypass revoked
    // source access (Forbidden must precede even attempting to open disk_path).
    sqlx::query("UPDATE exports SET status='ready' WHERE id=$1").bind(export_id).execute(pool).await?;
    let download=Box::pin(http::export::download_export(axum::extract::State(state.clone()),auth::keycloak::AuthUser(reader.clone()),axum::extract::Path(export_id))).await;
    assert!(matches!(download,Err(fosnie_backend::AppError::Forbidden(_))));
    let history=Box::pin(http::chats::list_messages(axum::extract::State(state.clone()),auth::keycloak::AuthUser(reader.clone()),axum::extract::Path(chat))).await;
    assert!(matches!(history,Err(fosnie_backend::AppError::Forbidden(_))));
    doc_denied.store(false,Ordering::SeqCst);
    let private_message=Uuid::now_v7();
    sqlx::query("INSERT INTO messages(id,chat_id,role,sequence_number,content) SELECT $1,$2,'user',COALESCE(MAX(sequence_number),0)+1,'Later private question' FROM messages WHERE chat_id=$2")
        .bind(private_message).bind(chat).execute(pool).await?;
    assert!(Box::pin(http::export::require_chat_read(state,reader,chat)).await.is_err());
    sqlx::query("DELETE FROM messages WHERE id=$1").bind(private_message).execute(pool).await?;
    Box::pin(http::export::require_chat_read(state,reader,chat)).await?;
    for case in 0..7 {
        match case {
            0=>{doc_denied.store(true,Ordering::SeqCst);},
            1=>{denied.store(2,Ordering::SeqCst);},
            2=>{sqlx::query("DELETE FROM kb_access_grants WHERE kb_id=$1 AND principal_id=$2").bind(kb).bind(recipient).execute(pool).await?;},
            3=>{sqlx::query("DELETE FROM access_grants WHERE resource_id=$1 AND principal_id=$2").bind(project).bind(recipient).execute(pool).await?;},
            4=>{sqlx::query("UPDATE users SET deactivated_at=now() WHERE id=$1").bind(recipient).execute(pool).await?;},
            5=>{sqlx::query("DELETE FROM group_chat_members WHERE group_chat_id=$1 AND user_id=$2").bind(group).bind(recipient).execute(pool).await?;},
            _=>{sqlx::query("DELETE FROM chat_shares WHERE chat_id=$1").bind(chat).execute(pool).await?;},
        }
        assert!(Box::pin(http::export::require_chat_read(state,reader,chat)).await.is_err(),"read revoke {case}");
        doc_denied.store(false,Ordering::SeqCst);denied.store(0,Ordering::SeqCst);grants(pool,recipient,kb,project).await?;
        sqlx::query("UPDATE users SET deactivated_at=NULL WHERE id=$1").bind(recipient).execute(pool).await?;
        sqlx::query("INSERT INTO group_chat_members(group_chat_id,user_id,role) VALUES($1,$2,'member') ON CONFLICT DO NOTHING").bind(group).bind(recipient).execute(pool).await?;
        sqlx::query("INSERT INTO chat_shares(chat_id,group_chat_id,shared_by) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(chat).bind(group).bind(uid).execute(pool).await?;
        Box::pin(http::export::require_chat_read(state,reader,chat)).await?;
    }
    let newcomer=Uuid::now_v7();
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Later group member','user')").bind(newcomer).bind(format!("{newcomer}@delivery.test")).execute(pool).await?;
    grants(pool,newcomer,kb,project).await?;
    sqlx::query("INSERT INTO group_chat_members(group_chat_id,user_id,role) VALUES($1,$2,'member')").bind(group).bind(newcomer).execute(pool).await?;
    let new_reader=auth::load_context(pool,newcomer).await?;
    assert!(Box::pin(http::export::require_chat_read(state,&new_reader,chat)).await.is_err());
    let other=Uuid::now_v7();
    sqlx::query("INSERT INTO group_chats(id,kind,name,created_by) VALUES($1,'group','Other synthetic group',$2)").bind(other).bind(uid).execute(pool).await?;
    sqlx::query("INSERT INTO group_chat_members(group_chat_id,user_id,role) VALUES($1,$2,'owner'),($1,$3,'member')").bind(other).bind(uid).bind(recipient).execute(pool).await?;
    let owner=auth::load_context(pool,uid).await?;
    assert!(Box::pin(http::chats::share_chat(axum::extract::State(state.clone()),auth::keycloak::AuthUser(owner.clone()),axum::extract::Path(chat),axum::Json(http::chats::ShareChat {group_chat_id:other}))).await.is_err());
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM chat_shares WHERE group_chat_id=$1").bind(other).fetch_one(pool).await?,0);
    // Ordinary project chat reads remain unchanged; the guard survives deletion
    // of its workflow and destination, so project access cannot bypass revocation.
    let ordinary=Uuid::now_v7();
    sqlx::query("INSERT INTO chats(id,owner_user_id,project_id,title) VALUES($1,$2,$3,'Ordinary chat')").bind(ordinary).bind(uid).bind(project).execute(pool).await?;
    Box::pin(http::export::require_chat_read(state,reader,ordinary)).await?;
    sqlx::query("DELETE FROM workflows WHERE id=$1").bind(workflow).execute(pool).await?;
    Box::pin(http::export::require_chat_read(state,reader,chat)).await?;
    sqlx::query("DELETE FROM group_chats WHERE id=$1").bind(group).execute(pool).await?;
    assert!(Box::pin(http::export::require_chat_read(state,reader,chat)).await.is_err());
    Box::pin(http::export::require_chat_read(state,&owner,chat)).await?;
    Box::pin(http::export::require_chat_read(state,admin,chat)).await?;
    Ok(())
    })
}
