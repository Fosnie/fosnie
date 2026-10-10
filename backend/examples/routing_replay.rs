// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Bounded JSONL adapter to the production routing primitives. No DB/network.
use std::{io::{self,BufRead,Read,Write},collections::BTreeSet};
use serde::Deserialize;
use serde_json::{json,Value};
use uuid::Uuid;
use fosnie_backend::model_routing::{*,budget::RunBudget,engine,classifier,fallback};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    schema_version:u32,case_id:String,decision_id:Uuid,
    policy:RoutingPolicy,requirements:WorkloadRequirements,constraints:RoutingConstraints,profiles:Vec<ModelProfile>,
    classifier_reply:Option<String>,fixed_provider_id:Option<Uuid>,
    #[serde(default)] classifier_failure:bool,
    primary_failure:Option<fallback::FailureKind>,
    #[serde(default)] helpers:Vec<CallPurpose>,
}
fn spend(budget:&RunBudget,profile:&ModelProfile,req:&WorkloadRequirements,confirmed:bool)->Result<(),RoutingError> {
    let reservation=budget.reserve(u64::from(req.input_tokens),req.output_tokens,engine::estimate_cost(profile,req))?;
    if confirmed {reservation.finish(Some(u64::from(req.input_tokens.min(128))+u64::from(req.output_tokens.min(48))),None)?;}
    Ok(())
}
fn replay(input:Input)->anyhow::Result<Value> {
    anyhow::ensure!(input.schema_version==1 && input.case_id.len()<=128 && input.profiles.len()<=32 && input.helpers.len()<=8,"replay bounds");
    engine::validate_policy(&input.policy)?;
    anyhow::ensure!(input.policy.rollout==RolloutMode::Active,"offline replay supports Active proposals only");
    let ids:BTreeSet<_>=input.profiles.iter().map(|p|p.provider_id).collect();
    anyhow::ensure!(ids.len()==input.profiles.len(),"duplicate profile IDs");
    anyhow::ensure!(input.helpers.iter().all(|p|matches!(p,CallPurpose::UtilitySummary|CallPurpose::WebPlan|CallPurpose::WebSynthesis)),"unsupported replay helper");
    let constraints=input.constraints.intersect(&input.policy.constraints);let budget=RunBudget::new(constraints.clone());
    let baseline=input.requirements.clone();let mut req=baseline.clone();let mut rule=engine::RuleMatch::default();
    let mut classified=false;let mut classifier_calls=0;let mut fallback_count=0;let mut stages=Vec::new();let mut selected=None;
    let result=(||->Result<RoutingDecision,RoutingError>{
        if input.fixed_provider_id.is_none() && input.policy.rollout!=RolloutMode::Off && input.policy.strategy!=RoutingStrategy::Classifier {
            rule=engine::apply_rules(&input.policy,&mut req,CallPurpose::Primary)?;
        }
        let main=constraints.intersect(&RoutingConstraints{allowed_providers:rule.allowed_providers.clone(),..Default::default()});
        let empty=main.allowed_providers.as_ref().is_some_and(|p|p.is_empty());
        if empty && input.policy.rollout==RolloutMode::Active && input.fixed_provider_id.is_none() {return Err(RoutingError::NoEligibleModel("empty executor pool"));}
        if !empty && input.fixed_provider_id.is_none() && input.policy.rollout!=RolloutMode::Off && input.policy.strategy!=RoutingStrategy::Rules && rule.target.is_none() {
            let judged=(||->Result<(),RoutingError>{
                let judge=input.profiles.iter().find(|p|Some(p.provider_id)==input.policy.classifier_provider_id).ok_or(RoutingError::ProviderUnavailable)?;
                let judge_req=WorkloadRequirements{input_tokens:input.policy.classifier.max_input_tokens,output_tokens:input.policy.classifier.max_output_tokens,structured_output:true,..Default::default()};
                if let Some(reason)=engine::target_incompatibility(judge,&judge_req,&constraints) {return Err(RoutingError::Incompatible(reason));}
                spend(&budget,judge,&judge_req,!input.classifier_failure)?;classifier_calls+=1;
                stages.push(json!({"purpose":"router_classify","provider_id":judge.provider_id,"estimated_cost_microusd":engine::estimate_cost(judge,&judge_req),"confirmed":!input.classifier_failure}));
                if input.classifier_failure {return Err(RoutingError::Invalid("synthetic failed judge"));}
                req=classifier::merge(&req,classifier::parse(input.classifier_reply.as_deref().ok_or(RoutingError::Invalid("missing fixture reply"))?)?);
                classified=true;Ok(())
            })();
            if judged.is_err() {req.uncertain=true;}
        }
        let mut decision=if let Some(id)=input.fixed_provider_id {
            let p=input.profiles.iter().find(|p|p.provider_id==id).ok_or(RoutingError::ProviderUnavailable)?;
            if let Some(reason)=engine::target_incompatibility(p,&baseline,&constraints) {return Err(RoutingError::Incompatible(reason));}
            RoutingDecision{id:input.decision_id,schema_version:1,policy_id:input.policy.id,policy_revision:input.policy.revision,
                strategy:input.policy.strategy,proposed:Some(TargetReference{provider_id:id,provider_revision:p.provider_revision,profile_revision:p.revision,model:p.model.clone(),effort:baseline.effort.clone()}),
                actual:None,reason_codes:vec!["fixed_baseline".into()],excluded_counts:Default::default(),estimated_cost_microusd:engine::estimate_cost(p,&baseline)}
        } else {engine::select(&input.profiles,&req,&input.policy,&constraints,input.policy.objective,&rule,rule.target.is_none()&&!classified)?};
        decision.id=input.decision_id;
        let target=if input.policy.rollout==RolloutMode::Active || input.fixed_provider_id.is_some() {decision.proposed.clone().unwrap()}
            else {engine::checked_default(&input.profiles,&baseline,&input.policy,&constraints)?};
        req.effort=target.effort.clone();let profile=input.profiles.iter().find(|p|p.provider_id==target.provider_id).unwrap();
        spend(&budget,profile,&req,input.primary_failure.is_none())?;
        stages.push(json!({"purpose":"primary","provider_id":profile.provider_id,"estimated_cost_microusd":engine::estimate_cost(profile,&req),"confirmed":input.primary_failure.is_none()}));
        selected=Some(target);
        if let Some(failure)=input.primary_failure {
            let selection=if let Some(provider_id)=input.fixed_provider_id {ModelSelection::Manual{provider_id}}
                else {ModelSelection::Auto{objective:input.policy.objective,policy_id:Some(input.policy.id)}};
            if !fallback::AttemptState::default().can_retry(&input.policy,&selection,failure) {return Err(RoutingError::Incompatible("synthetic primary failure; no retry"));}
            let used=budget.snapshot();let mut remaining=constraints.clone();
            remaining.max_calls=remaining.max_calls.map(|v|v.saturating_sub(used.calls));
            remaining.max_total_tokens=remaining.max_total_tokens.map(|v|v.saturating_sub(used.tokens));
            remaining.max_cost_microusd=remaining.max_cost_microusd.map(|v|v.saturating_sub(used.cost_microusd));
            let profiles:Vec<_>=input.profiles.iter().filter(|p|Some(p.provider_id)!=selected.as_ref().map(|p|p.provider_id)).cloned().collect();
            let alternate=engine::select(&profiles,&req,&input.policy,&remaining,input.policy.objective,&engine::RuleMatch{allowed_providers:rule.allowed_providers.clone(),..Default::default()},false)?;
            let target=alternate.proposed.unwrap();let profile=profiles.iter().find(|p|p.provider_id==target.provider_id).unwrap();
            spend(&budget,profile,&req,true)?;fallback_count=1;selected=Some(target);
            stages.push(json!({"purpose":"primary","provider_id":profile.provider_id,"estimated_cost_microusd":engine::estimate_cost(profile,&req),"confirmed":true}));
        }
        for &purpose in &input.helpers {
            let id=input.policy.utility_assignments.get(&purpose).copied().unwrap_or(selected.as_ref().unwrap().provider_id);
            let p=input.profiles.iter().find(|p|p.provider_id==id).ok_or(RoutingError::ProviderUnavailable)?;
            let helper=WorkloadRequirements{input_tokens:256,output_tokens:128,..Default::default()};
            if let Some(reason)=engine::target_incompatibility(p,&helper,&constraints) {return Err(RoutingError::Incompatible(reason));}
            spend(&budget,p,&helper,true)?;stages.push(json!({"purpose":purpose,"provider_id":id,"estimated_cost_microusd":engine::estimate_cost(p,&helper),"confirmed":true}));
        }
        Ok(decision)
    })();
    let error=result.as_ref().err().map(ToString::to_string);let success=result.is_ok();
    Ok(json!({"schema_version":1,"mode":"offline_mock","case_id":input.case_id,"decision_id":input.decision_id,"ok":success,
        "decision":result.ok(),"simulated_target":selected,"actual_target":null,"effective_requirements":req,
        "executor_allowed_providers":rule.allowed_providers,"constraints":constraints,"ledger":budget.snapshot(),"stages":stages,
        "classifier_calls":classifier_calls,"fallback_count":fallback_count,"error":error,"actual_cost_microusd":null,
        "latency_ms":null,"ttft_ms":null,"model_quality_status":"not_run"}))
}
fn main()->anyhow::Result<()> {
    let stdin=io::stdin();let mut reader=stdin.lock();let mut stdout=io::BufWriter::new(io::stdout().lock());let mut count=0;
    loop {
        let mut line=String::new();let size=reader.by_ref().take(65_537).read_line(&mut line)?;
        if size==0 {break;}count+=1;
        anyhow::ensure!(size<=65_536 && count<=5000,"replay input exceeds bounded JSONL limits");
        let input:Input=serde_json::from_str(&line).map_err(|_|anyhow::anyhow!("invalid replay schema"))?;
        writeln!(stdout,"{}",replay(input)?)?;
    }
    stdout.flush()?;Ok(())
}
