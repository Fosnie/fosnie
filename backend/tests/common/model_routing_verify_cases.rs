// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc,atomic::{AtomicUsize,AtomicBool,Ordering}};
use serde_json::{Value,json};
use uuid::Uuid;
use fosnie_backend::{auth::{AuthContext,PlatformRole,rbac::{FlatRbacPolicy,ResourceType,Permission}},
    ext::RbacPolicy,model_routing::*,config::BootConfig,state::AppStateBuilder,groundedness::routed::verify};

struct SourceAcl {mode:Arc<AtomicUsize>,revoked:Arc<AtomicBool>,doc:Uuid}
struct Grounding(Arc<AtomicBool>);
#[async_trait::async_trait]
impl fosnie_backend::ext::FeatureResolver for Grounding {
    async fn enabled_for_user(&self,_:&fosnie_backend::AppState,_:Option<Uuid>,feature:&str)->bool {feature=="groundedness" && self.0.load(Ordering::SeqCst)}
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
        assert_eq!(ctx.role,PlatformRole::User,"original actor survives verification");
        if self.mode.load(Ordering::SeqCst)==10 {return Err(fosnie_backend::AppError::Unavailable("ACL failed".into()));}
        Ok(if self.mode.load(Ordering::SeqCst)==9 || self.revoked.load(Ordering::SeqCst) {vec![self.doc]} else {vec![]})
    }
}

pub async fn check_verify(pool:&sqlx::PgPool,admin:&AuthContext,policy_id:Uuid)->anyhow::Result<()> {
    let mode=Arc::new(AtomicUsize::new(0));let count=Arc::new(AtomicUsize::new(0));let revoked=Arc::new(AtomicBool::new(false));
    let entered=Arc::new(tokio::sync::Notify::new());let kb=Uuid::now_v7();let doc=Uuid::now_v7();
    let enabled=Arc::new(AtomicBool::new(false));
    let app=axum::Router::new().route("/verify",axum::routing::post({
        let mode=mode.clone();let count=count.clone();let revoked=revoked.clone();let entered=entered.clone();let pool=pool.clone();let enabled=enabled.clone();
        move |axum::Json(body):axum::Json<Value>| {let mode=mode.clone();let count=count.clone();let revoked=revoked.clone();let entered=entered.clone();let pool=pool.clone();let enabled=enabled.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);let case=mode.load(Ordering::SeqCst);
            let r=&body["overrides"]["routing"];let target=&r["fixed_assignments"]["verify"];
            assert_eq!(r["purpose"],"verify");assert_eq!(target["model"],"fixed-model");assert!(r["assignments"].as_object().unwrap().is_empty());
            assert_eq!(body["hhem_filter"],false);assert_eq!(body["answer"],"🙂 Wrong.");
            if matches!(case,14|15|34) {if case!=15 {entered.notify_one();}tokio::time::sleep(std::time::Duration::from_secs(5)).await;}
            if matches!(case,11|33) {revoked.store(true,Ordering::SeqCst);}
            if case==32 {sqlx::query("UPDATE messages SET content='edited' WHERE content='<think>private</think>🙂 Wrong.'").execute(&pool).await.unwrap();}
            if case==35 {enabled.store(false,Ordering::SeqCst);}
            if case==40 {sqlx::query("UPDATE config_settings SET value='always' WHERE key='rag.model_search_mode'").execute(&pool).await.unwrap();}
            if case==12 {sqlx::query("DELETE FROM chat_kb_links WHERE kb_id=$1").bind(kb).execute(&pool).await.unwrap();}
            if case==21 {sqlx::query("UPDATE config_settings SET value='0.5' WHERE key='groundedness.threshold'").execute(&pool).await.unwrap();}
            let mut result=json!({"score":0.375,"total":1,"flagged":1,"model":"fixed-model",
                "spans":[{"start":2,"end":7,"text":"Wrong","label":"contradicted","score":0.9}],
                "fixed_routing_report":{"lease_id":r["fixed_budget"]["id"],"complete":true,"calls":[{
                    "call_id":Uuid::now_v7(),"lease_id":r["fixed_budget"]["id"],"decision_id":r["decision_id"],"role":"verify",
                    "provider_id":target["provider_id"],"provider_revision":target["provider_revision"],"profile_revision":target["profile_revision"],
                    "status":"completed","latency_ms":1,"input_bytes":100,"output_bytes":200}]}});
            match case {
                1=>{result["spans"]=json!([]);result["flagged"]=json!(0);result["score"]=json!(1.0);},
                2=>{result.as_object_mut().unwrap().remove("fixed_routing_report");},
                3=>result["model"]=json!("wrong"),4=>result["spans"][0]["text"]=json!("forged"),
                5=>result["score"]=json!(1.0),6=>{result["spans"].as_array_mut().unwrap().push(json!({"start":3,"end":7,"text":"rong","label":"contradicted","score":0.9}));result["flagged"]=json!(2);},
                7=>result["score"]=Value::Null,8=>result["fixed_routing_report"]["calls"]=json!([]),
                23=>{result.as_object_mut().unwrap().remove("spans");},24=>result["fixed_routing_report"]["calls"][0]["status"]=json!("failed"),_=>{}
            }
            axum::Json(result)
        }}
    }));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let address=listener.local_addr()?;
    let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
    let mut boot=BootConfig::default();boot.ml.base_url=format!("http://{address}");
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
        .with_rbac(Arc::new(SourceAcl {mode:mode.clone(),revoked:revoked.clone(),doc})).with_features(Arc::new(Grounding(enabled.clone()))).build();
    let mut actor=admin.clone();actor.role=PlatformRole::User;
    sqlx::query("INSERT INTO knowledge_bases(id,name,owner_id,embedding_model_id,embedding_dimension,status) VALUES($1,'Verifier library',$2,'routed-embed',384,'ready')")
        .bind(kb).bind(actor.user_id).execute(pool).await?;
    let mut policy=store::load_policy(&state,admin,Some(policy_id)).await?;
    policy.strategy=RoutingStrategy::Rules;policy.rules.clear();
    for case in 0..25 {
        mode.store(case,Ordering::SeqCst);revoked.store(false,Ordering::SeqCst);
        for (key,value) in [("groundedness.threshold",if case==18 {"NaN"} else {"0"}),("groundedness.hhem_filter",if case==19 {"true"} else {"false"})] {
            sqlx::query("INSERT INTO config_settings(key,value,value_type) VALUES($1,$2,'string') ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value")
                .bind(key).bind(value).execute(pool).await?;
        }
        policy.constraints.max_calls=Some(1);policy.constraints.max_duration_ms=Some(if case==15 {1000} else {10000});
        policy=store::save_policy(&state,admin,policy.id,store::SavePolicy {label:"Verifier test".into(),scope:"deployment".into(),expected_revision:policy.revision,is_default:true,policy:policy.clone()}).await?;
        let chat=Uuid::now_v7();let turn=Uuid::now_v7();
        sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Verification')").bind(chat).bind(actor.user_id).execute(pool).await?;
        let roles=if case==16 {vec![]} else {vec![fixed::FixedRole::Verify]};
        let run=fosnie_backend::chat::routing::prepare(&state,&actor,chat,turn,"question",Some(policy_id),RoutingObjective::Balanced,
            WorkloadRequirements {output_tokens:1024,..Default::default()},&roles,&tokio::sync::Notify::new()).await?;
        sqlx::query("INSERT INTO chat_kb_links(chat_id,kb_id,attached_by) VALUES($1,$2,$3)").bind(chat).bind(kb).bind(actor.user_id).execute(pool).await?;
        if case==13 {run.cancel();}
        let before=count.load(Ordering::SeqCst);let kbs=if case==22 {vec![Uuid::now_v7()]} else {vec![kb]};
        let context=if case==20 {"x".repeat(1024*1024+1)} else {"evidence".into()};
        let work=Box::pin(verify(&state,&run,&kbs,&[],&context,"question","🙂 Wrong."));
        let result=if case==14 {
            tokio::time::timeout(std::time::Duration::from_secs(3),async {
                let (result,_)=tokio::join!(work,async {entered.notified().await;run.cancel();});result
            }).await?
        } else {work.await};
        assert_eq!(result.is_ok(),matches!(case,0|1|17),"verify {case}: {:?}",result.as_ref().err());
        let sent=matches!(case,0..=8|11|12|14|15|17|21|23|24);
        assert_eq!(count.load(Ordering::SeqCst)-before,usize::from(sent),"verify {case}");
        assert_eq!(run.usage().calls,u32::from(sent && case!=8),"verify {case}");
        if case==0 {assert_eq!(result?.score,0.375);assert_eq!(run.usage().unknown_token_calls,1);assert_eq!(run.usage().unknown_cost_calls,1);}
        if case==17 {
            assert!(verify(&state,&run,&kbs,&[],&context,"question","🙂 Wrong.").await.is_err());
            assert_eq!(count.load(Ordering::SeqCst)-before,1,"shared budget cannot fund a second verifier");
        }
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    use fosnie_backend::{groundedness::routed::{LiveCheck,verify_message},ws::protocol::ServerFrame};
    for case in 30..41 {
        mode.store(case,Ordering::SeqCst);revoked.store(false,Ordering::SeqCst);enabled.store(false,Ordering::SeqCst);
        sqlx::query("UPDATE config_settings SET value='0' WHERE key='groundedness.threshold'").execute(pool).await?;
        sqlx::query("UPDATE config_settings SET value='off' WHERE key='rag.model_search_mode'").execute(pool).await?;
        let chat=Uuid::now_v7();let turn=Uuid::now_v7();let message=Uuid::now_v7();
        sqlx::query("INSERT INTO chats(id,owner_user_id,title) VALUES($1,$2,'Live verification')").bind(chat).bind(actor.user_id).execute(pool).await?;
        let run=fosnie_backend::chat::routing::prepare(&state,&actor,chat,turn,"question",Some(policy_id),RoutingObjective::Balanced,
            WorkloadRequirements {output_tokens:1024,..Default::default()},&[fixed::FixedRole::Verify],&tokio::sync::Notify::new()).await?;
        sqlx::query("INSERT INTO chat_kb_links(chat_id,kb_id,attached_by) VALUES($1,$2,$3)").bind(chat).bind(kb).bind(actor.user_id).execute(pool).await?;
        let content="<think>private</think>🙂 Wrong.";
        sqlx::query("INSERT INTO messages(id,chat_id,turn_id,role,sequence_number,content,completed_at,routing_decision) VALUES($1,$2,$3,'assistant',1,$4,now(),$5)")
            .bind(message).bind(chat).bind(turn).bind(content).bind(json!(run.plan().decision)).execute(pool).await?;
        if case==38 {sqlx::query("UPDATE messages SET routing_decision='{}'::jsonb WHERE id=$1").bind(message).execute(pool).await?;}
        if case==31 {
            sqlx::query("CREATE FUNCTION refuse_verification_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action_type='groundedness.verified' THEN RAISE EXCEPTION 'test audit failure'; END IF; RETURN NEW; END $$").execute(pool).await?;
            sqlx::query("CREATE TRIGGER refuse_verification_audit BEFORE INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION refuse_verification_audit()").execute(pool).await?;
        }
        if case==39 {sqlx::query("ALTER TABLE claim_verdicts ADD CONSTRAINT reject_test_span CHECK (claim_text <> 'Wrong')").execute(pool).await?;}
        enabled.store(true,Ordering::SeqCst);
        let check=LiveCheck {message_id:message,content:if case==37 {"stale".into()} else {content.into()},question:"question".into(),evidence:"evidence".into(),kbs:vec![kb],denied:vec![]};
        let (tx,mut rx)=tokio::sync::mpsc::channel(8);state.hub.register(Uuid::now_v7(),actor.user_id.unwrap(),None,tx);
        let before=count.load(Ordering::SeqCst);
        let work=Box::pin(verify_message(&state,&run,&check));
        let result=if case==34 {
            tokio::time::timeout(std::time::Duration::from_secs(3),async {let (r,_)=tokio::join!(work,async {entered.notified().await;run.cancel();});r}).await?
        } else {work.await};
        let success=matches!(case,30|36);
        assert_eq!(result.is_ok(),success,"live {case}: {:?}",result.err());
        if case==36 {verify_message(&state,&run,&check).await?;}
        assert_eq!(count.load(Ordering::SeqCst)-before,usize::from(!matches!(case,37|38)),"idempotent/stale callback {case}");
        let stored:bool=sqlx::query_scalar("SELECT groundedness IS NOT NULL FROM messages WHERE id=$1").bind(message).fetch_one(pool).await?;
        let runs:i64=sqlx::query_scalar("SELECT count(*) FROM verification_runs WHERE target_id=$1").bind(message).fetch_one(pool).await?;
        let spans:i64=sqlx::query_scalar("SELECT count(*) FROM claim_verdicts WHERE run_id IN (SELECT id FROM verification_runs WHERE target_id=$1)").bind(message).fetch_one(pool).await?;
        let audit:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action_type='groundedness.verified' AND resource_id=$1").bind(message).fetch_one(pool).await?;
        assert_eq!((stored,runs,spans,audit),(success,i64::from(success),i64::from(success),i64::from(success)),"atomic persistence {case}");
        if success {assert!(matches!(rx.try_recv(),Ok(ServerFrame::ChatGroundedness {score:Some(0.375),..})));assert!(rx.try_recv().is_err());}
        else {assert!(rx.try_recv().is_err(),"failed verification must not publish success");}
        if case==31 {sqlx::query("DROP TRIGGER refuse_verification_audit ON audit_events").execute(pool).await?;sqlx::query("DROP FUNCTION refuse_verification_audit()").execute(pool).await?;}
        if case==39 {sqlx::query("ALTER TABLE claim_verdicts DROP CONSTRAINT reject_test_span").execute(pool).await?;}
        sqlx::query("DELETE FROM verification_runs WHERE target_id=$1").bind(message).execute(pool).await?;
        sqlx::query("DELETE FROM chats WHERE id=$1").bind(chat).execute(pool).await?;
    }
    sqlx::query("DELETE FROM knowledge_bases WHERE id=$1").bind(kb).execute(pool).await?;
    server.abort();Ok(())
}
