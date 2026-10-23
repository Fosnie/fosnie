// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Real Rust -> ML -> native HTTP with synthetic responses and no external calls.
use super::*;
use fosnie_backend::model_routing::{run::RoutedRun,vnext::{catalogue::VariantRef,profile::*,profiles,variants}};
use fosnie_protocol::routing_vnext::RoutingIntentV2;

pub async fn exercise(state:&AppState,admin:&AuthContext,small:Uuid,large:Uuid,judge:Uuid,
    trace:&Arc<Mutex<Vec<Value>>>)->anyhow::Result<()> {
    let mut refs=Vec::new();
    for provider in [small,large] {
        let deployment=variants::catalogue(state,admin).await?.deployments.into_iter().find(|d|d.id==provider).unwrap();
        let id=Uuid::now_v7();
        let body:variants::SaveVariant=serde_json::from_value(json!({"schema_version":2,"expected_revision":0,
            "expected_model_config_revision":deployment.model_config_revision,"scope":"deployment","provider_id":provider,
            "label":format!("Builder {provider}"),"effort":{"kind":"provider_default"},"generation":{"max_output_tokens":512}}))?;
        let saved=variants::save(state,admin,id,body).await?;refs.push(VariantRef{id:saved.id,revision:saved.revision});
    }
    let uid=Uuid::now_v7();
    sqlx::query("INSERT INTO users(id,email,display_name,role) VALUES($1,$2,'Ordinary routing user','user')")
        .bind(uid).bind(format!("{uid}@routing.test")).execute(&state.pg).await?;
    let actor=AuthContext{user_id:Some(uid),role:PlatformRole::User,..admin.clone()};
    assert!(!store::byok_enabled(state).await?);
    let shared=variants::catalogue(state,&actor).await?.deployments.into_iter().find(|d|d.id==small).unwrap();
    let personal_id=Uuid::now_v7();
    let personal:variants::SaveVariant=serde_json::from_value(json!({"schema_version":2,"expected_revision":0,
        "expected_model_config_revision":shared.model_config_revision,"scope":"user","provider_id":small,
        "label":"My shared-model variant","effort":{"kind":"provider_default"},"generation":{"max_output_tokens":256}}))?;
    variants::save(state,&actor,personal_id,personal).await?;
    assert!(variants::catalogue(state,&actor).await?.variants.iter().any(|v|v.variant.id==personal_id));
    assert!(!variants::catalogue(state,admin).await?.variants.iter().any(|v|v.variant.id==personal_id));
    let id=Uuid::now_v7();
    let mut profile=profiles::save(state,&actor,id,profiles::SaveProfile{schema_version:2,scope:"user".into(),label:"Personal builder".into(),expected_revision:0,is_default:false,
        policy:serde_json::from_value(json!({"id":id,"revision":1,"strategy":"hybrid","rollout":"active","default_provider_id":large,
            "classifier_provider_id":judge,"classifier":{"cloud_timeout_ms":10000},"constraints":{"max_calls":4,"max_duration_ms":30000}}))?,
        config:serde_json::from_value(json!({"schema_version":2,"membership":{"kind":"all_available"},"max_active_variants":256,
            "quality":{"assurance":"declared","max_relative_loss_bp":0,"minimum_absolute_quality_bp":null},
            "reference":{"kind":"configured","variant":refs[1],"allow_uncalibrated":true},"fallback_variant_ids":[],"groups":[],"selector_sha256":null,"verifier_sha256":null}))?}).await?;
    // The preceding browser scenario deliberately leaves the shared policy
    // local-only. A personal profile must not be able to route around it.
    let blocked_context=RoutingContext{actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Primary};
    let blocked_intent=RoutingIntentV2{schema_version:2,selection:ModelSelection::Auto{objective:RoutingObjective::Balanced,policy_id:Some(id)},required_profile_version:2,variant:None,options:None};
    let generations_before=trace.lock().unwrap().iter().filter(|r|r["stream"]==true).count();
    assert!(fosnie_backend::model_routing::vnext::execution::plan(state,&blocked_context,blocked_intent,
        WorkloadRequirements{minimum_tier:ModelTier::Advanced,input_tokens:256,output_tokens:512,..Default::default()},
        "Write an honours project about Kubernetes.",&tokio::sync::Notify::new()).await.is_err());
    assert_eq!(trace.lock().unwrap().iter().filter(|r|r["stream"]==true).count(),generations_before);
    // Restore the fixture's shared policy as its administrator before testing
    // the independent local/cloud routing cases below.
    let mut shared=store::load_policy(state,admin,None).await?;
    assert!(shared.constraints.local_only);shared.constraints.local_only=false;
    store::save_policy(state,admin,shared.id,store::SavePolicy{label:"Routing stack policy".into(),scope:"deployment".into(),
        is_default:true,expected_revision:shared.revision,policy:shared}).await?;
    let cases=[
        ("When is the next Celtic match at Celtic Park?",small,None),
        ("Write an honours project about intercommunication of Kubernetes microservices and monitoring.",large,None),
        ("I am tired. Recommend a thriller for tonight.",large,Some((Difficulty::Routine,refs[1]))),
        ("Write an honours project about Kubernetes microservices.",small,Some((Difficulty::Complex,refs[0]))),
    ];
    let mut evidence=Vec::new();
    for (prompt,expected,assignment) in cases {
        if let Some((level,variant))=assignment {
            let mut config=profile.config.clone();config.groups=vec![TaskGroup{id:Uuid::now_v7(),label:"My exact assignment".into(),priority:10,
                conditions:Default::default(),difficulty:Some(DifficultyCondition{levels:[level].into(),min_confidence_bp:7500,include_unknown:false}),
                allowed_variants:None,excluded_variants:Default::default(),assignment:Some(GroupAssignment::Pin{variant})}];
            profile=profiles::save(state,&actor,id,profiles::SaveProfile{schema_version:2,scope:"user".into(),label:profile.label.clone(),expected_revision:profile.revision,
                is_default:false,policy:profile.policy.clone(),config}).await?;
        }
        let context=RoutingContext{actor:actor.clone(),chat_id:None,project_id:None,run_id:Uuid::now_v7(),purpose:CallPurpose::Primary};
        let intent=RoutingIntentV2{schema_version:2,selection:ModelSelection::Auto{objective:RoutingObjective::Balanced,policy_id:Some(id)},required_profile_version:2,variant:None,options:None};
        let run=RoutedRun::selected_vnext(state,context,intent,WorkloadRequirements{input_tokens:256,output_tokens:512,..Default::default()},prompt,&[],&tokio::sync::Notify::new()).await?;
        assert_eq!(run.plan().assignments[&CallPurpose::Primary].provider_id,expected,"{prompt}");
        let request=fosnie_backend::ml::GenerateRequest {
            messages:vec![json!({"role":"user","content":prompt})],
            sampling:fosnie_backend::ml::Sampling{max_tokens:Some(512),..Default::default()},
            model:None,tools:None,overrides:Default::default(),
        };
        let mut stream=run.generate(state,request).await?;let mut completed=false;
        while let Some(event)=stream.recv().await? {if event["type"]=="done"{completed=true;}}
        assert!(completed,"native stream did not finish");
        let calls=trace.lock().unwrap();let native=calls.last().unwrap();
        assert_eq!(native["model"],if expected==small{"routing-e2e-local"}else{"routing-e2e-cloud"});
        assert_eq!(native["messages"].as_array().unwrap().last().unwrap()["content"],prompt);
        evidence.push(json!({"prompt":prompt,"actual_native_model":native["model"],"expected_provider":expected,
            "reason_codes":run.plan().decision.reason_codes,"difficulty":run.plan().vnext_context.as_ref().unwrap().difficulty}));
    }
    if let Some(path)=std::env::var_os("FOSNIE_ROUTING_EVIDENCE_DIR") {
        std::fs::write(std::path::PathBuf::from(path).join("builder-native-dispatch.json"),serde_json::to_vec_pretty(&json!({
            "cases":evidence,"ordinary_user":true,"BYOK_enabled":false,"personal_variant_of_shared_model":true,
            "shared_local_only_enforced":true,"native_HTTP":"loopback_synthetic","external_model_calls":0}))?)?;
    }
    eprintln!("Builder: ordinary user, BYOK off, routine/complex selection and both inverse assignments reached the intended native model.");
    Ok(())
}
