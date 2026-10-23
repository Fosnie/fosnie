// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::*;
use fosnie_backend::model_routing::vnext::{execution, profile::*, profiles};
#[path="model_routing_vnext_health_cases.rs"]
mod health_cases;
use fosnie_protocol::routing_vnext::{RoutingIntentV2, VariantSelection};

pub fn exercise_profiles<'a>(
    state: &'a AppState,
    admin: &'a AuthContext,
    user: &'a AuthContext,
    other: &'a AuthContext,
) -> std::pin::Pin<Box<dyn std::future::Future<Output=anyhow::Result<()>>+Send+'a>> {
    Box::pin(async move {
    let catalogue = variants::catalogue(state, user).await?;
    let chosen = catalogue
        .variants
        .iter()
        .find(|e| e.configuration_current && e.binding_current)
        .unwrap()
        .variant
        .clone();
    let variant = VariantRef {
        id: chosen.id,
        revision: chosen.revision,
    };
    let config: RoutingProfileConfig = serde_json::from_value(
        json!({"schema_version":2,"membership":{"kind":"all_available"},"max_active_variants":256,
        "quality":{"assurance":"declared","max_relative_loss_bp":200,"minimum_absolute_quality_bp":null},
        "reference":{"kind":"configured","variant":variant,"allow_uncalibrated":true},"fallback_variant_ids":[],"groups":[],"selector_sha256":null,"verifier_sha256":null}),
    )?;
    let id = Uuid::now_v7();
    let policy: RoutingPolicy = serde_json::from_value(
        json!({"id":id,"revision":1,"rollout":"active","strategy":"rules","objective":"balanced",
        "constraints":{"max_calls":2,"max_output_tokens":256},"default_provider_id":chosen.provider_id,"classifier_provider_id":null}),
    )?;
    let saved = profiles::save(
        state,
        user,
        id,
        profiles::SaveProfile {
            schema_version: 2,
            label: "Personal Full Auto".into(),
            scope: "user".into(),
            expected_revision: 0,
            is_default: true,
            policy: policy.clone(),
            config: config.clone(),
        },
    )
    .await?;
    assert_eq!(saved.policy.id, id);
    assert!(
        store::load_policy(state, user, Some(id)).await.is_err(),
        "v1 must reject v2 semantics"
    );
    assert!(
        profiles::load(state, other, id).await.is_err(),
        "no cross-owner profile access"
    );
    assert!(
        profiles::save(
            state,
            other,
            id,
            profiles::SaveProfile {
                schema_version: 2,
                label: "Overwrite".into(),
                scope: "user".into(),
                expected_revision: saved.revision,
                is_default: false,
                policy: policy.clone(),
                config: config.clone()
            }
        )
        .await
        .is_err()
    );
    assert!(
        store::save_policy(
            state,
            user,
            id,
            store::SavePolicy {
                label: "v1 overwrite".into(),
                scope: "user".into(),
                expected_revision: saved.revision,
                is_default: true,
                policy: saved.policy.clone()
            }
        )
        .await
        .is_err()
    );
    let context = RoutingContext {
        actor: user.clone(),
        chat_id: None,
        project_id: None,
        run_id: Uuid::now_v7(),
        purpose: CallPurpose::Primary,
    };
    let intent = RoutingIntentV2 {
        schema_version: 2,
        selection: ModelSelection::Auto {
            policy_id: Some(id),
            objective: RoutingObjective::Economy,
        },
        required_profile_version: 2,
        variant: None,
        options: None,
    };
    let requirements = WorkloadRequirements {
        input_tokens: 16,
        output_tokens: 128,
        ..Default::default()
    };
    let (plan, budget) = execution::plan(
        state,
        &context,
        intent.clone(),
        requirements.clone(),
        "fixture",
        &tokio::sync::Notify::new(),
    )
    .await?;
    assert_eq!(
        budget.snapshot().calls,
        0,
        "planning makes no model request"
    );
    assert_eq!(plan.schema_version, 2);
    assert!(plan.profile_snapshot.as_ref().unwrap().candidates.len() >= 100);
    assert!(
        plan.decision
            .reason_codes
            .contains(&"configured_model_roles".to_owned())
    );
    assert_eq!(
        plan.assignments[&CallPurpose::Primary]
            .variant
            .as_ref()
            .unwrap()
            .variant,
        variant
    );
    assert_eq!(
        serde_json::to_value(&plan.assignments[&CallPurpose::Primary])?,
        serde_json::to_value(&plan.assignments[&CallPurpose::ToolLoop])?
    );
    execution::validate_plan(state, &context, &plan).await?;
    let target = resolve_target(
        state,
        &context,
        &plan.assignments[&CallPurpose::Primary],
        &plan.constraints,
    )
    .await?;
    assert_eq!(target.schema_version, 2);
    assert_eq!(
        target.generation.as_ref().unwrap().max_output_tokens,
        chosen.behaviour.generation.max_output_tokens
    );
    assert!(serde_json::to_string(&plan)?.find("api_key").is_none());
    // Preparing a native envelope shares the SAME ledger; no HTTP request is
    // made here. An absent report keeps conservative reserved spend.
    let stage = dispatch::StageRequest {
        requirements: WorkloadRequirements {
            effort: target.effort.clone(),
            ..requirements.clone()
        },
        max_calls: 1,
    };
    let prepared = dispatch::prepare(
        state,
        &context,
        &plan,
        &budget,
        &[(CallPurpose::Primary, stage)].into(),
        std::time::Duration::from_secs(3),
    )
    .await?;
    assert_eq!(prepared.envelope.schema_version, 2);
    assert_eq!(
        prepared.envelope.assignments[&CallPurpose::Primary]
            .variant
            .as_ref()
            .unwrap()
            .variant,
        variant
    );
    let _ = prepared.finish(None);
    assert_eq!(budget.snapshot().calls, 1);
    let manual = RoutingIntentV2 {
        schema_version: 2,
        selection: ModelSelection::Manual {
            provider_id: chosen.provider_id,
        },
        required_profile_version: 1,
        variant: Some(VariantSelection {
            id: chosen.id,
            expected_revision: chosen.revision,
        }),
        options: None,
    };
    let (manual_plan, _) = execution::plan(
        state,
        &context,
        manual.clone(),
        requirements.clone(),
        "fixture",
        &tokio::sync::Notify::new(),
    )
    .await?;
    assert_eq!(
        manual_plan.assignments[&CallPurpose::Final]
            .variant
            .as_ref()
            .unwrap()
            .variant,
        variant
    );
    let wide=WorkloadRequirements{input_tokens:16,output_tokens:4096,..Default::default()};
    let (capped_plan,capped_budget)=execution::plan(state,&context,manual.clone(),wide.clone(),"variant cap fixture",&tokio::sync::Notify::new()).await?;
    let mut stage=wide;stage.effort=capped_plan.assignments[&CallPurpose::Primary].effort.clone();
    let grant=fosnie_backend::model_routing::dispatch::prepare(state,&context,&capped_plan,&capped_budget,
      &[(CallPurpose::Primary,fosnie_backend::model_routing::dispatch::StageRequest{requirements:stage,max_calls:1})].into(),std::time::Duration::from_secs(10)).await?;
    assert_eq!(grant.envelope.budget.as_ref().unwrap().allowances[&CallPurpose::Primary].output_tokens,256);
    // Prepare reserves the existing ledger; it sends no request to any model.
    assert_eq!(capped_budget.snapshot().calls,1);drop(grant);
    let mut stale = manual;
    stale.variant.as_mut().unwrap().expected_revision += 1;
    assert!(
        execution::plan(
            state,
            &context,
            stale,
            requirements.clone(),
            "fixture",
            &tokio::sync::Notify::new()
        )
        .await
        .is_err()
    );
    // Both memberships use the same reference path; selected filters the pool.
    let custom_id = Uuid::now_v7();
    let mut custom_policy = policy.clone();
    custom_policy.id = custom_id;
    let mut custom = config.clone();
    custom.membership = PoolMembership::Selected {
        variant_ids: [chosen.id].into(),
    };
    profiles::save(
        state,
        user,
        custom_id,
        profiles::SaveProfile {
            schema_version: 2,
            label: "Custom".into(),
            scope: "user".into(),
            expected_revision: 0,
            is_default: false,
            policy: custom_policy.clone(),
            config: custom.clone(),
        },
    )
    .await?;
    let mut custom_intent = intent.clone();
    custom_intent.selection = ModelSelection::Auto {
        policy_id: Some(custom_id),
        objective: RoutingObjective::Balanced,
    };
    let (custom_plan, _) = execution::plan(
        state,
        &context,
        custom_intent,
        requirements.clone(),
        "fixture",
        &tokio::sync::Notify::new(),
    )
    .await?;
    assert_eq!(
        custom_plan
            .profile_snapshot
            .as_ref()
            .unwrap()
            .candidates
            .len(),
        1
    );
    let exported = profiles::export(state, user, custom_id).await?;
    assert!(exported.config.selector_sha256.is_none());
    let import_id = Uuid::now_v7();
    let imported = profiles::import(
        state,
        user,
        import_id,
        profiles::ImportProfile {
            schema_version: 2,
            scope: "user".into(),
            expected_revision: 0,
            draft: exported,
            variant_mapping: [(chosen.id, variant)].into(),
            provider_mapping: [(chosen.provider_id, chosen.provider_id)].into(),
        },
    )
    .await?;
    assert_eq!(imported.id, import_id);
    assert!(!imported.is_default);
    // Old evaluation flags are normalised away; they no longer gate routing.
    let calibrated_id = Uuid::now_v7();
    let mut calibrated_policy = policy.clone();
    calibrated_policy.id = calibrated_id;
    let mut calibrated = config.clone();
    calibrated.quality.assurance = Assurance::Calibrated;
    calibrated.selector_sha256=Some("a".repeat(64));
    calibrated.verifier_sha256=Some("b".repeat(64));
    let normalised=profiles::save(
        state,
        user,
        calibrated_id,
        profiles::SaveProfile {
            schema_version: 2,
            label: "Imported legacy profile".into(),
            scope: "user".into(),
            expected_revision: 0,
            is_default: false,
            policy: calibrated_policy,
            config: calibrated,
        },
    )
    .await?;
    let mut calibrated_intent = intent.clone();
    calibrated_intent.selection = ModelSelection::Auto {
        policy_id: Some(calibrated_id),
        objective: RoutingObjective::Economy,
    };
    assert_eq!(normalised.config.quality.assurance,Assurance::Declared);
    assert!(normalised.config.selector_sha256.is_none() && normalised.config.verifier_sha256.is_none());
    let (_,normalised_budget)=execution::plan(
            state,
            &context,
            calibrated_intent,
            requirements.clone(),
            "fixture",
            &tokio::sync::Notify::new()
        )
        .await?;
    assert_eq!(normalised_budget.snapshot().calls,0);
    // New catalogue membership invalidates the old frozen planning snapshot.
    let dep = catalogue
        .deployments
        .iter()
        .find(|d| d.id == chosen.provider_id)
        .unwrap();
    let mut extra = variant_body(dep.id, dep.model_config_revision, "user", "none");
    extra.generation.max_output_tokens = Some(173);
    variants::save(state, user, Uuid::now_v7(), extra).await?;
    assert!(
        execution::validate_plan(state, &context, &plan)
            .await
            .is_err()
    );
    let (fresh, _) = execution::plan(
        state,
        &context,
        intent,
        requirements,
        "fixture",
        &tokio::sync::Notify::new(),
    )
    .await?;
    assert_ne!(
        fresh.profile_snapshot.as_ref().unwrap().hash,
        plan.profile_snapshot.as_ref().unwrap().hash
    );
    // Default explicitly resets an Auto/variant binding while keeping mandatory
    // limits of the selected default policy; it does not run Auto classification.
    let reset = RoutingIntentV2 {
        schema_version: 2,
        selection: ModelSelection::Default,
        required_profile_version: 1,
        variant: None,
        options: None,
    };
    let (reset_plan, _) = execution::plan(
        state,
        &context,
        reset,
        WorkloadRequirements {
            output_tokens: 128,
            ..Default::default()
        },
        "fixture",
        &tokio::sync::Notify::new(),
    )
    .await?;
    assert!(reset_plan.profile_snapshot.is_none());
    assert!(
        reset_plan.assignments[&CallPurpose::Final]
            .variant
            .is_none()
    );
    // Shared project binding CAS uses original project ACLs and rejects personal
    // profile installation. No project binding contains a captured actor/key.
    let project = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO projects(id,name,owner_user_id) VALUES($1,'Routing binding fixture',$2)",
    )
    .bind(project)
    .bind(user.user_id)
    .execute(&state.pg)
    .await?;
    let original =
        fosnie_backend::model_routing::vnext::bindings::project(state, user, project).await?;
    use fosnie_backend::model_routing::vnext::bindings;
    assert!(bindings::project(state, other, project).await.is_err());
    let private_intent = RoutingIntentV2 {
        schema_version: 2,
        selection: ModelSelection::Auto {
            policy_id: Some(id),
            objective: RoutingObjective::Balanced,
        },
        required_profile_version: 2,
        variant: None,
        options: None,
    };
    assert!(
        bindings::save_project(
            state,
            user,
            project,
            bindings::SaveProjectBinding {
                schema_version: 2,
                expected_revision: original.revision,
                routing_intent: Some(private_intent),
                clear: false
            }
        )
        .await
        .is_err()
    );
    let shared_intent = RoutingIntentV2 {
        schema_version: 2,
        selection: ModelSelection::Manual {
            provider_id: chosen.provider_id,
        },
        required_profile_version: 1,
        variant: Some(VariantSelection {
            id: chosen.id,
            expected_revision: chosen.revision,
        }),
        options: None,
    };
    let shared = bindings::save_project(
        state,
        user,
        project,
        bindings::SaveProjectBinding {
            schema_version: 2,
            expected_revision: original.revision,
            routing_intent: Some(shared_intent.clone()),
            clear: false,
        },
    )
    .await?;
    assert_eq!(shared.routing_intent.unwrap(), shared_intent);
    assert!(
        bindings::save_project(
            state,
            user,
            project,
            bindings::SaveProjectBinding {
                schema_version: 2,
                expected_revision: original.revision,
                routing_intent: None,
                clear: true
            }
        )
        .await
        .is_err()
    );
    // Reference restrictions apply even when pins/preferences would suggest a
    // candidate. Equal-priority conflict and unknown difficulty are pure D0 tests.
    let mut denied = custom;
    denied.groups.push(TaskGroup {
        id: Uuid::now_v7(),
        label: "Deny reference".into(),
        priority: 10,
        conditions: RuleConditions::default(),
        difficulty: None,
        allowed_variants: None,
        excluded_variants: [chosen.id].into(),
        assignment: None,
    });
    assert!(
        denied
            .resolve_groups(
                &[chosen.id].into(),
                &WorkloadRequirements::default(),
                CallPurpose::Primary,
                DifficultySignal::Unknown
            )
            .is_err()
    );
    // D3 previews freeze new public source cases with explicit allocation;
    // they cannot start model calls while the separate opt-in gate is disabled.
    health_cases::exercise(state,admin).await?;
    Ok(())
    })
}
