// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Opt-in ML pipeline transport. A successful response requires all granted
//! lanes to report completion. Dropping/cancelling never refunds unknown work.
use serde_json::Value;
use tokio::sync::Notify;
use super::{mixed::{PreparedMixedRequest, Reports}, RoutingError};
use super::fallback::{OpenFailure,FailureKind};

const MAX_BODY: usize = 32 * 1024 * 1024;
const MAX_LINE: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub enum JsonOperation { Retrieve, WebSearch, Ingest, Verify, EmbedDimension, ReadDocument, ChatStep, ClassifyPrompt, VerifyDraft, RepairDraft, ResearchTriage }
impl JsonOperation {
    fn path(self) -> &'static str { match self {
        Self::Retrieve => "/retrieve", Self::WebSearch => "/web_search", Self::Ingest => "/ingest",
        Self::Verify => "/verify", Self::EmbedDimension => "/embed-dimension",
        Self::ReadDocument => "/read-document", Self::ChatStep => "/chat-step", Self::ClassifyPrompt => "/classify-prompt",
        Self::VerifyDraft => "/verify-draft", Self::RepairDraft => "/repair-draft",
        Self::ResearchTriage => "/research/routed-triage",
    } }
}
#[derive(Clone, Copy)]
pub enum StreamOperation { Retrieve, WebSearch, Research, Reindex, Generate }
impl StreamOperation {
    fn path(self) -> &'static str { match self {
        Self::Retrieve => "/retrieve", Self::WebSearch => "/web_search", Self::Research => "/deep_research",
        Self::Reindex => "/reindex-embeddings", Self::Generate => "/generate",
    } }
    fn terminal(self) -> &'static str { if matches!(self, Self::Reindex) { "built" } else { "done" } }
}

/// Reuse per ML service. Never follow a redirect with decrypted target credentials.
pub struct PipelineClient { http:reqwest::Client, base_url:String, state:Option<crate::AppState> }
impl PipelineClient {
    pub fn new(base_url:&str, shared_secret:&str) -> crate::Result<Self> {
        let url=reqwest::Url::parse(base_url).map_err(|_|invalid("invalid ML endpoint"))?;
        if !matches!(url.scheme(),"http"|"https") || url.host_str().is_none() || !url.username().is_empty()
            || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err(invalid("invalid ML endpoint"));
        }
        let mut headers=reqwest::header::HeaderMap::new();
        if !shared_secret.is_empty() {
            let mut value=reqwest::header::HeaderValue::from_str(shared_secret).map_err(|_|invalid("invalid ML credential"))?;
            value.set_sensitive(true); headers.insert("x-fosnie-ml-key",value);
        }
        let http=reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).default_headers(headers)
            .build().map_err(|_|unavailable("could not create routed ML client"))?;
        Ok(Self { http,base_url:base_url.trim_end_matches('/').into(),state:None })
    }
    pub fn from_state(state:&crate::AppState) -> crate::Result<Self> {
        let mut client=Self::new(&state.boot.ml.base_url,&state.boot.ml.shared_secret)?;
        client.state=Some(state.clone());Ok(client)
    }
    pub async fn json(&self, operation:JsonOperation, body:Value, mut prepared:PreparedMixedRequest, cancel:&Notify) -> crate::Result<Value> {
        refresh(&mut prepared)?;
        let body=body_with_routing(body,&prepared,Some(false))?;
        let wait=prepared.grant.remaining()?;
        guarded(cancel,wait,async {
            let response=self.http.post(format!("{}{}",self.base_url,operation.path())).json(&body).send().await
                .map_err(|_|unavailable("routed ML connection failed"))?;
            let value=read_json(response).await?;
            let failed=provider_failure(&value,&prepared,false)?;
            let target=prepared.envelope.assignments.get(&prepared.envelope.purpose).cloned();
            let purpose=prepared.envelope.purpose;
            reconcile(prepared,&value)?;
            if let Some(failure)=failed {
                if let (Some(state),Some(target))=(&self.state,target) {super::health::note(state,&target,purpose,&failure).await?;}
                return Err(failure.app());
            }
            Ok(value)
        }).await
    }
    pub async fn stream(&self, operation:StreamOperation, body:Value, mut prepared:PreparedMixedRequest, cancel:&Notify) -> crate::Result<PipelineStream> {
        refresh(&mut prepared)?;
        let body=body_with_routing(body,&prepared,Some(true))?;
        let wait=prepared.grant.remaining()?;
        guarded(cancel,wait,async {
            let response=self.http.post(format!("{}{}",self.base_url,operation.path())).json(&body).send().await
                .map_err(|_|unavailable("routed ML service connection failed"))?;
            // A failed shared ML service is not a confirmed provider failure.
            check_status(&response)?;
            Ok(PipelineStream { response:Some(response),prepared:Some(prepared),buffer:Vec::new(),terminal:operation.terminal(),
                generation:matches!(operation,StreamOperation::Generate),observed:false,state:self.state.clone() })
        }).await
    }
    pub async fn transcribe(&self, audio:Vec<u8>, mime:&str, language:Option<&str>, mut prepared:PreparedMixedRequest, cancel:&Notify) -> crate::Result<Value> {
        if audio.is_empty() || audio.len()>MAX_BODY { return Err(invalid("invalid routed audio size")); }
        refresh(&mut prepared)?;
        let overrides=prepared.envelope.apply(Default::default())?;
        let header=serde_json::to_string(&overrides).map_err(|_|invalid("invalid inference overrides"))?;
        let mut header=reqwest::header::HeaderValue::from_str(&header).map_err(|_|invalid("invalid inference overrides header"))?;
        header.set_sensitive(true);
        let wait=prepared.grant.remaining()?;
        guarded(cancel,wait,async {
            let mut url=reqwest::Url::parse(&format!("{}/transcribe",self.base_url)).map_err(|_|invalid("invalid ML endpoint"))?;
            if let Some(language)=language { url.query_pairs_mut().append_pair("language",language); }
            let request=self.http.post(url).header("content-type",mime)
                .header("x-fosnie-overrides",header).body(audio);
            let value=read_json(request.send().await.map_err(|_|unavailable("routed transcription connection failed"))?).await?;
            let text=value.get("text").and_then(Value::as_str).ok_or_else(||invalid("missing transcription text"))?;
            audio_report(&reports(&value)?,super::fixed::FixedRole::Stt,text.len(),false)?;
            reconcile(prepared,&value)?;
            Ok(value)
        }).await
    }
    pub async fn speech(&self, body:Value, mut prepared:PreparedMixedRequest, cancel:&Notify) -> crate::Result<AudioResponse> {
        refresh(&mut prepared)?;
        let body=body_with_routing(body,&prepared,None)?;
        let wait=prepared.grant.remaining()?;
        guarded(cancel,wait,async {
            let response=self.http.post(format!("{}/speech",self.base_url)).json(&body).send().await
                .map_err(|_|unavailable("routed speech connection failed"))?;
            check_status(&response)?;
            let report=response.headers().get("x-fosnie-fixed-routing-report")
                .ok_or_else(||invalid("missing speech inference report"))?;
            let reports=Reports { routing_report:None,fixed_routing_report:Some(serde_json::from_slice(report.as_bytes())
                .map_err(|_|invalid("invalid speech inference report"))?) };
            let mime=response.headers().get("content-type").and_then(|v|v.to_str().ok()).unwrap_or("application/octet-stream").to_owned();
            if !mime.starts_with("audio/") && mime!="application/octet-stream" {return Err(invalid("invalid speech media type"));}
            let bytes=read_body(response).await?;
            if bytes.is_empty() {return Err(invalid("empty speech audio"));}
            audio_report(&reports,super::fixed::FixedRole::Tts,bytes.len(),true)?;
            let input_bytes=reports.fixed_routing_report.as_ref().unwrap().calls[0].input_bytes;
            // Only a fully received body can complete the request.
            if !prepared.finish(&reports)? { return Err(invalid("incomplete speech inference report")); }
            Ok(AudioResponse { bytes,mime,input_bytes })
        }).await
    }
}

pub struct AudioResponse { pub bytes:Vec<u8>,pub mime:String,pub input_bytes:u64 }
// Audio primitives execute exactly one provider request. An empty/failed report
// must not refund the reserved call or make received bytes a successful result.
fn audio_report(reports:&Reports,role:super::fixed::FixedRole,size:usize,exact:bool)->crate::Result<()> {
    let report=reports.fixed_routing_report.as_ref().ok_or_else(||invalid("missing audio inference report"))?;
    if !report.complete || report.calls.len()!=1 {return Err(invalid("audio requires one completed inference call"));}
    let call=&report.calls[0];
    if call.role!=role || !matches!(call.status,super::lease::CallStatus::Completed)
        || !call.output_bytes.is_some_and(|n|if exact {n==size as u64} else {n>=size as u64}) {
        return Err(invalid("audio body differs from completed inference report"));
    }
    Ok(())
}
pub struct PipelineStream {
    response:Option<reqwest::Response>,prepared:Option<PreparedMixedRequest>,buffer:Vec<u8>,terminal:&'static str,
    generation:bool,observed:bool,state:Option<crate::AppState>,
}
impl PipelineStream {
    pub(crate) fn is_finished(&self) -> bool {self.prepared.is_none()}
    pub async fn recv(&mut self,cancel:&Notify) -> crate::Result<Option<Value>> {
        let Some(prepared)=self.prepared.as_ref() else { return Ok(None); };
        let wait=match prepared.grant.remaining() { Ok(wait)=>wait,Err(error)=>{ self.close(); return Err(error.into()); } };
        let result=guarded(cancel,wait,self.next()).await;
        if result.is_err() { self.close(); }
        result
    }
    fn close(&mut self) { self.response.take(); self.prepared.take(); self.buffer.clear(); }
    async fn next(&mut self) -> crate::Result<Option<Value>> {
        loop {
            let line=if let Some(end)=self.buffer.iter().position(|b|*b==b'\n') {
                Some(self.buffer.drain(..=end).collect::<Vec<_>>())
            } else {
                let chunk=self.response.as_mut().unwrap().chunk().await.map_err(|_|unavailable("routed stream interrupted"))?;
                match chunk {
                    Some(chunk)=>{
                        if self.buffer.len().saturating_add(chunk.len())>MAX_LINE { return Err(invalid("routed stream line too large")); }
                        self.buffer.extend_from_slice(&chunk); None
                    }
                    None if !self.buffer.is_empty()=>Some(std::mem::take(&mut self.buffer)),
                    None=>return Err(unavailable("routed stream ended before completion")),
                }
            };
            let Some(line)=line else { continue; };
            if line.iter().all(u8::is_ascii_whitespace) { continue; }
            let event:Value=serde_json::from_slice(&line).map_err(|_|invalid("invalid routed stream event"))?;
            let kind=event.get("type").and_then(Value::as_str).ok_or_else(||invalid("missing routed stream event type"))?;
            if kind==self.terminal || kind=="error" {
                let prepared=self.prepared.take().unwrap(); self.response.take(); self.buffer.clear();
                if kind=="error" {
                    // An error can still contain a complete accounting report.
                    // Never pass upstream exception text (which may contain keys).
                    let reports=reports(&event)?;
                    let failure=if self.generation {provider_failure(&event,&prepared,self.observed)?} else {None};
                    let target=prepared.envelope.assignments.get(&prepared.envelope.purpose).cloned();
                    let purpose=prepared.envelope.purpose;
                    prepared.finish(&reports)?;
                    if let Some(failure)=failure {
                        if let (Some(state),Some(target))=(&self.state,target) {super::health::note(state,&target,purpose,&failure).await?;}
                        return Err(failure.app());
                    }
                    return Err(unavailable("routed pipeline failed"));
                }
                if self.generation {
                    // /generate always executes exactly one LLM call. Reject an
                    // empty success report BEFORE it can refund the reservation.
                    let report=reports(&event)?;
                    if !report.routing_report.as_ref().is_some_and(|r|r.calls.len()==1
                        && r.calls[0].purpose==prepared.envelope.purpose
                        && matches!(r.calls[0].status,super::lease::CallStatus::Completed)) {
                        return Err(invalid("generation requires one completed inference call"));
                    }
                }
                reconcile(prepared,&event)?;
            }
            self.observed=true;
            return Ok(Some(event));
        }
    }
}

fn refresh(prepared:&mut PreparedMixedRequest) -> crate::Result<()> {
    let wait=prepared.grant.remaining()?;
    let ms=wait.as_millis().min(86_400_000) as u64;
    if ms==0 { return Err(RoutingError::BudgetExhausted.into()); }
    if let Some(lease)=prepared.envelope.budget.as_mut() { lease.timeout_ms=lease.timeout_ms.min(ms); }
    if let Some(lease)=prepared.envelope.fixed_budget.as_mut() { lease.timeout_ms=lease.timeout_ms.min(ms); }
    Ok(())
}
fn body_with_routing(mut body:Value,prepared:&PreparedMixedRequest,stream:Option<bool>) -> crate::Result<Value> {
    let object=body.as_object_mut().ok_or_else(||invalid("routed request must be an object"))?;
    let overrides=match object.remove("overrides") { None|Some(Value::Null)=>Default::default(),Some(Value::Object(map))=>map,
        _=>return Err(invalid("invalid inference overrides")) };
    object.insert("overrides".into(),Value::Object(prepared.envelope.apply(overrides)?));
    if let Some(stream)=stream { object.insert("stream".into(),Value::Bool(stream)); }
    Ok(body)
}
fn provider_failure(value:&Value,prepared:&PreparedMixedRequest,observed:bool)->crate::Result<Option<super::failure::ProviderFailure>> {
    let Some(raw)=value.get("routing_failure").filter(|v|!v.is_null()) else {return Ok(None);};
    let mut failure:super::failure::ProviderFailure=serde_json::from_value(raw.clone()).map_err(|_|invalid("invalid provider failure metadata"))?;
    if !failure.valid() {return Err(invalid("invalid provider failure metadata"));}
    let report=reports(value)?;
    if !report.routing_report.as_ref().is_some_and(|r|r.complete && r.calls.len()==1
        && r.calls[0].purpose==prepared.envelope.purpose && matches!(r.calls[0].status,super::lease::CallStatus::Failed)) {
        return Err(invalid("provider failure requires an accounted failed call"));
    }
    failure.before_output &= !observed;
    Ok(Some(failure))
}
fn reports(value:&Value) -> crate::Result<Reports> {
    serde_json::from_value(value.clone()).map_err(|_|invalid("invalid inference reports"))
}
fn reconcile(prepared:PreparedMixedRequest,value:&Value) -> crate::Result<()> {
    if !prepared.finish(&reports(value)?)? { return Err(invalid("incomplete inference reports")); }
    Ok(())
}
fn contract_failure(status:u16, reason:Option<&str>) -> Option<&'static str> {
    if status!=422 {return None;}
    // Never surface an arbitrary upstream header, response body or model text.
    Some(match reason? {
        "unsupported_contract"=>"routed output schema rejected: unsupported_contract",
        "incomplete_response"=>"routed output schema rejected: incomplete_response",
        "invalid_content"=>"routed output schema rejected: invalid_content",
        "output_too_large"=>"routed output schema rejected: output_too_large",
        "markdown_fence"=>"routed output schema rejected: markdown_fence",
        "invalid_json"=>"routed output schema rejected: invalid_json",
        "duplicate_keys"=>"routed output schema rejected: duplicate_keys",
        "fields_mismatch"=>"routed output schema rejected: fields_mismatch",
        "scalar_type_mismatch"=>"routed output schema rejected: scalar_type_mismatch",
        "enum_mismatch"=>"routed output schema rejected: enum_mismatch",
        "bounds_mismatch"=>"routed output schema rejected: bounds_mismatch",
        _=>return None,
    })
}
fn check_status(response:&reqwest::Response) -> crate::Result<()> {
    if let Some(message)=contract_failure(response.status().as_u16(),response.headers().get("x-fosnie-output-contract").and_then(|v|v.to_str().ok())) {
        return Err(unavailable(message));
    }
    if !response.status().is_success() { return Err(unavailable("routed ML request rejected")); } Ok(())
}
async fn read_body(mut response:reqwest::Response) -> crate::Result<Vec<u8>> {
    check_status(&response)?;
    if response.content_length().is_some_and(|n|n>MAX_BODY as u64) { return Err(invalid("routed ML response too large")); }
    let mut body=Vec::new();
    while let Some(chunk)=response.chunk().await.map_err(|_|unavailable("routed ML response interrupted"))? {
        if body.len().saturating_add(chunk.len())>MAX_BODY { return Err(invalid("routed ML response too large")); }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
async fn read_json(response:reqwest::Response) -> crate::Result<Value> {
    serde_json::from_slice(&read_body(response).await?).map_err(|_|invalid("invalid routed ML response"))
}
async fn guarded<T>(cancel:&Notify,wait:std::time::Duration,future:impl std::future::Future<Output=crate::Result<T>>) -> crate::Result<T> {
    tokio::select! { biased;
        _=cancel.notified()=>Err(RoutingError::Cancelled.into()),
        result=tokio::time::timeout(wait,future)=>result.map_err(|_|unavailable("routed ML deadline exceeded"))?,
    }
}
fn invalid(message:&'static str) -> crate::AppError { RoutingError::Invalid(message).into() }
fn unavailable(message:&'static str) -> crate::AppError { crate::AppError::Unavailable(message.into()) }

#[cfg(test)]
mod contract_failure_tests {
    use super::*;
    #[test]
    fn safe_contract_reason_requires_422_and_a_known_category() {
        assert_eq!(contract_failure(422,Some("markdown_fence")),Some("routed output schema rejected: markdown_fence"));
        assert_eq!(contract_failure(200,Some("markdown_fence")),None);
        assert_eq!(contract_failure(502,Some("markdown_fence")),None);
        assert_eq!(contract_failure(422,None),None);
        assert_eq!(contract_failure(422,Some("model answer or credential")),None);
    }
}
