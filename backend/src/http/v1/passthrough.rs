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

//! Passthrough completions: the caller addresses a configured model directly.
//!
//! Stateless by design. The messages travel to the provider as sent, nothing is
//! recorded as a conversation, and the reply carries honest completion
//! semantics. What the platform adds is the wrapper: who may call, which
//! provider that label resolves to (including the caller's own key for it),
//! how often they may call, and a metered record of what it cost.

use std::sync::Arc;

use axum::Json;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tokio::sync::Notify;

use crate::audit::{self, AuditEvent};
use crate::auth::AuthContext;
use crate::ml::{self, GenEvent, GenStream};
use crate::state::AppState;

use super::chat::{ChatCompletionRequest, Emit, completion, completion_id, now_unix};
use super::error::ApiError;
use super::sse::{StreamState, TurnGuard, collect, stream_response};
use super::{models, sse};

/// Everything the event loop needs after the upstream call has been made.
pub struct Source {
    stream: GenStream,
    usage: ml::Usage,
    finish_reason: Option<String>,
    /// Set once a terminal event has been seen, so the loop stops.
    ended: bool,
    meter: Meter,
}

/// What gets written to the audit trail when the completion finishes.
struct Meter {
    state: AppState,
    ctx: AuthContext,
    model_label: String,
    recorded: bool,
}

impl Meter {
    async fn record(&mut self, usage: &ml::Usage) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        let mut event = AuditEvent::action("api.completion.finished", self.ctx.role.as_str());
        event.actor_user_id = self.ctx.user_id;
        // A passthrough completion has no conversation to point at, which is
        // exactly why it is a distinct action rather than a reuse of the chat
        // one: an audit row should not claim a resource that does not exist.
        event.resource_type = Some("api_completion".into());
        event.model_agent_traceability =
            Some(json!({ "model": self.model_label, "agent_id": serde_json::Value::Null }));
        // Same field names as a chat turn, so one rollup reads both.
        event.token_usage = Some(json!({
            "prompt_tokens": usage.prompt_tokens,
            "completion_tokens": usage.completion_tokens,
        }));
        // Durable rather than queued: the usage rollups read this row, so it
        // must not be droppable under load.
        if let Err(e) = audit::append(&self.state.pg, &event).await {
            tracing::error!(error = %e, "failed to record API completion usage");
        }
        metrics::counter!("api_completions_total", "model" => self.model_label.clone())
            .increment(1);
    }
}

pub async fn run(
    state: &AppState,
    ctx: &AuthContext,
    req: ChatCompletionRequest,
) -> Result<Response, ApiError> {
    let uid = models::user_id(ctx)?;
    let provider_id = models::resolve_label(state, uid, &req.model).await?;

    // Resolve through the platform's own precedence so a user's own key for
    // this provider is honoured exactly as it would be in the application.
    let selected =
        crate::providers::resolve_llm(&state.pg, state.message_key, Some(uid), None, Some(provider_id))
            .await
            .map_err(ApiError::from)?;
    let overrides = ml::provider_overrides_with_llm(state, Some(uid), selected.as_ref()).await;

    let gen = ml::GenerateRequest {
        messages: req.ml_messages(),
        sampling: req.sampling(),
        model: None,
        tools: None,
        overrides,
    };
    let stream = ml::generate(&state.http, &state.boot.ml.base_url, &gen)
        .await
        .map_err(ApiError::from)?;

    let source = Source {
        stream,
        usage: ml::Usage::default(),
        finish_reason: None,
        ended: false,
        meter: Meter {
            state: state.clone(),
            ctx: ctx.clone(),
            model_label: req.model.clone(),
            recorded: false,
        },
    };

    // Nothing here parks on the cancel handle: dropping the upstream stream is
    // what stops the provider request. The guard still owns one so the shape
    // matches the agent path and a future waiter is not forgotten.
    let guard = TurnGuard::new(Arc::new(Notify::new()), None);
    let id = completion_id();
    let created = now_unix();
    let mut st = StreamState::new(guard, source, id.clone(), req.model.clone(), created);

    if req.stream {
        Ok(stream_response(st))
    } else {
        let agg = collect(&mut st).await?;
        Ok(Json(completion(&id, &req.model, created, &agg)).into_response())
    }
}

impl super::sse::EmitSource for Source {
    async fn next(&mut self) -> Option<Emit> {
        next_emit(self).await
    }
}

/// One step of the upstream stream, translated.
async fn next_emit(src: &mut Source) -> Option<Emit> {
    if src.ended {
        return None;
    }
    loop {
        match src.stream.recv().await {
            Some(GenEvent::Token { delta }) => {
                return Some(Emit::Delta { content: delta, reasoning: String::new() });
            }
            Some(GenEvent::Reasoning { delta }) => {
                return Some(Emit::Delta { content: String::new(), reasoning: delta });
            }
            Some(GenEvent::ToolCall { name, .. }) => {
                // Unreachable: a request carrying tools is refused before the
                // upstream call. A provider that volunteers one anyway has
                // nothing to call it with, so it is dropped rather than
                // half-rendered.
                tracing::warn!(tool = %name, "provider emitted a tool call on the passthrough surface");
            }
            Some(GenEvent::Done { finish_reason, usage, .. }) => {
                src.ended = true;
                src.usage = usage;
                src.finish_reason = finish_reason;
                src.meter.record(&src.usage).await;
                return Some(Emit::Final {
                    finish_reason: src.finish_reason.clone().unwrap_or_else(|| "stop".into()),
                    usage: src.usage.clone(),
                    citations: None,
                });
            }
            Some(GenEvent::Error { message }) => {
                src.ended = true;
                // Whatever tokens were produced before the failure still cost
                // something, so the meter is written either way.
                src.meter.record(&src.usage).await;
                return Some(Emit::Failed(ApiError::unavailable(message)));
            }
            None => {
                src.ended = true;
                src.meter.record(&src.usage).await;
                return Some(Emit::Final {
                    finish_reason: "stop".into(),
                    usage: src.usage.clone(),
                    citations: sse::citations_value(&[]),
                });
            }
        }
    }
}
