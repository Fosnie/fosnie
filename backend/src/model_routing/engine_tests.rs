// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::{engine::*, *};

fn profile(id: u128, tier: ModelTier, price: Option<u64>) -> ModelProfile {
    let mut p: ModelProfile = serde_json::from_value(serde_json::json!({
        "provider_id": uuid::Uuid::from_u128(id), "provider_revision": 1, "revision": 1,
        "adapter": "openai_chat", "model": format!("opaque-{id}"), "auto_enabled": true,
        "locality": "local", "locality_trusted": true, "quality_status": "declared",
        "capabilities": {"context_tokens": 32000, "max_output_tokens": 8192, "tools": "verified", "efforts": ["low", "high"]}
    })).unwrap();
    p.tier = tier;
    p.pricing.input_microusd_per_million = price;
    p.pricing.output_microusd_per_million = price;
    p
}
fn policy() -> RoutingPolicy {
    serde_json::from_value(serde_json::json!({"id": uuid::Uuid::from_u128(100), "revision":1,
        "default_provider_id": uuid::Uuid::from_u128(2), "classifier_provider_id":null})).unwrap()
}
fn request() -> WorkloadRequirements {
    WorkloadRequirements { minimum_tier: ModelTier::Economy, input_tokens: 100, output_tokens: 500, ..Default::default() }
}

fn measured(mut p:ModelProfile,passed:u32,purpose:CallPurpose)->ModelProfile {
    let workload=WorkloadRequirements{task:TaskKind::General,..request()};
    let batch:learning::EvidenceBatch=serde_json::from_value(serde_json::json!({
        "schema_version":1,"source":"live","dataset_sha256":"a".repeat(64),"profile_sha256":learning::fingerprint(&p),
        "evaluator_version":"fixture-v1","purpose":purpose,"workload":workload,
        "families":(0..40).map(|i|serde_json::json!({"id":format!("family-{i}"),"split":"test"})).collect::<Vec<_>>(),
        "observations":(0..passed).map(|i|serde_json::json!({"family_id":format!("family-{i}"),"outcome":"pass"})).collect::<Vec<_>>()
    })).unwrap();
    let mut record=learning::aggregate(&p,batch).unwrap();assert!(!record.reviewed);record.reviewed=true;
    assert_eq!(record.families,40);p.quality_evidence=vec![record];p
}

#[test]
fn p12_measured_floor_objectives_and_explicit_choices() {
    let mut p=policy();p.learned_ranking=true;
    let req=WorkloadRequirements{task:TaskKind::General,..request()};
    let mut profiles=vec![measured(profile(1,ModelTier::Economy,Some(0)),37,CallPurpose::Primary),
        measured(profile(2,ModelTier::Advanced,Some(1000)),40,CallPurpose::Primary)];
    let select=|profiles:&[ModelProfile],objective,rule:&RuleMatch,default|super::engine::select(profiles,&req,&p,&RoutingConstraints::default(),objective,rule,default).unwrap();
    assert_eq!(select(&profiles,RoutingObjective::Economy,&RuleMatch::default(),false).proposed.unwrap().provider_id,profiles[0].provider_id);
    assert_eq!(select(&profiles,RoutingObjective::Quality,&RuleMatch::default(),false).proposed.unwrap().provider_id,profiles[1].provider_id);
    profiles[0].quality_evidence[0].passed=20;
    let result=select(&profiles,RoutingObjective::Economy,&RuleMatch::default(),false);
    assert_eq!(result.proposed.unwrap().provider_id,profiles[1].provider_id);assert_eq!(result.excluded_counts["measured_quality_floor"],1);
    assert!(result.reason_codes.iter().any(|s|s.starts_with("learned_evidence:")));
    let forced=RuleMatch{target:Some(profiles[0].provider_id),..Default::default()};
    assert_eq!(select(&profiles,RoutingObjective::Economy,&forced,false).proposed.unwrap().provider_id,profiles[0].provider_id);
    assert!(!select(&profiles,RoutingObjective::Economy,&forced,false).reason_codes.iter().any(|s|s.starts_with("learned_")));
    profiles[1].quality_evidence[0].passed=20;
    assert!(super::engine::select(&profiles,&req,&p,&RoutingConstraints::default(),RoutingObjective::Economy,&RuleMatch::default(),false).is_err());
}

#[test]
fn p12_evidence_requires_comparable_current_reviewed_live_cohorts() {
    let mut p=policy();p.learned_ranking=true;
    let req=WorkloadRequirements{task:TaskKind::General,..request()};
    for invalid in 0..10 {
        let mut profiles=vec![measured(profile(1,ModelTier::Economy,Some(0)),20,CallPurpose::Primary),
            measured(profile(2,ModelTier::Advanced,Some(1000)),40,CallPurpose::Primary)];
        let row=&mut profiles[1].quality_evidence[0];
        match invalid {0=>row.source=learning::EvidenceSource::Synthetic,1=>row.reviewed=false,2=>row.dataset_sha256="b".repeat(64),
            3=>row.families_sha256="b".repeat(64),4=>row.evaluator_version="other".into(),5=>row.purpose=CallPurpose::WebPlan,
            6=>row.workload.input_tokens=10000,7=>row.workload.effort=Some("low".into()),8=>row.families=19,_=>profiles[1].provider_revision+=1};
        let d=select(&profiles,&req,&p,&RoutingConstraints::default(),RoutingObjective::Economy,&RuleMatch::default(),false).unwrap();
        assert_eq!(d.proposed.unwrap().provider_id,profiles[0].provider_id,"variant {invalid}");
        assert!(d.reason_codes.contains(&"learned_insufficient_evidence".into()));
    }
}

#[test]
fn p12_per_stage_similarity_and_profile_invalidation() {
    let p=measured(profile(1,ModelTier::Balanced,Some(1)),40,CallPurpose::WebPlan);
    let mut req=WorkloadRequirements{task:TaskKind::General,..request()};
    assert!(learning::nearest(&p,&req,CallPurpose::Primary).is_none());
    assert!(learning::nearest(&p,&req,CallPurpose::WebPlan).is_some());
    assert!(learning::nearest(&p,&WorkloadRequirements{minimum_tier:ModelTier::Advanced,..req.clone()},CallPurpose::WebPlan).is_none());
    req.input_tokens*=2;assert!(learning::nearest(&p,&req,CallPurpose::WebPlan).is_some());
    req.input_tokens+=1;assert!(learning::nearest(&p,&req,CallPurpose::WebPlan).is_none());
    let mut changed=p.clone();changed.revision+=1;assert_eq!(learning::fingerprint(&p),learning::fingerprint(&changed));
    changed.capabilities.tools=EvidenceStatus::Unknown;assert!(learning::validate(&changed).is_err());
    assert!(learning::nearest(&changed,&request(),CallPurpose::WebPlan).is_none());
    assert!(learning::lower_bound(20,20)>8000);assert_eq!(learning::lower_bound(1,0),0);
}

#[test]
fn p12_aggregation_rejects_leakage_duplicates_and_stale_profile() {
    let p=profile(1,ModelTier::Balanced,Some(0));
    let value=serde_json::json!({"schema_version":1,"source":"synthetic","dataset_sha256":"a".repeat(64),
        "profile_sha256":learning::fingerprint(&p),"evaluator_version":"fixture-v1","purpose":"primary",
        "workload":WorkloadRequirements{task:TaskKind::General,..request()},
        "families":[{"id":"a","split":"test"},{"id":"b","split":"test"},{"id":"c","split":"test"}],
        "observations":[{"family_id":"a","outcome":"pass"},{"family_id":"b","outcome":"unknown"}]});
    let record=learning::aggregate(&p,serde_json::from_value(value.clone()).unwrap()).unwrap();
    assert_eq!(record.families,3);assert_eq!(record.passed,1);assert!(!record.reviewed);assert_eq!(record.source,learning::EvidenceSource::Synthetic);
    for invalid in 0..4 {
        let mut v=value.clone();match invalid {
            0=>v["families"][0]["split"]=serde_json::json!("train"),1=>v["observations"][1]["family_id"]=serde_json::json!("a"),
            2=>v["observations"][1]["family_id"]=serde_json::json!("not-in-manifest"),_=>v["profile_sha256"]=serde_json::json!("stale"),
        }
        if let Ok(batch)=serde_json::from_value(v) {assert!(learning::aggregate(&p,batch).is_err());}
    }
}

#[test]
fn p12_quality_contract_is_structural_and_bounded() {
    use quality::{QualityContract as Contract,QualityFailure as Failure};
    let contract=Contract::JsonObject{required_fields:vec!["answer".into()]};
    assert_eq!(contract.check(" "),Err(Failure::EmptyAnswer));assert_eq!(contract.check("[]"),Err(Failure::InvalidJson));
    assert_eq!(contract.check("{\"other\":1}"),Err(Failure::MissingFields));assert!(contract.check("{\"answer\":null}").is_ok());
    assert!(Contract::Nonempty.check("an answer").is_ok());
    assert!(Contract::JsonObject{required_fields:vec!["a".into(),"a".into()]}.validate().is_err());
    let mut p=policy();p.quality_escalation=true;assert!(validate_policy(&p).is_err());
    p.constraints=RoutingConstraints{max_calls:Some(2),max_output_tokens:Some(1024),max_total_tokens:Some(16000),max_duration_ms:Some(30000),..Default::default()};
    assert!(validate_policy(&p).is_ok());p.constraints.max_duration_ms=Some(120001);assert!(validate_policy(&p).is_err());
}

#[test]
fn r13_r17_objective_changes_preferences_within_the_eligible_pool() {
    let profiles = vec![profile(1, ModelTier::Economy, Some(1_000_000)), profile(2, ModelTier::Advanced, Some(10_000_000))];
    let constraints = RoutingConstraints::default();
    let cheap = select(&profiles, &request(), &policy(), &constraints, RoutingObjective::Economy, &RuleMatch::default(), false).unwrap();
    assert_eq!(cheap.proposed.unwrap().provider_id, profiles[0].provider_id);
    let quality = select(&profiles, &request(), &policy(), &constraints, RoutingObjective::Quality, &RuleMatch::default(), false).unwrap();
    assert_eq!(quality.proposed.unwrap().provider_id, profiles[1].provider_id);
    let complex = WorkloadRequirements { minimum_tier: ModelTier::Advanced, ..request() };
    assert_eq!(select(&profiles, &complex, &policy(), &constraints, RoutingObjective::Economy, &RuleMatch::default(), false).unwrap().proposed.unwrap().provider_id, profiles[1].provider_id);
}

#[test]
fn r11_r14_forced_rule_cannot_override_policy_or_capability() {
    let mut p = profile(1, ModelTier::Advanced, Some(1));
    let rule = RuleMatch { target: Some(p.provider_id), ..Default::default() };
    let deny = RoutingConstraints { allowed_providers: Some(Default::default()), ..Default::default() };
    assert!(select(&[p.clone()], &request(), &policy(), &deny, RoutingObjective::Quality, &rule, false).is_err());
    p.capabilities.tools = EvidenceStatus::Unknown;
    let req = WorkloadRequirements { tools: true, ..request() };
    assert!(select(&[p], &req, &policy(), &RoutingConstraints::default(), RoutingObjective::Quality, &rule, false).is_err());
}

#[test]
fn r13_priority_stable_id_and_requirements_only_tighten() {
    let mut p = policy();
    p.rules = vec![
        RoutingRule { id: uuid::Uuid::from_u128(2), priority: 10, conditions: Default::default(), action: RuleAction::Select { provider_id: uuid::Uuid::from_u128(2), effort: None } },
        RoutingRule { id: uuid::Uuid::from_u128(1), priority: 10, conditions: Default::default(), action: RuleAction::Require { minimum_tier: ModelTier::Economy } },
    ];
    let mut req = WorkloadRequirements { minimum_tier: ModelTier::Advanced, ..request() };
    let matched = apply_rules(&p, &mut req, CallPurpose::Primary).unwrap();
    assert_eq!(req.minimum_tier, ModelTier::Advanced);
    assert_eq!(matched.matched, vec![uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2)]);
    assert!(!matched.classify);
}

#[test]
fn r20_unknown_price_never_counts_as_free() {
    let p = profile(1, ModelTier::Advanced, None);
    let cap = RoutingConstraints { max_cost_microusd: Some(1_000_000), ..Default::default() };
    assert!(select(&[p.clone()], &request(), &policy(), &cap, RoutingObjective::Economy, &RuleMatch::default(), false).is_err());
    let decision = select(&[p], &request(), &policy(), &RoutingConstraints::default(), RoutingObjective::Economy, &RuleMatch::default(), false).unwrap();
    assert!(decision.estimated_cost_microusd.is_none());
}

#[test]
fn r13_r17_unknown_task_uses_checked_default_not_random_rank() {
    let profiles = vec![profile(1, ModelTier::Economy, Some(0)), profile(2, ModelTier::Advanced, Some(5))];
    let req = WorkloadRequirements { uncertain: true, ..request() };
    assert_eq!(select(&profiles, &req, &policy(), &RoutingConstraints::default(), RoutingObjective::Economy, &RuleMatch::default(), false).unwrap().proposed.unwrap().provider_id, profiles[1].provider_id);
    assert!(select(&profiles[..1], &req, &policy(), &RoutingConstraints::default(), RoutingObjective::Economy, &RuleMatch::default(), false).is_err());
}

#[test]
fn r13_rules_never_invokes_classifier_and_local_claim_needs_attestation() {
    let mut policy = policy();
    policy.rules.push(RoutingRule { id: uuid::Uuid::nil(), priority: 1, conditions: Default::default(), action: RuleAction::Classify });
    assert!(validate_policy(&policy).is_err());
    let mut profile = profile(1, ModelTier::Advanced, None);
    profile.locality_trusted = false;
    assert_eq!(incompatibility(&profile, &request(), &RoutingConstraints { local_only: true, ..Default::default() }), Some("policy"));
}

#[test]
fn nested_rules_match_all_any_and_measured_fields() {
    let mut p=policy();
    let conditions:RuleConditions=serde_json::from_value(serde_json::json!({"purposes":["primary"],
        "all":[{"tools":true},{"fresh_information":true}],
        "any":[{"languages":["en-GB"],"tasks":["current_fact"]},{"languages":["ru"],"structured_output":true}]})).unwrap();
    p.rules=vec![RoutingRule {id:uuid::Uuid::from_u128(1),priority:1,conditions,action:RuleAction::Select{provider_id:uuid::Uuid::from_u128(1),effort:None}}];
    let mut req=WorkloadRequirements{task:TaskKind::CurrentFact,language:Some("EN-gb".into()),tools:true,fresh_information:true,..request()};
    assert!(apply_rules(&p,&mut req,CallPurpose::Primary).unwrap().target.is_some());
    req.language=Some("ru".into());
    assert!(apply_rules(&p,&mut req,CallPurpose::Primary).unwrap().target.is_none());
    req.structured_output=true;
    assert!(apply_rules(&p,&mut req,CallPurpose::Primary).unwrap().target.is_some());
    req.tools=false;
    assert!(apply_rules(&p,&mut req,CallPurpose::Primary).unwrap().target.is_none());
    req.tools=true;
    assert!(apply_rules(&p,&mut req,CallPurpose::Final).unwrap().target.is_none());
}

#[test]
fn rule_pools_intersect_and_cannot_override_global_pool_or_forced_default() {
    let mut p=policy();
    p.rules=vec![RoutingRule{id:uuid::Uuid::from_u128(1),priority:3,conditions:Default::default(),
        action:RuleAction::RestrictPool{allowed_providers:[uuid::Uuid::from_u128(1),uuid::Uuid::from_u128(2)].into()}},
        RoutingRule{id:uuid::Uuid::from_u128(2),priority:2,conditions:Default::default(),
        action:RuleAction::RestrictPool{allowed_providers:[uuid::Uuid::from_u128(2),uuid::Uuid::from_u128(3)].into()}},
        RoutingRule{id:uuid::Uuid::from_u128(3),priority:1,conditions:Default::default(),
        action:RuleAction::Select{provider_id:uuid::Uuid::from_u128(1),effort:None}}];
    let profiles=vec![profile(1,ModelTier::Economy,Some(0)),profile(2,ModelTier::Advanced,Some(1))];
    let mut req=request();let mut rule=apply_rules(&p,&mut req,CallPurpose::Primary).unwrap();
    assert_eq!(rule.allowed_providers,Some([uuid::Uuid::from_u128(2)].into()));
    assert_eq!(rule.matched.len(),3);
    assert!(select(&profiles,&req,&p,&Default::default(),RoutingObjective::Economy,&rule,false).is_err());
    rule.target=None;
    assert_eq!(select(&profiles,&req,&p,&Default::default(),RoutingObjective::Economy,&rule,false).unwrap().proposed.unwrap().provider_id,uuid::Uuid::from_u128(2));
    p.default_provider_id=Some(uuid::Uuid::from_u128(1));
    assert!(select(&profiles,&req,&p,&Default::default(),RoutingObjective::Economy,&rule,true).is_err());
    let global=RoutingConstraints{allowed_providers:Some([uuid::Uuid::from_u128(1)].into()),..Default::default()};
    assert!(select(&profiles,&req,&p,&global,RoutingObjective::Quality,&rule,false).is_err());
}

#[test]
fn rule_dsl_and_classifier_config_are_strict_and_bounded() {
    let mut p=policy();
    let rule=RoutingRule{id:uuid::Uuid::nil(),priority:1,conditions:Default::default(),action:RuleAction::Require{minimum_tier:ModelTier::Advanced}};
    p.rules=vec![rule.clone()];
    for invalid in [serde_json::json!({"not":{}}),serde_json::json!({"all":[],"model_name":"premium"}),serde_json::json!({"tasks":["invented"]})] {
        assert!(serde_json::from_value::<RuleConditions>(invalid).is_err());
    }
    for invalid in [serde_json::json!({"kind":"restrict_pool","allowed_providers":[],"url":"https://bad.test"}),serde_json::json!({"kind":"execute"})] {
        assert!(serde_json::from_value::<RuleAction>(invalid).is_err());
    }
    for invalid in [serde_json::json!({"languages":[""]}),serde_json::json!({"languages":["en--GB"]}),
        serde_json::json!({"languages":["ru; exec"]}),serde_json::json!({"min_input_tokens":5,"max_input_tokens":4}),
        serde_json::json!({"all":[{"any":[{"all":[{"all":[{}]}]}]}]})] {
        p.rules[0].conditions=serde_json::from_value(invalid).unwrap();assert!(validate_policy(&p).is_err());
    }
    p.rules[0]=rule.clone();p.rules[0].conditions.any=vec![Default::default();17];assert!(validate_policy(&p).is_err());
    p.rules=(0..40).map(|i|{let mut r=rule.clone();r.id=uuid::Uuid::from_u128(i);r.conditions.any=vec![Default::default();16];r}).collect();
    assert!(validate_policy(&p).is_err());
    p.rules.clear();p.classifier.max_input_tokens=1;assert!(validate_policy(&p).is_err());
    p.classifier=Default::default();p.classifier.cloud_timeout_ms=49;assert!(validate_policy(&p).is_err());
    p.classifier=Default::default();p.classifier.max_output_tokens=2049;assert!(validate_policy(&p).is_err());
    p.classifier=Default::default();assert!(validate_policy(&p).is_ok());
}


#[test]
fn model_effort_presets_are_candidate_specific_and_explicit_effort_wins() {
    let mut local=profile(1,ModelTier::Economy,Some(0));local.capabilities.efforts=["none".into()].into();local.auto_effort=Some("none".into());
    let mut cloud=profile(2,ModelTier::Advanced,Some(100));cloud.auto_effort=Some("high".into());
    let profiles=[local.clone(),cloud.clone()];let req=request();let constraints=RoutingConstraints::default();let p=policy();
    let decision=select(&profiles,&req,&p,&constraints,RoutingObjective::Economy,&RuleMatch::default(),false).unwrap();
    assert_eq!(decision.proposed.unwrap().effort.as_deref(),Some("none"));assert!(decision.reason_codes.contains(&"model_effort_preset".into()));
    let decision=select(&profiles,&req,&p,&constraints,RoutingObjective::Quality,&RuleMatch::default(),false).unwrap();
    assert_eq!(decision.proposed.unwrap().effort.as_deref(),Some("high"));
    // A retry uses the remaining candidate's preset, without carrying local none.
    assert_eq!(select(&[cloud.clone()],&req,&p,&constraints,RoutingObjective::Economy,&RuleMatch::default(),false).unwrap().proposed.unwrap().effort.as_deref(),Some("high"));
    let explicit=WorkloadRequirements{effort:Some("low".into()),..req.clone()};
    let decision=select(&profiles,&explicit,&p,&constraints,RoutingObjective::Economy,&RuleMatch::default(),false).unwrap();
    assert_eq!(decision.proposed.unwrap().effort.as_deref(),Some("low"));assert_eq!(decision.excluded_counts["effort"],1);
    let rule=RuleMatch{target:Some(cloud.provider_id),effort:Some("low".into()),..Default::default()};
    assert_eq!(select(&profiles,&req,&p,&constraints,RoutingObjective::Quality,&rule,false).unwrap().proposed.unwrap().effort.as_deref(),Some("low"));
    assert!(select(&profiles,&WorkloadRequirements{effort:Some("high".into()),..req.clone()},&p,&constraints,RoutingObjective::Quality,&rule,false).is_err());
    assert_eq!(checked_default(&profiles,&req,&p,&constraints).unwrap().effort.as_deref(),Some("high"));
    // Explicit targets use unchanged requirements, not Auto presets.
    assert_eq!(req.effort,None);assert_eq!(target_incompatibility(&local,&req,&constraints),None);
    local.auto_effort=Some("high".into());assert!(validate_auto_effort(&local).is_err());
    assert_eq!(incompatibility(&local,&req,&constraints),Some("effort"));
    for value in ["", " none", "none ", "0123456789012345678901234"] {
        cloud.capabilities.efforts.insert(value.into());cloud.auto_effort=Some(value.into());assert!(validate_auto_effort(&cloud).is_err());
    }
}

#[test]
fn effort_presets_invalidate_evidence_without_changing_legacy_fingerprints() {
    use sha2::{Digest,Sha256};
    let mut p=measured(profile(1,ModelTier::Balanced,Some(0)),40,CallPurpose::Primary);
    let caps=&p.capabilities;
    let old=serde_json::json!(["fosnie-routing-profile-v1",p.provider_id,p.provider_revision,p.adapter,
        p.model,[caps.tools,caps.vision,caps.structured_output,caps.context_tokens,caps.max_output_tokens,caps.efforts],
        p.tier,p.task_tiers.iter().collect::<Vec<_>>()]);
    let old_hash=hex::encode(Sha256::digest(old.to_string().as_bytes()));assert_eq!(learning::fingerprint(&p),old_hash);
    p.auto_effort=Some("high".into());assert_ne!(learning::fingerprint(&p),old_hash);assert!(learning::validate(&p).is_err());
    let req=WorkloadRequirements{task:TaskKind::General,..request()};
    assert!(learning::nearest(&p,&auto_requirements(&p,&req),CallPurpose::Primary).is_none());
    p.auto_effort=None;assert_eq!(learning::fingerprint(&p),old_hash);learning::validate(&p).unwrap();
}

#[test]
fn task_exclusions_cover_ranked_forced_default_and_manual_selection() {
    let mut local=profile(1,ModelTier::Economy,Some(0));
    local.task_exclusions.insert(TaskKind::Reasoning);
    let profiles=vec![local.clone(),profile(2,ModelTier::Balanced,Some(1000))];
    let req=WorkloadRequirements{task:TaskKind::Reasoning,..request()};
    let constraints=RoutingConstraints::default();let p=policy();
    let result=select(&profiles,&req,&p,&constraints,RoutingObjective::Economy,&RuleMatch::default(),false).unwrap();
    assert_eq!(result.proposed.unwrap().provider_id,profiles[1].provider_id);
    assert_eq!(result.excluded_counts["task_excluded"],1);
    let forced=RuleMatch{target:Some(local.provider_id),..Default::default()};
    assert!(select(&profiles,&req,&p,&constraints,RoutingObjective::Quality,&forced,false).is_err());
    let mut p=p;p.default_provider_id=Some(local.provider_id);
    assert!(select(&profiles,&req,&p,&constraints,RoutingObjective::Balanced,&RuleMatch::default(),true).is_err());
    // Manual/fixed capability checks do not become Auto quality declarations.
    assert_eq!(target_incompatibility(&local,&req,&constraints),None);
    assert_eq!(incompatibility(&local,&WorkloadRequirements{task:TaskKind::General,..req.clone()},&constraints),None);
    assert!(select(&[local.clone()],&req,&p,&RoutingConstraints{local_only:true,..Default::default()},RoutingObjective::Economy,&RuleMatch::default(),false).is_err());
    local.task_exclusions.insert(TaskKind::Unknown);
    assert_eq!(incompatibility(&local,&request(),&constraints),Some("task_excluded"));
}

#[test]
fn task_exclusions_invalidate_evidence_and_preserve_empty_legacy_profiles() {
    let mut p=measured(profile(1,ModelTier::Balanced,Some(0)),40,CallPurpose::Primary);
    let old=learning::fingerprint(&p);
    let value=serde_json::to_value(&p).unwrap();assert!(value.get("task_exclusions").is_none());
    let restored:ModelProfile=serde_json::from_value(value).unwrap();
    assert!(restored.task_exclusions.is_empty());assert_eq!(learning::fingerprint(&restored),old);
    p.task_exclusions.insert(TaskKind::Reasoning);
    assert_ne!(learning::fingerprint(&p),old);assert!(learning::validate(&p).is_err());
    p.task_exclusions.clear();assert_eq!(learning::fingerprint(&p),old);learning::validate(&p).unwrap();
}

fn measured_preset(mut p:ModelProfile,effort:&str,passed:u32)->ModelProfile {
    p.capabilities.efforts.insert(effort.into());p.auto_effort=Some(effort.into());
    let mut p=measured(p,passed,CallPurpose::Primary);
    p.quality_evidence[0].workload.effort=Some(effort.into());
    learning::validate(&p).unwrap();p
}

#[test]
fn learned_floor_compares_model_specific_effort_presets() {
    // Fixture "live" enum exercises the admission contract, not real quality.
    let mut policy=policy();policy.learned_ranking=true;
    let req=WorkloadRequirements{task:TaskKind::General,..request()};
    let mut profiles=vec![measured_preset(profile(1,ModelTier::Economy,Some(0)),"none",20),
        measured_preset(profile(2,ModelTier::Advanced,Some(1000)),"high",40)];
    let choose=|profiles:&[ModelProfile],req:&WorkloadRequirements|select(profiles,req,&policy,&RoutingConstraints::default(),RoutingObjective::Economy,&RuleMatch::default(),false).unwrap();
    let result=choose(&profiles,&req);
    assert_eq!(result.proposed.as_ref().unwrap().provider_id,profiles[1].provider_id);
    assert_eq!(result.proposed.as_ref().unwrap().effort.as_deref(),Some("high"));
    assert_eq!(result.excluded_counts["measured_quality_floor"],1);
    assert!(result.reason_codes.iter().any(|r|r.starts_with("learned_evidence:")));
    profiles[0].quality_evidence[0].passed=37;
    assert_eq!(choose(&profiles,&req).proposed.unwrap().provider_id,profiles[0].provider_id);
    // Explicit effort cannot use the local candidate's none observations.
    let explicit=WorkloadRequirements{effort:Some("high".into()),..req.clone()};
    assert!(learning::nearest(&profiles[0],&auto_requirements(&profiles[0],&explicit),CallPurpose::Primary).is_none());
    let explicit_result=choose(&profiles,&explicit);
    assert!(explicit_result.reason_codes.contains(&"learned_insufficient_evidence".into()));
    assert_eq!(explicit_result.proposed.unwrap().effort.as_deref(),Some("high"));
}

#[test]
fn mixed_presets_do_not_relax_other_workload_or_cohort_bindings() {
    let mut policy=policy();policy.learned_ranking=true;
    let req=WorkloadRequirements{task:TaskKind::General,..request()};
    for variant in 0..6 {
        let mut profiles=vec![measured_preset(profile(1,ModelTier::Economy,Some(0)),"none",20),
            measured_preset(profile(2,ModelTier::Advanced,Some(1000)),"high",40)];
        let row=&mut profiles[1].quality_evidence[0];
        match variant {
            0=>row.workload.input_tokens=200,1=>row.workload.output_tokens=1000,
            2=>row.dataset_sha256="b".repeat(64),3=>row.families_sha256="b".repeat(64),
            4=>row.evaluator_version="other".into(),_=>row.workload.effort=Some("low".into()),
        }
        let result=select(&profiles,&req,&policy,&RoutingConstraints::default(),RoutingObjective::Economy,&RuleMatch::default(),false).unwrap();
        assert!(result.reason_codes.contains(&"learned_insufficient_evidence".into()),"variant {variant}");
    }
}


#[test]
fn json_output_effort_restrictions_cover_auto_rules_defaults_and_explicit_targets() {
    let mut local=profile(1,ModelTier::Economy,Some(0));
    local.capabilities.structured_output=EvidenceStatus::Declared;
    local.capabilities.efforts.insert("none".into());local.auto_effort=Some("none".into());
    local.capabilities.structured_output_efforts=Some(["high".into()].into());
    let mut cloud=profile(2,ModelTier::Economy,Some(1_000_000));
    cloud.capabilities.structured_output=EvidenceStatus::Declared;cloud.auto_effort=Some("high".into());
    let mut req=WorkloadRequirements{structured_output:true,..request()};
    let constraints=RoutingConstraints::default();let mut policy=policy();policy.default_provider_id=Some(local.provider_id);
    let models=vec![local.clone(),cloud.clone()];
    let chosen=select(&models,&req,&policy,&constraints,RoutingObjective::Economy,&RuleMatch::default(),false).unwrap();
    assert_eq!(chosen.proposed.unwrap().provider_id,cloud.provider_id);
    assert_eq!(chosen.excluded_counts.get("structured_output_effort"),Some(&1));
    let forced=RuleMatch{target:Some(local.provider_id),..Default::default()};
    assert!(select(&models,&req,&policy,&constraints,RoutingObjective::Economy,&forced,false).is_err());
    assert!(checked_default(&models,&req,&policy,&constraints).is_err());
    assert_eq!(target_incompatibility(&local,&req,&constraints),Some("structured_output_effort"));
    req.effort=Some("none".into());
    assert_eq!(target_incompatibility(&local,&req,&constraints),Some("structured_output_effort"));
    req.effort=Some("high".into());
    assert_eq!(target_incompatibility(&local,&req,&constraints),None);
    assert_eq!(select(&models,&req,&policy,&constraints,RoutingObjective::Economy,&RuleMatch::default(),false).unwrap().proposed.unwrap().provider_id,local.provider_id);
    req.effort=None;req.structured_output=false;
    assert_eq!(select(&models,&req,&policy,&constraints,RoutingObjective::Economy,&RuleMatch::default(),false).unwrap().proposed.unwrap().provider_id,local.provider_id);
    local.capabilities.structured_output_efforts=Some(Default::default());
    assert_eq!(structured_output_incompatibility(&local,Some("high")),Some("structured_output_effort"));
    assert_eq!(structured_output_incompatibility(&local,None),Some("structured_output_effort"));
    local.capabilities.structured_output_efforts=None;
    assert_eq!(structured_output_incompatibility(&local,None),None);
}

#[test]
fn json_output_effort_restrictions_preserve_legacy_fingerprints_and_invalidate_evidence() {
    let mut p=measured(profile(1,ModelTier::Economy,Some(0)),40,CallPurpose::Primary);
    let previous=learning::fingerprint(&p);let value=serde_json::to_value(&p).unwrap();
    assert!(value["capabilities"].get("structured_output_efforts").is_none());
    let restored:ModelProfile=serde_json::from_value(value).unwrap();
    assert_eq!(learning::fingerprint(&restored),previous);validate_auto_effort(&restored).unwrap();
    p.capabilities.structured_output_efforts=Some(["high".into()].into());
    validate_auto_effort(&p).unwrap();assert_ne!(learning::fingerprint(&p),previous);assert!(learning::validate(&p).is_err());
    p.capabilities.structured_output_efforts=Some(Default::default());
    validate_auto_effort(&p).unwrap();assert_ne!(learning::fingerprint(&p),previous);
    for bad in ["unsupported"," high","", "abcdefghijklmnopqrstuvwxy"] {
        p.capabilities.structured_output_efforts=Some([bad.into()].into());assert!(validate_auto_effort(&p).is_err());
    }
    p.capabilities.structured_output_efforts=None;assert_eq!(learning::fingerprint(&p),previous);learning::validate(&p).unwrap();
}


#[test]
fn skipped_judge_cannot_bypass_unknown_default_task_exclusions() {
    let mut p=profile(2,ModelTier::Balanced,Some(1));let policy=policy();let mut req=request();
    assert!(classifier_free_default(&[p.clone()],&req,&policy,&RoutingConstraints::default()).is_ok());
    p.task_exclusions.insert(TaskKind::Reasoning);
    assert!(classifier_free_default(&[p.clone()],&req,&policy,&RoutingConstraints::default()).is_err());
    req.task=TaskKind::General;
    assert!(classifier_free_default(&[p.clone()],&req,&policy,&RoutingConstraints::default()).is_ok());
    req.task=TaskKind::Reasoning;
    assert!(classifier_free_default(&[p],&req,&policy,&RoutingConstraints::default()).is_err());
}
