// Copyright 2026 Private AI Ltd (SC881079)
// SPDX-License-Identifier: Apache-2.0
//! Safe diagnostic DTO; native bodies never become product error messages.
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use super::fallback::{FailureKind,OpenFailure};

#[derive(Debug,Clone,Deserialize,Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderFailure {
    pub schema_version:u8,
    pub kind:FailureKind,
    pub before_output:bool,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub retry_after_seconds:Option<u32>,
}
impl ProviderFailure {
    pub fn valid(&self)->bool {self.schema_version==1 && self.retry_after_seconds.is_none_or(|n|(1..=86400).contains(&n))
        && !matches!(self.kind,FailureKind::Cancelled|FailureKind::Budget|FailureKind::MidStream)}
    pub fn app(self)->crate::AppError {crate::AppError::Other(anyhow::Error::new(OpenFailure(self)))}
    pub fn from_error(error:&crate::AppError)->Option<Self> {
        match error {crate::AppError::Other(e)=>e.downcast_ref::<OpenFailure>().map(|e|e.0.clone()),_=>None}
    }
    pub fn code(&self)->&'static str {match self.kind {
        FailureKind::Connection=>"provider_connection",FailureKind::Timeout=>"provider_timeout",
        FailureKind::RateLimited=>"provider_rate_limited",FailureKind::QuotaExhausted=>"provider_quota_exhausted",
        FailureKind::PaymentRequired=>"provider_payment_required",FailureKind::Unauthorized=>"provider_credentials",
        FailureKind::InvalidRequest=>"provider_request_rejected",_=>"provider_unavailable"}}
    pub fn message(&self)->&'static str {match self.kind {
        FailureKind::QuotaExhausted=>"The provider reported an exhausted quota. Choose another permitted model or contact the provider manager.",
        FailureKind::PaymentRequired=>"The provider requires a billing update. Choose another permitted model or contact the provider manager.",
        FailureKind::Unauthorized=>"The provider rejected its credentials or access. Check your own key or contact the provider manager.",
        FailureKind::RateLimited=>"The provider is rate limited. Wait or choose another permitted model.",
        FailureKind::Timeout=>"The model did not respond within the allowed time.",
        FailureKind::InvalidRequest=>"The provider rejected this request. Check the model configuration and context limits.",
        _=>"The model is unavailable. Choose another permitted model or try again later."}}
    pub fn public(&self)->Value {json!({"schema_version":1,"code":self.code(),"source":"provider",
        "before_output":self.before_output,"retry_after_seconds":self.retry_after_seconds,
        "can_retry":self.before_output && matches!(self.kind,FailureKind::Connection|FailureKind::Timeout|FailureKind::Unavailable|FailureKind::RateLimited),
        "message":self.message()})}
}
pub fn public(error:&crate::AppError)->Value {
    if let Some(failure)=ProviderFailure::from_error(error) {return failure.public();}
    let (code,message)=match error {
        crate::AppError::Forbidden(_)|crate::AppError::Unauthorized(_)=>("access_denied","Access is unavailable. Check your permissions before retrying."),
        crate::AppError::Conflict(_)=>("configuration_changed","The routing configuration changed. Reload it before trying again."),
        crate::AppError::TooManyRequests(_)=>("run_budget","The routing allowance is exhausted. Review the run limits."),
        crate::AppError::Validation(_)=>("request_incompatible","This request has no compatible execution path. Review its model, context and capabilities."),
        crate::AppError::Unavailable(message) if message.starts_with("routed ML")=>("infrastructure_unavailable","The inference service is unavailable. Switching model cannot repair this service; try again later."),
        crate::AppError::Unavailable(message) if message=="routing request cancelled"=>("cancelled","Generation stopped."),
        crate::AppError::Unavailable(message) if message.starts_with("no eligible model:")=>("no_eligible_model","No model meets the current permissions, capabilities and limits."),
        _=>("routing_unavailable","Routing could not finish. Check the configuration or try again later."),
    };
    json!({"schema_version":1,"code":code,"source":"routing","before_output":false,"can_retry":false,"message":message})
}
pub async fn notify_chat(state:&crate::AppState,turn:uuid::Uuid,chat:Option<uuid::Uuid>,error:&crate::AppError,
    tx:&tokio::sync::mpsc::Sender<crate::ws::protocol::ServerFrame>,plain_text:bool) {
    let mut value=public(error);value["can_retry"]=serde_json::json!(plain_text && value["can_retry"]==true);
    if let Some(seconds)=value["retry_after_seconds"].as_u64() {
        value["retry_at_ms"]=serde_json::json!((time::OffsetDateTime::now_utc().unix_timestamp_nanos()/1_000_000) as u64+seconds*1000);
    }
    let id:Option<uuid::Uuid>=sqlx::query_scalar("UPDATE messages SET routing_failure=$3 WHERE turn_id=$1 AND chat_id=$2 AND role='assistant' AND routing_selection IS NOT NULL RETURNING id")
        .bind(turn).bind(chat).bind(&value).fetch_optional(&state.pg).await.ok().flatten();
    let _=tx.send(crate::ws::protocol::ServerFrame::ChatRoutingFailure {turn_id:turn,chat_id:chat,message_id:id,failure:value}).await;
}
