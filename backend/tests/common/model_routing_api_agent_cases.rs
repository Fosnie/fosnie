// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{pin::Pin,future::Future,sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}},time::Duration};
use axum::{body::{Body,to_bytes},http::{Request,StatusCode},response::Response};
use serde_json::{Value,json};
use tower::ServiceExt;
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,rbac::{FlatRbacPolicy,ResourceType,Permission}},config::BootConfig,
    state::AppStateBuilder,model_routing::store};
struct Features;
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Features {
    async fn enabled_for_user(&self,_:&AppState,_:Option<Uuid>,feature:&str)->bool {feature=="public_api"}
}
struct Acl {deny:Arc<AtomicBool>,doc:Uuid}
#[async_trait::async_trait]
impl fosnie_backend::ext::RbacPolicy for Acl {
    async fn may_grant(&self,p:&sqlx::PgPool,a:&AuthContext,k:ResourceType,id:Uuid)->fosnie_backend::Result<bool> {FlatRbacPolicy.may_grant(p,a,k,id).await}
    async fn can(&self,p:&sqlx::PgPool,a:&AuthContext,k:ResourceType,id:Uuid,m:Permission)->fosnie_backend::Result<bool> {FlatRbacPolicy.can(p,a,k,id,m).await}
    async fn denied_kb_doc_ids(&self,_:&sqlx::PgPool,_:&AuthContext,_:&[Uuid])->fosnie_backend::Result<Vec<Uuid>> {Ok(if self.deny.load(Ordering::SeqCst) {vec![self.doc]} else {vec![]})}
}
async fn request(state:AppState,token:String,body:Value,chat:Option<Uuid>)->anyhow::Result<Response> {
    Ok(tokio::spawn(async move {
        let mut req=Request::builder().method("POST").uri("/v1/chat/completions").header("authorization",format!("Bearer {token}"))
            .header("content-type","application/json");
        if let Some(chat)=chat {req=req.header("x-fosnie-chat-id",chat.to_string());}
        fosnie_backend::http::v1::router(&state).with_state(state).oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap()
    }).await?)
}
async fn value(response:Response)->anyhow::Result<Value> {Ok(serde_json::from_slice(&to_bytes(response.into_body(),1_000_000).await?)?)}
async fn settled(pool:&sqlx::PgPool,chat:Uuid)->anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let n:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type='api.agent.routed' AND model_agent_traceability->>'chat_id'=$1")
                .bind(chat.to_string()).fetch_one(pool).await?;
            if n>0 {return Ok::<_,sqlx::Error>(());}
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await??;Ok(())
}
pub fn check<'a>(pool:&'a sqlx::PgPool,provider:Uuid,admin:&'a AuthContext)->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
        let uid=Uuid::now_v7();let key=Uuid::now_v7();let agent=Uuid::now_v7();let other=Uuid::now_v7();let kb=Uuid::now_v7();let doc=Uuid::now_v7();
        let calls=Arc::new(AtomicUsize::new(0));let mode=Arc::new(AtomicUsize::new(0));let steps=Arc::new(AtomicUsize::new(0));
        let deny=Arc::new(AtomicBool::new(false));let entered=Arc::new(tokio::sync::Notify::new());
        let app=axum::Router::new().route("/generate",axum::routing::post({
            let calls=calls.clone();let mode=mode.clone();let pool=pool.clone();let entered=entered.clone();
            move |axum::Json(body):axum::Json<Value>| {let calls=calls.clone();let mode=mode.clone();let pool=pool.clone();let entered=entered.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);entered.notify_one();
                assert!(body["messages"].to_string().contains("API agent instruction"));
                let mode=mode.load(Ordering::SeqCst);
                if mode==2 {sqlx::query("UPDATE agents SET system_prompt='Changed' WHERE id=$1").bind(agent).execute(&pool).await.unwrap();}
                if mode==5 {assert!(body["messages"].to_string().contains("API library evidence"));}
                let lines=vec![json!({"type":"token","delta":"API agent answer."}).to_string()+"\n",
                    json!({"type":"done","model":"catalog-model","finish_reason":"stop","usage":{"prompt_tokens":999,"completion_tokens":999},
                        "routing_report":super::routing_mock_report(&body)}).to_string()+"\n"];
                Body::from_stream(futures_util::stream::unfold((lines.into_iter(),0),move |(mut lines,index)|async move {
                    let line=lines.next()?;if mode==1 && index==1 {tokio::time::sleep(Duration::from_secs(3)).await;}
                    Some((Ok::<_,std::convert::Infallible>(line),(lines,index+1)))
                }))
            }}
        })).route("/chat-step",axum::routing::post({
            let calls=calls.clone();let steps=steps.clone();
            move |axum::Json(body):axum::Json<Value>| {let calls=calls.clone();let steps=steps.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);
                assert_eq!(body["overrides"]["routing"]["purpose"],"tool_loop");
                let mut result=json!({"content":"API web evidence answer.","usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)});
                if steps.fetch_add(1,Ordering::SeqCst)==0 {result["tool_calls"]=json!([{"id":"web-1","name":"web_search","arguments":{"query":"Glasgow weather","depth":"standard"}}]);}
                axum::Json(result)
            }}
        })).route("/web_search",axum::routing::post({
            let calls=calls.clone();let mode=mode.clone();let pool=pool.clone();
            move |axum::Json(body):axum::Json<Value>| {let calls=calls.clone();let mode=mode.clone();let pool=pool.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);
                if mode.load(Ordering::SeqCst)==4 {sqlx::query("UPDATE api_keys SET revoked_at=now() WHERE id=$1").bind(key).execute(&pool).await.unwrap();tokio::time::sleep(Duration::from_millis(600)).await;}
                let mut result=super::workflow_agent_cases::reports(&body,&["web_plan","web_grade"],&["rerank","ocr"]);
                result["type"]=json!("done");result["digest"]=json!("API web evidence");result["citations"]=json!([]);format!("{result}\n")
            }}
        })).route("/retrieve",axum::routing::post({
            let calls=calls.clone();
            move |axum::Json(body):axum::Json<Value>| {let calls=calls.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);assert_eq!(body["kb_ids"],json!([kb]));
                let mut result=super::workflow_agent_cases::reports(&body,&["retrieval_decompose","retrieval_grade"],&["embed"]);
                result["type"]=json!("done");result["context"]=json!("API library evidence");result["citations"]=json!([]);format!("{result}\n")
            }}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
        let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
        let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{address}");boot.features.agents_enabled=true;
        let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
            .with_features(Arc::new(Features)).with_rbac(Arc::new(Acl {deny:deny.clone(),doc})).build();
        for id in [uid,other] {sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'API agent test','user')").bind(id).bind(format!("{id}@routing.test")).execute(pool).await?;}
        let (token,hash,prefix)=fosnie_backend::auth::api_key::mint();
        sqlx::query("INSERT INTO api_keys(id,user_id,name,token_hash,display_prefix) VALUES($1,$2,'api-agent',$3,$4)").bind(key).bind(uid).bind(hash).bind(prefix).execute(pool).await?;
        sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'API agent','API agent instruction',$2,$3,ARRAY['general'])")
            .bind(agent).bind(json!({"max_tokens":512,"web_max_fetches":2})).bind(uid).execute(pool).await?;
        sqlx::query("INSERT INTO knowledge_bases(id,name,owner_id,embedding_model_id,embedding_dimension,status) VALUES($1,'API sources',$2,'routed-embed',384,'ready')").bind(kb).bind(uid).execute(pool).await?;
        sqlx::query("INSERT INTO kb_documents(id,kb_id,original_filename,mime,bytes_path,ingest_status) VALUES($1,$2,'api.txt','text/plain','synthetic.txt','ready')").bind(doc).bind(kb).execute(pool).await?;
        for (k,v,t) in [("routing.public_api_enabled","true","bool"),("rag.model_search_mode","off","string"),("rag.rerank_enabled","false","bool"),("rag.synthesis_mode","unified","string")] {
            sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,$2,$3::config_value_type) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,value_type=EXCLUDED.value_type")
                .bind(k).bind(v).bind(t).execute(pool).await?;
        }
        let mut policy=store::load_policy(&state,admin,None).await?;policy.constraints.max_calls=Some(200);policy.constraints.max_total_tokens=None;
        store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"API agent".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy}).await?;
        let body=||json!({"model":format!("agent/{agent}"),"model_selection":{"mode":"default"},"messages":[{"role":"user","content":"Synthetic agent question"}]});
        let mut chat=None;
        for selection in [json!({"mode":"default"}),json!({"mode":"manual","provider_id":provider}),json!({"mode":"auto"})] {
            let mut b=body();b["model_selection"]=selection.clone();let response=request(state.clone(),token.clone(),b,chat).await?;
            let status=response.status();let next=response.headers().get("x-fosnie-chat-id").and_then(|h|h.to_str().ok()).and_then(|s|Uuid::parse_str(s).ok());
            let response=value(response).await?;assert_eq!(status,StatusCode::OK,"{response}");
            if chat.is_some() {assert_eq!(chat,next);}chat=next;
            assert_eq!(response["model"],format!("agent/{agent}"));assert_eq!(response["fosnie_routing"]["selection"]["mode"],selection["mode"]);
            assert_eq!(response["fosnie_routing"]["usage"]["calls"],1);
            assert_eq!(response["usage"]["total_tokens"],140,"agent usage also uses the validated report");
        }
        let before=calls.load(Ordering::SeqCst);
        let mut legacy=body();legacy.as_object_mut().unwrap().remove("model_selection");
        assert_eq!(request(state.clone(),token.clone(),legacy,chat).await?.status(),StatusCode::BAD_REQUEST);
        let alien=Uuid::now_v7();sqlx::query("INSERT INTO agents(id,name,system_prompt,created_by,modes) VALUES($1,'Other','Other',$2,ARRAY['general'])").bind(alien).bind(uid).execute(pool).await?;
        let mut b=body();b["model"]=json!(format!("agent/{alien}"));assert_ne!(request(state.clone(),token.clone(),b,chat).await?.status(),StatusCode::OK);
        let mut b=body();b["max_tokens"]=json!(512);assert_eq!(request(state.clone(),token.clone(),b,None).await?.status(),StatusCode::BAD_REQUEST);
        let mut b=body();b["messages"]=json!([{"role":"system","content":"Override"},{"role":"user","content":"Question"}]);
        assert_eq!(request(state.clone(),token.clone(),b,None).await?.status(),StatusCode::BAD_REQUEST);
        sqlx::query("UPDATE chats SET owner_user_id=$2 WHERE id=$1").bind(chat).bind(other).execute(pool).await?;
        assert_eq!(request(state.clone(),token.clone(),body(),chat).await?.status(),StatusCode::NOT_FOUND);
        sqlx::query("UPDATE chats SET owner_user_id=$2 WHERE id=$1").bind(chat).bind(uid).execute(pool).await?;
        sqlx::query("UPDATE agents SET params=params||'{\"temperature\":0.4}'::jsonb WHERE id=$1").bind(agent).execute(pool).await?;
        assert_eq!(request(state.clone(),token.clone(),body(),chat).await?.status(),StatusCode::CONFLICT);
        sqlx::query("UPDATE agents SET params=$2 WHERE id=$1").bind(agent).bind(json!({"max_tokens":512,"web_max_fetches":2})).execute(pool).await?;
        assert_eq!(calls.load(Ordering::SeqCst),before);
        mode.store(2,Ordering::SeqCst);let response=request(state.clone(),token.clone(),body(),None).await?;
        assert_ne!(response.status(),StatusCode::OK,"agent changed during generation must not publish");
        sqlx::query("UPDATE agents SET system_prompt='API agent instruction' WHERE id=$1").bind(agent).execute(pool).await?;
        mode.store(1,Ordering::SeqCst);let mut b=body();b["stream"]=json!(true);
        let response=request(state.clone(),token.clone(),b,None).await?;
        let busy:Uuid=response.headers()["x-fosnie-chat-id"].to_str()?.parse()?;
        assert_eq!(request(state.clone(),token.clone(),body(),Some(busy)).await?.status(),StatusCode::CONFLICT);
        drop(response);settled(pool,busy).await?;
        let running:i64=sqlx::query_scalar("SELECT count(*) FROM agent_runs WHERE chat_id=$1 AND status='running'").bind(busy).fetch_one(pool).await?;assert_eq!(running,0);
        mode.store(0,Ordering::SeqCst);let mut b=body();b["stream"]=json!(true);
        let response=request(state.clone(),token.clone(),b,None).await?;let wire=String::from_utf8(to_bytes(response.into_body(),1_000_000).await?.to_vec())?;
        assert!(wire.contains("fosnie_routing") && wire.contains("[DONE]"));
        sqlx::query("INSERT INTO agent_tools(agent_id,tool_name) VALUES($1,'web_search')").bind(agent).execute(pool).await?;
        for mode_value in [3,4] {
            mode.store(mode_value,Ordering::SeqCst);steps.store(0,Ordering::SeqCst);let before=calls.load(Ordering::SeqCst);
            let response=request(state.clone(),token.clone(),body(),None).await?;let status=response.status();let response=value(response).await?;
            if mode_value==3 {assert_eq!(status,StatusCode::OK,"{response}");assert_eq!(response["fosnie_routing"]["usage"]["calls"],6);}
            else {assert_ne!(status,StatusCode::OK);assert_eq!(calls.load(Ordering::SeqCst),before+2,"key revoked inside web tool prevents the next LLM step");}
            sqlx::query("UPDATE api_keys SET revoked_at=NULL WHERE id=$1").bind(key).execute(pool).await?;
        }
        sqlx::query("DELETE FROM agent_tools WHERE agent_id=$1").bind(agent).execute(pool).await?;
        sqlx::query("INSERT INTO agent_project_knowledge(agent_id,project_knowledge_id) VALUES($1,$2)").bind(agent).bind(kb).execute(pool).await?;
        mode.store(5,Ordering::SeqCst);let response=request(state.clone(),token.clone(),body(),None).await?;
        let status=response.status();let source_chat=response.headers().get("x-fosnie-chat-id").and_then(|v|v.to_str().ok()).and_then(|v|Uuid::parse_str(v).ok());
        let response=value(response).await?;assert_eq!(status,StatusCode::OK,"{response}");assert_eq!(response["fosnie_routing"]["usage"]["calls"],4);
        deny.store(true,Ordering::SeqCst);let before=calls.load(Ordering::SeqCst);
        assert_ne!(request(state.clone(),token.clone(),body(),source_chat).await?.status(),StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst),before,"old source history is not sent after document denial");
        deny.store(false,Ordering::SeqCst);
        sqlx::query("UPDATE config_settings SET value='false' WHERE key='routing.public_api_enabled'").execute(pool).await?;
        assert_eq!(request(state.clone(),token.clone(),body(),None).await?.status(),StatusCode::SERVICE_UNAVAILABLE);
        server.abort();Ok(())
    })
}
