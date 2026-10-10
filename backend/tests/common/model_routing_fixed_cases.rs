// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
// Included by the isolated-schema integration suite; not a standalone test lane.
pub async fn check_fixed(pool:sqlx::PgPool) -> anyhow::Result<()> {
    use fosnie_backend::{auth::{AuthContext,PlatformRole},config::BootConfig,ext::{ProviderRegistry,ResolvedProvider},
        model_routing::{*,fixed::*},state::AppStateBuilder};
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    use uuid::Uuid;
    struct Registry(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl ProviderRegistry for Registry {
        async fn resolve(&self,_:&sqlx::PgPool,role:&str,_:Option<Uuid>) -> fosnie_backend::Result<Option<ResolvedProvider>> {
            self.0.fetch_add(1,Ordering::SeqCst);
            assert_ne!(role,"llm");
            Ok(Some(ResolvedProvider { base_url:Some("http://fixed.test/v1".into()),model:Some("fixed-model".into()),
                api_key:Some("injected-fixed-key".into()),enabled:true,reasoning_mode:None }))
        }
        async fn resolve_active_embed(&self,_:&sqlx::PgPool,_:Uuid,_:i64) -> fosnie_backend::Result<Option<ResolvedProvider>> {
            self.0.fetch_add(1,Ordering::SeqCst);
            Ok(Some(ResolvedProvider { base_url:Some("http://active.test/v1".into()),model:Some("active-model".into()),
                api_key:Some("active-key".into()),enabled:true,reasoning_mode:None }))
        }
        async fn resolve_desired_embed(&self,_:&sqlx::PgPool,_:Uuid,_:i64) -> fosnie_backend::Result<Option<ResolvedProvider>> {
            self.0.fetch_add(1,Ordering::SeqCst);
            Ok(Some(ResolvedProvider { base_url:Some("http://desired.test/v1".into()),model:Some("future-model".into()),
                api_key:Some("desired-key".into()),enabled:true,reasoning_mode:None }))
        }
        async fn resolve_fixed_id(&self,pool:&sqlx::PgPool,role:&str,user:Option<Uuid>,_:Uuid) -> fosnie_backend::Result<Option<ResolvedProvider>> {
            self.resolve(pool,role,user).await
        }
        async fn resolve_llm_id(&self,_:&sqlx::PgPool,_:Option<Uuid>,_:Uuid) -> fosnie_backend::Result<Option<ResolvedProvider>> {
            self.0.fetch_add(1,Ordering::SeqCst);
            Ok(Some(ResolvedProvider { base_url:Some("http://fixed.test/v1".into()),model:Some("fixed-model".into()),
                api_key:Some("injected-llm-key".into()),enabled:true,reasoning_mode:None }))
        }
    }
    let user=Uuid::now_v7();
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Fixed','client_admin')")
        .bind(user).bind(format!("{user}@routing.test")).execute(&pool).await?;
    let admin=AuthContext { user_id:Some(user),email:None,role:PlatformRole::ClientAdmin,display_name:None,break_glass:false,mfa_enroll_only:false };
    let mut regular=admin.clone(); regular.role=PlatformRole::User;
    let count=Arc::new(AtomicUsize::new(0));
    let mut boot=BootConfig::default(); boot.providers.user_byok_enabled=true;
    let state=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(boot))
        .with_providers(Arc::new(Registry(count.clone()))).build();
    let context=RoutingContext { actor:admin.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Primary };
    let local=RoutingConstraints { local_only:true,..Default::default() };
    let mut embed=None;
    for role in [FixedRole::Embed,FixedRole::Rerank,FixedRole::Ocr,FixedRole::Stt,FixedRole::Tts,FixedRole::Verify] {
        let id=Uuid::now_v7();
        sqlx::query("INSERT INTO provider_configs(id,role,scope,base_url,model) VALUES($1,$2,'deployment','http://fixed.test/v1','fixed-model')")
            .bind(id).bind(role.name()).execute(&pool).await?;
        let profile=FixedProfile { provider_id:id,provider_revision:1,revision:1,role,model:"fixed-model".into(),active_index_revision:None,desired_index_revision:None,
            locality:Locality::Cloud,locality_trusted:true,ocr_pdf_mode:None };
        let body=||SaveFixedProfile { expected_revision:0,profile:profile.clone() };
        assert!(save_profile(&state,&regular,id,body()).await.is_err());
        let mut saved=save_profile(&state,&admin,id,body()).await?;
        let before=count.load(Ordering::SeqCst);
        assert!(resolve(&state,&context,role,None,&local).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst),before,"local-only blocks fixed cloud roles before secrets");
        saved.locality=Locality::Local;
        saved=save_profile(&state,&admin,id,SaveFixedProfile { expected_revision:saved.revision,profile:saved }).await?;
        let target=resolve(&state,&context,role,Some(&saved.reference()),&local).await?;
        assert_eq!(target.reference.role,role);
        assert_eq!(target.api_key,"injected-fixed-key");
        assert!(!format!("{target:?}").contains("injected-fixed-key"));
        assert_eq!(count.load(Ordering::SeqCst),before+1);
        let none=RoutingConstraints { allowed_providers:Some(Default::default()),..Default::default() };
        assert!(resolve(&state,&context,role,None,&none).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst),before+1);
        if role==FixedRole::Embed { embed=Some(saved); }
    }
    let mut profile=embed.unwrap();
    // Personal fixed roles follow BYOK ownership; platform-wide roles never do.
    let own=Uuid::now_v7();
    sqlx::query("INSERT INTO provider_configs(id,role,scope,scope_id,base_url,model) VALUES($1,'tts','user',$2,'http://fixed.test/v1','fixed-model')")
        .bind(own).bind(user).execute(&pool).await?;
    let personal=FixedProfile { provider_id:own,provider_revision:1,revision:1,role:FixedRole::Tts,model:"fixed-model".into(),
        active_index_revision:None,desired_index_revision:None,locality:Locality::Local,locality_trusted:false,ocr_pdf_mode:None };
    let personal=save_profile(&state,&regular,own,SaveFixedProfile { expected_revision:0,profile:personal }).await?;
    let mut foreign=regular.clone(); foreign.user_id=Some(Uuid::now_v7());
    assert!(save_profile(&state,&foreign,own,SaveFixedProfile { expected_revision:personal.revision,profile:personal.clone() }).await.is_err());
    let mut forged=personal.clone(); forged.locality_trusted=true;
    assert!(save_profile(&state,&regular,own,SaveFixedProfile { expected_revision:personal.revision,profile:forged }).await.is_err());
    let before=count.load(Ordering::SeqCst);
    assert!(resolve(&state,&context,FixedRole::Tts,Some(&personal.reference()),&local).await.is_err(),"BYOK cannot self-attest locality");
    assert_eq!(count.load(Ordering::SeqCst),before);
    resolve(&state,&context,FixedRole::Tts,Some(&personal.reference()),&RoutingConstraints::default()).await?;
    let mut disabled_boot=BootConfig::default(); disabled_boot.providers.user_byok_enabled=false;
    let disabled=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(disabled_boot))
        .with_providers(Arc::new(Registry(count.clone()))).build();
    let before=count.load(Ordering::SeqCst);
    assert!(resolve(&disabled,&context,FixedRole::Tts,Some(&personal.reference()),&RoutingConstraints::default()).await.is_err(),"pinned BYOK cannot silently become deployment target");
    assert_eq!(count.load(Ordering::SeqCst),before);
    delete_profile(&state,&regular,own,personal.revision).await?;
    let recreated=save_profile(&state,&regular,own,SaveFixedProfile { expected_revision:0,profile:personal.clone() }).await?;
    assert!(recreated.revision>personal.revision);
    delete_profile(&state,&regular,own,recreated.revision).await?;
    sqlx::query("DELETE FROM provider_configs WHERE id=$1").bind(own).execute(&pool).await?;
    sqlx::query("INSERT INTO embedding_index(id,embed_model,embed_base_url,dim,collection_name) VALUES(1,'active-model','http://active.test/v1',384,'fixed_test_collection')")
        .execute(&pool).await?;
    let before=count.load(Ordering::SeqCst);
    assert!(resolve(&state,&context,FixedRole::Embed,None,&local).await.is_err(),"profile must attest the active index instead of desired provider");
    assert_eq!(count.load(Ordering::SeqCst),before);
    let active_revision:i64=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_one(&pool).await?;
    profile.model="active-model".into(); profile.active_index_revision=Some(active_revision);
    profile=save_profile(&state,&admin,profile.provider_id,SaveFixedProfile { expected_revision:profile.revision,profile }).await?;
    let target=resolve(&state,&context,FixedRole::Embed,Some(&profile.reference()),&local).await?;
    assert_eq!(target.reference.model,"active-model");
    assert_eq!(target.base_url,"http://active.test/v1");
    assert_eq!(target.api_key,"active-key");
    assert_eq!(target.embedding_dimension,Some(384));
    assert_eq!(target.embedding_collection.as_deref(),Some("fixed_test_collection"));
    // Fixed preparation carries active provenance to ML and uses the same run
    // ledger as LLM dispatch. Nothing sends a real model request in this test.
    let policy_id=Uuid::now_v7();
    let policy=store::save_policy(&state,&admin,policy_id,store::SavePolicy { label:"fixed lease fixture".into(),scope:"user".into(),
        expected_revision:0,is_default:false,policy:RoutingPolicy { id:policy_id,revision:1,rollout:RolloutMode::Off,
            strategy:RoutingStrategy::Rules,objective:RoutingObjective::Balanced,constraints:local.clone(),rules:vec![],
            default_provider_id:None,classifier_provider_id:None,utility_assignments:Default::default(),technical_fallback:false,classifier:Default::default(),quality_escalation:false,learned_ranking:false } }).await?;
    let mut plan=ExecutionPlan { id:context.run_id,schema_version:SCHEMA_VERSION,actor_user_id:admin.user_id,selection:ModelSelection::Default,
        decision:RoutingDecision { id:Uuid::now_v7(),schema_version:SCHEMA_VERSION,policy_id,policy_revision:policy.revision,
            strategy:RoutingStrategy::Rules,proposed:None,actual:None,reason_codes:vec![],excluded_counts:Default::default(),estimated_cost_microusd:None },
        constraints:local.clone(),assignments:Default::default(),fixed_assignments:Default::default() };
    let mut fixed_context=context.clone(); fixed_context.purpose=CallPurpose::Embed;
    fixed_lease::pin(&state,&context,&mut plan,&[FixedRole::Embed]).await?;
    let encoded=serde_json::to_string(&plan)?;
    assert!(!encoded.contains("api_key") && !encoded.contains("active-key") && !encoded.contains("base_url"));
    let plan:ExecutionPlan=serde_json::from_str(&encoded)?;
    let allowances=||[(FixedRole::Embed,fixed_lease::FixedAllowance { max_calls:2,input_bytes:1024,output_bytes:8192 })].into();
    let budget=budget::RunBudget::new(RoutingConstraints { max_calls:Some(3),..Default::default() });
    budget.reserve(10,10,None)?;
    let prepared=fixed_lease::prepare(&state,&fixed_context,&plan,&budget,allowances(),std::time::Duration::from_secs(1)).await?;
    let wire=prepared.envelope.apply(Default::default())?;
    assert_eq!(wire["routing"]["fixed_assignments"]["embed"]["active_index_revision"],active_revision);
    assert_eq!(wire["routing"]["fixed_assignments"]["embed"]["embedding_dimension"],384);
    assert_eq!(budget.snapshot().calls,3);
    assert!(budget.reserve(1,1,None).is_err(),"fixed helpers and LLM share the call cap");
    let empty=fixed_lease::FixedReport { lease_id:prepared.envelope.fixed_budget.as_ref().unwrap().id,complete:true,calls:vec![] };
    prepared.finish(Some(&empty))?;
    assert_eq!(budget.snapshot().calls,1);
    assert_eq!(budget.snapshot().unknown_token_calls,0);
    let before=count.load(Ordering::SeqCst);
    let mut wrong_actor=fixed_context.clone(); wrong_actor.actor.user_id=Some(Uuid::now_v7());
    assert!(fixed_lease::prepare(&state,&wrong_actor,&plan,&budget,allowances(),std::time::Duration::from_secs(1)).await.is_err());
    let mut stale=plan.clone(); stale.fixed_assignments.get_mut(&FixedRole::Embed).unwrap().profile_revision+=1;
    assert!(fixed_lease::prepare(&state,&fixed_context,&stale,&budget,allowances(),std::time::Duration::from_secs(1)).await.is_err());
    assert_eq!(count.load(Ordering::SeqCst),before,"actor/stale attestation reject before decrypt");
    let money=budget::RunBudget::new(RoutingConstraints { max_cost_microusd:Some(100),..Default::default() });
    assert!(fixed_lease::prepare(&state,&fixed_context,&plan,&money,allowances(),std::time::Duration::from_secs(1)).await.is_err());
    assert_eq!(money.snapshot().calls,0);
    sqlx::query("UPDATE embedding_index SET desired_model='future-model',desired_base_url='http://desired.test/v1',desired_dim=768 WHERE id=1").execute(&pool).await?;
    let desired_revision:i64=sqlx::query_scalar("SELECT desired_routing_revision FROM embedding_index WHERE id=1").fetch_one(&pool).await?;
    let desired=FixedProfile { role:FixedRole::EmbedDesired,model:"future-model".into(),active_index_revision:None,
        desired_index_revision:Some(desired_revision),locality:Locality::Cloud,..profile.clone() };
    assert!(save_profile(&state,&regular,profile.provider_id,SaveFixedProfile { expected_revision:0,profile:desired.clone() }).await.is_err());
    let mut desired=save_profile(&state,&admin,profile.provider_id,SaveFixedProfile { expected_revision:0,profile:desired }).await?;
    let before=count.load(Ordering::SeqCst);
    assert!(resolve(&state,&context,FixedRole::EmbedDesired,Some(&desired.reference()),&local).await.is_err());
    assert_eq!(count.load(Ordering::SeqCst),before,"desired locality checks precede secrets independently from active locality");
    desired.locality=Locality::Local;
    let desired=save_profile(&state,&admin,profile.provider_id,SaveFixedProfile { expected_revision:desired.revision,profile:desired }).await?;
    let desired_target=resolve(&state,&context,FixedRole::EmbedDesired,Some(&desired.reference()),&local).await?;
    assert_eq!(desired_target.api_key,"desired-key");
    assert_eq!(desired_target.embedding_collection.as_deref(),Some("pai_kb__768__future_model"));
    assert_eq!(desired_target.embedding_dimension,Some(768));
    let unsupported=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(BootConfig::default()))
        .with_providers(Arc::new(NoActive)).build();
    assert!(resolve(&unsupported,&context,FixedRole::EmbedDesired,Some(&desired.reference()),&local).await.is_err(),"unsupported desired registry never falls through to DB/active key");
    assert_eq!(resolve(&state,&context,FixedRole::Embed,Some(&profile.reference()),&local).await?.api_key,"active-key");
    // Verification modes share endpoint ownership, not the model attestation.
    let live=catalog(&state,&admin).await?.into_iter().find(|p|p.role==FixedRole::Verify).unwrap();
    let live:FixedProfile=serde_json::from_value(live.profile.unwrap())?;
    let claims=FixedProfile { role:FixedRole::VerifyClaims,model:"claims-model".into(),..live.clone() };
    assert!(save_profile(&state,&regular,live.provider_id,SaveFixedProfile { expected_revision:0,profile:claims.clone() }).await.is_err());
    let claims=save_profile(&state,&admin,live.provider_id,SaveFixedProfile { expected_revision:0,profile:claims }).await?;
    assert_eq!(resolve(&state,&context,FixedRole::VerifyClaims,Some(&claims.reference()),&local).await?.reference.model,"claims-model");
    assert_eq!(resolve(&state,&context,FixedRole::Verify,Some(&live.reference()),&local).await?.reference.model,"fixed-model");
    assert!(resolve(&state,&context,FixedRole::VerifyClaims,Some(&live.reference()),&local).await.is_err());
    let mut pinned=plan.clone();
    fixed_lease::pin(&state,&context,&mut pinned,&[FixedRole::EmbedDesired,FixedRole::Verify,FixedRole::VerifyClaims]).await?;
    assert_eq!(pinned.fixed_assignments.len(),4);
    // One preparation reserves LLM + desired embeddings, while carrying the
    // active index only as source provenance (no inference allowance for it).
    let llm_id=Uuid::now_v7();
    let llm_revision:i64=sqlx::query_scalar("INSERT INTO provider_configs(id,role,scope,base_url,model) VALUES($1,'llm','deployment','http://fixed.test/v1','fixed-model') RETURNING routing_revision")
        .bind(llm_id).fetch_one(&pool).await?;
    let llm_profile:ModelProfile=serde_json::from_value(serde_json::json!({"provider_id":llm_id,"provider_revision":llm_revision,"revision":1,
        "adapter":"openai_chat","model":"fixed-model","locality":"local","locality_trusted":true,
        "capabilities":{"context_tokens":10000,"max_output_tokens":1000}}))?;
    let llm_profile=store::save_profile(&state,&admin,llm_id,store::SaveProfile { expected_revision:0,profile:llm_profile }).await?;
    let mut mixed_plan=pinned.clone();
    mixed_plan.assignments.insert(CallPurpose::Primary,TargetReference { provider_id:llm_id,provider_revision:llm_revision,
        profile_revision:llm_profile.revision,model:llm_profile.model.clone(),effort:None });
    let stages=[(CallPurpose::Primary,dispatch::StageRequest { max_calls:2,
        requirements:WorkloadRequirements { input_tokens:100,output_tokens:20,..Default::default() } })].into();
    let desired_allowances=||[(FixedRole::EmbedDesired,fixed_lease::FixedAllowance { max_calls:2,input_bytes:1024,output_bytes:8192 })].into();
    let references=[FixedRole::Embed].into();
    let mixed_budget=budget::RunBudget::new(RoutingConstraints { max_calls:Some(4),..Default::default() });
    let request=mixed::prepare(&state,&context,&mixed_plan,&mixed_budget,&stages,desired_allowances(),&references,std::time::Duration::from_secs(2)).await?;
    assert_eq!(mixed_budget.snapshot().calls,4);
    assert_eq!(mixed_budget.snapshot().unknown_token_calls,2);
    assert_eq!(request.envelope.fixed_assignments[&FixedRole::Embed].reference.active_index_revision,Some(active_revision));
    assert_eq!(request.envelope.fixed_assignments[&FixedRole::EmbedDesired].api_key,"desired-key");
    assert!(!request.envelope.fixed_budget.as_ref().unwrap().allowances.contains_key(&FixedRole::Embed));
    let reports=mixed::Reports { routing_report:Some(lease::LeaseReport { lease_id:request.envelope.budget.as_ref().unwrap().id,complete:true,calls:vec![] }),
        fixed_routing_report:Some(fixed_lease::FixedReport { lease_id:request.envelope.fixed_budget.as_ref().unwrap().id,complete:true,calls:vec![] }) };
    assert!(request.finish(&reports)?); assert_eq!(mixed_budget.snapshot().calls,0);
    let too_small=budget::RunBudget::new(RoutingConstraints { max_calls:Some(3),..Default::default() });
    assert!(mixed::prepare(&state,&context,&mixed_plan,&too_small,&stages,desired_allowances(),&references,std::time::Duration::from_secs(2)).await.is_err());
    assert_eq!(too_small.snapshot().calls,0,"fixed rejection cannot leave the LLM lane charged");
    let mut stale_mixed=mixed_plan.clone(); stale_mixed.fixed_assignments.get_mut(&FixedRole::EmbedDesired).unwrap().desired_index_revision=Some(0);
    assert!(mixed::prepare(&state,&context,&stale_mixed,&mixed_budget,&stages,desired_allowances(),&references,std::time::Duration::from_secs(2)).await.is_err());
    assert_eq!(mixed_budget.snapshot().calls,0,"failed target preparation cannot charge any lane");
    struct DenyEmbed;
    #[async_trait::async_trait]
    impl InferencePolicy for DenyEmbed {
        async fn constraints(&self,_:&sqlx::PgPool,context:&RoutingContext) -> fosnie_backend::Result<RoutingConstraints> {
            Ok(RoutingConstraints { allowed_providers:if context.purpose==CallPurpose::Embed { Some(Default::default()) } else { None },..Default::default() })
        }
    }
    let denied=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(BootConfig::default()))
        .with_providers(Arc::new(Registry(count.clone()))).with_inference_policy(Arc::new(DenyEmbed)).build();
    let before=count.load(Ordering::SeqCst);
    assert!(mixed::prepare(&denied,&context,&mixed_plan,&mixed_budget,&stages,desired_allowances(),&references,std::time::Duration::from_secs(2)).await.is_err());
    assert_eq!(count.load(Ordering::SeqCst),before,"all purpose-specific edition restrictions precede decryption");
    assert_eq!(mixed_budget.snapshot().calls,0);
    sqlx::query("DELETE FROM provider_configs WHERE id=$1").bind(llm_id).execute(&pool).await?;
    let encoded=serde_json::to_string(&pinned)?;
    assert!(!encoded.contains("desired-key") && !encoded.contains("active-key") && !encoded.contains("base_url"));
    let mut pinned:ExecutionPlan=serde_json::from_str(&encoded)?;
    sqlx::query("UPDATE embedding_index SET reindex_done=13,status='reindexing' WHERE id=1").execute(&pool).await?;
    let stable:(i64,i64)=sqlx::query_as("SELECT routing_revision,desired_routing_revision FROM embedding_index WHERE id=1").fetch_one(&pool).await?;
    assert_eq!(stable,(active_revision,desired_revision));
    let db=fosnie_backend::providers::DbProviderRegistry::new(None);
    assert_eq!(db.resolve_desired_embed(&pool,profile.provider_id,desired_revision).await?.unwrap().model.as_deref(),Some("future-model"));
    sqlx::query("UPDATE embedding_index SET desired_api_key_encrypted='broken-desired-key' WHERE id=1").execute(&pool).await?;
    let changed_desired:i64=sqlx::query_scalar("SELECT desired_routing_revision FROM embedding_index WHERE id=1").fetch_one(&pool).await?;
    assert!(changed_desired>desired_revision);
    assert!(db.resolve_desired_embed(&pool,profile.provider_id,changed_desired).await.is_err());
    let before=count.load(Ordering::SeqCst);
    assert!(fixed_lease::pin(&state,&context,&mut pinned,&[FixedRole::EmbedDesired]).await.is_err(),"resume never repins a changed staged index");
    assert_eq!(count.load(Ordering::SeqCst),before);
    assert_eq!(pinned.fixed_assignments[&FixedRole::EmbedDesired].desired_index_revision,Some(desired_revision));
    assert_eq!(resolve(&state,&context,FixedRole::Embed,Some(&profile.reference()),&local).await?.reference.active_index_revision,Some(active_revision));
    delete_profile_stage(&state,&admin,profile.provider_id,desired.revision,Some(FixedRole::EmbedDesired)).await?;
    delete_profile_stage(&state,&admin,live.provider_id,claims.revision,Some(FixedRole::VerifyClaims)).await?;
    resolve(&state,&context,FixedRole::Verify,Some(&live.reference()),&local).await?;
    sqlx::query("DELETE FROM routing_policies WHERE id=$1").bind(policy_id).execute(&pool).await?;
    // A wrapper that has not implemented active-index resolution cannot be
    // bypassed by calling Core's DB implementation directly.
    struct NoActive;
    #[async_trait::async_trait]
    impl ProviderRegistry for NoActive {
        async fn resolve(&self,_:&sqlx::PgPool,_:&str,_:Option<Uuid>) -> fosnie_backend::Result<Option<ResolvedProvider>> {
            panic!("active index must not use the generic role fallback")
        }
    }
    let no_active=AppStateBuilder::new(pool.clone(),fosnie_backend::cache::create_pool("redis://127.0.0.1:16389")?,Arc::new(BootConfig::default()))
        .with_providers(Arc::new(NoActive)).build();
    assert!(resolve(&no_active,&context,FixedRole::Embed,Some(&profile.reference()),&local).await.is_err());
    sqlx::query("UPDATE embedding_index SET desired_model='future-model',reindex_done=12,status='reindexing' WHERE id=1").execute(&pool).await?;
    let still:i64=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_one(&pool).await?;
    assert_eq!(still,active_revision,"staging/progress does not invalidate active provenance");
    resolve(&state,&context,FixedRole::Embed,Some(&profile.reference()),&local).await?;
    sqlx::query("UPDATE embedding_index SET embed_api_key_encrypted='invalid-ciphertext' WHERE id=1").execute(&pool).await?;
    let changed:i64=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_one(&pool).await?;
    assert!(changed>still,"active credential changes invalidate attestation without disclosing secrets");
    let before=count.load(Ordering::SeqCst);
    assert!(resolve(&state,&context,FixedRole::Embed,Some(&profile.reference()),&local).await.is_err());
    assert_eq!(count.load(Ordering::SeqCst),before);
    // Default registry does not swallow active-index decryption failures.
    let db=fosnie_backend::providers::DbProviderRegistry::new(None);
    assert!(db.resolve_fixed_id(&pool,"tts",Some(user),own).await?.is_none(),"deleted personal target never resolves the deployment key");
    assert!(db.resolve_fixed_id(&pool,"llm",Some(user),own).await.is_err());
    assert!(db.resolve_active_embed(&pool,profile.provider_id,changed).await.is_err());
    sqlx::query("UPDATE embedding_index SET embed_api_key_encrypted=NULL WHERE id=1").execute(&pool).await?;
    let fresh:i64=sqlx::query_scalar("SELECT routing_revision FROM embedding_index WHERE id=1").fetch_one(&pool).await?;
    let unauthenticated=db.resolve_active_embed(&pool,profile.provider_id,fresh).await?.unwrap();
    assert_eq!(unauthenticated.api_key,None,"explicit unauthenticated active endpoint remains usable");
    assert_eq!(unauthenticated.model.as_deref(),Some("active-model"));
    let cat=serde_json::to_string(&catalog(&state,&admin).await?)?;
    assert!(!cat.contains("api_key") && !cat.contains("invalid-ciphertext") && !cat.contains("active-key"));
    assert!(delete_profile(&state,&regular,profile.provider_id,profile.revision).await.is_err());
    delete_profile(&state,&admin,profile.provider_id,profile.revision).await?;
    assert!(delete_profile(&state,&admin,profile.provider_id,profile.revision).await.is_err());
    // Leave the common upgrade suite's other checks independent of this fixture.
    sqlx::query("DELETE FROM embedding_index WHERE id=1").execute(&pool).await?;
    Ok(())
}
