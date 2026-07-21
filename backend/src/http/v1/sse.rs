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

//! Turning a stream of [`Emit`]s into a server-sent-event response, and the
//! disconnect handling that goes with it.
//!
//! The interesting part is cancellation. There is no "client went away" signal
//! to poll for: the runtime simply drops the response future, and with it this
//! stream and everything it owns. So the cancellation handle is held by a value
//! *inside* the stream state, whose `Drop` fires it. Nothing has to notice the
//! disconnect; it is structural.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use serde_json::Value;
use tokio::sync::Notify;

use super::chat::{Aggregate, Emit, chunk};
use super::error::ApiError;

/// How often to send an SSE comment while a slow model is still thinking.
///
/// The WebSocket transport keeps its connections alive with protocol pings;
/// SSE has no equivalent, so an intermediary that times out an idle response
/// would kill a long prefill before the first token.
const KEEPALIVE_SECS: u64 = 15;

/// Owns whatever the turn needs kept alive, and cancels it when dropped.
pub struct TurnGuard {
    cancel: Arc<Notify>,
    finished: bool,
    /// Anything that must outlive the handler and die with the response: for a
    /// raw model call, the upstream stream itself (dropping it aborts the
    /// reader and so cancels the provider request).
    _keep: Option<Box<dyn Send>>,
}

impl TurnGuard {
    pub fn new(cancel: Arc<Notify>, keep: Option<Box<dyn Send>>) -> Self {
        Self { cancel, finished: false, _keep: keep }
    }

    /// Mark the turn as ended normally, so dropping the guard is a no-op.
    pub fn finish(&mut self) {
        self.finished = true;
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        if !self.finished {
            // `notify_one` rather than `notify_waiters`: it stores a permit when
            // nobody is parked yet, so a turn that reaches its next cancellation
            // point later still sees it. Best-effort by nature — one permit
            // wakes one waiter — which matches how an interrupted turn behaves
            // over the WebSocket transport.
            self.cancel.notify_one();
        }
    }
}

/// A producer of completion steps.
///
/// A trait rather than a closure because both renderings below need to borrow
/// the producer mutably across an await, which a closure returning a future
/// cannot express.
pub trait EmitSource: Send + 'static {
    fn next(&mut self) -> impl std::future::Future<Output = Option<Emit>> + Send;
}

/// State threaded through the stream: the guard plus the render context.
pub struct StreamState<S> {
    pub guard: TurnGuard,
    pub source: S,
    pub id: String,
    pub model: String,
    pub created: i64,
    /// Set once the terminal chunk has gone out; the next poll emits `[DONE]`.
    done_sent: bool,
    closed: bool,
}

impl<S> StreamState<S> {
    pub fn new(guard: TurnGuard, source: S, id: String, model: String, created: i64) -> Self {
        Self { guard, source, id, model, created, done_sent: false, closed: false }
    }
}

/// Render a source of [`Emit`]s as an SSE response.
///
/// `next` is polled until it yields `None`. A `Failed` emit is written as an
/// error event and ends the stream: once the response head has gone out there
/// is no status code left to return, so the failure has to ride the body in the
/// shape client SDKs already parse.
pub fn stream_response<S: EmitSource>(state: StreamState<S>) -> Response {
    Sse::new(events(state))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(KEEPALIVE_SECS)))
        .into_response()
}

fn events<S: EmitSource>(state: StreamState<S>) -> impl Stream<Item = Result<Event, Infallible>> {
    futures_util::stream::unfold(state, move |mut st| {
        async move {
            if st.closed {
                return None;
            }
            if st.done_sent {
                st.closed = true;
                // The literal sentinel every client watches for.
                return Some((Ok(Event::default().data("[DONE]")), st));
            }
            match st.source.next().await {
                Some(Emit::Failed(e)) => {
                    st.done_sent = true;
                    Some((Ok(e.sse_event()), st))
                }
                Some(emit) => {
                    if matches!(emit, Emit::Final { .. }) {
                        st.done_sent = true;
                        st.guard.finish();
                    }
                    let ev = Event::default()
                        .data(chunk(&st.id, &st.model, st.created, &emit).to_string());
                    Some((Ok(ev), st))
                }
                None => {
                    // The source ended without a terminal emit (the turn was cut
                    // short). Close the stream honestly rather than inventing a
                    // completion the model never produced.
                    st.done_sent = true;
                    st.guard.finish();
                    let emit = Emit::Final {
                        finish_reason: "stop".into(),
                        usage: Default::default(),
                        citations: None,
                    };
                    let ev = Event::default()
                        .data(chunk(&st.id, &st.model, st.created, &emit).to_string());
                    Some((Ok(ev), st))
                }
            }
        }
    })
}

/// Drain the same source into one response body.
///
/// Shares the source with the streaming path so the two can never disagree
/// about what an event means; only the rendering differs.
pub async fn collect<S: EmitSource>(state: &mut StreamState<S>) -> Result<Aggregate, ApiError> {
    let mut agg = Aggregate::default();
    while let Some(emit) = state.source.next().await {
        let terminal = matches!(emit, Emit::Final { .. } | Emit::Failed(_));
        agg.push(emit);
        if terminal {
            break;
        }
    }
    state.guard.finish();
    if let Some(e) = agg.failed.take() {
        return Err(e);
    }
    Ok(agg)
}

/// Citations as the non-standard field attached to a terminal chunk.
pub fn citations_value(items: &[Value]) -> Option<Value> {
    if items.is_empty() { None } else { Some(Value::Array(items.to_vec())) }
}
