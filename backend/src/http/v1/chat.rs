// Copyright 2026 Private AI Ltd (SC881079)
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `POST /v1/chat/completions` — the request shape, its validation, and the
//! response vocabulary both modes render into.
//!
//! The two modes produce very different things internally (a raw model stream
//! versus a full pipeline turn) but must emit byte-identical envelopes, and the
//! streaming and non-streaming forms of each must agree. So both modes are
//! written against one small vocabulary, [`Emit`], which is rendered either as
//! stream chunks or folded into a single response. Adding a field means
//! touching one translation, never four.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::api_key::ApiKeyAuth;
use crate::ml;
use crate::state::AppState;

use super::error::ApiError;
use super::models::AGENT_PREFIX;

/// Header carrying the conversation a request belongs to, and the one the
/// response was recorded under. Sending back what a previous response returned
/// continues that conversation; omitting it starts a new one.
pub const CHAT_ID_HEADER: &str = "x-fosnie-chat-id";

#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    #[serde(default)]
    pub messages: Vec<ReqMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<i32>,
    /// Newer clients send this instead of `max_tokens`; treated as the same cap.
    #[serde(default)]
    pub max_completion_tokens: Option<i32>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub stop: Option<Value>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    /// Accepted and ignored: an end-user identifier for the caller's own
    /// analytics. The key already identifies the account for ours.
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReqMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

impl ReqMessage {
    /// The message as plain text. OpenAI allows `content` to be either a string
    /// or an array of typed parts; only the text parts mean anything here.
    pub fn text(&self) -> String {
        match &self.content {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    }
}

impl ChatCompletionRequest {
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.messages.is_empty() {
            return Err(ApiError::bad_request("'messages' must contain at least one message"));
        }
        // `n` is routinely sent as an explicit 1, and sometimes as null, so only
        // an actual request for several completions is refused.
        if let Some(n) = self.n {
            if n != 1 {
                return Err(ApiError::bad_request(
                    "only one completion per request is supported (n must be 1)",
                ));
            }
        }
        // Client-side tools are refused rather than silently dropped. In
        // passthrough they would proxy arbitrary tool traffic around the
        // platform's governance; in agent mode the tools belong to the agent and
        // a caller's schema has no standing. Either way, quietly ignoring them
        // would leave a client waiting for calls that never come.
        let has_tools = self.tools.as_ref().is_some_and(|t| !t.is_empty())
            || self.messages.iter().any(|m| m.tool_calls.as_ref().is_some_and(|t| !t.is_empty()));
        if has_tools {
            return Err(ApiError::bad_request(
                "tools are not supported in passthrough mode; use an agent model",
            ));
        }
        Ok(())
    }

    pub fn sampling(&self) -> ml::Sampling {
        ml::Sampling {
            temperature: self.temperature,
            top_p: self.top_p,
            max_tokens: self.max_tokens.or(self.max_completion_tokens),
            frequency_penalty: self.frequency_penalty,
            presence_penalty: self.presence_penalty,
            reasoning_effort: None,
        }
    }

    /// The messages as the generation service takes them: role plus text, with
    /// the tool-carrying fields already refused by validation.
    pub fn ml_messages(&self) -> Vec<ml::Message> {
        self.messages
            .iter()
            .map(|m| json!({ "role": m.role, "content": m.text() }))
            .collect()
    }

    /// The final user message: what the agent pipeline treats as the question.
    pub fn last_user_text(&self) -> Option<String> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.text())
            .filter(|t| !t.trim().is_empty())
    }
}

/// One step of a completion, mode-independent.
#[derive(Debug, Clone)]
pub enum Emit {
    /// Answer text, reasoning text, or both.
    Delta { content: String, reasoning: String },
    /// The turn ended normally.
    Final {
        finish_reason: String,
        usage: ml::Usage,
        citations: Option<Value>,
    },
    /// The turn failed. Terminal.
    Failed(ApiError),
}

/// Accumulates [`Emit`]s into a single response body for a non-streaming call.
#[derive(Default)]
pub struct Aggregate {
    pub content: String,
    pub reasoning: String,
    pub finish_reason: Option<String>,
    pub usage: ml::Usage,
    pub citations: Option<Value>,
    pub failed: Option<ApiError>,
}

impl Aggregate {
    pub fn push(&mut self, emit: Emit) {
        match emit {
            Emit::Delta { content, reasoning } => {
                self.content.push_str(&content);
                self.reasoning.push_str(&reasoning);
            }
            Emit::Final { finish_reason, usage, citations } => {
                self.finish_reason = Some(finish_reason);
                self.usage = usage;
                self.citations = citations;
            }
            Emit::Failed(e) => self.failed = Some(e),
        }
    }
}

/// The `chat.completion.chunk` an [`Emit`] renders to mid-stream.
pub fn chunk(id: &str, model: &str, created: i64, emit: &Emit) -> Value {
    let mut choice = json!({ "index": 0, "delta": {}, "finish_reason": Value::Null });
    let mut extra: Option<(&str, Value)> = None;
    let mut usage = Value::Null;

    match emit {
        Emit::Delta { content, reasoning } => {
            let d = choice["delta"].as_object_mut().expect("delta object");
            if !content.is_empty() {
                d.insert("content".into(), json!(content));
            }
            if !reasoning.is_empty() {
                // The field name several providers converged on for a visible
                // reasoning channel. Clients that do not know it ignore it.
                d.insert("reasoning_content".into(), json!(reasoning));
            }
        }
        Emit::Final { finish_reason, usage: u, citations } => {
            choice["finish_reason"] = json!(finish_reason);
            usage = usage_json(u);
            if let Some(c) = citations {
                extra = Some(("citations", c.clone()));
            }
        }
        Emit::Failed(_) => {}
    }

    let mut out = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [choice],
    });
    if !usage.is_null() {
        out["usage"] = usage;
    }
    if let Some((k, v)) = extra {
        out[k] = v;
    }
    out
}

/// The single `chat.completion` a non-streaming call returns.
pub fn completion(id: &str, model: &str, created: i64, agg: &Aggregate) -> Value {
    let mut message = json!({ "role": "assistant", "content": agg.content });
    if !agg.reasoning.is_empty() {
        message["reasoning_content"] = json!(agg.reasoning);
    }
    let mut out = json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": agg.finish_reason.clone().unwrap_or_else(|| "stop".into()),
        }],
        "usage": usage_json(&agg.usage),
    });
    if let Some(c) = &agg.citations {
        out["citations"] = c.clone();
    }
    out
}

fn usage_json(u: &ml::Usage) -> Value {
    let prompt = u.prompt_tokens.unwrap_or(0);
    let completion = u.completion_tokens.unwrap_or(0);
    let mut out = json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
    });
    if let Some(r) = u.reasoning_tokens {
        out["completion_tokens_details"] = json!({ "reasoning_tokens": r });
    }
    out
}

/// A completion id in the shape clients expect to be able to log and correlate.
pub fn completion_id() -> String {
    format!("chatcmpl-{}", Uuid::now_v7().simple())
}

pub fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The one entry point; the `model` field decides which pipeline runs.
pub async fn completions(
    State(state): State<AppState>,
    ApiKeyAuth(ctx, key_id): ApiKeyAuth,
    headers: HeaderMap,
    axum::Json(req): axum::Json<ChatCompletionRequest>,
) -> Result<Response, ApiError> {
    super::require_enabled_for(&state, &ctx).await?;
    super::rate_limit(&state, key_id).await?;
    req.validate()?;

    // Tested before the provider labels: the prefix is what selects the agent
    // pipeline, so it wins any collision with a label that looks like one.
    if let Some(rest) = req.model.strip_prefix(AGENT_PREFIX) {
        let agent_id = Uuid::parse_str(rest.trim())
            .map_err(|_| ApiError::model_not_found(&req.model))?;
        return super::agent::run(&state, &ctx, req, agent_id, &headers).await;
    }
    super::passthrough::run(&state, &ctx, req).await
}
