// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
use serde_json::{Value,json};
use uuid::Uuid;
use fosnie_backend::{auth::{AuthContext,PlatformRole},model_routing::*,config::BootConfig,state::AppStateBuilder};

pub async fn check_web(pool:&sqlx::PgPool,admin:&AuthContext,policy_id:Uuid) -> anyhow::Result<()> {
    let count=Arc::new(AtomicUsize::new(0)); let mode=Arc::new(AtomicUsize::new(0));
    let steps=Arc::new(AtomicUsize::new(0));let entered=Arc::new(tokio::sync::Notify::new());
    let app=axum::Router::new().route("/web_search",axum::routing::post({
        let count=count.clone(); let mode=mode.clone(); let entered=entered.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let mode=mode.clone();let entered=entered.clone(); async move {
            count.fetch_add(1,Ordering::SeqCst);
            let scenario=mode.load(Ordering::SeqCst);
            if matches!(scenario,16|17) {entered.notify_one();tokio::time::sleep(std::time::Duration::from_secs(5)).await;}
            if scenario==20 {tokio::time::sleep(std::time::Duration::from_secs(5)).await;}
            assert_eq!(body["domain_allowlist"],"weather.test"); assert_eq!(body["allowlist_only"],true);
            assert_eq!(body["domain_blocklist"],"blocked.test"); assert_eq!(body["robots_policy"],"respect");
            assert_eq!(body["max_fetches"],2); assert_eq!(body["depth"],if mode.load(Ordering::SeqCst)==8 {"quick"} else {"standard"});
            let routing=&body["overrides"]["routing"];
            assert!(routing["fixed_assignments"].get("ocr").is_some());
            let mut calls=vec![];
            for purpose in ["web_plan","web_grade"] {
                if mode.load(Ordering::SeqCst)==8 {break;} // quick needs only fixed services
                let mut one=body.clone();one["overrides"]["routing"]["purpose"]=json!(purpose);
                let mut record=super::routing_mock_report(&one)["calls"][0].clone();
                record["usage"]["output_tokens"]=json!(20);record["usage"]["total_tokens"]=json!(120);
                calls.push(record);
            }
            let fixed_calls:Vec<_>=["rerank","ocr"].into_iter().map(|role| {
                let target=&routing["fixed_assignments"][role];
                json!({"call_id":Uuid::now_v7(),"lease_id":routing["fixed_budget"]["id"],"decision_id":routing["decision_id"],
                    "role":role,"provider_id":target["provider_id"],"provider_revision":target["provider_revision"],"profile_revision":target["profile_revision"],
                    "status":"completed","latency_ms":1,"input_bytes":20,"output_bytes":10})
            }).collect();
            let mut done=json!({"type":"done","digest":"Test weather evidence","citations":[],
                "routing_report":{"lease_id":routing["budget"]["id"],"complete":true,"calls":calls},
                "fixed_routing_report":{"lease_id":routing["fixed_budget"]["id"],"complete":true,"calls":fixed_calls}});
            if scenario>=10 {done["citations"]=json!([{"url":"https://weather.test/report","title":"Weather","domain":"weather.test","quote_text":"Test weather evidence"}]);}
            if matches!(scenario,2|12) { done.as_object_mut().unwrap().remove("fixed_routing_report"); }
            if matches!(scenario,3|13) { return "{\"type\":\"progress\",\"stage\":\"fetch\"}\n".to_string(); }
            format!("{done}\n")
        }}
    }));
    let app=app.route("/chat-step",axum::routing::post({
        let count=count.clone();let mode=mode.clone();let steps=steps.clone();let pool=pool.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let mode=mode.clone();let steps=steps.clone();let pool=pool.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            assert_eq!(body["overrides"]["routing"]["purpose"],"tool_loop");
            assert_eq!(body["sampling"]["max_tokens"],512);
            assert_eq!(body["tools"][0]["function"]["name"],"web_search");
            let mut response=json!({"content":"The weather report is available [1].","usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)});
            if steps.fetch_add(1,Ordering::SeqCst)==0 {
                if mode.load(Ordering::SeqCst)==19 {
                    sqlx::query("UPDATE agents SET params=jsonb_set(params,'{web_max_fetches}','1') WHERE name='Routed web agent'").execute(&pool).await.unwrap();
                }
                response["tool_calls"]=json!([{"id":"web-1","name":if mode.load(Ordering::SeqCst)==14 {"read_document"} else {"web_search"},"arguments":{"query":"weather?","depth":"standard"}}]);
            } else {assert!(body["messages"].to_string().contains("Test weather evidence"));assert!(body["messages"].to_string().contains("UNTRUSTED"));}
            axum::Json(response)
        }}
    }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let addr=listener.local_addr()?;
    let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
    let mut boot=BootConfig::default();boot.features.agents_enabled=true;boot.ml.base_url=format!("http://{addr}");
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot)).build();
    let mut actor=admin.clone();actor.role=PlatformRole::User;
    let mut policy=store::load_policy(&state,admin,Some(policy_id)).await?;
    for (key,value,kind) in [("web_search.allowlist","weather.test","string"),("web_search.blocklist","blocked.test","string"),
        ("web_search.allowlist_only","true","bool"),("web_search.robots_policy","respect","string")] {
        sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,$2,$3::config_value_type) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
            .bind(key).bind(value).bind(kind).execute(pool).await?;
    }
    for scenario in 0..9 {
        mode.store(scenario,Ordering::SeqCst);
        sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES('integration.web_search.enabled',$1,'bool') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
            .bind(if scenario==0 {"false"} else {"true"}).execute(pool).await?;
        policy.constraints.max_calls=Some(if scenario==5 {63} else {200});
        policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Web test".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone()}).await?;
        let chat=Uuid::now_v7();let turn=Uuid::now_v7();
        sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Web test')").bind(chat).bind(actor.user_id).execute(pool).await?;
        let run=fosnie_backend::chat::routing::prepare(&state,&actor,chat,turn,"weather?",Some(policy_id),RoutingObjective::Balanced,
            WorkloadRequirements {output_tokens:1024,..Default::default()},&[fixed::FixedRole::Rerank,fixed::FixedRole::Ocr],&tokio::sync::Notify::new()).await?;
        let names=if scenario==4 {vec![]} else {vec!["web_search".into()]};
        let authorised=fosnie_backend::tools::AuthorisedTools::build(&names,&names,false,&Default::default());
        let budget=fosnie_backend::tools::WebBudget {depth_max:None,max_fetches:Some(2)};
        let args=json!({"query":"weather?","depth":if scenario==6 {"deep"} else if scenario==8 {"quick"} else {"standard"}});
        if scenario==7 {run.cancel();}
        let before=count.load(Ordering::SeqCst);let (tx,_rx)=tokio::sync::mpsc::channel(16);
        let result=fosnie_backend::web_search::routed::search(&state,&run,&authorised,Some(&budget),&args,&tx).await;
        assert_eq!(result.is_ok(),scenario==1 || scenario==8,"scenario {scenario}: {:?}",result.as_ref().err());
        assert_eq!(count.load(Ordering::SeqCst)-before,usize::from(matches!(scenario,1|2|3|8)),"no fallback or denied egress");
        if scenario==1 || scenario==8 {assert_eq!(result?.digest,"Test weather evidence");assert_eq!(run.usage().calls,if scenario==1 {4} else {2});assert_eq!(run.usage().tokens,if scenario==1 {240} else {0});}
        else if scenario==2 || scenario==3 {assert_eq!(run.usage().calls,64);}
        else {assert_eq!(run.usage().calls,0);}
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    use fosnie_backend::{chat::{run_turn,origin::TurnContext},ws::protocol::ServerFrame};
    let agent=Uuid::now_v7();
    sqlx::query("INSERT INTO agents(id,name,system_prompt,params,created_by,modes) VALUES($1,'Routed web agent','Use web evidence',$2,$3,ARRAY['general'])")
        .bind(agent).bind(json!({"web_max_fetches":2,"max_tokens":512})).bind(actor.user_id).execute(pool).await?;
    sqlx::query("INSERT INTO agent_tools(agent_id,tool_name) VALUES($1,'web_search')").bind(agent).execute(pool).await?;
    for scenario in 10..21 {
        mode.store(scenario,Ordering::SeqCst);steps.store(0,Ordering::SeqCst);
        sqlx::query("UPDATE agents SET params=$2 WHERE id=$1").bind(agent).bind(json!({"web_max_fetches":2,"max_tokens":512})).execute(pool).await?;
        sqlx::query("UPDATE config_settings SET value=$1 WHERE key='integration.web_search.enabled'")
            .bind(if scenario==11 {"false"} else {"true"}).execute(pool).await?;
        policy.constraints.max_calls=Some(if scenario==15 {64} else {200});
        policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Web chat".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone()}).await?;
        let chat=Uuid::now_v7();let turn=Uuid::now_v7();let cancel=Arc::new(tokio::sync::Notify::new());
        sqlx::query("INSERT INTO chats(id,owner_user_id,agent_id,title) VALUES($1,$2,$3,'Web chat')").bind(chat).bind(actor.user_id).bind(agent).execute(pool).await?;
        let run=fosnie_backend::chat::routing::prepare(&state,&actor,chat,turn,"weather?",Some(policy_id),RoutingObjective::Balanced,
            WorkloadRequirements {output_tokens:1024,..Default::default()},&[],&cancel).await?;
        assert!(run.plan().fixed_assignments.contains_key(&fixed::FixedRole::Ocr),"preparation pins web dependencies");
        if scenario==18 {sqlx::query("INSERT INTO agent_tools(agent_id,tool_name) VALUES($1,'edit_document')").bind(agent).execute(pool).await?;}
        let before=count.load(Ordering::SeqCst);let (tx,mut rx)=tokio::sync::mpsc::channel(128);
        let mut turn_state=state.clone();
        if scenario==20 {let mut boot=(*state.boot).clone();boot.tool_timeout_secs.insert("web".into(),1);turn_state.boot=Arc::new(boot);}
        let work=Box::pin(run_turn(&turn_state,TurnContext::web(&actor).with_routing(&run),turn,Some(chat),None,None,"weather?".into(),vec![],vec![],false,None,None,None,None,&tx,cancel.clone()));
        if matches!(scenario,16|17) {
            let stop=async {
                entered.notified().await;
                if scenario==17 {cancel.notify_one();} else {
                    let id:Uuid=sqlx::query_scalar("SELECT id FROM agent_runs WHERE chat_id=$1 AND turn_id=$2").bind(chat).bind(turn).fetch_one(pool).await.unwrap();
                    fosnie_backend::agent::kill(&state,id).await;
                }
            };
            tokio::time::timeout(std::time::Duration::from_secs(3),async {tokio::join!(work,stop);}).await?;
        } else {work.await;}
        let mut completed=0;let mut errors=vec![];
        while let Ok(frame)=rx.try_recv() {match frame {ServerFrame::ChatCompleted {..}=>completed+=1,ServerFrame::ChatError {message,..}=>errors.push(message),_=>{}}}
        assert_eq!(completed,usize::from(scenario==10),"chat {scenario}: {errors:?}");
        assert_eq!(errors.len(),usize::from(scenario!=10),"chat {scenario}: {errors:?}");
        let expected=match scenario {10=>3,11|14|15|19=>1,18=>0,_=>2};
        assert_eq!(count.load(Ordering::SeqCst)-before,expected,"chat {scenario}: {errors:?}");
        if scenario!=18 {
            let (id,status,tokens,checkpoint):(Uuid,String,i32,Value)=sqlx::query_as("SELECT id,status::text,token_used,checkpoint FROM agent_runs WHERE chat_id=$1 AND turn_id=$2").bind(chat).bind(turn).fetch_one(pool).await?;
            assert_eq!(status,if scenario==10 {"completed"} else if scenario==16 {"cancelled"} else {"failed"});
            assert!(!fosnie_backend::agent::alive(&state,id).await);
            assert_eq!(checkpoint["routing_usage"]["calls"],json!(run.usage().calls));
            assert_eq!(checkpoint["routing_usage"]["resumable"],false);
            if scenario==10 {
                assert_eq!(run.usage().calls,6);assert_eq!(tokens,520);
                assert!(checkpoint["messages"].to_string().contains("Test weather evidence"));
                let n:i64=sqlx::query_scalar("SELECT count(*) FROM web_citations c JOIN messages m ON c.message_id=m.id WHERE m.chat_id=$1").bind(chat).fetch_one(pool).await?;
                assert_eq!(n,1);
                let actual:Value=sqlx::query_scalar("SELECT routing_decision FROM messages WHERE chat_id=$1 AND role='assistant'").bind(chat).fetch_one(pool).await?;
                assert_eq!(actual["actual"]["model"],"catalog-model");
            } else if matches!(scenario,12|13|16|17|20) {assert_eq!(run.usage().calls,65);}
            else {assert_eq!(run.usage().calls,1);}
            if scenario!=10 {
                let closed:bool=sqlx::query_scalar("SELECT interrupted_at IS NOT NULL AND completed_at IS NULL FROM messages WHERE chat_id=$1 AND role='assistant'").bind(chat).fetch_one(pool).await?;
                assert!(closed);
            }
            sqlx::query("DELETE FROM agent_runs WHERE id=$1").bind(id).execute(pool).await?;
        } else {
            sqlx::query("DELETE FROM agent_tools WHERE agent_id=$1 AND tool_name='edit_document'").bind(agent).execute(pool).await?;
        }
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    for (params,prompt) in [(json!({"token_budget":1000}),"weather?"),(json!({}),"Create a PDF report")] {
        sqlx::query("UPDATE agents SET params=$2 WHERE id=$1").bind(agent).bind(params).execute(pool).await?;
        let chat=Uuid::now_v7();let before=count.load(Ordering::SeqCst);
        sqlx::query("INSERT INTO chats(id,owner_user_id,agent_id,title) VALUES($1,$2,$3,'Blocked agent')").bind(chat).bind(actor.user_id).bind(agent).execute(pool).await?;
        assert!(fosnie_backend::chat::routing::prepare(&state,&actor,chat,Uuid::now_v7(),prompt,Some(policy_id),RoutingObjective::Balanced,
            WorkloadRequirements {output_tokens:1024,..Default::default()},&[],&tokio::sync::Notify::new()).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst),before);
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    sqlx::query("DELETE FROM agents WHERE id=$1").bind(agent).execute(pool).await?;
    server.abort();Ok(())
}
