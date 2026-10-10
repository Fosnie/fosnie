// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Synthetic HTTP acceptance: no live models, credentials or notifications.
use std::sync::{Arc,Mutex,atomic::{AtomicUsize,AtomicBool,Ordering}};
use axum::{body::{Body,to_bytes},http::{Request,StatusCode},response::IntoResponse};
use serde_json::{json,Value};
use sqlx::{Connection,Executor};
use tower::ServiceExt;
use uuid::Uuid;
use fosnie_backend::{AppState,auth::{AuthContext,PlatformRole},config::BootConfig,state::AppStateBuilder,
    model_routing::{*,store},ext::{ProviderRegistry,ResolvedProvider}};

struct Features;
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Features {
    async fn enabled_for_user(&self,_:&AppState,_:Option<Uuid>,feature:&str)->bool {feature=="public_api"}
}
struct Registry(Arc<Mutex<Vec<Uuid>>>);
#[async_trait::async_trait]
impl ProviderRegistry for Registry {
    async fn resolve(&self,_:&sqlx::PgPool,_:&str,_:Option<Uuid>)->fosnie_backend::Result<Option<ResolvedProvider>> {panic!("no legacy resolution")}
    async fn resolve_llm_id(&self,p:&sqlx::PgPool,u:Option<Uuid>,id:Uuid)->fosnie_backend::Result<Option<ResolvedProvider>> {
        self.0.lock().unwrap().push(id);
        fosnie_backend::providers::DbProviderRegistry::new(None).resolve_llm_id(p,u,id).await
    }
}
struct Restricted {deny:Arc<AtomicBool>,alternate:Uuid}
#[async_trait::async_trait]
impl InferencePolicy for Restricted {
    fn single_candidate_fast_path(&self)->bool {true}
    async fn constraints(&self,p:&sqlx::PgPool,_:&RoutingContext)->fosnie_backend::Result<RoutingConstraints> {
        let ids:Vec<Uuid>=sqlx::query_scalar("SELECT id FROM provider_configs").fetch_all(p).await?;
        Ok(RoutingConstraints {local_only:true,allowed_providers:Some(ids.into_iter()
            .filter(|id|!self.deny.load(Ordering::SeqCst)||*id!=self.alternate).collect()),..Default::default()})
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {fn drop(&mut self){self.0.abort();}}
fn report(body:&Value,failed:bool)->Value {
    let routing=&body["overrides"]["routing"];let purpose=routing["purpose"].as_str().unwrap();let target=&routing["assignments"][purpose];
    json!({"lease_id":routing["budget"]["id"],"complete":true,"calls":[{
        "lease_id":routing["budget"]["id"],"call_id":Uuid::now_v7(),"decision_id":routing["decision_id"],
        "provider_id":target["provider_id"],"provider_revision":target["provider_revision"],"profile_revision":target["profile_revision"],
        "purpose":purpose,"status":if failed {"failed"} else {"completed"},"latency_ms":1,
        "usage":if failed {json!({"input_tokens":null,"output_tokens":null,"total_tokens":null,"reasoning_tokens":null,"cached_input_tokens":null,"raw_usage":{}})}
            else {json!({"input_tokens":12,"output_tokens":8,"total_tokens":20,"reasoning_tokens":null,"cached_input_tokens":null,"raw_usage":{}})}
    }]})
}
async fn raw_request(state:AppState,token:&str,body:Value)->anyhow::Result<(StatusCode,String)> {
    sqlx::query("DELETE FROM routing_provider_health").execute(&state.pg).await?;
    sqlx::query("DELETE FROM routing_credential_health").execute(&state.pg).await?;
    let token=token.to_owned();
    tokio::spawn(async move {
        let response=fosnie_backend::http::v1::router(&state).with_state(state).oneshot(Request::builder()
            .method("POST").uri("/v1/chat/completions").header("authorization",format!("Bearer {token}"))
            .header("content-type","application/json").body(Body::from(body.to_string()))?).await?;
        let status=response.status();let bytes=to_bytes(response.into_body(),1_000_000).await?;
        Ok::<_,anyhow::Error>((status,String::from_utf8(bytes.to_vec())?))
    }).await?
}
async fn request(state:AppState,token:&str,body:Value)->anyhow::Result<(StatusCode,Value)> {
    let (status,text)=raw_request(state,token,body).await?;Ok((status,serde_json::from_str(&text)?))
}
async fn save(state:&AppState,admin:&AuthContext,mut policy:RoutingPolicy)->anyhow::Result<RoutingPolicy> {
    policy.revision=sqlx::query_scalar("SELECT revision FROM routing_policies WHERE id=$1")
        .bind(policy.id).fetch_optional(&state.pg).await?.unwrap_or(0);
    Ok(store::save_policy(state,admin,policy.id,store::SavePolicy {label:"Fallback fixture".into(),scope:"deployment".into(),
        expected_revision:policy.revision,is_default:true,policy}).await?)
}
async fn acceptance(pool:sqlx::PgPool)->anyhow::Result<()> {
    let first=Uuid::from_u128(101);let alternate=Uuid::from_u128(102);let uid=Uuid::now_v7();let foreign=Uuid::now_v7();let manager=Uuid::now_v7();let key=Uuid::now_v7();
    let mode=Arc::new(AtomicUsize::new(0));let deny=Arc::new(AtomicBool::new(false));
    let requests=Arc::new(Mutex::new(Vec::<Value>::new()));let resolved=Arc::new(Mutex::new(Vec::new()));
    let app=axum::Router::new().route("/generate",axum::routing::post({
        let mode=mode.clone();let requests=requests.clone();let pool=pool.clone();let deny=deny.clone();
        move |axum::Json(body):axum::Json<Value>| {let mode=mode.clone();let requests=requests.clone();let pool=pool.clone();let deny=deny.clone();async move {
            let current=mode.load(Ordering::SeqCst);let id=body["overrides"]["routing"]["assignments"]["final"]["provider_id"].as_str().unwrap().parse::<Uuid>().unwrap();
            requests.lock().unwrap().push(body.clone());
            assert!([first,alternate].contains(&id),"weak, incompatible, cloud or foreign target dispatched");
            if current>=30 {
                if id==first && current==36 {deny.store(true,Ordering::SeqCst);}
                if id==first && current==37 {sqlx::query("UPDATE api_keys SET revoked_at=now() WHERE id=$1").bind(key).execute(&pool).await.unwrap();}
                if id==first && current==42 {sqlx::query("UPDATE routing_policies SET revision=revision+1 WHERE is_default").execute(&pool).await.unwrap();}
                if id==first && current==43 {sqlx::query("UPDATE provider_configs SET model='changed' WHERE id=$1").bind(first).execute(&pool).await.unwrap();}
                if id==first && current==45 || id==alternate && current==35 {
                    return Body::from(format!("{}\n",json!({"type":"error","message":"secret-must-not-escape","routing_report":report(&body,true),
                        "routing_failure":{"schema_version":1,"kind":"unavailable","before_output":true}}))).into_response();
                }
                let answer=if current==44 || id==alternate && ![34,45].contains(&current) {"{\"answer\":\"Accepted\"}".to_owned()}
                    else {match current {32=>"   ".into(),33=>"{}".into(),41=>"x".repeat(1_048_577),_=>"rejected-content-never-exposed".into()}};
                let mut done=json!({"type":"done","finish_reason":match current {38=>"length",39=>"content_filter",_=>"stop"},"routing_report":report(&body,false)});
                if current==40 {done["type"]=json!("tool_call");}
                return Body::from(format!("{}\n{}\n{}\n",json!({"type":"reasoning","delta":"private-reasoning"}),json!({"type":"token","delta":answer}),done)).into_response();
            }
            if id==alternate && current!=4 {
                return Body::from(format!("{}\n{}\n",json!({"type":"token","delta":"Fallback answer"}),
                    json!({"type":"done","finish_reason":"stop","routing_report":report(&body,false)}))).into_response();
            }
            if current==11 {sqlx::query("UPDATE api_keys SET revoked_at=now() WHERE id=$1").bind(key).execute(&pool).await.unwrap();}
            if current==12 {deny.store(true,Ordering::SeqCst);}
            if current==13 {sqlx::query("UPDATE routing_policies SET revision=revision+1 WHERE is_default").execute(&pool).await.unwrap();}
            if current==14 {sqlx::query("UPDATE routing_api_alias SET enabled=false,revision=revision+1 WHERE id=1").execute(&pool).await.unwrap();}
            if [5,6].contains(&current) {return (match current {1=>StatusCode::SERVICE_UNAVAILABLE,5=>StatusCode::UNAUTHORIZED,6=>StatusCode::BAD_REQUEST,_=>StatusCode::TOO_MANY_REQUESTS},"synthetic rejection").into_response();}
            let mut error=json!({"type":"error","message":"secret-must-not-escape","routing_report":report(&body,true),
                "routing_failure":{"schema_version":1,"kind":match current {2=>"connection",3=>"unavailable",16=>"invented",_=>"rate_limited"},"before_output":current!=15}});
            if current==7 {error.as_object_mut().unwrap().remove("routing_report");}
            if current==8 {error["routing_report"]["calls"][0]["provider_id"]=json!(Uuid::now_v7());}
            if current==17 {error["routing_report"]["complete"]=json!(false);}
            if current==18 {error["routing_report"]["calls"][0]["status"]=json!("completed");}
            if current==19 {error["routing_report"]["calls"]=json!([]);}
            let prefix=match current {9=>format!("{}\n",json!({"type":"token","delta":"partial"})),10=>format!("{}\n",json!({"type":"reasoning","delta":"partial"})),_=>String::new()};
            Body::from(format!("{prefix}{error}\n")).into_response()
        }}
    })).route("/chat-step",axum::routing::post({let requests=requests.clone();move |axum::Json(body):axum::Json<Value>| {
        let requests=requests.clone();async move {requests.lock().unwrap().push(body.clone());
            axum::Json(json!({"content":json!({"task":"general","minimum_tier":"advanced","fresh_information":false,
                "tools":false,"vision":false,"structured_output":false,"confidence":0.99,"uncertain":false}).to_string(),"routing_report":report(&body,false)}))}
    }}));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
    let _server=Server(tokio::spawn(async move{axum::serve(listener,app).await.unwrap();}));
    let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{address}");
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
        .with_features(Arc::new(Features)).with_providers(Arc::new(Registry(resolved.clone())))
        .with_inference_policy(Arc::new(Restricted {deny:deny.clone(),alternate})).build();
    for id in [uid,foreign,manager] {sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Synthetic fallback',$3::platform_role)")
        .bind(id).bind(format!("{id}@routing.test")).bind(if id==manager {"client_admin"} else {"user"}).execute(&pool).await?;}
    let admin=AuthContext {user_id:Some(manager),role:PlatformRole::ClientAdmin,email:None,display_name:None,break_glass:false,mfa_enroll_only:false};
    let (token,hash,prefix)=fosnie_backend::auth::api_key::mint();
    sqlx::query("INSERT INTO api_keys(id,user_id,name,token_hash,display_prefix) VALUES($1,$2,'fallback',$3,$4)").bind(key).bind(uid).bind(hash).bind(prefix).execute(&pool).await?;
    for (id,tier,efforts,locality,owner,cost) in [(first,"advanced",vec!["high"],"local",None,0),(alternate,"advanced",vec!["high"],"local",None,1_000_000),
        (Uuid::from_u128(103),"economy",vec!["high"],"local",None,0),(Uuid::from_u128(104),"advanced",vec![],"local",None,0),
        (Uuid::from_u128(105),"advanced",vec!["high"],"cloud",None,0),(Uuid::from_u128(106),"advanced",vec!["high"],"local",Some(foreign),0)] {
        let model=format!("synthetic-{id}");
        sqlx::query("INSERT INTO provider_configs(id,role,scope,scope_id,label,model,base_url) VALUES($1,'llm',$2,$3,$4,$4,'http://synthetic.test/v1')")
            .bind(id).bind(if owner.is_some(){"user"}else{"deployment"}).bind(owner).bind(&model).execute(&pool).await?;
        let profile:ModelProfile=serde_json::from_value(json!({"provider_id":id,"provider_revision":1,"revision":1,"model":model,"adapter":"openai_chat",
            "locality":locality,"locality_trusted":owner.is_none(),"tier":tier,"quality_status":"declared","auto_enabled":true,"capabilities":{"context_tokens":32000,"max_output_tokens":4096,"structured_output":"declared","efforts":efforts},
            "pricing":{"input_microusd_per_million":cost,"output_microusd_per_million":cost}}))?;
        if owner.is_some() {sqlx::query("INSERT INTO model_profiles(provider_id,provider_revision,profile) VALUES($1,1,$2)").bind(id).bind(serde_json::to_value(profile)?).execute(&pool).await?;}
        else {store::save_profile(&state,&admin,id,store::SaveProfile {expected_revision:0,profile}).await?;}
    }
    sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES('routing.public_api_enabled','true','bool') ON CONFLICT(key) DO UPDATE SET value='true',value_type='bool'").execute(&pool).await?;
    let mut policy:RoutingPolicy=serde_json::from_value(json!({"id":Uuid::now_v7(),"revision":1,
        "default_provider_id":first,"classifier_provider_id":null}))?;
    policy.rollout=RolloutMode::Active;policy.technical_fallback=true;policy.default_provider_id=Some(first);policy.strategy=RoutingStrategy::Rules;
    policy.constraints=RoutingConstraints {local_only:true,max_calls:Some(3),max_total_tokens:Some(100_000),..Default::default()};
    policy.rules=vec![RoutingRule{id:Uuid::now_v7(),priority:2,conditions:Default::default(),action:RuleAction::Require{minimum_tier:ModelTier::Advanced}},
        RoutingRule{id:Uuid::now_v7(),priority:1,conditions:Default::default(),action:RuleAction::Select{provider_id:first,effort:Some("high".into())}}];
    policy=save(&state,&admin,policy).await?;
    let body_policy_id=policy.id;
    let body=||json!({"model_selection":{"mode":"auto","objective":"economy","policy_id":body_policy_id},"max_tokens":128,"messages":[{"role":"user","content":"Synthetic fallback prompt"}]});
    for fault in [0,1,2,3,20] {
        mode.store(fault,Ordering::SeqCst);let before=requests.lock().unwrap().len();let (status,value)=request(state.clone(),&token,body()).await?;
        assert_eq!(status,StatusCode::OK,"fault {fault}: {value}");assert_eq!(requests.lock().unwrap().len(),before+2);
        let routing=&value["fosnie_routing"];assert_eq!(routing["decision"]["proposed"]["provider_id"],json!(first));
        assert_eq!(routing["decision"]["actual"]["provider_id"],json!(alternate));assert_eq!(routing["decision"]["actual"]["effort"],"high");
        assert_eq!(value["model"],format!("synthetic-{alternate}"));assert_eq!(routing["usage"]["calls"],2);
        assert!(routing["usage"]["tokens"].as_u64().unwrap()>20,"failed spend stays reserved");
        assert_eq!(value["usage"]["total_tokens"],20);assert_eq!(routing["fallback"]["retry_index"],1);
        assert!(!value.to_string().contains("secret-must-not-escape"));
    }

    // A model-specific preset must not leak into the cross-model retry witness.
    let mut first_profile=store::current_profiles(store::catalog(&state,&admin).await?).into_iter().find(|p|p.provider_id==first).unwrap();
    first_profile.capabilities.efforts.insert("none".into());first_profile.auto_effort=Some("none".into());
    store::save_profile(&state,&admin,first,store::SaveProfile{expected_revision:first_profile.revision,profile:first_profile}).await?;
    let mut alternate_profile=store::current_profiles(store::catalog(&state,&admin).await?).into_iter().find(|p|p.provider_id==alternate).unwrap();
    alternate_profile.auto_effort=Some("high".into());
    store::save_profile(&state,&admin,alternate,store::SaveProfile{expected_revision:alternate_profile.revision,profile:alternate_profile}).await?;
    policy.constraints.allowed_providers=Some([first,alternate].into_iter().collect());
    policy.rules[1].action=RuleAction::Select{provider_id:first,effort:None};policy=save(&state,&admin,policy).await?;
    mode.store(0,Ordering::SeqCst);let before=requests.lock().unwrap().len();let (status,value)=request(state.clone(),&token,body()).await?;
    assert_eq!(status,StatusCode::OK,"{value}");assert_eq!(requests.lock().unwrap().len(),before+2);
    assert_eq!(value["fosnie_routing"]["decision"]["proposed"]["effort"],"none");
    assert_eq!(value["fosnie_routing"]["decision"]["actual"]["effort"],"high");
    for id in [first,alternate] {
        let mut profile=store::current_profiles(store::catalog(&state,&admin).await?).into_iter().find(|p|p.provider_id==id).unwrap();
        profile.auto_effort=None;
        store::save_profile(&state,&admin,id,store::SaveProfile{expected_revision:profile.revision,profile}).await?;
    }
    policy.constraints.allowed_providers=None;
    // Restore an explicit hard effort for the remaining existing guard cases.
    policy.rules[1].action=RuleAction::Select{provider_id:first,effort:Some("high".into())};policy=save(&state,&admin,policy).await?;
    for fault in [4,5,6,7,8,9,10,15,16,17,18,19] {
        mode.store(fault,Ordering::SeqCst);let before=requests.lock().unwrap().len();let (status,value)=request(state.clone(),&token,body()).await?;
        assert_ne!(status,StatusCode::OK,"fault {fault}");assert_eq!(requests.lock().unwrap().len(),before+if fault==4{2}else{1},"fault {fault}");
        assert!(!value.to_string().contains("secret-must-not-escape"));
    }
    for fault in [0,9,10] {
        mode.store(fault,Ordering::SeqCst);let before=requests.lock().unwrap().len();let mut b=body();b["stream"]=json!(true);
        let (status,text)=raw_request(state.clone(),&token,b).await?;assert_eq!(status,StatusCode::OK);
        assert!(text.contains("[DONE]"));assert_eq!(requests.lock().unwrap().len(),before+if fault==0{2}else{1});
        let events:Vec<Value>=text.lines().filter_map(|l|l.strip_prefix("data: ")).filter(|l|*l!="[DONE]").map(serde_json::from_str).collect::<Result<_,_>>()?;
        if fault==0 {assert!(events.iter().any(|e|e["fosnie_routing"]["decision"]["actual"]["provider_id"]==json!(alternate)));}
        else {assert!(events.iter().any(|e|e["error"].is_object()));assert!(!events.iter().any(|e|e["choices"][0]["finish_reason"]=="stop"));}
        assert!(!text.contains("secret-must-not-escape"));
    }
    // Revocation, policy revision and the live Enterprise seam win before the
    // alternate's key resolution. No report text can reopen those boundaries.
    for fault in [11,12,13] {
        mode.store(fault,Ordering::SeqCst);let before=resolved.lock().unwrap().len();let (status,_)=request(state.clone(),&token,body()).await?;
        assert_ne!(status,StatusCode::OK);assert_eq!(resolved.lock().unwrap().len(),before+1);
        sqlx::query("UPDATE api_keys SET revoked_at=NULL WHERE id=$1").bind(key).execute(&pool).await?;deny.store(false,Ordering::SeqCst);
        if fault==13 {save(&state,&admin,policy.clone()).await?;}
    }
    mode.store(0,Ordering::SeqCst);
    for selection in [json!({"mode":"manual","provider_id":first}),json!({"mode":"default"})] {
        let mut b=body();b["model_selection"]=selection;let before=requests.lock().unwrap().len();
        assert_ne!(request(state.clone(),&token,b).await?.0,StatusCode::OK);assert_eq!(requests.lock().unwrap().len(),before+1);
    }
    for (rollout,enabled,calls) in [(RolloutMode::Off,true,3),(RolloutMode::Shadow,true,3),(RolloutMode::Active,false,3),(RolloutMode::Active,true,1)] {
        let mut p=policy.clone();p.rollout=rollout;p.technical_fallback=enabled;p.constraints.max_calls=Some(calls);save(&state,&admin,p).await?;
        let before=requests.lock().unwrap().len();assert_ne!(request(state.clone(),&token,body()).await?.0,StatusCode::OK);
        assert_eq!(requests.lock().unwrap().len(),before+1);
    }
    let first_bound={let captured=requests.lock().unwrap();let allowance=&captured.last().unwrap()["overrides"]["routing"]["budget"]["allowances"]["final"];
        allowance["input_tokens"].as_u64().unwrap()+allowance["output_tokens"].as_u64().unwrap()};
    for monetary in [false,true] {
        let mut p=policy.clone();if monetary {p.constraints.max_cost_microusd=Some(1);}else {p.constraints.max_total_tokens=Some(first_bound+1);}
        save(&state,&admin,p).await?;let before=resolved.lock().unwrap().len();
        assert_ne!(request(state.clone(),&token,body()).await?.0,StatusCode::OK);assert_eq!(resolved.lock().unwrap().len(),before+1);
    }
    // Main rule pool survives into retry; judge/utility pools remain separate.
    for include_alternate in [false,true] {
        let mut p=policy.clone();p.rules.insert(0,RoutingRule{id:Uuid::now_v7(),priority:3,conditions:Default::default(),
            action:RuleAction::RestrictPool{allowed_providers:if include_alternate {[first,alternate].into()}else{[first].into()}}});
        save(&state,&admin,p).await?;let before=resolved.lock().unwrap().len();let (status,_)=request(state.clone(),&token,body()).await?;
        assert_eq!(status==StatusCode::OK,include_alternate);assert_eq!(resolved.lock().unwrap().len(),before+if include_alternate{2}else{1});
    }
    // Classifier requirements and its spend survive; no second judge call.
    let mut p=policy.clone();p.strategy=RoutingStrategy::Classifier;p.classifier_provider_id=Some(alternate);p.rules.clear();
    p.constraints.allowed_providers=Some([first,alternate,Uuid::from_u128(103)].into());save(&state,&admin,p).await?;
    let before=requests.lock().unwrap().len();let (status,value)=request(state.clone(),&token,body()).await?;
    assert_eq!(status,StatusCode::OK,"{value}");{
        let captured=requests.lock().unwrap();let suffix=&captured[before..];
        assert_eq!(suffix.len(),3);assert_eq!(suffix.iter().filter(|v|v["overrides"]["routing"]["purpose"]=="router_classify").count(),1);
        assert_eq!(value["fosnie_routing"]["usage"]["calls"],3);
    }
    // Alias response name stays virtual, actual provider is the successful one.
    save(&state,&admin,policy.clone()).await?;
    let revision=fosnie_backend::http::v1::alias::describe(&state).await?["revision"].as_i64().unwrap();
    let _=fosnie_backend::http::v1::alias::save(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(admin.clone()),
        axum::Json(fosnie_backend::http::v1::alias::Save{name:"fosnie-test/auto".into(),enabled:true,objective:RoutingObjective::Economy,expected_revision:revision})).await?;
    let alias_body=||json!({"model":"fosnie-test/auto","max_tokens":128,"messages":[{"role":"user","content":"Synthetic alias"}]});
    let (status,value)=request(state.clone(),&token,alias_body()).await?;assert_eq!(status,StatusCode::OK,"{value}");
    assert_eq!(value["model"],"fosnie-test/auto");assert_eq!(value["fosnie_routing"]["decision"]["actual"]["provider_id"],json!(alternate));
    mode.store(14,Ordering::SeqCst);let before=resolved.lock().unwrap().len();assert_ne!(request(state.clone(),&token,alias_body()).await?.0,StatusCode::OK);
    assert_eq!(resolved.lock().unwrap().len(),before+1);
    let traces:Vec<Value>=sqlx::query_scalar("SELECT model_agent_traceability FROM audit_events WHERE action_type='routing.fallback'").fetch_all(&pool).await?;
    assert!(!traces.is_empty());assert!(traces.iter().all(|t|t["attempt"]["failed_target"]["provider_id"]==json!(first)&&t["attempt"]["planned_target"]["provider_id"]==json!(alternate)));
    assert!(traces.iter().all(|t|!t.to_string().contains("Synthetic fallback prompt")&&!t.to_string().contains("secret-must-not-escape")));
    // P12: same production path/ledger/ACL fixture, buffered structural checks.
    let mut profile=store::current_profiles(store::catalog(&state,&admin).await?).into_iter().find(|p|p.provider_id==first).unwrap();
    profile.tier=ModelTier::Balanced;
    let profile=store::save_profile(&state,&admin,first,store::SaveProfile{expected_revision:profile.revision,profile}).await?;
    policy.rules[0].action=RuleAction::Require{minimum_tier:ModelTier::Balanced};
    policy.quality_escalation=true;
    policy.constraints.max_output_tokens=Some(128);policy.constraints.max_duration_ms=Some(30_000);
    policy=save(&state,&admin,policy).await?;
    let quality_body=||json!({"model_selection":{"mode":"auto","objective":"economy","policy_id":policy.id},"max_tokens":128,
        "quality_contract":{"kind":"json_object","required_fields":["answer"]},"messages":[{"role":"user","content":"Return a JSON object with an answer field."}]});
    for (fault,kind) in [(30,"invalid_json"),(32,"empty_answer"),(33,"missing_fields"),(44,"")] {
        mode.store(fault,Ordering::SeqCst);let before=requests.lock().unwrap().len();let (status,value)=request(state.clone(),&token,quality_body()).await?;
        assert_eq!(status,StatusCode::OK,"quality {fault}: {value}");
        let calls=if fault==44 {1}else{2};assert_eq!(requests.lock().unwrap().len(),before+calls);
        assert_eq!(value["choices"][0]["message"]["content"],"{\"answer\":\"Accepted\"}");
        assert_eq!(value["fosnie_routing"]["usage"]["calls"],calls);assert_eq!(value["fosnie_routing"]["usage"]["tokens"],calls*20);
        if fault!=44 {
            assert_eq!(value["fosnie_routing"]["quality_escalation"]["failure"],kind);
            assert_eq!(value["fosnie_routing"]["decision"]["actual"]["provider_id"],json!(alternate));
            assert_eq!(value["fosnie_routing"]["decision"]["actual"]["effort"],"high");
        }
        assert!(!value.to_string().contains("rejected-content-never-exposed"));
    }
    for fault in [34,35,36,37,38,39,40,41,42,43,45] {
        mode.store(fault,Ordering::SeqCst);let before=requests.lock().unwrap().len();let (status,value)=request(state.clone(),&token,quality_body()).await?;
        assert_ne!(status,StatusCode::OK,"quality {fault}");assert_eq!(requests.lock().unwrap().len(),before+if [34,35,45].contains(&fault){2}else{1},"quality {fault}: {value}");
        assert!(!value.to_string().contains("rejected-content-never-exposed"));assert!(!value.to_string().contains("private-reasoning"));
        deny.store(false,Ordering::SeqCst);
        sqlx::query("UPDATE api_keys SET revoked_at=NULL WHERE id=$1").bind(key).execute(&pool).await?;
        save(&state,&admin,policy.clone()).await?;
        if fault==43 {
            sqlx::query("UPDATE provider_configs SET model=$2 WHERE id=$1").bind(first).bind(&profile.model).execute(&pool).await?;
            let mut restored=profile.clone();restored.provider_revision=sqlx::query_scalar("SELECT routing_revision FROM provider_configs WHERE id=$1").bind(first).fetch_one(&pool).await?;
            store::save_profile(&state,&admin,first,store::SaveProfile{expected_revision:restored.revision,profile:restored}).await?;
        }
    }
    mode.store(30,Ordering::SeqCst);
    // No output calls for malformed, streaming, manual or disabled contracts.
    for variant in 0..5 {
        let mut b=quality_body();match variant {
            0=>b["stream"]=json!(true),1=>b["quality_contract"]=json!({"kind":"json_object","required_fields":["answer","answer"]}),
            2=>b["model_selection"]=json!({"mode":"default"}),3=>b["quality_contract"]=Value::Null,
            _=>{let mut p=policy.clone();p.quality_escalation=false;save(&state,&admin,p).await?;},
        }
        let before=requests.lock().unwrap().len();assert_ne!(request(state.clone(),&token,b).await?.0,StatusCode::OK);
        assert_eq!(requests.lock().unwrap().len(),before,"invalid quality variant {variant}");
    }
    // Call cap and rule pool cannot be bypassed by a quality failure.
    for restrict_pool in [false,true] {
        let mut p=policy.clone();if restrict_pool {p.rules.insert(0,RoutingRule{id:Uuid::now_v7(),priority:10,conditions:Default::default(),action:RuleAction::RestrictPool{allowed_providers:[first].into_iter().collect()}});}
        else {p.constraints.max_calls=Some(1);}
        save(&state,&admin,p).await?;let before=requests.lock().unwrap().len();assert_ne!(request(state.clone(),&token,quality_body()).await?.0,StatusCode::OK);
        assert_eq!(requests.lock().unwrap().len(),before+1);
    }
    let traces:Vec<Value>=sqlx::query_scalar("SELECT model_agent_traceability FROM audit_events WHERE action_type='routing.quality_escalation'").fetch_all(&pool).await?;
    assert_eq!(traces.len(),5);assert!(traces.iter().all(|t|t["attempt"]["planned_target"]["provider_id"]==json!(alternate)));
    assert!(traces.iter().all(|t|!t.to_string().contains("rejected-content-never-exposed")&&!t.to_string().contains("private-reasoning")));
    // Only provider managers can import trusted ranking evidence, even for BYOK.
    let own=Uuid::from_u128(107);
    sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES('providers.user_byok_enabled','true','bool') ON CONFLICT(key) DO UPDATE SET value='true'").execute(&pool).await?;
    sqlx::query("INSERT INTO provider_configs(id,role,scope,scope_id,label,model,base_url) VALUES($1,'llm','user',$2,'Own','own','http://synthetic.test/v1')")
        .bind(own).bind(uid).execute(&pool).await?;
    let user=AuthContext{user_id:Some(uid),role:PlatformRole::User,email:None,display_name:None,break_glass:false,mfa_enroll_only:false};
    let mut owned=profile.clone();owned.provider_id=own;owned.provider_revision=1;owned.model="own".into();owned.locality_trusted=false;
    owned=store::save_profile(&state,&user,own,store::SaveProfile{expected_revision:0,profile:owned}).await?;
    owned.quality_evidence=vec![learning::EvaluationEvidence{id:Uuid::now_v7(),source:learning::EvidenceSource::Live,dataset_sha256:"a".repeat(64),families_sha256:"b".repeat(64),
        profile_sha256:learning::fingerprint(&owned),evaluator_version:"synthetic-fixture-attestation".into(),purpose:CallPurpose::Primary,
        workload:WorkloadRequirements{task:TaskKind::General,input_tokens:100,output_tokens:128,..Default::default()},families:40,passed:40,reviewed:true}];
    assert!(store::save_profile(&state,&user,own,store::SaveProfile{expected_revision:owned.revision,profile:owned}).await.is_err());
    let mut deployment=store::current_profiles(store::catalog(&state,&admin).await?).into_iter().find(|p|p.provider_id==first).unwrap();
    deployment.quality_evidence=vec![learning::EvaluationEvidence{id:Uuid::now_v7(),source:learning::EvidenceSource::Synthetic,dataset_sha256:"a".repeat(64),families_sha256:"b".repeat(64),
        profile_sha256:learning::fingerprint(&deployment),evaluator_version:"fixture".into(),purpose:CallPurpose::Primary,
        workload:WorkloadRequirements{task:TaskKind::General,input_tokens:100,output_tokens:128,..Default::default()},families:40,passed:40,reviewed:false}];
    let saved=store::save_profile(&state,&admin,first,store::SaveProfile{expected_revision:deployment.revision,profile:deployment.clone()}).await?;
    assert!(store::save_profile(&state,&admin,first,store::SaveProfile{expected_revision:deployment.revision,profile:deployment}).await.is_err());
    let mut stale=saved.clone();stale.tier=ModelTier::Advanced;
    assert!(store::save_profile(&state,&admin,first,store::SaveProfile{expected_revision:saved.revision,profile:stale}).await.is_err());
    // Browser-style single-candidate optimisation cannot bypass measured quality.
    let mut measured=saved.clone();measured.tier=ModelTier::Advanced;
    measured.quality_evidence[0].profile_sha256=learning::fingerprint(&measured);
    measured.quality_evidence[0].source=learning::EvidenceSource::Live;
    measured.quality_evidence[0].reviewed=true;measured.quality_evidence[0].passed=0;
    measured.quality_evidence[0].workload.minimum_tier=ModelTier::Advanced;
    measured.quality_evidence[0].workload.input_tokens=1536;
    store::save_profile(&state,&admin,first,store::SaveProfile{expected_revision:saved.revision,profile:measured}).await?;
    let mut learned=policy.clone();learned.quality_escalation=false;learned.learned_ranking=true;
    learned.strategy=RoutingStrategy::Classifier;learned.classifier_provider_id=Some(first);learned.rules.clear();
    learned.constraints.allowed_providers=Some([first].into_iter().collect());
    learned.constraints.max_output_tokens=Some(1024); // includes the classifier's 512-token allowance
    save(&state,&admin,learned).await?;
    let mut b=quality_body();b.as_object_mut().unwrap().remove("quality_contract");
    mode.store(44,Ordering::SeqCst);let before=requests.lock().unwrap().len();
    let (status,value)=request(state.clone(),&token,b).await?;
    assert_ne!(status,StatusCode::OK,"failed measured quality must refuse the sole candidate: {value}");
    let captured=requests.lock().unwrap();let suffix=&captured[before..];assert_eq!(suffix.len(),1);
    assert_eq!(suffix[0]["overrides"]["routing"]["purpose"],"router_classify");
    Ok(())
}

#[tokio::test]
async fn r05_stateless_bounded_technical_fallback()->anyhow::Result<()> {
    let options:sqlx::postgres::PgConnectOptions=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL").expect("isolated routing database required").parse()?;
    anyhow::ensure!(options.get_database().is_some_and(|d|d.starts_with("routing_")),"dedicated routing_ database required");
    let mut conn=sqlx::PgConnection::connect_with(&options).await?;let schema=format!("routing_fallback_{}",Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&mut conn).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"))).execute(&mut conn).await?;
    let result=async {
        for migration in sqlx::migrate!("./migrations").iter() {sqlx::raw_sql(migration.sql.clone()).execute(&mut conn).await?;}
        let pool=sqlx::postgres::PgPoolOptions::new().max_connections(5).after_connect({let schema=schema.clone();move |conn,_|{
            let sql=sqlx::AssertSqlSafe(format!("SET search_path TO {schema}"));Box::pin(async move {sqlx::raw_sql(sql).execute(conn).await?;Ok(())})
        }}).connect_with(options).await?;
        let checked=tokio::spawn({let pool=pool.clone();async move{Box::pin(acceptance(pool)).await}}).await;
        pool.close().await;checked?
    }.await;
    conn.execute("SET search_path TO public").await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE"))).execute(&mut conn).await?;result
}
