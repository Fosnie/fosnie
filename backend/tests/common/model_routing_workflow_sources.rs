// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::atomic::{AtomicBool,AtomicUsize,Ordering};
use serde_json::{Value,json};
use uuid::Uuid;
use fosnie_backend::{AppState,auth::AuthContext,events::{self,ActorType,NewEvent},workflows,
    model_routing::{Locality,RoutingStrategy,store,fixed::{self,FixedProfile,FixedRole,SaveFixedProfile}}};

pub async fn check(state:&AppState,admin:&AuthContext,uid:Uuid,kb:Uuid,doc:Uuid,project:Uuid,
    mode:&AtomicUsize,denied:&AtomicBool,count:&AtomicUsize)->anyhow::Result<()> {
    let pool=&state.pg;
    sqlx::query("INSERT INTO embedding_index(id,embed_model,embed_base_url,dim,collection_name) VALUES(1,'routed-embed','http://embed.test/v1',384,'workflow_sources_index') ON CONFLICT(id) DO UPDATE SET embed_model=EXCLUDED.embed_model,embed_base_url=EXCLUDED.embed_base_url,dim=EXCLUDED.dim,collection_name=EXCLUDED.collection_name").execute(pool).await?;
    let index:i64=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_one(pool).await?;
    let (embed,revision):(Uuid,i64)=sqlx::query_as("SELECT id,routing_revision FROM provider_configs WHERE role='embed' AND scope='deployment' LIMIT 1").fetch_one(pool).await?;
    let expected=sqlx::query_scalar::<_,i64>("SELECT revision FROM fixed_inference_profiles WHERE provider_id=$1 AND slot='embed'").bind(embed).fetch_optional(pool).await?.unwrap_or(0);
    fixed::save_profile(state,admin,embed,SaveFixedProfile {expected_revision:expected,profile:FixedProfile {
        provider_id:embed,provider_revision:revision,revision:1,role:FixedRole::Embed,model:"routed-embed".into(),active_index_revision:Some(index),
        desired_index_revision:None,locality:Locality::Local,locality_trusted:true,ocr_pdf_mode:None
    }}).await?;
    let mut policy=store::load_policy(state,admin,None).await?;
    policy.constraints.max_calls=Some(200);policy.strategy=RoutingStrategy::Rules;policy.rules.clear();
    store::save_policy(state,admin,policy.id,store::SavePolicy {label:"Workflow sources".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy}).await?;
    for (key,value,kind) in [("rag.model_search_mode","off","string"),("rag.synthesis_mode","unified","string"),("rag.rerank_enabled","false","bool")] {
        sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,$2,$3::config_value_type) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,value_type=EXCLUDED.value_type")
            .bind(key).bind(value).bind(kind).execute(pool).await?;
    }
    let other=Uuid::now_v7();
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Other source owner','user')").bind(other).bind(format!("{other}@workflow.test")).execute(pool).await?;
    sqlx::query("INSERT INTO projects(id,name,owner_user_id) VALUES($1,'Workflow sources',$2)").bind(project).bind(uid).execute(pool).await?;
    sqlx::query("INSERT INTO knowledge_bases(id,name,owner_id,embedding_model_id,embedding_dimension,status) VALUES($1,'Workflow sources',$2,'routed-embed',384,'ready')").bind(kb).bind(uid).execute(pool).await?;
    sqlx::query("INSERT INTO kb_documents(id,kb_id,original_filename,mime,bytes_path,ingest_status) VALUES($1,$2,'current-name.txt','text/plain','mock-source.txt','ready')").bind(doc).bind(kb).execute(pool).await?;
    for case in 0..12 {
        mode.store(case,Ordering::SeqCst);denied.store(false,Ordering::SeqCst);
        sqlx::query("UPDATE knowledge_bases SET owner_id=$2,restricted=false WHERE id=$1").bind(kb).bind(uid).execute(pool).await?;
        sqlx::query("UPDATE projects SET owner_user_id=$2 WHERE id=$1").bind(project).bind(uid).execute(pool).await?;
        sqlx::query("UPDATE kb_documents SET original_filename='current-name.txt' WHERE id=$1").bind(doc).execute(pool).await?;
        sqlx::query("DELETE FROM project_kb_links WHERE project_id=$1").bind(project).execute(pool).await?;
        let project_scope=matches!(case,1|5|7|10).then_some(project);
        if project_scope.is_some() {sqlx::query("INSERT INTO project_kb_links(project_id,kb_id,attached_by) VALUES($1,$2,$3)").bind(project).bind(kb).bind(uid).execute(pool).await?;}
        let workflow=Uuid::now_v7();let before=count.load(Ordering::SeqCst);
        let mut cfg=json!({"prompt":"Frozen workflow prompt {{filename}} {{payload.mime}} {{kb_id}} {{doc_id}}","model_selection":{"mode":"default"}});
        if case==1 {let policy=store::load_policy(state,admin,None).await?;cfg["model_selection"]=json!({"mode":"manual","provider_id":policy.default_provider_id.unwrap()});}
        if case==11 {cfg["model_selection"]=json!({"mode":"auto","objective":"balanced"});}
        if project_scope.is_none() {cfg["kb_ids"]=json!([kb]);}
        sqlx::query("INSERT INTO workflows(id,name,owner_id,enabled,trigger_event_type,action_type,action_config,project_id) VALUES($1,'Sources',$2,true,'document.ingested','agent_run',$3,$4)")
            .bind(workflow).bind(uid).bind(&cfg).bind(project_scope).execute(pool).await?;
        let mut tx=pool.begin().await?;
        events::emit_with(&mut tx,&NewEvent::new(events::DOCUMENT_INGESTED,ActorType::Human).actor(Some(uid))
            .resource(if case==8 {"document"} else {"kb_document"},doc).project(project_scope).payload(json!({"filename":"Private event payload"}))).await?;
        tx.commit().await?;assert_eq!(workflows::dispatch_once(state).await?,1,"source case {case}");
        let id:Uuid=sqlx::query_scalar("SELECT id FROM workflow_runs WHERE workflow_id=$1").bind(workflow).fetch_one(pool).await?;
        let captured:Value=sqlx::query_scalar("SELECT input FROM workflow_routing_inputs WHERE run_id=$1").bind(id).fetch_one(pool).await?;
        assert_eq!(captured["project_kb_ids"],if project_scope.is_some() {json!([kb])} else {json!([])});
        if case==2 {sqlx::query("UPDATE knowledge_bases SET owner_id=$2 WHERE id=$1").bind(kb).bind(other).execute(pool).await?;}
        if case==3 {denied.store(true,Ordering::SeqCst);}
        if case==7 {sqlx::query("DELETE FROM project_kb_links WHERE project_id=$1").bind(project).execute(pool).await?;}
        if case==9 {sqlx::query("UPDATE knowledge_bases SET restricted=true WHERE id=$1").bind(kb).execute(pool).await?;}
        if case==10 {sqlx::query("UPDATE projects SET owner_user_id=$2 WHERE id=$1").bind(project).bind(other).execute(pool).await?;}
        let result=Box::pin(workflows::run(state,id)).await;
        let (status,outcome,error):(String,Option<Value>,Option<String>)=sqlx::query_as("SELECT status::text,outcome,error FROM workflow_runs WHERE id=$1").bind(id).fetch_one(pool).await?;
        let success=case<2 || case==11;
        assert_eq!(status,if success {"succeeded"} else {"failed"},"source case {case}: {result:?} {error:?}");
        let expected_calls=match case {0|1|6|11=>2,4|5=>1,_=>0};
        assert_eq!(count.load(Ordering::SeqCst)-before,expected_calls,"source case {case}: {error:?}");
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM messages WHERE turn_id=$1 AND role='assistant' AND completed_at IS NOT NULL").bind(id).fetch_one(pool).await?,i64::from(success),"source case {case}");
        if success {
            let outcome=outcome.unwrap();assert_eq!(outcome["usage"]["calls"],4);
            let chat:Uuid=serde_json::from_value(outcome["output_chat_id"].clone())?;
            assert_eq!(sqlx::query_scalar::<_,Option<Uuid>>("SELECT project_id FROM chats WHERE id=$1").bind(chat).fetch_one(pool).await?,project_scope);
            assert_eq!(sqlx::query_scalar::<_,Uuid>("SELECT kb_id FROM chat_kb_links WHERE chat_id=$1").bind(chat).fetch_all(pool).await?,vec![kb]);
        }
        let calls=count.load(Ordering::SeqCst);Box::pin(workflows::run(state,id)).await?;assert_eq!(count.load(Ordering::SeqCst),calls);
        sqlx::query("UPDATE workflows SET enabled=false WHERE id=$1").bind(workflow).execute(pool).await?;
    }
    denied.store(false,Ordering::SeqCst);Ok(())
}
