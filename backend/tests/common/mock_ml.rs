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

//! An in-process stand-in for the ML service, with a call counter per route.
//!
//! Some behaviours are only observable as an ML call that did or did not happen, or
//! as the arguments it was made with. "This turn did not retrieve" and "both paths
//! searched the same scope" are both invisible in the frames a turn emits. So the
//! mock answers the handful of endpoints a chat turn touches, counts every request,
//! and journals every request body.
//!
//! The assertions that matter run against those **captured inputs** — what the code
//! under test sent. Response bodies are fixed canned values rather than computed
//! from the request, deliberately: a mock that derived its answers could be made to
//! agree with a wrong implementation, whereas a recorded input cannot. Tests assert
//! on what the turn sent and on what it then did with the answer, never on the mock
//! being clever.

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use serde_json::{json, Value};

/// How many times each endpoint was called.
#[derive(Default)]
pub struct MlCalls {
    pub retrieve: AtomicUsize,
    pub generate: AtomicUsize,
    pub embed: AtomicUsize,
    pub model_info: AtomicUsize,
    pub memory_search: AtomicUsize,
    pub chat_step: AtomicUsize,
    /// `/retrieve` calls currently being served. A search whose caller went away
    /// takes its handler down with it, so this returning to zero is how a test sees
    /// that an abort really reached the upstream request rather than merely
    /// stopping someone from waiting on it.
    pub retrieve_active: AtomicUsize,
    pub other: AtomicUsize,
    /// The tool names offered to the model on each `/generate`, in call order.
    /// Which tools a turn put in front of the model is not visible in any frame it
    /// emits, and it is exactly what some gating rules are about.
    pub offered_tools: Mutex<Vec<Vec<String>>>,
    /// Every `/retrieve` request body, in call order.
    pub retrieve_bodies: Mutex<Vec<Value>>,
    /// Every `/generate` request body, in call order — the composed system prompt
    /// travels in its messages.
    pub generate_bodies: Mutex<Vec<Value>>,
}

/// One captured `/retrieve` call: what was searched for, and within what scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrieveArgs {
    pub query: String,
    /// Sorted, so an assertion compares scope rather than iteration order.
    pub kb_ids: Vec<String>,
    /// Sorted, likewise.
    pub deny_doc_ids: Vec<String>,
}

fn strings_at(body: &Value, key: &str) -> Vec<String> {
    let mut v: Vec<String> = body
        .get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    v.sort();
    v
}

impl MlCalls {
    pub fn retrieves(&self) -> usize {
        self.retrieve.load(Ordering::SeqCst)
    }
    pub fn generates(&self) -> usize {
        self.generate.load(Ordering::SeqCst)
    }
    pub fn retrieves_in_flight(&self) -> usize {
        self.retrieve_active.load(Ordering::SeqCst)
    }
    /// Whether `name` was offered on any `/generate` this turn made.
    pub fn was_offered(&self, name: &str) -> bool {
        self.offered_tools.lock().unwrap().iter().any(|set| set.iter().any(|t| t == name))
    }
    pub fn offered(&self) -> Vec<Vec<String>> {
        self.offered_tools.lock().unwrap().clone()
    }

    /// The captured `/retrieve` calls, in order.
    pub fn retrieve_args(&self) -> Vec<RetrieveArgs> {
        self.retrieve_bodies
            .lock()
            .unwrap()
            .iter()
            .map(|b| RetrieveArgs {
                query: b.get("prompt").and_then(Value::as_str).unwrap_or_default().to_string(),
                kb_ids: strings_at(b, "kb_ids"),
                deny_doc_ids: strings_at(b, "deny_doc_ids"),
            })
            .collect()
    }

    /// Every system prompt sent to `/generate`, in call order.
    ///
    /// A turn makes more than one generation call — naming a new chat is one of
    /// them — so callers should pick the ones they mean by content rather than by
    /// position, which shifts as unrelated behaviour changes.
    pub fn system_prompts(&self) -> Vec<String> {
        self.generate_bodies
            .lock()
            .unwrap()
            .iter()
            .filter_map(|b| {
                let msgs = b.get("messages")?.as_array()?;
                let sys = msgs.iter().find(|m| m.get("role").and_then(Value::as_str) == Some("system"))?;
                Some(sys.get("content")?.as_str()?.to_string())
            })
            .collect()
    }

    /// The system prompts that contain `marker`, in call order.
    pub fn system_prompts_containing(&self, marker: &str) -> Vec<String> {
        self.system_prompts().into_iter().filter(|p| p.contains(marker)).collect()
    }
}

/// What the mock answers with. Set before a test drives a turn.
pub struct MlScript {
    /// The retrieval context handed back on `/retrieve`.
    pub retrieve_context: String,
    /// Quote text of the single citation returned with it.
    pub retrieve_quote: String,
    /// Gap diagnostics returned with it.
    pub retrieve_debug: Value,
    /// Tokens streamed from `/generate`, in order.
    pub generate_tokens: Vec<String>,
    /// When set, the FIRST tool-loop step asks to call this tool instead of
    /// answering; later steps answer normally. Lets a test drive a tool through real
    /// dispatch rather than assert on the request the model was sent.
    pub generate_tool_call: Option<(String, Value)>,
    /// Hold `/retrieve` open until the test releases it. Lets a test put a search
    /// reliably in the "still running" state and act on it, instead of racing a
    /// sleep against a search that may already have finished.
    pub retrieve_latch: Option<Arc<Notify>>,
}

impl Default for MlScript {
    fn default() -> Self {
        Self {
            retrieve_context: "[D1] Retrieved by the turn itself.".into(),
            retrieve_quote: "retrieved by the turn itself".into(),
            retrieve_debug: json!({ "gap_needs_exhausted": 0, "gap_stop_reason": "sufficient", "gap_unresolved": [] }),
            generate_tokens: vec!["Answer.".into()],
            generate_tool_call: None,
            retrieve_latch: None,
        }
    }
}

pub struct MockMl {
    pub base_url: String,
    pub calls: Arc<MlCalls>,
}

struct Inner {
    calls: Arc<MlCalls>,
    script: Arc<MlScript>,
}

impl Clone for Inner {
    fn clone(&self) -> Self {
        Self { calls: self.calls.clone(), script: self.script.clone() }
    }
}

/// Start the mock on an ephemeral port and return its base URL plus the counters.
pub async fn spawn(script: MlScript) -> MockMl {
    let calls = Arc::new(MlCalls::default());
    let inner = Inner { calls: calls.clone(), script: Arc::new(script) };

    let app = axum::Router::new()
        .route("/retrieve", post(retrieve))
        .route("/generate", post(generate))
        .route("/chat-step", post(chat_step))
        .route("/embed", post(embed))
        .route("/model-info", get(model_info))
        .route("/memory/search", post(memory_search))
        .fallback(fallback)
        .with_state(inner);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    MockMl { base_url: format!("http://127.0.0.1:{port}"), calls }
}

/// Newline-delimited JSON, the shape the retrieval and generation streams use.
fn ndjson(lines: Vec<Value>) -> Response {
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    ([(axum::http::header::CONTENT_TYPE, "application/x-ndjson")], body).into_response()
}

async fn retrieve(State(s): State<Inner>, Json(body): Json<Value>) -> Response {
    s.calls.retrieve.fetch_add(1, Ordering::SeqCst);
    s.calls.retrieve_bodies.lock().unwrap().push(body);

    // Decrements however this handler ends, including when the server drops it
    // because the caller disconnected.
    struct Active(Arc<MlCalls>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.retrieve_active.fetch_sub(1, Ordering::SeqCst);
        }
    }
    s.calls.retrieve_active.fetch_add(1, Ordering::SeqCst);
    let _active = Active(s.calls.clone());

    if let Some(latch) = &s.script.retrieve_latch {
        latch.notified().await;
    }
    ndjson(vec![
        json!({ "type": "progress", "stage": "searching", "detail": "searching" }),
        json!({
            "type": "done",
            "context": s.script.retrieve_context,
            "citations": [{ "quote_text": s.script.retrieve_quote }],
            "parts": [],
            "debug": s.script.retrieve_debug,
        }),
    ])
}

/// The tool names a request put in front of the model.
fn offered_names(body: &Value) -> Vec<String> {
    body.get("tools")
        .and_then(Value::as_array)
        .map(|ts| {
            ts.iter()
                .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The tool-decision step: a non-streamed round where the model either calls tools
/// or declines to. This is where a turn's tool set is actually offered.
async fn chat_step(State(s): State<Inner>, Json(body): Json<Value>) -> Json<Value> {
    let nth = s.calls.chat_step.fetch_add(1, Ordering::SeqCst);
    s.calls.offered_tools.lock().unwrap().push(offered_names(&body));
    if nth == 0 {
        if let Some((name, args)) = &s.script.generate_tool_call {
            return Json(json!({
                "content": "",
                "tool_calls": [{ "id": "call_1", "name": name, "arguments": args }],
                "finish_reason": "tool_calls",
            }));
        }
    }
    Json(json!({ "content": "", "tool_calls": [], "finish_reason": "stop" }))
}

async fn generate(State(s): State<Inner>, Json(body): Json<Value>) -> Response {
    s.calls.generate.fetch_add(1, Ordering::SeqCst);
    s.calls.offered_tools.lock().unwrap().push(offered_names(&body));
    s.calls.generate_bodies.lock().unwrap().push(body);
    let mut lines: Vec<Value> =
        s.script.generate_tokens.iter().map(|t| json!({ "type": "token", "delta": t })).collect();
    lines.push(json!({ "type": "done", "finish_reason": "stop" }));
    ndjson(lines)
}

async fn embed(State(s): State<Inner>, _body: Json<Value>) -> Json<Value> {
    s.calls.embed.fetch_add(1, Ordering::SeqCst);
    Json(json!({ "embeddings": [[0.0, 0.0, 0.0, 0.0]] }))
}

async fn model_info(State(s): State<Inner>) -> Json<Value> {
    s.calls.model_info.fetch_add(1, Ordering::SeqCst);
    Json(json!({ "max_model_len": 32768 }))
}

async fn memory_search(State(s): State<Inner>, _body: Json<Value>) -> Json<Value> {
    s.calls.memory_search.fetch_add(1, Ordering::SeqCst);
    Json(json!({ "results": [] }))
}

async fn fallback(State(s): State<Inner>) -> Json<Value> {
    s.calls.other.fetch_add(1, Ordering::SeqCst);
    Json(json!({}))
}
