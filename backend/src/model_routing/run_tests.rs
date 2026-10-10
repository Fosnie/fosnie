// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::*;
use uuid::Uuid;
use crate::auth::{AuthContext,PlatformRole};
fn parts() -> (RoutingContext,ExecutionPlan) {
    let context=RoutingContext { actor:AuthContext { user_id:Some(Uuid::now_v7()),email:Some("private@example.test".into()),
        display_name:None,role:PlatformRole::User,break_glass:false,mfa_enroll_only:false },chat_id:Some(Uuid::now_v7()),
        project_id:Some(Uuid::now_v7()),run_id:Uuid::now_v7(),purpose:CallPurpose::Primary };
    let reference=TargetReference { provider_id:Uuid::now_v7(),provider_revision:2,profile_revision:3,model:"pinned-model".into(),effort:None };
    let assignments=[CallPurpose::Primary,CallPurpose::ToolLoop,CallPurpose::Final,CallPurpose::UtilityTitle,CallPurpose::UtilitySummary]
        .into_iter().map(|p|(p,reference.clone())).collect();
    let plan=ExecutionPlan { id:context.run_id,schema_version:1,actor_user_id:context.actor.user_id,selection:ModelSelection::Default,
        decision:RoutingDecision { id:Uuid::now_v7(),schema_version:1,policy_id:Uuid::now_v7(),policy_revision:1,strategy:RoutingStrategy::Rules,
            proposed:Some(reference),actual:None,reason_codes:vec![],excluded_counts:Default::default(),estimated_cost_microusd:None },
        constraints:Default::default(),assignments,fixed_assignments:Default::default() };
    (context,plan)
}
fn client() -> PipelineClient { PipelineClient::new("http://127.0.0.1:9","never-log-secret").unwrap() }
fn fixture(limits:RoutingConstraints) -> RoutedRun {
    let (context,mut plan)=parts(); plan.constraints=limits;
    let client=client();
    RoutedRun::from_planned(context,plan,RunBudget::new(Default::default()),client).unwrap()
}
#[test]
fn runtime_rejects_wrong_actor_run_schema_or_unpinned_answer_models() {
    for mode in 0..7 {
        let (mut context,mut plan)=parts();
        match mode { 0=>context.actor.user_id=None,1=>plan.id=Uuid::now_v7(),2=>plan.schema_version=99,
            3=>{plan.assignments.remove(&CallPurpose::Primary);},4=>{plan.assignments.get_mut(&CallPurpose::Final).unwrap().model="other".into();},
            5=>{plan.assignments.get_mut(&CallPurpose::ToolLoop).unwrap().effort=Some("high".into());},_=>context.actor.mfa_enroll_only=true }
        assert!(RoutedRun::from_planned(context,plan,RunBudget::new(Default::default()),client()).is_err());
    }
}
#[test]
fn runtime_is_bound_to_authenticated_turn_and_existing_chat() {
    let run=fixture(Default::default()); let context=run.context(CallPurpose::Primary);
    assert!(run.validate_turn(&context.actor,context.run_id,context.chat_id).is_ok());
    assert!(run.validate_turn(&context.actor,Uuid::now_v7(),context.chat_id).is_err());
    assert!(run.validate_turn(&context.actor,context.run_id,None).is_err());
    assert!(run.validate_turn(&context.actor,context.run_id,Some(Uuid::now_v7())).is_err());
    for change in 0..4 {
        let mut actor=context.actor.clone();
        match change { 0=>actor.user_id=Some(Uuid::now_v7()),1=>actor.role=PlatformRole::ClientAdmin,2=>actor.break_glass=true,_=>actor.mfa_enroll_only=true }
        assert!(run.validate_turn(&actor,context.run_id,context.chat_id).is_err());
    }
}
#[tokio::test]
async fn child_keeps_original_actor_scope_plan_and_existing_spend() {
    let run=fixture(RoutingConstraints { max_calls:Some(2),..Default::default() });
    run.0.budget.reserve(20,30,None).unwrap();
    let expected=run.context(CallPurpose::UtilityTitle); let decision=run.plan().decision.id;
    let (context,id)=run.spawn(|child|async move {
        child.0.budget.reserve(1,2,None)?;
        Ok((child.context(CallPurpose::UtilityTitle),child.plan().decision.id))
    }).await.unwrap().unwrap();
    assert_eq!(context.actor.user_id,expected.actor.user_id); assert_eq!(context.actor.role,PlatformRole::User);
    assert_eq!(context.chat_id,expected.chat_id); assert_eq!(context.project_id,expected.project_id); assert_eq!(context.run_id,expected.run_id);
    assert_eq!(id,decision); assert_eq!(run.usage().calls,2); assert_eq!(run.usage().tokens,53);
    assert!(run.0.budget.reserve(0,1,None).is_err());
    let debug=format!("{run:?}"); assert!(!debug.contains("private@example") && !debug.contains("never-log-secret"));
}
#[tokio::test]
async fn cancellation_broadcasts_to_all_children_and_late_children() {
    let run=fixture(Default::default()); let barrier=Arc::new(tokio::sync::Barrier::new(3));
    let mut children=Vec::new();
    for _ in 0..2 { let barrier=barrier.clone(); children.push(run.spawn(move|_|async move {
        barrier.wait().await; std::future::pending::<crate::Result<()>>().await
    })); }
    barrier.wait().await; run.cancel();
    for child in children { assert!(tokio::time::timeout(Duration::from_secs(1),child).await.unwrap().unwrap().is_err()); }
    assert!(run.spawn::<(),_,_>(|_|async { panic!("cancelled child must not start") }).await.unwrap().is_err());
    assert!(run.is_cancelled());
}
#[tokio::test]
async fn external_notify_cancels_the_whole_runtime_stickily() {
    let run=fixture(Default::default()); let cancel=Notify::new(); cancel.notify_one();
    assert!(run.drive(&cancel,async { Ok(42) }).await.is_err());
    assert!(run.guard(async { Ok(43) }).await.is_err());
}
#[tokio::test]
async fn helper_deadline_is_shared_not_restarted_for_a_clone() {
    let run=fixture(RoutingConstraints { max_duration_ms:Some(25),..Default::default() });
    assert!(run.guard(std::future::pending::<crate::Result<()>>()).await.is_err());
    assert!(run.clone().guard(async { Ok(()) }).await.is_err());
}
#[tokio::test]
async fn exhausted_run_does_not_start_another_helper() {
    let run=fixture(Default::default()); run.0.budget.exhaust();
    assert!(run.guard::<()>(async { panic!("exhausted helper must not start") }).await.is_err());
}

#[tokio::test]
#[ignore = "explicit isolated PostgreSQL lane: FOSNIE_ROUTING_TEST_DATABASE_URL"]
async fn durable_stream_checkpoints_before_http_and_before_terminal_delivery() -> anyhow::Result<()> {
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize,Ordering};
    use sqlx::Executor;
    let url=std::env::var("FOSNIE_ROUTING_TEST_DATABASE_URL")?;
    let config:sqlx::postgres::PgConnectOptions=url.parse()?;
    anyhow::ensure!(config.get_database().is_some_and(|d|d.starts_with("routing_")),"use an isolated routing_ database");
    let admin=sqlx::PgPool::connect(&url).await?;
    let schema=format!("routing_stream_{}",Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}"))).execute(&admin).await?;
    let pool=sqlx::postgres::PgPoolOptions::new().max_connections(2)
        .connect_with(config.options([("search_path",schema.as_str())])).await?;
    let checked=tokio::spawn({let pool=pool.clone();async move {
        // Use the production schema, including the workflow/actor fences that
        // the real checkpoint queries reference even for a document job.
        for migration in sqlx::migrate!("./migrations").iter() {
            sqlx::raw_sql(migration.sql.clone()).execute(&pool).await?;
        }
        let mode=Arc::new(AtomicUsize::new(0));let sent=Arc::new(AtomicUsize::new(0));
        let app=axum::Router::new().route("/deep_research",axum::routing::post({
            let pool=pool.clone();let mode=mode.clone();let sent=sent.clone();
            move|axum::Json(body):axum::Json<Value>|{let pool=pool.clone();let mode=mode.clone();let sent=sent.clone();async move {
                sent.fetch_add(1,Ordering::SeqCst);
                let id=body["job"].as_str().unwrap();let case=mode.load(Ordering::SeqCst);
                let stored:i64=sqlx::query_scalar("SELECT (usage->>'calls')::bigint FROM routed_jobs WHERE id=$1")
                    .bind(Uuid::parse_str(id).unwrap()).fetch_one(&pool).await.unwrap();
                assert_eq!(stored,2,"reservation must already be durable when HTTP starts");
                let mut done=json!({"type":"done","fixed_routing_report":{
                    "lease_id":body["overrides"]["routing"]["fixed_budget"]["id"],"complete":true,"calls":[]}});
                if case==2 {done.as_object_mut().unwrap().remove("fixed_routing_report");}
                if case==3 {sqlx::query("UPDATE routed_jobs SET owner=$2 WHERE id=$1")
                    .bind(Uuid::parse_str(id).unwrap()).bind(Uuid::now_v7()).execute(&pool).await.unwrap();}
                if case==1 {"{\"type\":\"progress\"}\n".to_string()}
                else {format!("{{\"type\":\"progress\"}}\n{done}\n")}
            }}
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await?;let addr=listener.local_addr()?;
        let server=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
        for case in 0..7 {
            mode.store(case,Ordering::SeqCst);
            let mut run=fixture(Default::default());let id=Uuid::now_v7();let owner=Uuid::now_v7();
            let actor=run.context(CallPurpose::Primary).actor.user_id.unwrap();
            sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Durable stream fixture','user')")
                .bind(actor).bind(format!("{actor}@routing.test")).execute(&pool).await?;
            sqlx::query("INSERT INTO tasks(id,task_type) VALUES($1,'verify_draft')").bind(id).execute(&pool).await?;
            sqlx::query("INSERT INTO routed_jobs(id,task_id,actor_user_id,kind,context,plan,owner,status,lease_until,deadline,input,limits,usage) VALUES($1,$1,$3,'verify_draft','{\"role\":\"user\"}','{}',$2,'running',now()+interval '1 minute',now()+interval '1 hour','{}','{}','{}')")
                .bind(id).bind(owner).bind(actor).execute(&pool).await?;
            let inner=Arc::get_mut(&mut run.0).unwrap();
            inner.client=PipelineClient::new(&format!("http://{addr}"),"")?;
            inner.durable=Some(super::super::durable::Lease {pool:pool.clone(),id,owner});
            let targets=[(FixedRole::Ocr,super::super::fixed::FixedTarget {schema_version:1,
                reference:super::super::fixed::FixedReference {provider_id:Uuid::now_v7(),provider_revision:1,profile_revision:1,
                    role:FixedRole::Ocr,model:"ocr".into(),active_index_revision:None,desired_index_revision:None},
                base_url:"http://unused.test".into(),api_key:"".into(),embedding_dimension:None,embedding_collection:None,ocr_pdf_mode:None})].into();
            let grant=mixed::MixedGrant::reserve(&run.0.budget,run.plan().decision.id,&Default::default(),&[],Default::default(),
                &targets,[(FixedRole::Ocr,FixedAllowance {max_calls:2,input_bytes:100,output_bytes:100})].into(),Duration::from_secs(30))?;
            let envelope=grant.envelope(run.plan().decision.id,CallPurpose::Ocr,Default::default(),targets);
            let prepared=mixed::PreparedMixedRequest {envelope,grant};
            if case==6 {sqlx::query("UPDATE routed_jobs SET status='cancelled',owner=NULL,lease_until=NULL WHERE id=$1").bind(id).execute(&pool).await?;}
            let serial=Some(run.0.serial.clone().lock_owned().await);
            let before=sent.load(Ordering::SeqCst);
            let stream=run.open_stream(StreamOperation::Research,json!({"job":id}),prepared,serial).await;
            if case==6 {
                assert!(stream.is_err());assert_eq!(sent.load(Ordering::SeqCst),before);
                assert!(run.0.serial.try_lock().is_ok());continue;
            }
            let mut stream=stream?;
            assert_eq!(stream.recv().await?.unwrap()["type"],"progress");
            assert!(run.0.serial.try_lock().is_err(),"progress must keep the serial lock");
            if case==5 {drop(stream);} else {
                if case==4 {run.cancel();}
                let terminal=stream.recv().await;
                assert_eq!(terminal.is_ok(),case==0,"terminal {case}: {terminal:?}");
                if case==0 {assert_eq!(terminal?.unwrap()["type"],"done");assert!(stream.recv().await?.is_none());}
            }
            assert!(run.0.serial.try_lock().is_ok(),"terminal/drop must release the serial lock");
            let stored:i64=sqlx::query_scalar("SELECT (usage->>'calls')::bigint FROM routed_jobs WHERE id=$1")
                .bind(id).fetch_one(&pool).await?;
            assert_eq!(stored,if case==0 {0} else {2},"durable usage {case}");
        }
        server.abort();Ok::<_,anyhow::Error>(())
    }}).await;
    pool.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE"))).execute(&admin).await?;
    admin.close().await;checked?
}

struct BrowserOutputGuard(Uuid);
#[async_trait::async_trait]
impl AccessGuard for BrowserOutputGuard {
    async fn check(&self)->crate::Result<()> {Ok(())}
    fn browser_chat(&self)->Option<Uuid> {Some(self.0)}
}
#[test]
fn browser_output_ceiling_does_not_turn_fixed_bytes_into_token_evidence() {
    let run=fixture(RoutingConstraints {max_calls:Some(3),..Default::default()});
    let chat=run.context(CallPurpose::Primary).chat_id.unwrap();
    let run=run.with_access(Arc::new(BrowserOutputGuard(chat))).unwrap();
    assert_eq!(run.surface_output(2048),1024);
    let stages=|output|BTreeMap::from([(CallPurpose::Final,StageRequest {
        max_calls:1,requirements:WorkloadRequirements {output_tokens:output,..Default::default()}
    })]);
    assert!(run.validate_surface_stages(&stages(1024)).is_ok());
    assert!(run.validate_surface_stages(&stages(1025)).is_err());
    assert_eq!(run.usage().calls,0);
    run.0.budget.reserve_unmetered_many(1).unwrap().pop().unwrap().finish(None,None).unwrap();
    assert_eq!(run.usage().unknown_token_calls,1);
    run.0.budget.reserve(20,512,Some(0)).unwrap().finish(Some(40),Some(0)).unwrap();
    assert_eq!(run.usage().calls,2);
    assert_eq!(run.usage().tokens,40);
    assert_eq!(run.usage().unknown_token_calls,1);
}
#[test]
fn browser_fixed_services_still_reject_explicit_unknown_token_caps() {
    for limits in [RoutingConstraints {max_output_tokens:Some(1024),..Default::default()},
        RoutingConstraints {max_total_tokens:Some(30000),..Default::default()}] {
        let run=fixture(limits);
        let chat=run.context(CallPurpose::Primary).chat_id.unwrap();
        let run=run.with_access(Arc::new(BrowserOutputGuard(chat))).unwrap();
        assert!(run.0.budget.reserve_unmetered_many(1).is_err());
        assert_eq!(run.usage().calls,0);
    }
    assert_eq!(fixture(Default::default()).surface_output(2048),2048);
}
