// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
use super::*;
use super::super::{fixed::FixedReference,transport::{PipelineClient,JsonOperation,StreamOperation}};
use serde_json::{json,Value};
use uuid::Uuid;

fn fixture(budget:&RunBudget,llm_calls:u32,fixed_calls:u32,timeout:Duration) -> Result<PreparedMixedRequest,RoutingError> {
    fixture_role(budget,llm_calls,fixed_calls,timeout,FixedRole::Embed)
}
fn fixture_role(budget:&RunBudget,llm_calls:u32,fixed_calls:u32,timeout:Duration,role:FixedRole) -> Result<PreparedMixedRequest,RoutingError> {
    let profile:ModelProfile=serde_json::from_value(json!({"provider_id":Uuid::from_u128(1),"provider_revision":1,"revision":1,
        "adapter":"openai_chat","model":"test","locality":"local","locality_trusted":true,
        "capabilities":{"context_tokens":10000,"max_output_tokens":1000},
        "pricing":{"input_microusd_per_million":1000000,"output_microusd_per_million":1000000}})).unwrap();
    let llm=InferenceTarget { generation:None,variant:None, schema_version:1,provider_id:profile.provider_id,provider_revision:1,profile_revision:1,
        adapter:AdapterKind::OpenaiChat,base_url:"http://mock.invalid".into(),model:"test".into(),api_key:"llm-secret".into(),effort:None };
    let fixed=FixedTarget { schema_version:1,reference:FixedReference { provider_id:Uuid::from_u128(3),provider_revision:3,
        profile_revision:4,role,model:"fixed".into(),active_index_revision:if role==FixedRole::Embed { Some(5) } else { None },desired_index_revision:None },
        base_url:"http://fixed.invalid".into(),api_key:"fixed-secret".into(),embedding_dimension:if role==FixedRole::Embed { Some(2) } else { None },
        embedding_collection:if role==FixedRole::Embed { Some("active".into()) } else { None },ocr_pdf_mode:None };
    let assignments=if llm_calls>0 { [(CallPurpose::RetrievalGrade,llm)].into() } else { BTreeMap::new() };
    let fixed_assignments=if fixed_calls>0 { [(role,fixed)].into() } else { BTreeMap::new() };
    let allowances=if llm_calls>0 { [(CallPurpose::RetrievalGrade,Allowance { max_calls:llm_calls,input_tokens:100,output_tokens:20 })].into() } else { BTreeMap::new() };
    let fixed_allowances=if fixed_calls>0 { [(role,FixedAllowance { max_calls:fixed_calls,input_bytes:100,output_bytes:200 })].into() } else { BTreeMap::new() };
    let id=Uuid::from_u128(2);
    let grant=MixedGrant::reserve(budget,id,&assignments,&[profile],allowances,&fixed_assignments,fixed_allowances,timeout)?;
    let envelope=InferenceEnvelope { schema_version:1,decision_id:id,purpose:if llm_calls>0 { CallPurpose::RetrievalGrade } else { role.purpose() },
        assignments,fixed_assignments,budget:grant.llm.as_ref().map(|g|g.wire.clone()),fixed_budget:grant.fixed.as_ref().map(|g|g.wire.clone()) };
    Ok(PreparedMixedRequest { envelope,grant,probes:vec![] })
}
fn prepared(budget:&RunBudget) -> PreparedMixedRequest { fixture(budget,2,2,Duration::from_secs(2)).unwrap() }
fn response(envelope:&Value) -> Value {
    let mut out=json!({"type":"done","text":"готово"});
    if let Some(lease)=envelope.get("budget") {
        out["routing_report"]=json!({"lease_id":lease["id"],"complete":true,"calls":[{
            "lease_id":lease["id"],"decision_id":envelope["decision_id"],"call_id":Uuid::now_v7(),
            "provider_id":Uuid::from_u128(1),"provider_revision":1,"profile_revision":1,
            "purpose":"retrieval_grade","status":"completed","latency_ms":1,
            "usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15,"raw_usage":{}}}]});
    }
    if let Some(lease)=envelope.get("fixed_budget") {
        let role=lease["allowances"].as_object().unwrap().keys().next().unwrap();
        let target=&envelope["fixed_assignments"][role];
        out["fixed_routing_report"]=json!({"lease_id":lease["id"],"complete":true,"calls":[{
            "lease_id":lease["id"],"decision_id":envelope["decision_id"],"call_id":Uuid::now_v7(),
            "provider_id":Uuid::from_u128(3),"provider_revision":3,"profile_revision":4,
            "role":role,"active_index_revision":target["active_index_revision"],"desired_index_revision":target["desired_index_revision"],
            "status":"completed","latency_ms":1,"input_bytes":30,"output_bytes":40}]});
    }
    out
}
fn reports_for(prepared:&PreparedMixedRequest) -> Reports { serde_json::from_value(response(&serde_json::to_value(&prepared.envelope).unwrap())).unwrap() }

#[test]
fn mixed_reservation_rejects_without_partial_charges() {
    for limits in [RoutingConstraints { max_calls:Some(3),..Default::default() },
        RoutingConstraints { max_cost_microusd:Some(1000),..Default::default() },
        RoutingConstraints { max_total_tokens:Some(1000),..Default::default() },
        RoutingConstraints { max_output_tokens:Some(1000),..Default::default() }] {
        let budget=RunBudget::new(limits);
        assert!(fixture(&budget,2,2,Duration::from_secs(1)).is_err());
        let usage=budget.snapshot(); assert_eq!(usage.calls,0); assert_eq!(usage.tokens,0);
        assert_eq!(usage.cost_microusd,0); assert_eq!(usage.unknown_token_calls,0); assert_eq!(usage.unknown_cost_calls,0);
    }
    let budget=RunBudget::new(Default::default());
    assert!(fixture(&budget,1024,1,Duration::from_secs(1)).is_err()); assert_eq!(budget.snapshot().calls,0);
    assert!(fixture(&budget,1,1,Duration::ZERO).is_err()); assert_eq!(budget.snapshot().calls,0);
}
#[test]
fn mixed_concurrent_batches_share_one_atomic_call_limit() {
    let budget=RunBudget::new(RoutingConstraints { max_calls:Some(4),..Default::default() });
    let threads:Vec<_>=(0..8).map(|_|{ let budget=budget.clone(); std::thread::spawn(move||fixture(&budget,2,2,Duration::from_secs(1)).is_ok()) }).collect();
    assert_eq!(threads.into_iter().map(|h|h.join().unwrap()).filter(|ok|*ok).count(),1);
    assert_eq!(budget.snapshot().calls,4); assert_eq!(budget.snapshot().unknown_token_calls,2);
}
#[test]
fn mixed_complete_reports_release_only_unused_and_keep_unknown_fixed_spend() {
    let budget=RunBudget::new(Default::default()); let prepared=prepared(&budget); let reports=reports_for(&prepared);
    assert!(prepared.finish(&reports).unwrap());
    let usage=budget.snapshot(); assert_eq!(usage.calls,2); assert_eq!(usage.tokens,15);
    assert_eq!(usage.cost_microusd,120); assert_eq!(usage.unknown_token_calls,1); assert_eq!(usage.unknown_cost_calls,1);
    budget.restrict(&RoutingConstraints { max_cost_microusd:Some(1000),..Default::default() });
    assert!(budget.reserve(1,1,Some(0)).is_err());
}
#[test]
fn mixed_missing_partial_or_invalid_second_report_never_refunds_first() {
    for mode in 0..6 {
        let budget=RunBudget::new(Default::default()); let prepared=prepared(&budget); let mut reports=reports_for(&prepared);
        match mode { 0=>reports.fixed_routing_report=None,1=>reports.fixed_routing_report.as_mut().unwrap().complete=false,
            2=>reports.fixed_routing_report.as_mut().unwrap().calls[0].profile_revision=999,
            3=>reports.routing_report=None,4=>reports.routing_report.as_mut().unwrap().complete=false,
            _=>reports.fixed_routing_report.as_mut().unwrap().calls[0].call_id=reports.routing_report.as_ref().unwrap().calls[0].call_id }
        let result=prepared.finish(&reports);
        if mode==2 || mode==5 { assert!(result.is_err()); } else { assert!(!result.unwrap()); }
        assert_eq!(budget.snapshot().calls,4); assert_eq!(budget.snapshot().tokens,240); assert_eq!(budget.snapshot().unknown_token_calls,2);
    }
}
#[test]
fn mixed_partial_report_overrun_exhausts_run_without_refund() {
    for llm in [true,false] {
        let budget=RunBudget::new(Default::default()); let prepared=prepared(&budget); let mut reports=reports_for(&prepared);
        if llm { let report=reports.routing_report.as_mut().unwrap(); report.complete=false;
            report.calls[0].usage.output_tokens=Some(30); report.calls[0].usage.total_tokens=Some(40);
        } else { let report=reports.fixed_routing_report.as_mut().unwrap(); report.complete=false; report.calls[0].output_bytes=Some(201); }
        assert!(prepared.finish(&reports).is_err()); assert!(budget.snapshot().exhausted); assert_eq!(budget.snapshot().calls,4);
    }
}
#[test]
fn mixed_single_lane_and_unexpected_report_are_distinct() {
    for (llm,fixed) in [(2,0),(0,2)] {
        let budget=RunBudget::new(Default::default()); let prepared=fixture(&budget,llm,fixed,Duration::from_secs(1)).unwrap();
        let reports=reports_for(&prepared); assert!(prepared.finish(&reports).unwrap()); assert_eq!(budget.snapshot().calls,1);
    }
    let budget=RunBudget::new(Default::default()); let prepared=fixture(&budget,2,0,Duration::from_secs(1)).unwrap();
    let mut reports=reports_for(&prepared); let extra=fixture(&RunBudget::new(Default::default()),0,2,Duration::from_secs(1)).unwrap();
    reports.fixed_routing_report=reports_for(&extra).fixed_routing_report;
    assert!(prepared.finish(&reports).is_err()); assert_eq!(budget.snapshot().calls,2);
}
#[test]
fn mixed_prepared_requests_stop_after_another_lane_exhausts_the_run() {
    let budget=RunBudget::new(Default::default()); let pending=prepared(&budget); let other=prepared(&budget);
    let mut reports=reports_for(&other); reports.fixed_routing_report.as_mut().unwrap().calls[0].output_bytes=Some(201);
    assert!(other.finish(&reports).is_err()); assert!(pending.grant.remaining().is_err());
    assert_eq!(budget.snapshot().calls,8);
}

#[derive(Clone)]
struct Mock { mode:&'static str,seen:std::sync::Arc<std::sync::atomic::AtomicUsize> }
async fn handler(axum::extract::State(mock):axum::extract::State<Mock>,request:axum::extract::Request) -> axum::response::Response {
    use axum::response::IntoResponse;
    mock.seen.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
    assert_eq!(request.headers()["x-fosnie-ml-key"],"ml-secret");
    let path=request.uri().path().to_owned();
    let header=request.headers().get("x-fosnie-overrides").cloned();
    let bytes=axum::body::to_bytes(request.into_body(),1024*1024).await.unwrap();
    let body:Value=if let Some(header)=header { assert_eq!(&bytes[..],b"audio"); json!({"overrides":serde_json::from_slice::<Value>(header.as_bytes()).unwrap()}) }
        else { serde_json::from_slice(&bytes).unwrap() };
    let envelope=&body["overrides"]["routing"];
    assert_eq!(envelope["schema_version"],1);
    if path=="/speech" { assert!(envelope["fixed_assignments"].get("tts").is_some()); }
    if path=="/transcribe" { assert!(envelope["fixed_assignments"].get("stt").is_some()); }
    let mut out=response(envelope);
    match mock.mode {
        "missing"=>{out.as_object_mut().unwrap().remove("fixed_routing_report");},
        "bad"=>out["fixed_routing_report"]["calls"][0]["provider_revision"]=json!(999),
        "partial"=>out["fixed_routing_report"]["complete"]=json!(false),
        "redirect"=>return (axum::http::StatusCode::TEMPORARY_REDIRECT,[("location","/unexpected")]).into_response(),
        "slow"=>tokio::time::sleep(Duration::from_millis(500)).await,
        _=>{}
    }
    if path=="/speech" {
        out["fixed_routing_report"]["calls"][0]["output_bytes"]=json!(b"RIFFaudio".len());
        return ([("content-type","audio/wav"),("x-fosnie-fixed-routing-report",&out["fixed_routing_report"].to_string())],b"RIFFaudio".to_vec()).into_response();
    }
    if body["stream"]==true {
        if path=="/reindex-embeddings" { out["type"]=json!("built"); }
        if mock.mode=="error" { out["type"]=json!("error"); out["message"]=json!("secret from upstream"); }
        let mut data=b"{\"type\":\"progress\",\"text\":\"working\"}\n".to_vec();
        if mock.mode!="eof" { data.extend(serde_json::to_vec(&out).unwrap()); }
        // Deliberately split inside UTF-8/JSON; the terminal has no final newline.
        let chunks:Vec<_>=data.chunks(7).map(|c|Ok::<_,std::convert::Infallible>(axum::body::Bytes::copy_from_slice(c))).collect();
        return axum::response::Response::builder().header("content-type","application/x-ndjson")
            .body(axum::body::Body::from_stream(futures_util::stream::iter(chunks))).unwrap();
    }
    axum::Json(out).into_response()
}
struct Server { task:tokio::task::JoinHandle<()>,client:PipelineClient,seen:std::sync::Arc<std::sync::atomic::AtomicUsize> }
impl Drop for Server { fn drop(&mut self) { self.task.abort(); } }
async fn server(mode:&'static str) -> Server {
    let seen=std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app=axum::Router::new().fallback(handler).with_state(Mock { mode,seen:seen.clone() });
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client=PipelineClient::new(&format!("http://{}",listener.local_addr().unwrap()),"ml-secret").unwrap();
    let task=tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); }); Server { task,client,seen }
}
#[tokio::test]
async fn mixed_json_transport_requires_both_complete_reports() {
    for mode in ["complete","missing","partial","bad","redirect"] {
        let server=server(mode).await; let budget=RunBudget::new(Default::default());
        let result=server.client.json(JsonOperation::Retrieve,json!({"overrides":{"routing":"forged"}}),prepared(&budget),&tokio::sync::Notify::new()).await;
        assert_eq!(result.is_ok(),mode=="complete"); assert_eq!(budget.snapshot().calls,if mode=="complete" {2} else {4});
        assert_eq!(server.seen.load(std::sync::atomic::Ordering::SeqCst),1,"redirects and errors never retry");
    }
}
#[tokio::test]
async fn mixed_stream_transport_closes_on_terminal_error_or_incomplete_eof() {
    for mode in ["complete","missing","partial","bad","eof","error"] {
        let server=server(mode).await; let budget=RunBudget::new(Default::default()); let cancel=tokio::sync::Notify::new();
        let mut stream=server.client.stream(StreamOperation::Retrieve,json!({}),prepared(&budget),&cancel).await.unwrap();
        assert_eq!(stream.recv(&cancel).await.unwrap().unwrap()["type"],"progress");
        let result=stream.recv(&cancel).await;
        assert_eq!(result.is_ok(),mode=="complete");
        if let Err(error)=&result { assert!(!error.to_string().contains("secret from upstream")); }
        assert!(stream.recv(&cancel).await.unwrap().is_none());
        assert_eq!(budget.snapshot().calls,if matches!(mode,"complete"|"error") {2} else {4});
    }
}
#[tokio::test]
async fn mixed_transport_deadline_and_precancel_never_release_unknown_spend() {
    let server=server("slow").await;
    let budget=RunBudget::new(Default::default()); let cancel=tokio::sync::Notify::new(); cancel.notify_one();
    assert!(server.client.json(JsonOperation::Retrieve,json!({}),prepared(&budget),&cancel).await.is_err());
    assert_eq!(server.seen.load(std::sync::atomic::Ordering::SeqCst),0); assert_eq!(budget.snapshot().calls,4);
    let budget=RunBudget::new(Default::default()); let request=fixture(&budget,2,2,Duration::from_millis(50)).unwrap();
    assert!(server.client.json(JsonOperation::Retrieve,json!({}),request,&cancel).await.is_err());
    assert_eq!(server.seen.load(std::sync::atomic::Ordering::SeqCst),1); assert_eq!(budget.snapshot().calls,4);
}
#[tokio::test]
async fn mixed_audio_and_transcription_keep_reports_and_payloads() {
    let server=server("complete").await; let cancel=tokio::sync::Notify::new();
    let budget=RunBudget::new(Default::default()); let request=fixture_role(&budget,0,2,Duration::from_secs(2),FixedRole::Tts).unwrap();
    let audio=server.client.speech(json!({"text":"hello"}),request,&cancel).await.unwrap();
    assert_eq!(audio.mime,"audio/wav"); assert_eq!(audio.bytes,b"RIFFaudio"); assert_eq!(budget.snapshot().calls,1);
    let budget=RunBudget::new(Default::default()); let request=fixture_role(&budget,0,2,Duration::from_secs(2),FixedRole::Stt).unwrap();
    let text=server.client.transcribe(b"audio".to_vec(),"audio/wav",Some("en"),request,&cancel).await.unwrap();
    assert_eq!(text["text"],"готово"); assert_eq!(budget.snapshot().calls,1);
    let budget=RunBudget::new(Default::default());
    let request=fixture_role(&budget,2,2,Duration::from_secs(2),FixedRole::Tts).unwrap();
    assert!(server.client.speech(json!({"text":"hello"}),request,&cancel).await.is_err(),"audio cannot hide a missing LLM report");
    assert_eq!(budget.snapshot().calls,4);
}
#[tokio::test]
async fn mixed_reindex_built_and_stream_cancellation_are_terminal() {
    let server=server("complete").await; let cancel=tokio::sync::Notify::new(); let budget=RunBudget::new(Default::default());
    let mut stream=server.client.stream(StreamOperation::Reindex,json!({}),prepared(&budget),&cancel).await.unwrap();
    stream.recv(&cancel).await.unwrap(); assert_eq!(stream.recv(&cancel).await.unwrap().unwrap()["type"],"built");
    assert!(stream.recv(&cancel).await.unwrap().is_none()); assert_eq!(budget.snapshot().calls,2);
    let budget=RunBudget::new(Default::default());
    let mut stream=server.client.stream(StreamOperation::Retrieve,json!({}),prepared(&budget),&cancel).await.unwrap();
    stream.recv(&cancel).await.unwrap(); cancel.notify_one(); assert!(stream.recv(&cancel).await.is_err());
    assert!(stream.recv(&cancel).await.unwrap().is_none()); assert_eq!(budget.snapshot().calls,4);
}
