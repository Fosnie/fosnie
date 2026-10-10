// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use fosnie_backend::{auth::{AuthContext,PlatformRole,rbac::{ResourceType,Permission,FlatRbacPolicy}},
    model_routing::{*,fixed::{FixedRole,FixedProfile,SaveFixedProfile}},ext::RbacPolicy};
use std::sync::{Arc,atomic::{AtomicBool,AtomicUsize,Ordering}};
use uuid::Uuid;
use serde_json::{Value,json};

struct SourceAcl { denied:Uuid,fail:Arc<AtomicBool> }
struct Grounding(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Grounding {
    async fn enabled_for_user(&self,_:&fosnie_backend::AppState,_:Option<Uuid>,feature:&str)->bool {feature=="groundedness" && self.0.load(Ordering::SeqCst)==40}
}
#[async_trait::async_trait]
impl RbacPolicy for SourceAcl {
    async fn can(&self,pool:&sqlx::PgPool,ctx:&AuthContext,kind:ResourceType,id:Uuid,permission:Permission)->fosnie_backend::Result<bool> {
        FlatRbacPolicy.can(pool,ctx,kind,id,permission).await
    }
    async fn may_grant(&self,pool:&sqlx::PgPool,ctx:&AuthContext,kind:ResourceType,id:Uuid)->fosnie_backend::Result<bool> {
        FlatRbacPolicy.may_grant(pool,ctx,kind,id).await
    }
    async fn denied_kb_doc_ids(&self,_:&sqlx::PgPool,ctx:&AuthContext,_:&[Uuid])->fosnie_backend::Result<Vec<Uuid>> {
        assert_eq!(ctx.role,PlatformRole::User,"source ACL receives the original role");
        if self.fail.load(Ordering::SeqCst) { return Err(fosnie_backend::AppError::Unavailable("ACL unavailable".into())); }
        Ok(vec![self.denied])
    }
}

pub async fn check_rag(pool:&sqlx::PgPool,admin:&AuthContext,mut policy:RoutingPolicy)->anyhow::Result<()> {
    use fosnie_backend::{chat::{run_turn,origin::TurnContext},ws::protocol::ServerFrame,config::BootConfig,state::AppStateBuilder};
    let kb=Uuid::now_v7(); let denied=Uuid::now_v7();
    let count=Arc::new(AtomicUsize::new(0)); let mode=Arc::new(AtomicUsize::new(0)); let acl_fail=Arc::new(AtomicBool::new(false));
    let steps=Arc::new(AtomicUsize::new(0));
    let part_started=Arc::new(tokio::sync::Notify::new());
    let actual_evidence=Arc::new(std::sync::Mutex::new(String::new()));
    let app=axum::Router::new().route("/retrieve",axum::routing::post({
        let count=count.clone(); let mode=mode.clone();
        move |axum::Json(body):axum::Json<Value>| { let count=count.clone(); let mode=mode.clone(); async move {
            count.fetch_add(1,Ordering::SeqCst);
            assert_eq!(body["kb_ids"],json!([kb])); assert_eq!(body["deny_doc_ids"],json!([denied]));
            let routing=&body["overrides"]["routing"];
            assert_eq!(routing["fixed_assignments"]["embed"]["embedding_collection"],"routed_chat_index");
            assert_eq!(routing["fixed_assignments"]["embed"]["model"],"routed-embed");
            assert!(routing["fixed_assignments"].get("embed_desired").is_none());
            let mut calls=vec![];
            for purpose in ["retrieval_decompose","retrieval_grade"] {
                let mut one=body.clone(); one["overrides"]["routing"]["purpose"]=json!(purpose);
                calls.push(super::routing_mock_report(&one)["calls"][0].clone());
            }
            let fixed_calls:Vec<Value>=["embed","rerank"].into_iter().map(|role|{
                let target=&routing["fixed_assignments"][role];
                json!({"call_id":Uuid::now_v7(),"lease_id":routing["fixed_budget"]["id"],"decision_id":routing["decision_id"],
                    "role":role,"provider_id":target["provider_id"],"provider_revision":target["provider_revision"],"profile_revision":target["profile_revision"],
                    "active_index_revision":target["active_index_revision"],"status":"completed","latency_ms":1,"input_bytes":20,"output_bytes":10})
            }).collect();
            let mut done=json!({"type":"done","context":"The authorized library says the notice period is thirty days.","citations":[],
                "routing_report":{"lease_id":routing["budget"]["id"],"complete":true,"calls":calls},
                "fixed_routing_report":{"lease_id":routing["fixed_budget"]["id"],"complete":true,"calls":fixed_calls}});
            let scenario=mode.load(Ordering::SeqCst);
            if (20..34).contains(&scenario) {
                let n=if scenario==29 {17} else if matches!(scenario,24|27) {4} else {3};
                done["parts"]=json!((0..n).map(|i|json!({"title":format!("Part {}",(b'A'+i as u8) as char),
                    "context":if scenario==30 && i==2 {"oversized ".repeat(20000)} else {format!("Evidence slice {}",(b'A'+i as u8) as char)},"has_evidence":true})).collect::<Vec<_>>());
                if scenario==25 {
                    let mut calls=vec![];
                    for (purpose,n) in [("retrieval_decompose",8),("retrieval_grade",64)] {
                        for _ in 0..n {let mut one=body.clone();one["overrides"]["routing"]["purpose"]=json!(purpose);calls.push(super::routing_mock_report(&one)["calls"][0].clone());}
                    }
                    let mut fixed=vec![];
                    for (source,n) in [(0,32),(1,14)] {for _ in 0..n {let mut r=done["fixed_routing_report"]["calls"][source].clone();r["call_id"]=json!(Uuid::now_v7());fixed.push(r);}}
                    done["routing_report"]["calls"]=json!(calls);done["fixed_routing_report"]["calls"]=json!(fixed);
                }
            }
            let top_up=body["max_rounds"]==1;
            if top_up { done["context"]=json!("Documents:\n[D1] An extra passage confirms thirty days."); }
            let mut stream="{\"type\":\"progress\",\"stage\":\"search\"}\n".to_string();
            if mode.load(Ordering::SeqCst)==1 || (top_up && matches!(mode.load(Ordering::SeqCst),12|13)) { done.as_object_mut().unwrap().remove("fixed_routing_report"); }
            if mode.load(Ordering::SeqCst)!=2 { stream.push_str(&done.to_string()); stream.push('\n'); }
            stream
        }}
    })).route("/chat-step",axum::routing::post({
        let count=count.clone(); let mode=mode.clone(); let steps=steps.clone();
        move |axum::Json(body):axum::Json<Value>| { let count=count.clone(); let mode=mode.clone(); let steps=steps.clone(); async move {
            count.fetch_add(1,Ordering::SeqCst);
            assert_eq!(body["overrides"]["routing"]["purpose"],"tool_loop");
            assert_eq!(body["tools"][0]["function"]["name"],"search_library");
            let mut response=json!({"content":"Thirty days.","usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)});
            if steps.fetch_add(1,Ordering::SeqCst)==0 {
                response["tool_calls"]=json!([{"id":"search-1","name":if mode.load(Ordering::SeqCst)==14 {"web_search"} else {"search_library"},"arguments":{"query":"Find the notice section"}}]);
            } else { assert!(body["messages"].to_string().contains("extra passage")); }
            axum::Json(response)
        }}
    })).route("/generate",axum::routing::post({
        let count=count.clone(); let mode=mode.clone();let part_started=part_started.clone();let acl_fail=acl_fail.clone();let actual_evidence=actual_evidence.clone();
        move |axum::Json(body):axum::Json<Value>| { let count=count.clone(); let mode=mode.clone();let part_started=part_started.clone();let acl_fail=acl_fail.clone();let actual_evidence=actual_evidence.clone(); async move {
            count.fetch_add(1,Ordering::SeqCst);
            let scenario=mode.load(Ordering::SeqCst);
            if scenario==40 {
                assert!(body["messages"].to_string().contains("thirty days"));
                *actual_evidence.lock().unwrap()=body["messages"][0]["content"].as_str().unwrap().split_once("<retrieved-context>\n").unwrap().1.split_once("\n</retrieved-context>").unwrap().0.into();
                return format!("{}\n{}\n{}\n",json!({"type":"reasoning","delta":"Private reasoning"}),json!({"type":"token","delta":"Thirty days."}),
                    json!({"type":"done","model":"catalog-model","finish_reason":"stop","usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)}));
            }
            if scenario>=20 {
                let messages=body["messages"].to_string();
                assert!(messages.contains("Original multipart question"),"original request must survive per-part composition");
                assert!(messages.contains("Previous user context") && messages.contains("Previous assistant context"),"history must survive per-part composition");
                assert_eq!(body["overrides"]["routing"]["purpose"],"final");
                assert!(body["tools"].is_null());
                let system=body["messages"][0]["content"].as_str().unwrap();
                let label=if system.contains("Evidence slice A") {'A'} else if system.contains("Evidence slice B") {'B'} else if system.contains("Evidence slice C") {'C'} else {'D'};
                assert_ne!(label,'D',"queued part must not run after cancellation or ACL failure");
                for other in ['A','B','C'] {if label!=other {assert!(!system.contains(&format!("Evidence slice {other}")));}}
                let continued=body["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().starts_with("Continue the answer");
                if continued {assert!(messages.contains("Answer A."));}
                if matches!(scenario,24|32) {part_started.notify_one();tokio::time::sleep(std::time::Duration::from_secs(5)).await;}
                if scenario==27 {acl_fail.store(true,Ordering::SeqCst);}
                // Later parts finish first; the client must still see A, B, C.
                tokio::time::sleep(std::time::Duration::from_millis(if label=='A' {180} else {20})).await;
                let mut terminal=json!({"type":"done","model":if scenario==26 && label=='B' {"wrong-model"} else {"catalog-model"},
                    "finish_reason":if scenario==31 || (scenario==28 && label=='A' && !continued) {"length"} else {"stop"},
                    "usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)});
                if scenario==21 && label=='B' {terminal.as_object_mut().unwrap().remove("routing_report");}
                let token=json!({"type":"token","delta":if continued {" Continued A.".into()} else {format!("Answer {label}.")}});
                if scenario==22 && label=='B' {return format!("{token}\n");}
                if scenario==23 && label=='B' {return format!("{}\n{terminal}\n",json!({"type":"tool_call","name":"search_library","arguments":{"query":"invented"}}));}
                if scenario==33 && label=='B' {return format!("{}{}\n{terminal}\n",format!("{token}\n"),(0..80).map(|_|format!("{}\n",json!({"type":"token","delta":" burst"}))).collect::<String>());}
                return format!("{token}\n{terminal}\n");
            }
            assert!(body["messages"].to_string().contains("thirty days"));
            if mode.load(Ordering::SeqCst)>=10 {
                let replay=body["messages"].as_array().unwrap().iter().any(|m|m["role"]=="tool");
                if !replay {
                    assert_eq!(body["tools"][0]["function"]["name"],"search_library");
                    return format!("{}\n{}\n",json!({"type":"tool_call","id":"search-1","name":if mode.load(Ordering::SeqCst)==15 {"web_search"} else {"search_library"},"arguments":{"query":"Find the notice section"}}),
                        json!({"type":"done","model":"catalog-model","finish_reason":"tool_calls","routing_report":super::routing_mock_report(&body),"usage":{"prompt_tokens":100,"completion_tokens":40}}));
                }
                assert!(body["messages"].to_string().contains("extra passage"));
            }
            format!("{}\n{}\n",json!({"type":"token","delta":"Thirty days."}),json!({"type":"done","model":"catalog-model","finish_reason":"stop",
                "usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)}))
        }}
    })).route("/verify",axum::routing::post({
        let count=count.clone();let actual_evidence=actual_evidence.clone();
        move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let actual_evidence=actual_evidence.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            assert_eq!(body["answer"],"Thirty days.");assert_eq!(body["context"],*actual_evidence.lock().unwrap(),"verify exactly the evidence the answering model received");
            let r=&body["overrides"]["routing"];let t=&r["fixed_assignments"]["verify"];
            axum::Json(json!({"score":0.5,"total":1,"flagged":1,"model":"fixed-model",
                "spans":[{"start":0,"end":6,"text":"Thirty","label":"contradicted","score":0.9}],
                "fixed_routing_report":{"lease_id":r["fixed_budget"]["id"],"complete":true,"calls":[{
                    "call_id":Uuid::now_v7(),"lease_id":r["fixed_budget"]["id"],"decision_id":r["decision_id"],"role":"verify",
                    "provider_id":t["provider_id"],"provider_revision":t["provider_revision"],"profile_revision":t["profile_revision"],
                    "status":"completed","latency_ms":1,"input_bytes":100,"output_bytes":100}]}}))
        }}
    }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?; let address=listener.local_addr()?;
    let server=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
    let mut boot=BootConfig::default(); boot.ml.base_url=format!("http://{address}");
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
        .with_rbac(Arc::new(SourceAcl { denied,fail:acl_fail.clone() })).with_features(Arc::new(Grounding(mode.clone()))).build();
    let mut actor=admin.clone(); actor.role=PlatformRole::User;
    policy.strategy=RoutingStrategy::Rules; policy.rules.clear(); policy.constraints.max_calls=Some(200);
    policy=store::save_policy(&state,admin,policy.id,store::SavePolicy { label:"Routed RAG".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone() }).await?;
    for (key,value,kind) in [("rag.model_search_mode","off","string"),("rag.synthesis_mode","unified","string"),("rag.rerank_enabled","true","bool")] {
        sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,$2,$3::config_value_type) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
            .bind(key).bind(value).bind(kind).execute(pool).await?;
    }
    sqlx::query("INSERT INTO embedding_index(id,embed_model,embed_base_url,dim,collection_name) VALUES(1,'routed-embed','http://embed.test/v1',384,'routed_chat_index')").execute(pool).await?;
    let mut index:i64=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_one(pool).await?;
    let (embed,revision):(Uuid,i64)=sqlx::query_as("SELECT id,routing_revision FROM provider_configs WHERE role='embed' AND scope='deployment' LIMIT 1").fetch_one(pool).await?;
    let mut profile=fixed::save_profile(&state,admin,embed,SaveFixedProfile { expected_revision:0,profile:FixedProfile {
        provider_id:embed,provider_revision:revision,revision:1,role:FixedRole::Embed,model:"routed-embed".into(),active_index_revision:Some(index),
        desired_index_revision:None,locality:Locality::Local,locality_trusted:true,ocr_pdf_mode:None
    }}).await?;
    sqlx::query("INSERT INTO knowledge_bases(id,name,owner_id,embedding_model_id,embedding_dimension,status) VALUES($1,'Routing library',$2,'routed-embed',384,'ready')")
        .bind(kb).bind(actor.user_id).execute(pool).await?;
    for scenario in 0..6 {
        mode.store(scenario,Ordering::SeqCst);
        let chat=Uuid::now_v7(); let turn=Uuid::now_v7(); let cancel=Arc::new(tokio::sync::Notify::new());
        sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Existing RAG chat')").bind(chat).bind(actor.user_id).execute(pool).await?;
        sqlx::query("INSERT INTO chat_kb_links(chat_id,kb_id,attached_by) VALUES($1,$2,$3)").bind(chat).bind(kb).bind(actor.user_id).execute(pool).await?;
        let run=fosnie_backend::chat::routing::prepare(&state,&actor,chat,turn,"What notice?",Some(policy.id),RoutingObjective::Balanced,
            WorkloadRequirements { output_tokens:1024,..Default::default() },&[],&cancel).await?;
        assert_eq!(run.usage().calls,0); assert_eq!(run.plan().fixed_assignments[&FixedRole::Embed].active_index_revision,Some(index));
        if scenario==3 { acl_fail.store(true,Ordering::SeqCst); }
        if scenario==4 { sqlx::query("UPDATE config_settings SET value='always' WHERE key='rag.model_search_mode'").execute(pool).await?; }
        if scenario==5 { sqlx::query("UPDATE embedding_index SET collection_name='changed_index' WHERE id=1").execute(pool).await?; }
        let before=count.load(Ordering::SeqCst); let (tx,mut rx)=tokio::sync::mpsc::channel(128);
        run_turn(&state,TurnContext::web(&actor).with_routing(&run),turn,Some(chat),None,None,"What notice?".into(),vec![],vec![],false,None,None,None,None,&tx,cancel).await;
        let mut errors=vec![]; let mut completed=0;
        while let Ok(frame)=rx.try_recv() { match frame { ServerFrame::ChatCompleted {..}=>completed+=1,ServerFrame::ChatError {message,..}=>errors.push(message),_=>{} } }
        assert_eq!(completed,usize::from(scenario==0),"scenario {scenario}: {errors:?}");
        assert_eq!(errors.len(),usize::from(scenario!=0),"scenario {scenario}: {errors:?}");
        assert_eq!(count.load(Ordering::SeqCst)-before,if scenario==0 {2} else if scenario<=2 {1} else {0},"no unplanned fallback: {errors:?}");
        if scenario==0 { assert_eq!(run.usage().calls,5); assert_eq!(run.usage().tokens,420); }
        if scenario==1 || scenario==2 { assert_eq!(run.usage().calls,120,"both lanes retain their reservations"); }
        acl_fail.store(false,Ordering::SeqCst);
        if scenario==4 { sqlx::query("UPDATE config_settings SET value='off' WHERE key='rag.model_search_mode'").execute(pool).await?; }
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    // Restore/reattest the deliberately invalidated index for model-driven search.
    sqlx::query("UPDATE embedding_index SET collection_name='routed_chat_index' WHERE id=1").execute(pool).await?;
    index=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_one(pool).await?;
    profile.active_index_revision=Some(index);
    profile=fixed::save_profile(&state,admin,embed,SaveFixedProfile { expected_revision:profile.revision,profile }).await?;
    let mut llm=store::current_profiles(store::catalog(&state,admin).await?).into_iter().find(|p|Some(p.provider_id)==policy.default_provider_id).unwrap();
    llm.capabilities.tools=EvidenceStatus::Declared;
    store::save_profile(&state,admin,llm.provider_id,store::SaveProfile { expected_revision:llm.revision,profile:llm }).await?;
    sqlx::query("UPDATE config_settings SET value='always' WHERE key='rag.model_search_mode'").execute(pool).await?;
    for scenario in 10..18 {
        mode.store(scenario,Ordering::SeqCst); steps.store(0,Ordering::SeqCst);
        policy.constraints.max_calls=Some(if scenario>=16 {120} else {200});
        policy=store::save_policy(&state,admin,policy.id,store::SavePolicy { label:"Search loop".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone() }).await?;
        let locus=if scenario%2==0 {"loop"} else {"midstream"};
        sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES('rag.model_search_locus',$1,'string') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
            .bind(locus).execute(pool).await?;
        let chat=Uuid::now_v7(); let turn=Uuid::now_v7(); let cancel=Arc::new(tokio::sync::Notify::new());
        sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Search test')").bind(chat).bind(actor.user_id).execute(pool).await?;
        sqlx::query("INSERT INTO chat_kb_links(chat_id,kb_id,attached_by) VALUES($1,$2,$3)").bind(chat).bind(kb).bind(actor.user_id).execute(pool).await?;
        let run=fosnie_backend::chat::routing::prepare(&state,&actor,chat,turn,"What notice?",Some(policy.id),RoutingObjective::Balanced,
            WorkloadRequirements { output_tokens:1024,..Default::default() },&[],&cancel).await?;
        let before=count.load(Ordering::SeqCst); let (tx,mut rx)=tokio::sync::mpsc::channel(128);
        run_turn(&state,TurnContext::web(&actor).with_routing(&run),turn,Some(chat),None,None,"What notice?".into(),vec![],vec![],false,None,None,None,None,&tx,cancel).await;
        let mut completed=0; let mut errors=vec![];
        while let Ok(frame)=rx.try_recv() { match frame {ServerFrame::ChatCompleted {..}=>completed+=1,ServerFrame::ChatError {message,..}=>errors.push(message),_=>{}} }
        let success=scenario<12;
        assert_eq!(completed,usize::from(success),"{locus}/{scenario}: {errors:?}");
        assert_eq!(errors.len(),usize::from(!success),"{locus}/{scenario}: {errors:?}");
        let expected_http=if success {4} else if scenario<14 {3} else {2};
        assert_eq!(count.load(Ordering::SeqCst)-before,expected_http,"no extra or legacy inference: {errors:?}");
        if success {
            assert_eq!(run.usage().calls,10); assert_eq!(run.usage().tokens,840);
            let actual:Value=sqlx::query_scalar("SELECT routing_decision FROM messages WHERE chat_id=$1 AND role='assistant'").bind(chat).fetch_one(pool).await?;
            assert_eq!(actual["actual"]["provider_id"],json!(policy.default_provider_id.unwrap()));
        } else {
            let closed:bool=sqlx::query_scalar("SELECT interrupted_at IS NOT NULL AND completed_at IS NULL FROM messages WHERE chat_id=$1 AND role='assistant'").bind(chat).fetch_one(pool).await?;
            assert!(closed);
            if scenario==12 || scenario==13 { assert_eq!(run.usage().calls,125,"failed top-up retains both reservations"); }
            if scenario>=16 { assert_eq!(run.usage().calls,5,"failed reservation is atomic"); }
        }
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    // Per-part workers share one ledger and cancellation lifetime. Later parts
    // complete first, including a stream larger than the bounded worker channel.
    for scenario in 20..34 {
        mode.store(scenario,Ordering::SeqCst);acl_fail.store(false,Ordering::SeqCst);
        for (key,value) in [("rag.synthesis_mode","per_part"),("rag.model_search_mode","off"),("rag.model_search_locus","loop"),
            ("chat.answer_max_continuations",if scenario==31 {"0"} else {"4"}),
            ("rag.synthesis_part_max_secs",if scenario==32 {"1"} else {"300"})] {
            sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,$2,'string') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
                .bind(key).bind(value).execute(pool).await?;
        }
        policy.constraints.max_calls=Some(if scenario==25 {120} else {200});
        policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Per-part RAG".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone()}).await?;
        let chat=Uuid::now_v7();let turn=Uuid::now_v7();let cancel=Arc::new(tokio::sync::Notify::new());
        sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Per-part test')").bind(chat).bind(actor.user_id).execute(pool).await?;
        sqlx::query("INSERT INTO chat_kb_links(chat_id,kb_id,attached_by) VALUES($1,$2,$3)").bind(chat).bind(kb).bind(actor.user_id).execute(pool).await?;
        for (seq,role,content) in [(1,"user","Previous user context"),(2,"assistant","Previous assistant context")] {
            sqlx::query("INSERT INTO messages(id,chat_id,sequence_number,role,content,completed_at) VALUES($1,$2,$3,$4::message_role,$5,now())")
                .bind(Uuid::now_v7()).bind(chat).bind(seq).bind(role).bind(content).execute(pool).await?;
        }
        let question="Original multipart question: explain Part A, Part B and Part C.";
        if scenario==20 {
            let before=count.load(Ordering::SeqCst);
            for (key,value,restore) in [("rag.model_search_mode","always","off"),("rag.synthesis_mode","unknown","per_part")] {
                sqlx::query("UPDATE config_settings SET value=$2 WHERE key=$1").bind(key).bind(value).execute(pool).await?;
                assert!(fosnie_backend::chat::routing::prepare(&state,&actor,chat,turn,question,Some(policy.id),RoutingObjective::Balanced,
                    WorkloadRequirements {output_tokens:1024,..Default::default()},&[],&cancel).await.is_err(),"unsupported synthesis combination must fail before inference");
                sqlx::query("UPDATE config_settings SET value=$2 WHERE key=$1").bind(key).bind(restore).execute(pool).await?;
            }
            assert_eq!(count.load(Ordering::SeqCst),before);
        }
        let run=fosnie_backend::chat::routing::prepare(&state,&actor,chat,turn,question,Some(policy.id),RoutingObjective::Balanced,
            WorkloadRequirements {output_tokens:1024,..Default::default()},&[],&cancel).await?;
        let before=count.load(Ordering::SeqCst);let (tx,mut rx)=tokio::sync::mpsc::channel(256);
        let work=Box::pin(run_turn(&state,TurnContext::web(&actor).with_routing(&run),turn,Some(chat),None,None,question.into(),vec![],vec![],false,None,None,None,None,&tx,cancel.clone()));
        if scenario==24 {
            tokio::time::timeout(std::time::Duration::from_secs(3),async {
                tokio::join!(work,async {part_started.notified().await;cancel.notify_one();});
            }).await?;
            let stopped=count.load(Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            assert_eq!(count.load(Ordering::SeqCst),stopped,"cancelled children must not dispatch more requests");
        } else {tokio::time::timeout(std::time::Duration::from_secs(8),work).await?;}
        let mut completed=0;let mut errors=vec![];let mut text=String::new();
        while let Ok(frame)=rx.try_recv() {match frame {
            ServerFrame::ChatCompleted {..}=>completed+=1,ServerFrame::ChatError {message,..}=>errors.push(message),
            ServerFrame::ChatToken {delta,..}=>text.push_str(&delta),_=>{}
        }}
        let success=matches!(scenario,20|28|31|33);
        assert_eq!(completed,usize::from(success),"per-part {scenario}: {errors:?}");
        // Explicit cancellation uses the existing cancelled frame path.
        if scenario!=24 {assert_eq!(errors.len(),usize::from(!success),"per-part {scenario}: {errors:?}");}
        let http=count.load(Ordering::SeqCst)-before;
        if success {
            assert_eq!(http,if scenario==28 {5} else {4},"per-part {scenario}");
            assert_eq!(run.usage().calls,if scenario==28 {8} else {7});
            assert_eq!(run.usage().tokens,if scenario==28 {840} else {700});
            let a=text.find("Answer A.").expect("part A");let b=text.find("Answer B.").expect("part B");let c=text.find("Answer C.").expect("part C");
            assert!(a<b && b<c,"parts must stay in request order");
            if scenario==28 {assert!(text.find("Continued A.").unwrap()<b);}
            if scenario==33 {assert_eq!(text.matches(" burst").count(),80);}
            let (saved,actual):(String,Value)=sqlx::query_as("SELECT content,routing_decision FROM messages WHERE chat_id=$1 AND role='assistant' AND sequence_number>2").bind(chat).fetch_one(pool).await?;
            assert_eq!(saved,text);assert_eq!(actual["actual"]["provider_id"],json!(policy.default_provider_id.unwrap()));
        } else {
            let closed:bool=sqlx::query_scalar("SELECT interrupted_at IS NOT NULL AND completed_at IS NULL FROM messages WHERE chat_id=$1 AND role='assistant' AND sequence_number>2").bind(chat).fetch_one(pool).await?;
            assert!(closed,"per-part {scenario} must close the assistant message");
            if matches!(scenario,29|30) {assert_eq!(http,1);assert_eq!(run.usage().calls,4,"reject every part before fan-out");}
            else {assert!(http<=4,"no fallback or queued fourth request: {scenario}/{http}");}
            if scenario==25 {assert!(run.usage().calls<=120);assert!(http<=3,"only two final reservations fit");}
        }
        acl_fail.store(false,Ordering::SeqCst);
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    for (key,value) in [("rag.synthesis_mode","unified"),("rag.synthesis_part_max_secs","300")] {
        sqlx::query("UPDATE config_settings SET value=$2 WHERE key=$1").bind(key).bind(value).execute(pool).await?;
    }
    mode.store(40,Ordering::SeqCst);
    let chat=Uuid::now_v7();let turn=Uuid::now_v7();let cancel=Arc::new(tokio::sync::Notify::new());
    sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Grounded answer')").bind(chat).bind(actor.user_id).execute(pool).await?;
    sqlx::query("INSERT INTO chat_kb_links(chat_id,kb_id,attached_by) VALUES($1,$2,$3)").bind(chat).bind(kb).bind(actor.user_id).execute(pool).await?;
    let run=fosnie_backend::chat::routing::prepare(&state,&actor,chat,turn,"What notice?",Some(policy.id),RoutingObjective::Balanced,
        WorkloadRequirements {output_tokens:1024,..Default::default()},&[],&cancel).await?;
    assert!(run.plan().fixed_assignments.contains_key(&FixedRole::Verify));
    let (tx,mut rx)=tokio::sync::mpsc::channel(128);let (live_tx,mut live_rx)=tokio::sync::mpsc::channel(8);let socket=Uuid::now_v7();
    state.hub.register(socket,actor.user_id.unwrap(),None,live_tx);
    let before=count.load(Ordering::SeqCst);
    run_turn(&state,TurnContext::web(&actor).with_routing(&run),turn,Some(chat),None,None,"What notice?".into(),vec![],vec![],false,None,None,None,None,&tx,cancel).await;
    let mut completed=false;
    while let Ok(frame)=rx.try_recv() {match frame {ServerFrame::ChatCompleted {..}=>completed=true,ServerFrame::ChatError {message,..}=>panic!("{message}"),_=>{}}}
    assert!(completed);
    let frame=tokio::time::timeout(std::time::Duration::from_secs(3),live_rx.recv()).await?.unwrap();
    assert!(matches!(frame,ServerFrame::ChatGroundedness {score:Some(0.5),flagged:1,..}));
    assert_eq!(count.load(Ordering::SeqCst)-before,3);assert_eq!(run.usage().calls,6);assert_eq!(run.usage().tokens,420);
    let summary:Value=sqlx::query_scalar("SELECT groundedness FROM messages WHERE chat_id=$1 AND role='assistant'").bind(chat).fetch_one(pool).await?;
    assert_eq!(summary["spans"][0]["text"],"Thirty");
    let audit:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type='groundedness.verified' AND payload->>'chat_id'=$1").bind(chat.to_string()).fetch_one(pool).await?;
    assert_eq!(audit,1);
    sqlx::query("DELETE FROM verification_runs WHERE target_id IN (SELECT id FROM messages WHERE chat_id=$1)").bind(chat).execute(pool).await?;
    sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    mode.store(0,Ordering::SeqCst);
    fixed::delete_profile(&state,admin,embed,profile.revision).await?;
    sqlx::query("DELETE FROM embedding_index WHERE id=1").execute(pool).await?;
    sqlx::query("DELETE FROM knowledge_bases WHERE id=$1").bind(kb).execute(pool).await?;
    server.abort();
    Ok(())
}
