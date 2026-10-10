// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::{*, lease::*, budget::RunBudget};
use uuid::Uuid;

fn setup(calls: u32) -> (RunBudget, GrantedLease) {
    let profile: ModelProfile = serde_json::from_value(serde_json::json!({
        "provider_id":Uuid::from_u128(1),"provider_revision":1,"revision":1,
        "adapter":"openai_chat","model":"test","locality":"local","locality_trusted":true,
        "capabilities":{"context_tokens":10000,"max_output_tokens":1000},
        "pricing":{"input_microusd_per_million":1000000,"output_microusd_per_million":1000000}
    })).unwrap();
    let target = InferenceTarget { schema_version:1,provider_id:profile.provider_id,
        provider_revision:1,profile_revision:1,adapter:AdapterKind::OpenaiChat,
        base_url:"http://mock.test".into(),model:"test".into(),api_key:"secret".into(),effort:None };
    let budget = RunBudget::new(RoutingConstraints { max_calls:Some(calls),max_total_tokens:Some(u64::from(calls)*120),..Default::default() });
    let grant = GrantedLease::reserve(&budget,Uuid::from_u128(2),&[(CallPurpose::WebGrade,target)].into(),&[profile],
        [(CallPurpose::WebGrade,Allowance { max_calls:calls,input_tokens:100,output_tokens:20 })].into(),std::time::Duration::from_secs(1)).unwrap();
    (budget,grant)
}

fn report(grant: &GrantedLease) -> LeaseReport {
    serde_json::from_value(serde_json::json!({"lease_id":grant.wire.id,"complete":true,"calls":[{
        "lease_id":grant.wire.id,"decision_id":Uuid::from_u128(2),"call_id":Uuid::now_v7(),
        "provider_id":Uuid::from_u128(1),"provider_revision":1,"profile_revision":1,
        "purpose":"web_grade","status":"completed","latency_ms":1,
        "usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15,
            "reasoning_tokens":2,"cached_input_tokens":5,"raw_usage":{}}
    }]})).unwrap()
}

#[test]
fn r24_complete_report_releases_only_proven_unused_calls() {
    let (budget,grant)=setup(3);
    let result=report(&grant);
    grant.finish(Some(&result)).unwrap();
    assert_eq!(budget.snapshot().calls,1);
    assert_eq!(budget.snapshot().tokens,15);
    assert_eq!(budget.snapshot().cost_microusd,120,"cost upper bound stays until native pricing reconciliation");
    budget.reserve(100,20,Some(120)).unwrap();
}

#[test]
fn r23_missing_partial_or_duplicate_reports_never_refund() {
    for variant in 0..4 {
        let (budget,grant)=setup(3);
        let mut result=report(&grant);
        match variant {
            0 => { grant.finish(None).unwrap(); },
            1 => { result.complete=false; grant.finish(Some(&result)).unwrap(); },
            2 => { result.calls.push(result.calls[0].clone()); assert!(grant.finish(Some(&result)).is_err()); },
            _ => { result.calls[0].provider_revision=2; assert!(grant.finish(Some(&result)).is_err()); },
        }
        assert_eq!(budget.snapshot().calls,3);
        assert_eq!(budget.snapshot().tokens,360);
    }
}

#[test]
fn r16_unknown_usage_is_reserved_and_inconsistent_totals_rejected() {
    let (budget,grant)=setup(1);
    let mut result=report(&grant);
    result.calls[0].usage.input_tokens=None;
    result.calls[0].usage.output_tokens=None;
    result.calls[0].usage.total_tokens=None;
    grant.finish(Some(&result)).unwrap();
    assert_eq!(budget.snapshot().tokens,120);
    let (budget,grant)=setup(1);
    let mut result=report(&grant);
    result.calls[0].usage.total_tokens=Some(1);
    assert!(grant.finish(Some(&result)).is_err());
    assert_eq!(budget.snapshot().tokens,120);
}

#[test]
fn r26_upstream_overrun_stops_future_reservations() {
    let (budget,grant)=setup(2);
    let mut result=report(&grant);
    result.calls[0].usage.input_tokens=Some(200);
    result.calls[0].usage.total_tokens=Some(205);
    assert!(grant.finish(Some(&result)).is_err());
    assert!(budget.snapshot().exhausted);
    assert!(budget.reserve(0,1,Some(0)).is_err());
}

#[test]
fn r26_output_overrun_cannot_borrow_unused_input_allowance() {
    let (budget,grant)=setup(2);
    let mut result=report(&grant);
    result.calls[0].usage.output_tokens=Some(30);
    result.calls[0].usage.total_tokens=Some(40);
    assert!(grant.finish(Some(&result)).is_err());
    assert!(budget.snapshot().exhausted);
    assert_eq!(budget.snapshot().tokens,160,"recorded spend plus unused slot stays charged");
}
