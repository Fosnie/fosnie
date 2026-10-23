// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{future::Future, pin::Pin, sync::{Arc, atomic::{AtomicUsize, Ordering}}};
use serde_json::{json, Value};
use uuid::Uuid;
use fosnie_backend::{auth::AuthContext, config::BootConfig, ext::{ProviderRegistry, ResolvedProvider},
    model_routing::admission, state::AppStateBuilder};

struct Registry(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl ProviderRegistry for Registry {
    async fn resolve(&self, _: &sqlx::PgPool, _: &str, _: Option<Uuid>) -> fosnie_backend::Result<Option<ResolvedProvider>> {
        self.0.fetch_add(1, Ordering::SeqCst); Ok(None)
    }
    async fn resolve_llm_id(&self, _: &sqlx::PgPool, _: Option<Uuid>, _: Uuid) -> fosnie_backend::Result<Option<ResolvedProvider>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(Some(ResolvedProvider { base_url: None, model: Some("legacy-test".into()), api_key: None, enabled: true, reasoning_mode: None }))
    }
}

pub fn check<'a>(pool: &'a sqlx::PgPool, actor: &'a AuthContext, provider: Uuid) -> Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
        let resolved=Arc::new(AtomicUsize::new(0)); let effects=Arc::new(AtomicUsize::new(0));
        let app=axum::Router::new().fallback(axum::routing::any({
            let effects=effects.clone(); move || { let effects=effects.clone(); async move {
                effects.fetch_add(1,Ordering::SeqCst); axum::http::StatusCode::INTERNAL_SERVER_ERROR
            }}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?; let address=listener.local_addr()?;
        let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let mut boot=BootConfig::default(); boot.ml.base_url=format!("http://{address}"); boot.features.agents_enabled=true;
        let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
            .with_providers(Arc::new(Registry(resolved.clone()))).build();
        admission::require_legacy_chat(&state,None).await?;
        let legacy=Uuid::now_v7();
        sqlx::query("INSERT INTO chats(id,owner_user_id,llm_provider_id) VALUES($1,$2,$3)").bind(legacy).bind(actor.user_id).bind(provider).execute(pool).await?;
        admission::require_legacy_chat(&state,Some(legacy)).await?;
        let _ = fosnie_backend::http::providers::set_chat_llm_provider(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(actor.clone()),
            axum::extract::Path(legacy),axum::Json(fosnie_backend::http::providers::SetChatLlm {provider_id:Some(provider)})).await?;
        fosnie_backend::providers::resolve_llm(&state,actor.user_id,Some(legacy),None).await?;
        fosnie_backend::providers::resolve_llm(&state,actor.user_id,Some(legacy),Some(provider)).await?;
        assert_eq!(resolved.load(Ordering::SeqCst),2,"migrated legacy Manual remains compatible");
        for evidence in ["auto","v2_manual","v2_default","v2_auto","intent","decision","voice","api","branch","job"] {
            let chat=Uuid::now_v7(); let message=Uuid::now_v7(); let turn=Uuid::now_v7();
            sqlx::query("INSERT INTO chats(id,owner_user_id) VALUES($1,$2)").bind(chat).bind(actor.user_id).execute(pool).await?;
            match evidence {
                "auto" => {sqlx::query("UPDATE chats SET model_selection='{\"mode\":\"auto\"}' WHERE id=$1").bind(chat).execute(pool).await?;},
                "v2_manual"|"v2_default"|"v2_auto" => {
                    let mode=evidence.strip_prefix("v2_").unwrap();
                    let selection=if mode=="manual" {json!({"mode":mode,"provider_id":provider})}else{json!({"mode":mode})};
                    sqlx::query("UPDATE chats SET model_selection=$2 WHERE id=$1").bind(chat)
                        .bind(json!({"schema_version":2,"required_profile_version":1,"selection":selection})).execute(pool).await?;
                },
                "intent"|"decision" => {
                    sqlx::query("INSERT INTO messages(id,chat_id,role,sequence_number,content,routing_selection,routing_decision) VALUES($1,$2,'assistant',1,'Saved answer',$3,$4)")
                        .bind(message).bind(chat).bind((evidence=="intent").then(||json!({"mode":"default"})))
                        .bind((evidence=="decision").then(||json!({"id":Uuid::now_v7()}))).execute(pool).await?;
                },
                "voice" => {sqlx::query("INSERT INTO voice_routed_chats(chat_id,source_scope) VALUES($1,'{}')").bind(chat).execute(pool).await?;},
                "api" => {sqlx::query("INSERT INTO api_routed_chats(chat_id,agent_id,source_scope) VALUES($1,$2,'{}')").bind(chat).bind(Uuid::now_v7()).execute(pool).await?;},
                "branch" => {sqlx::query("INSERT INTO routed_chat_branches(chat_id,source_scope) VALUES($1,'{}')").bind(chat).execute(pool).await?;},
                "job" => {
                    let task=Uuid::now_v7(); let job=Uuid::now_v7();
                    sqlx::query("INSERT INTO tasks(id,task_type,payload) VALUES($1,'deep_research','{}')").bind(task).execute(pool).await?;
                    sqlx::query("INSERT INTO routed_jobs(id,task_id,actor_user_id,kind,context,plan,limits,usage,input,deadline) VALUES($1,$2,$3,'deep_research',$4,'{}','{}','{}','{}',now()+interval '1 minute')")
                        .bind(job).bind(task).bind(actor.user_id).bind(json!({"chat_id":chat})).execute(pool).await?;
                    assert!(Box::pin(fosnie_backend::groundedness::verify_draft(&state,&json!({"run_id":job,"path":"unread-mock.pdf"}))).await.is_err(),"routed job ID must never enter legacy verifier");
                }, _=>unreachable!()
            }
            for requested in [None,Some(provider)] {
                assert!(fosnie_backend::providers::resolve_llm(&state,actor.user_id,Some(chat),requested).await.is_err(),"{evidence} must stop before credentials/default resolution");
                let saved:Value=sqlx::query_scalar("SELECT model_selection FROM chats WHERE id=$1").bind(chat).fetch_one(pool).await?;
                assert!(fosnie_backend::http::providers::set_chat_llm_provider(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(actor.clone()),
                    axum::extract::Path(chat),axum::Json(fosnie_backend::http::providers::SetChatLlm {provider_id:requested})).await.is_err(),"{evidence}: old picker cannot erase routed intent");
                let after:Value=sqlx::query_scalar("SELECT model_selection FROM chats WHERE id=$1").bind(chat).fetch_one(pool).await?;
                assert_eq!(saved,after);
            }
            assert!(admission::require_legacy_chat(&state,Some(chat)).await.is_err());
            let before:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(m) FROM messages m WHERE chat_id=$1 ORDER BY sequence_number").bind(chat).fetch_all(pool).await?;
            let (tx,mut rx)=tokio::sync::mpsc::channel(32);
            Box::pin(fosnie_backend::chat::run_turn(&state,fosnie_backend::chat::origin::TurnContext::web(actor),turn,Some(chat),None,None,
                "Legacy client message".into(),vec![],vec![],false,None,None,Some(provider),None,&tx,Arc::new(tokio::sync::Notify::new()))).await;
            assert!(matches!(rx.try_recv(),Ok(fosnie_backend::ws::protocol::ServerFrame::ChatError {..})),"{evidence}: explicit error to old client");
            assert!(fosnie_backend::chat::regenerate_prepare(&state,actor,chat,message,None).await.is_err());
            assert!(Box::pin(fosnie_backend::web_search::run_deep(&state,&json!({"chat_id":chat,"turn_id":turn,"query":"Frozen weather"}))).await.is_err());
            assert!(Box::pin(fosnie_backend::research::run_research(&state,&json!({"chat_id":chat,"turn_id":turn,"question":"Frozen research"}))).await.is_err());
            let after:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(m) FROM messages m WHERE chat_id=$1 ORDER BY sequence_number").bind(chat).fetch_all(pool).await?;
            assert_eq!(before,after,"{evidence}: legacy admission precedes history mutation");
            let run=Uuid::now_v7();
            sqlx::query("INSERT INTO agent_runs(id,chat_id,turn_id,acting_user_id,status,pending_tool,pending_args) VALUES($1,$2,$3,$4,'approved','generate_artefact','{}')")
                .bind(run).bind(chat).bind(turn).bind(actor.user_id).execute(pool).await?;
            Box::pin(fosnie_backend::agent::execute_approved(&state,run)).await?;
            Box::pin(fosnie_backend::agent::execute_approved(&state,run)).await?;
            let status:String=sqlx::query_scalar("SELECT status::text FROM agent_runs WHERE id=$1").bind(run).fetch_one(pool).await?;
            assert_eq!(status,"completed");
            let refusals:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE resource_id=$1 AND action_type='tool.resume_denied' AND payload->>'reason'='routed execution has no legacy tool resume'")
                .bind(run).fetch_one(pool).await?;
            assert_eq!(refusals,1,"no repeat dispatch or refusal after terminal completion");
        }
        // The checkpoint itself protects against replay even without a chat marker.
        for checkpoint in [json!({"routed":true}),json!({"resumable":false})] {
            let run=Uuid::now_v7();
            sqlx::query("INSERT INTO agent_runs(id,chat_id,turn_id,status,pending_tool,checkpoint) VALUES($1,$2,$3,'approved','generate_artefact',$4)")
                .bind(run).bind(legacy).bind(Uuid::now_v7()).bind(checkpoint).execute(pool).await?;
            Box::pin(fosnie_backend::agent::execute_approved(&state,run)).await?;
            let reason:String=sqlx::query_scalar("SELECT payload->>'reason' FROM audit_events WHERE resource_id=$1 AND action_type='tool.resume_denied'").bind(run).fetch_one(pool).await?;
            assert_eq!(reason,"routed execution has no legacy tool resume");
        }
        assert_eq!(resolved.load(Ordering::SeqCst),2);
        assert_eq!(effects.load(Ordering::SeqCst),0,"no inference/render endpoint reached by refused legacy calls");
        // Preserve the ordinary owner gate before exposing routing admission.
        let mut foreign=actor.clone(); foreign.user_id=Some(Uuid::now_v7());
        let result=fosnie_backend::http::providers::set_chat_llm_provider(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(foreign),
            axum::extract::Path(legacy),axum::Json(fosnie_backend::http::providers::SetChatLlm {provider_id:Some(provider)})).await;
        assert!(matches!(result,Err(fosnie_backend::AppError::NotFound(_))));
        server.abort(); Ok(())
    })
}
