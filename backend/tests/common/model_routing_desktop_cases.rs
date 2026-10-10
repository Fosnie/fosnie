// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::{future::Future,pin::Pin,sync::{Arc,atomic::{AtomicUsize,Ordering}},time::Duration};
use axum::Json;
use futures_util::{SinkExt,StreamExt};
use serde_json::{json,Value};
use tokio::sync::Notify;
use tokio_tungstenite::{WebSocketStream,MaybeTlsStream,tungstenite::Message};
use uuid::Uuid;
use fosnie_backend::{auth::{AuthContext,PlatformRole,device::DeviceCredential},chat::{browser,origin::ChatOrigin},config::BootConfig,model_routing::*,state::AppStateBuilder,ws::session};
type Socket=WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
async fn send(socket:&mut Socket,value:Value)->anyhow::Result<()> {socket.send(Message::Text(value.to_string())).await?;Ok(())}
async fn until(socket:&mut Socket,kind:&str)->anyhow::Result<Vec<Value>> {
        let mut frames=vec![];let deadline=tokio::time::Instant::now()+Duration::from_secs(8);
        while let Some(frame)=tokio::time::timeout_at(deadline,socket.next()).await.map_err(|_|anyhow::anyhow!("waiting for {kind}; received {frames:?}"))? {
            match frame? {
                Message::Text(text)=>{let value:Value=serde_json::from_str(&text)?;let done=value["type"]==kind || value["type"]=="chat.error";frames.push(value);if done {return Ok(frames);}},
                Message::Ping(bytes)=>socket.send(Message::Pong(bytes)).await?,_=>{},
            }
        }anyhow::bail!("socket closed before {kind}; received {frames:?}")
}
async fn ticket(http:&reqwest::Client,base:&str,token:&str)->anyhow::Result<String> {
    let response=http.post(format!("{base}/api/ws-ticket")).bearer_auth(token).send().await?;
    let status=response.status();let body:Value=response.json().await?;anyhow::ensure!(status.is_success(),"ticket: {status} {body}");Ok(body["ticket"].as_str().unwrap().into())
}
async fn connect(base:&str,ticket:&str)->anyhow::Result<Socket> {Ok(tokio_tungstenite::connect_async(format!("{}/ws?ticket={ticket}",base.replacen("http://","ws://",1))).await?.0)}
async fn settled(pool:&sqlx::PgPool,chat:Uuid)->anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {let mut tx=pool.begin().await?;let free:bool=sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))").bind(format!("browser-chat:{chat}")).fetch_one(&mut *tx).await?;tx.rollback().await?;
            if free {return Ok::<_,sqlx::Error>(());}tokio::time::sleep(Duration::from_millis(15)).await;}
    }).await??;Ok(())
}
fn request(chat:Option<Uuid>,selection:Option<Value>)->Value {
    let mut request=json!({"chat_id":chat,"content":"Synthetic remote desktop question"});
    if let Some(selection)=selection {request["model_selection"]=selection;}
    json!({"type":"chat.routed.send","request":request})
}
pub fn check(pool:&sqlx::PgPool)->Pin<Box<dyn Future<Output=anyhow::Result<()>>+Send+'_>> {
    Box::pin(async move {
        let calls=Arc::new(AtomicUsize::new(0));let judges=Arc::new(AtomicUsize::new(0));let mode=Arc::new(AtomicUsize::new(0));let entered=Arc::new(Notify::new());
        let ml=axum::Router::new().route("/chat-step",axum::routing::post({let calls=calls.clone();let judges=judges.clone();let mode=mode.clone();let entered=entered.clone();
            move |Json(body):Json<Value>| {let calls=calls.clone();let judges=judges.clone();let mode=mode.clone();let entered=entered.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);let purpose=body["overrides"]["routing"]["purpose"].as_str().unwrap();
                let content=if purpose=="utility_title" {"Remote desktop title".to_string()} else {assert_eq!(purpose,"router_classify");judges.fetch_add(1,Ordering::SeqCst);
                    if mode.load(Ordering::SeqCst)==1 {entered.notify_one();tokio::time::sleep(Duration::from_secs(3)).await;}
                    json!({"task":"general","minimum_tier":"balanced","fresh_information":false,"tools":false,"vision":false,"structured_output":false,"confidence":0.95,"uncertain":false}).to_string()};
                Json(json!({"content":content,"routing_report":super::routing_mock_report(&body)}))
            }}
        })).route("/generate",axum::routing::post({let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();
            move |Json(body):Json<Value>| {let calls=calls.clone();let mode=mode.clone();let entered=entered.clone();async move {
                calls.fetch_add(1,Ordering::SeqCst);let current=mode.load(Ordering::SeqCst);if current==2 {entered.notify_one();}
                assert_eq!(body["overrides"]["routing"]["assignments"]["final"]["model"],"desktop-local");
                let lines=vec![json!({"type":"token","delta":"Desktop answer"}).to_string()+"\n",
                    json!({"type":"done","finish_reason":"stop","usage":{"prompt_tokens":100,"completion_tokens":40},"routing_report":super::routing_mock_report(&body)}).to_string()+"\n"];
                axum::body::Body::from_stream(futures_util::stream::unfold((lines.into_iter(),0),move |(mut lines,index)|async move {
                    let line=lines.next()?;if current==2 && index==1 {tokio::time::sleep(Duration::from_secs(3)).await;}
                    Some((Ok::<_,std::convert::Infallible>(line),(lines,index+1)))
                }))
            }}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let ml_address=listener.local_addr()?;
        let ml_server=tokio::spawn(async move {axum::serve(listener,ml).await.unwrap();});
        let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{ml_address}");
        let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot)).build();
        let user=Uuid::now_v7();let manager=Uuid::now_v7();let foreign=Uuid::now_v7();let device=Uuid::now_v7();let foreign_device=Uuid::now_v7();let key=Uuid::now_v7();
        for (id,role) in [(user,"user"),(manager,"client_admin"),(foreign,"user")] {
            sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Synthetic desktop user',$3::platform_role)").bind(id).bind(format!("{id}@routing.test")).bind(role).execute(pool).await?;
        }
        let actor=AuthContext {user_id:Some(user),role:PlatformRole::User,email:None,display_name:None,break_glass:false,mfa_enroll_only:false};
        let admin=AuthContext {user_id:Some(manager),role:PlatformRole::ClientAdmin,..actor.clone()};
        for (id,owner) in [(device,user),(foreign_device,foreign)] {sqlx::query("INSERT INTO devices(id,user_id,name,platform) VALUES($1,$2,'Synthetic desktop','windows')").bind(id).bind(owner).execute(pool).await?;}
        let (token,hash,prefix)=fosnie_backend::auth::api_key::mint();
        sqlx::query("INSERT INTO api_keys(id,user_id,kind,device_id,name,token_hash,display_prefix) VALUES($1,$2,'device',$3,'desktop',$4,$5)").bind(key).bind(user).bind(device).bind(hash).bind(prefix).execute(pool).await?;
        let local=Uuid::now_v7();let cloud=Uuid::now_v7();
        for (id,model,locality) in [(local,"desktop-local","local"),(cloud,"desktop-cloud","cloud")] {
            sqlx::query("INSERT INTO provider_configs(id,role,scope,label,model,base_url) VALUES($1,'llm','deployment',$2,$2,'http://desktop.test/v1')").bind(id).bind(model).execute(pool).await?;
            let profile:ModelProfile=serde_json::from_value(json!({"provider_id":id,"provider_revision":1,"revision":1,"model":model,"adapter":"openai_chat","locality":locality,"locality_trusted":true,
                "quality_status":"declared","tier":"balanced","auto_enabled":true,"capabilities":{"context_tokens":32000,"max_output_tokens":8192,"structured_output":"declared","efforts":["high"]},
                "pricing":{"input_microusd_per_million":0,"output_microusd_per_million":0}}))?;
            store::save_profile(&state,&admin,id,store::SaveProfile {expected_revision:0,profile}).await?;
        }
        let policy:RoutingPolicy=serde_json::from_value(json!({"id":Uuid::now_v7(),"revision":1,"strategy":"rules","rollout":"active","default_provider_id":local,"classifier_provider_id":cloud,
            "rules":[{"id":Uuid::now_v7(),"priority":10,"conditions":{},"action":{"kind":"select","provider_id":local}}],"constraints":{"max_calls":12,"max_duration_ms":15000}}))?;
        let mut policy=store::save_policy(&state,&admin,policy.id,store::SavePolicy {label:"Desktop policy".into(),scope:"deployment".into(),expected_revision:0,is_default:true,policy}).await?;
        let credential=DeviceCredential {device_id:device,key_id:key};
        assert!(browser::Surface::authenticated(ChatOrigin::Desktop,Some(device),None).is_err());assert!(browser::Surface::authenticated(ChatOrigin::Web,Some(device),Some(key)).is_err());
        assert!(DeviceCredential {device_id:foreign_device,key_id:key}.validate(&state,&actor).await.is_err());
        assert!(credential.validate(&state,&AuthContext {role:PlatformRole::PowerUser,..actor.clone()}).await.is_err());
        sqlx::query("UPDATE api_keys SET expires_at=now()-interval '1 second' WHERE id=$1").bind(key).execute(pool).await?;
        assert!(credential.validate(&state,&actor).await.is_err());
        sqlx::query("UPDATE api_keys SET expires_at=NULL WHERE id=$1").bind(key).execute(pool).await?;
        let _=browser::set_enabled(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(admin.clone()),Json(fosnie_backend::http::voice_routing::Enabled {enabled:true})).await?;
        let _=fosnie_backend::chat::regenerate::set_enabled(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(admin),Json(fosnie_backend::http::voice_routing::Enabled {enabled:true})).await?;
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let base=format!("http://{}",listener.local_addr()?);
        let router=fosnie_backend::http::router(state.clone(),None,None,None,None);
        let api_server=tokio::spawn(async move {axum::serve(listener,router).await.unwrap();});let http=reqwest::Client::new();
        let caps:Value=http.get(format!("{base}/api/routing/browser-chat")).bearer_auth(&token).send().await?.error_for_status()?.json().await?;
        assert_eq!(caps["desktop"],true);assert_eq!(caps["desktop_credential"],"original_key_ticket_v1");
        let fresh=ticket(&http,&base,&token).await?;let mut socket=connect(&base,&fresh).await?;
        let hello=until(&mut socket,"hello").await?.pop().unwrap();let resume=hello["resume_token"].as_str().unwrap().to_string();
        let binding=session::lookup_bound_resume(&state.redis,&resume).await?.unwrap();assert_eq!(binding.device_key_id,Some(key));assert_eq!(binding.device_id,Some(device));
        assert!(session::redeem_bound_ticket(&state.redis,&fresh).await?.is_none(),"ticket and companion are single-use");
        send(&mut socket,json!({"type":"client.hello","client_kind":"web","capabilities":[]})).await?; // descriptive claims do not change device provenance
        let mut forged=request(None,Some(json!({"mode":"auto"})));forged["request"]["device_id"]=json!(foreign_device);
        // Malformed frames are deliberately discarded by the established wire
        // reader, rather than rendered as a conversation error. Ping fences it.
        send(&mut socket,forged).await?;send(&mut socket,json!({"type":"ping"})).await?;
        assert_eq!(until(&mut socket,"pong").await?.last().unwrap()["type"],"pong");assert_eq!(calls.load(Ordering::SeqCst),0);
        let mut first=request(None,Some(json!({"mode":"auto","objective":"economy"})));first["request"]["options"]=json!({"local_only":true,"effort":"high"});send(&mut socket,first).await?;
        let frames=until(&mut socket,"chat.completed").await?;assert_eq!(frames.last().unwrap()["type"],"chat.completed","{frames:?}");
        let chat:Uuid=frames.iter().find(|f|f["type"]=="chat.created").unwrap()["chat_id"].as_str().unwrap().parse()?;
        let answer:Uuid=frames.last().unwrap()["message_id"].as_str().unwrap().parse()?;settled(pool,chat).await?;
        let origin:String=sqlx::query_scalar("SELECT origin FROM chats WHERE id=$1").bind(chat).fetch_one(pool).await?;assert_eq!(origin,"desktop");assert_eq!(judges.load(Ordering::SeqCst),0);
        let saved:Value=http.get(format!("{base}/api/chats/{chat}/routing")).bearer_auth(&token).send().await?.error_for_status()?.json().await?;
        assert_eq!(saved["supported"],true);assert_eq!(saved["selection"]["mode"],"auto");assert_eq!(saved["options"]["local_only"],true);
        let web_saved=browser::saved(axum::extract::State(state.clone()),fosnie_backend::auth::keycloak::AuthUser(actor.clone()),axum::extract::Path(chat)).await?.0;
        assert_eq!(web_saved["supported"],false);assert_eq!(web_saved["selection"]["mode"],"auto");
        let messages:Value=http.get(format!("{base}/api/chats/{chat}/messages")).bearer_auth(&token).send().await?.error_for_status()?.json().await?;
        assert!(messages.as_array().unwrap().iter().any(|m|m["routing"]["actual"]["model"]=="desktop-local" && m["routing"]["actual"]["effort"]=="high"));
        send(&mut socket,request(Some(chat),Some(json!({"mode":"manual","provider_id":local})))).await?;assert_eq!(until(&mut socket,"chat.completed").await?.last().unwrap()["type"],"chat.completed");settled(pool,chat).await?;
        send(&mut socket,request(Some(chat),None)).await?;assert_eq!(until(&mut socket,"chat.completed").await?.last().unwrap()["type"],"chat.completed");settled(pool,chat).await?;
        let old_count:i64=sqlx::query_scalar("SELECT count(*) FROM messages WHERE chat_id=$1").bind(chat).fetch_one(pool).await?;
        send(&mut socket,json!({"type":"chat.routed.regenerate","chat_id":chat,"from_message_id":answer})).await?;
        let frames=until(&mut socket,"chat.completed").await?;assert_eq!(frames.last().unwrap()["type"],"chat.completed","{frames:?}");
        let branch:Uuid=frames.iter().find(|f|f["type"]=="chat.created").unwrap()["chat_id"].as_str().unwrap().parse()?;
        let row:(String,Value)=sqlx::query_as("SELECT origin,model_selection FROM chats WHERE id=$1").bind(branch).fetch_one(pool).await?;assert_eq!(row.0,"desktop");assert_eq!(row.1["mode"],"auto");
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM messages WHERE chat_id=$1").bind(chat).fetch_one(pool).await?,old_count);
        let before=calls.load(Ordering::SeqCst);send(&mut socket,json!({"type":"chat.send","chat_id":chat,"content":"Legacy downgrade"})).await?;
        assert_eq!(until(&mut socket,"chat.error").await?.last().unwrap()["type"],"chat.error");assert_eq!(calls.load(Ordering::SeqCst),before);
        // A local folder binding is refused before judge/history/inference.
        let workspace=Uuid::now_v7();sqlx::query("INSERT INTO device_workspaces(id,device_id,user_id,path,label,tier) VALUES($1,$2,$3,'C:/synthetic','Synthetic folder','ro')").bind(workspace).bind(device).bind(user).execute(pool).await?;
        sqlx::query("INSERT INTO chat_workspace(chat_id,workspace_id) VALUES($1,$2)").bind(chat).bind(workspace).execute(pool).await?;
        send(&mut socket,request(Some(chat),None)).await?;assert_eq!(until(&mut socket,"chat.error").await?.last().unwrap()["type"],"chat.error");assert_eq!(calls.load(Ordering::SeqCst),before);
        sqlx::query("DELETE FROM chat_workspace WHERE chat_id=$1").bind(chat).execute(pool).await?;
        // Old two-field tickets remain usable but cannot invent a credential for Auto.
        let old=session::issue_ticket(&state.redis,user,Some(device)).await?;let mut old_socket=connect(&base,&old).await?;let _=until(&mut old_socket,"hello").await?;
        send(&mut old_socket,request(Some(chat),None)).await?;let frames=until(&mut old_socket,"chat.error").await?;assert!(frames.last().unwrap()["message"].as_str().unwrap().contains("fresh ticket"));
        send(&mut old_socket,json!({"type":"ping"})).await?;assert_eq!(until(&mut old_socket,"pong").await?.last().unwrap()["type"],"pong");old_socket.close(None).await?;
        // Key rotation during an open response never substitutes the new live key.
        mode.store(2,Ordering::SeqCst);send(&mut socket,request(Some(chat),None)).await?;tokio::time::timeout(Duration::from_secs(5),entered.notified()).await?;let before=calls.load(Ordering::SeqCst);
        sqlx::query("UPDATE api_keys SET revoked_at=now() WHERE id=$1").bind(key).execute(pool).await?;
        let new_key=Uuid::now_v7();let (new_token,hash,prefix)=fosnie_backend::auth::api_key::mint();
        sqlx::query("INSERT INTO api_keys(id,user_id,kind,device_id,name,token_hash,display_prefix) VALUES($1,$2,'device',$3,'rotated',$4,$5)").bind(new_key).bind(user).bind(device).bind(hash).bind(prefix).execute(pool).await?;
        let frames=until(&mut socket,"chat.error").await?;assert!(!frames.iter().any(|f|f["type"]=="chat.completed"));assert_eq!(calls.load(Ordering::SeqCst),before);socket.close(None).await?;
        assert!(tokio_tungstenite::connect_async(format!("{}/ws?resume={resume}",base.replacen("http://","ws://",1))).await.is_err(),"old bound resume cannot use the new key");
        credential.validate(&state,&actor).await.unwrap_err();
        mode.store(0,Ordering::SeqCst);let fresh=ticket(&http,&base,&new_token).await?;let mut socket=connect(&base,&fresh).await?;let _=until(&mut socket,"hello").await?;
        // Revocation while judging cancels one pinned future, without final/replay.
        policy.rules.clear();policy.strategy=RoutingStrategy::Classifier;let revision=policy.revision;
        let policy_admin=AuthContext {user_id:Some(manager),role:PlatformRole::ClientAdmin,..actor.clone()};
        store::save_policy(&state,&policy_admin,policy.id,store::SavePolicy {label:"Desktop judge".into(),scope:"deployment".into(),expected_revision:revision,is_default:true,policy}).await?;
        mode.store(1,Ordering::SeqCst);let mut b=request(Some(chat),Some(json!({"mode":"auto"})));b["request"]["options"]=json!({"local_only":false,"effort":null});send(&mut socket,b).await?;
        tokio::time::timeout(Duration::from_secs(5),entered.notified()).await?;let before=calls.load(Ordering::SeqCst);
        sqlx::query("UPDATE devices SET revoked_at=now() WHERE id=$1").bind(device).execute(pool).await?;
        let frames=until(&mut socket,"chat.error").await?;assert!(!frames.iter().any(|f|f["type"]=="chat.completed"));assert_eq!(calls.load(Ordering::SeqCst),before);socket.close(None).await?;
        let traces:Vec<Value>=sqlx::query_scalar("SELECT model_agent_traceability FROM audit_events WHERE actor_user_id=$1 AND action_type='chat.routing.admitted'").bind(user).fetch_all(pool).await?;
        assert!(traces.iter().any(|t|t["origin"]=="desktop" && t["device_id"]==device.to_string() && t["device_key_id"]==key.to_string()));assert!(!serde_json::to_string(&traces)?.contains("Synthetic remote desktop question"));
        api_server.abort();ml_server.abort();Ok(())
    })
}
