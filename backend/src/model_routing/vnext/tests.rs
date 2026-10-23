// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::super::{
    AdapterKind, CallPurpose, ModelCapabilities, ModelPricing, WorkloadRequirements,
};
use super::{catalogue::*, pool::PoolSnapshot, profile::*};
use std::collections::BTreeSet;
use uuid::Uuid;

fn hash(n: u64) -> String {
    format!("{n:064x}")
}
fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn descriptor() -> BehaviourDescriptor {
    BehaviourDescriptor {
        adapter: AdapterKind::OpenaiResponses,
        model_id: "fixture-model".into(),
        model_version: Some("immutable-fixture-1".into()),
        local_digest: None,
        quantisation: None,
        template_sha256: hash(1),
        runtime_config_sha256: hash(2),
        effort: EffortSetting::Explicit {
            value: "high".into(),
        },
        generation: GenerationConfig {
            max_output_tokens: Some(1024),
            ..Default::default()
        },
    }
}
fn candidate(n: usize) -> CandidateRef {
    let provider = n % 33;
    let mut behaviour = descriptor();
    behaviour.generation.max_output_tokens = Some(1024 + n as u32);
    CandidateRef {
        provider_id: id(1 + provider as u128),
        provider_revision: 1,
        profile_revision: 2,
        model_config_revision: 1,
        variant: VariantRef {
            id: id(1000 + n as u128),
            revision: 3,
        },
        effort: behaviour.effort.clone(),
        behaviour_sha256: behaviour.fingerprint().unwrap(),
        connection: Some(ConnectionRef {
            id: id(100 + provider as u128 % 10),
            auth_revision: 1,
            quota_group_id: Some(id(200)),
            scope: CatalogueScope::Deployment,
        }),
    }
}
fn pool(count: usize) -> PoolSnapshot {
    PoolSnapshot::freeze(
        id(900),
        4,
        5,
        hash(3),
        hash(4),
        (0..count).map(candidate).collect(),
        256,
    )
    .unwrap()
}
fn profile() -> RoutingProfileConfig {
    RoutingProfileConfig {
        language_scope: None,
        workload_scope: None,
        recovery: None,
        schema_version: 2,
        membership: PoolMembership::AllAvailable,
        max_active_variants: 256,
        quality: QualityTarget {
            assurance: Assurance::Declared,
            max_relative_loss_bp: 200,
            minimum_absolute_quality_bp: None,
        },
        reference: ReferencePolicy::Configured {
            variant: candidate(0).variant,
            allow_uncalibrated: true,
        },
        fallback_variant_ids: BTreeSet::new(),
        groups: vec![],
        selector_sha256: None,
        verifier_sha256: None,
    }
}
fn group(n: u128) -> TaskGroup {
    TaskGroup {
        id: id(n),
        label: "Group".into(),
        priority: 10,
        conditions: Default::default(),
        difficulty: None,
        allowed_variants: None,
        excluded_variants: BTreeSet::new(),
        assignment: None,
    }
}

#[test]
fn configured_roles_and_user_assignments_route_in_both_directions() {
    use super::super::{ModelProfile,ModelTier,RoutingObjective,TaskKind};
    let variants:Vec<_>=(0..2).map(|n|{let behaviour=descriptor();ModelVariant {
        id:id(300+n),revision:1,scope:CatalogueScope::Deployment,provider_id:id(400+n),
        model_config_revision:1,label:format!("fixture-{n}"),behaviour_sha256:behaviour.fingerprint().unwrap(),behaviour,
    }}).collect();
    let models:Vec<ModelProfile>=(0..2).map(|n|serde_json::from_value(serde_json::json!({
        "provider_id":id(400+n),"provider_revision":1,"revision":1,"model":"fixture-model","adapter":"openai_responses",
        "tier":if n==0{"economy"}else{"advanced"},"quality_status":"declared","auto_enabled":true,
        "pricing":{"input_microusd_per_million":100+n*1000,"output_microusd_per_million":100+n*1000}
    })).unwrap()).collect();
    let candidates=variants.iter().zip(models.iter()).map(|(v,p)|(v.id,(v,p))).collect();
    let mut config=profile();
    config.reference=ReferencePolicy::Configured{variant:VariantRef{id:variants[1].id,revision:1},allow_uncalibrated:true};
    let mut req=WorkloadRequirements{task:TaskKind::CurrentFact,..Default::default()};
    let resolve=|config:&RoutingProfileConfig,req:&WorkloadRequirements,level|config.resolve_groups(
        &variants.iter().map(|v|v.id).collect(),req,CallPurpose::Primary,
        DifficultySignal::Known{level,confidence_bp:9500}).unwrap();
    let ordinary=resolve(&config,&req,Difficulty::Routine);
    assert_eq!(super::selection::choose(&config,&ordinary,&candidates,&req,RoutingObjective::Balanced,None).unwrap().0.id,variants[0].id);
    req.task=TaskKind::Research;req.minimum_tier=ModelTier::Advanced;
    let complex=resolve(&config,&req,Difficulty::Complex);
    assert_eq!(super::selection::choose(&config,&complex,&candidates,&req,RoutingObjective::Balanced,None).unwrap().0.id,variants[1].id);
    // Complexity describes the request. It must not veto the user's exact choice.
    let mut custom=group(600);custom.difficulty=Some(DifficultyCondition{levels:[Difficulty::Complex].into(),min_confidence_bp:7500,include_unknown:false});
    custom.assignment=Some(GroupAssignment::Pin{variant:VariantRef{id:variants[0].id,revision:1}});config.groups.push(custom);
    let complex=resolve(&config,&req,Difficulty::Complex);
    assert_eq!(super::selection::choose(&config,&complex,&candidates,&req,RoutingObjective::Quality,None).unwrap().0.id,variants[0].id);
    config.groups.clear();req.task=TaskKind::General;req.minimum_tier=ModelTier::Economy;
    let mut premium=group(601);premium.assignment=Some(GroupAssignment::Prefer{variant_id:variants[1].id});config.groups.push(premium);
    let routine=resolve(&config,&req,Difficulty::Routine);
    assert_eq!(super::selection::choose(&config,&routine,&candidates,&req,RoutingObjective::Economy,None).unwrap().0.id,variants[1].id);
    config.groups.clear();req.uncertain=true;
    let unknown=config.resolve_groups(&variants.iter().map(|v|v.id).collect(),&req,CallPurpose::Primary,DifficultySignal::Unknown).unwrap();
    let choice=super::selection::choose(&config,&unknown,&candidates,&req,RoutingObjective::Economy,None).unwrap();
    assert_eq!(choice.0.id,variants[1].id);assert_eq!(choice.1,"uncertain_task_default");
}
fn resolve(
    p: &RoutingProfileConfig,
    difficulty: DifficultySignal,
) -> Result<GroupResolution, super::super::RoutingError> {
    p.resolve_groups(
        &(0..4).map(|n| candidate(n).variant.id).collect(),
        &WorkloadRequirements::default(),
        CallPurpose::Primary,
        difficulty,
    )
}

#[test]
fn variant_behavior_reuses_credentials_but_never_effort_or_config_changes() {
    let behaviour = descriptor();
    let mut deployment = ModelDeployment {
        provider_id: id(1),
        scope: CatalogueScope::Deployment,
        provider_revision: 1,
        model_config_revision: 7,
        connection: candidate(0).connection,
        behaviour: descriptor(),
        capabilities: ModelCapabilities {
            efforts: ["low".into(), "high".into()].into(),
            max_output_tokens: Some(4096),
            ..Default::default()
        },
        pricing: ModelPricing::default(),
    };
    let variant = ModelVariant {
        id: id(10),
        revision: 2,
        scope: CatalogueScope::Deployment,
        provider_id: id(1),
        model_config_revision: 7,
        label: "High".into(),
        behaviour_sha256: behaviour.fingerprint().unwrap(),
        behaviour,
    };
    variant.validate(&deployment).unwrap();
    deployment.connection.as_mut().unwrap().auth_revision += 1;
    deployment.provider_revision += 1;
    variant.validate(&deployment).unwrap();
    let mut changed = variant.clone();
    changed.behaviour.effort = EffortSetting::Explicit {
        value: "low".into(),
    };
    assert_ne!(
        changed.behaviour.fingerprint().unwrap(),
        variant.behaviour_sha256
    );
    assert!(changed.validate(&deployment).is_err());
    changed.behaviour.effort = EffortSetting::Explicit {
        value: "xhigh".into(),
    };
    changed.behaviour_sha256 = changed.behaviour.fingerprint().unwrap();
    assert!(changed.validate(&deployment).is_err());
    changed = variant.clone();
    changed.behaviour.model_id = "different-model".into();
    changed.behaviour_sha256 = changed.behaviour.fingerprint().unwrap();
    assert!(changed.validate(&deployment).is_err());
    deployment.model_config_revision += 1;
    assert!(variant.validate(&deployment).is_err());
    let wire = serde_json::to_string(&variant).unwrap();
    assert!(!wire.contains("api_key") && !wire.contains("auth_revision"));
}

#[test]
fn personal_variants_use_shared_models_without_sharing_private_credentials_or_owners() {
    let mut deployment = ModelDeployment {
        provider_id: id(1),
        scope: CatalogueScope::Deployment,
        provider_revision: 1,
        model_config_revision: 1,
        connection: candidate(0).connection,
        behaviour: descriptor(),
        capabilities: ModelCapabilities {
            efforts: ["high".into()].into(),
            max_output_tokens: Some(4096),
            ..Default::default()
        },
        pricing: ModelPricing::default(),
    };
    let mut variant = ModelVariant {
        id: id(90),
        revision: 1,
        scope: CatalogueScope::User { owner_id: id(80) },
        provider_id: id(1),
        model_config_revision: 1,
        label: "Personal high".into(),
        behaviour_sha256: descriptor().fingerprint().unwrap(),
        behaviour: descriptor(),
    };
    variant.validate(&deployment).unwrap();
    deployment.scope = CatalogueScope::User { owner_id: id(81) };
    deployment.connection.as_mut().unwrap().scope = deployment.scope.clone();
    assert!(variant.validate(&deployment).is_err());
    variant.scope = deployment.scope.clone();
    variant.validate(&deployment).unwrap();
    variant.scope = CatalogueScope::Deployment;
    assert!(variant.validate(&deployment).is_err());
    deployment.connection.as_mut().unwrap().scope = CatalogueScope::Deployment;
    assert!(deployment.validate().is_err());
}

#[test]
fn fallback_reference_and_primary_pools_share_hard_restrictions_and_capacity() {
    let mut p = profile();
    p.membership = PoolMembership::Selected {
        variant_ids: [candidate(1).variant.id].into(),
    };
    p.fallback_variant_ids = [candidate(2).variant.id, id(9999)].into();
    let resolution = resolve(&p, DifficultySignal::Unknown).unwrap();
    assert_eq!(
        resolution.allowed_variants,
        [candidate(1).variant.id].into()
    );
    assert_eq!(
        resolution.fallback_variants,
        [candidate(2).variant.id].into()
    );
    assert_eq!(
        resolution.reference_variants,
        [candidate(0).variant.id].into()
    );
    p.max_active_variants = 2;
    assert!(resolve(&p, DifficultySignal::Unknown).is_err());
    p.max_active_variants = 256;
    let mut restriction = group(1);
    restriction.excluded_variants = [candidate(0).variant.id, candidate(2).variant.id].into();
    p.groups.push(restriction);
    let resolution = resolve(&p, DifficultySignal::Unknown).unwrap();
    assert!(resolution.fallback_variants.is_empty() && resolution.reference_variants.is_empty());
    p.quality.assurance = Assurance::Calibrated;
    let mut legacy: super::super::RoutingPolicy = serde_json::from_value(serde_json::json!({"id":id(90),"revision":1,"default_provider_id":null,"classifier_provider_id":null})).unwrap();
    p.validate_policy_extension(&legacy).unwrap();
    p.recovery=Some(super::recovery::Config{technical:true,source_escalation:false});
    assert!(p.validate_policy_extension(&legacy).is_err());
    legacy.technical_fallback=true;
    p.validate_policy_extension(&legacy).unwrap();
    p.recovery=None;
    legacy.rules.push(super::super::RoutingRule {
        id: id(91),
        priority: 1,
        conditions: Default::default(),
        action: super::super::RuleAction::Select {
            provider_id: id(1),
            effort: None,
        },
    });
    p.validate_policy_extension(&legacy).unwrap();
}

#[test]
fn snapshots_support_large_pools_without_silent_overflow_or_aba() {
    let snapshot = pool(100);
    snapshot.validate(256).unwrap();
    assert_eq!(
        snapshot
            .candidates
            .iter()
            .map(|c| c.provider_id)
            .collect::<BTreeSet<_>>()
            .len(),
        33
    );
    assert_eq!(
        snapshot
            .candidates
            .iter()
            .map(|c| c.connection.as_ref().unwrap().id)
            .collect::<BTreeSet<_>>()
            .len(),
        10
    );
    let mut reversed = snapshot.candidates.clone();
    reversed.reverse();
    assert_eq!(
        PoolSnapshot::freeze(id(900), 4, 5, hash(3), hash(4), reversed, 256)
            .unwrap()
            .hash,
        snapshot.hash
    );
    assert!(PoolSnapshot::freeze(
        id(900),
        4,
        5,
        hash(3),
        hash(4),
        (0..257).map(candidate).collect(),
        256
    )
    .is_err());
    let mut changed = snapshot.clone();
    changed.candidates[0]
        .connection
        .as_mut()
        .unwrap()
        .auth_revision += 1;
    assert!(changed.validate(256).is_err());
    changed = snapshot.clone();
    changed.authorisation_scope_sha256 = hash(99);
    assert!(changed.validate(256).is_err());
    changed = snapshot.clone();
    changed.policy_revision += 1;
    assert!(changed.validate(256).is_err());
    let mut duplicate = candidate(0);
    duplicate.variant.id = id(9999);
    assert!(PoolSnapshot::freeze(
        id(900),
        4,
        5,
        hash(3),
        hash(4),
        vec![candidate(0), duplicate],
        256
    )
    .is_err());
}

#[test]
fn groups_intersect_all_restrictions_and_explain_pin_precedence() {
    let mut p = profile();
    let mut first = group(1);
    let mut second = group(2);
    first.allowed_variants = Some([candidate(0).variant.id, candidate(1).variant.id].into());
    first.assignment = Some(GroupAssignment::Prefer {
        variant_id: candidate(0).variant.id,
    });
    second.excluded_variants.insert(candidate(0).variant.id);
    second.priority = 1;
    p.groups = vec![second, first];
    let resolution = resolve(&p, DifficultySignal::Unknown).unwrap();
    assert_eq!(
        resolution.allowed_variants,
        [candidate(1).variant.id].into()
    );
    assert!(resolution.preferences.is_empty());
    assert_eq!(resolution.matched_groups, vec![id(1), id(2)]);
    p.groups[1].assignment = Some(GroupAssignment::Pin {
        variant: candidate(0).variant,
    });
    assert!(
        resolve(&p, DifficultySignal::Unknown).is_err(),
        "pin cannot bypass an exclusion"
    );
    p.groups[0].excluded_variants.clear();
    p.groups[0].assignment = Some(GroupAssignment::Pin {
        variant: candidate(1).variant,
    });
    assert_eq!(
        resolve(&p, DifficultySignal::Unknown).unwrap().pin,
        Some(candidate(0).variant)
    );
    p.groups[0].priority = 10;
    assert!(resolve(&p, DifficultySignal::Unknown).is_err());
    p.quality.assurance = Assurance::Calibrated;
    assert!(p.validate().is_err());
}

#[test]
fn unknown_difficulty_never_routes_as_trivial_and_preferences_do_not_widen_selected_pool() {
    let mut p = profile();
    let mut trivial = group(1);
    trivial.difficulty = Some(DifficultyCondition {
        levels: [Difficulty::Trivial].into(),
        min_confidence_bp: 9000,
        include_unknown: false,
    });
    trivial.assignment = Some(GroupAssignment::Pin {
        variant: candidate(0).variant,
    });
    p.groups.push(trivial);
    assert!(resolve(&p, DifficultySignal::Unknown)
        .unwrap()
        .pin
        .is_none());
    assert!(resolve(
        &p,
        DifficultySignal::Known {
            level: Difficulty::Trivial,
            confidence_bp: 8000
        }
    )
    .unwrap()
    .pin
    .is_none());
    assert_eq!(
        resolve(
            &p,
            DifficultySignal::Known {
                level: Difficulty::Trivial,
                confidence_bp: 9500
            }
        )
        .unwrap()
        .pin,
        Some(candidate(0).variant)
    );
    assert!(resolve(
        &p,
        DifficultySignal::Known {
            level: Difficulty::Complex,
            confidence_bp: 10_001
        }
    )
    .is_err());
    p.membership = PoolMembership::Selected {
        variant_ids: [candidate(1).variant.id].into(),
    };
    assert!(resolve(
        &p,
        DifficultySignal::Known {
            level: Difficulty::Trivial,
            confidence_bp: 9500
        }
    )
    .is_err());
    p.groups[0].assignment = Some(GroupAssignment::Prefer {
        variant_id: candidate(0).variant.id,
    });
    let resolved = resolve(
        &p,
        DifficultySignal::Known {
            level: Difficulty::Trivial,
            confidence_bp: 9500,
        },
    )
    .unwrap();
    assert_eq!(resolved.allowed_variants, [candidate(1).variant.id].into());
    assert!(resolved.preferences.is_empty());
}

#[test]
fn hosted_gemma_variants_preserve_exact_levels_and_reject_numeric_budgets() {
    for model in ["gemma-4-26b-a4b-it","gemma-4-31b-it","models/gemma-4-31b-it"] {
        let mut b=descriptor();b.adapter=AdapterKind::Gemini;b.model_id=model.into();
        for effort in [None,Some("minimal"),Some("high")] {
            b.effort=effort.map(|s|EffortSetting::Explicit{value:s.into()}).unwrap_or(EffortSetting::ProviderDefault);
            super::variants::validate_adapter_generation(&b).unwrap();
        }
        for effort in ["none","low","medium","max"] {
            b.effort=EffortSetting::Explicit{value:effort.into()};
            assert!(super::variants::validate_adapter_generation(&b).is_err());
        }
        b.effort=EffortSetting::ProviderDefault;b.generation.max_reasoning_tokens=Some(1024);
        assert!(super::variants::validate_adapter_generation(&b).is_err());
    }
    let mut b=descriptor();b.adapter=AdapterKind::Gemini;b.effort=EffortSetting::ProviderDefault;
    b.generation.max_reasoning_tokens=Some(1024);
    for model in ["gemini-3.1-flash-lite","models/gemini-3.6-flash","gemini-4-argon"] {
        b.model_id=model.into();assert!(super::variants::validate_adapter_generation(&b).is_err());
    }
    b.model_id="gemini-2.5-flash".into();super::variants::validate_adapter_generation(&b).unwrap();
}
