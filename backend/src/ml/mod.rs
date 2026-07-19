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

//! Client to the Python ML/RAG service (the platform's LLM client). Rust never
//! calls the LLM directly (topology); generation streams through here.
//!
//! [`generate`] posts the composed messages and returns a [`GenStream`] of
//! NDJSON events. Dropping the stream aborts the reader task, which drops the
//! upstream HTTP response — cancelling the LLM generation and freeing its slot
//! (chat-turn cancel, not pause).

use anyhow::anyhow;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::AppError;

/// Per-request provider-override map (`{role}_base_url/_model/_api_key`, names per
/// `ml/app/config.py`). Empty ⇒ omitted from the body ⇒ the ML service keeps its
/// own `.env` default for that role.
pub type ProviderOverrides = serde_json::Map<String, serde_json::Value>;

/// Skip serialising an empty override map (keeps the wire identical when no
/// provider rows are configured).
fn omap_is_empty(m: &ProviderOverrides) -> bool {
    m.is_empty()
}

/// Resolve the provider-override map for a request via the [`ProviderRegistry`]
/// slot (`state.providers`). One entry per configured + enabled role; empty when
/// the registry returns nothing for every role (⇒ ML uses its `.env` defaults).
/// `user_id` is threaded for BYOK (4b); 4a has no user rows so it resolves the
/// deployment row or `None`.
pub async fn provider_overrides(
    state: &crate::state::AppState,
    user_id: Option<uuid::Uuid>,
) -> ProviderOverrides {
    provider_overrides_with_llm(state, user_id, None).await
}

/// As [`provider_overrides`], but the `llm` slot is taken from a caller-supplied
/// pre-resolved provider (the per-turn/per-chat pick, via
/// [`crate::providers::resolve_llm`]) instead of the single-row registry
/// `resolve`. `None` ⇒ identical to `provider_overrides` (llm resolved the default
/// way). Multiple named LLM providers live only here on the llm role; every other
/// role still resolves single-row through the registry.
pub async fn provider_overrides_with_llm(
    state: &crate::state::AppState,
    user_id: Option<uuid::Uuid>,
    llm_override: Option<&crate::ext::ResolvedProvider>,
) -> ProviderOverrides {
    let mut map = ProviderOverrides::new();
    for role in crate::providers::ROLES {
        // The llm role can be a per-turn selected provider (multi-LLM); when the
        // caller pre-resolved it, use that instead of the default single-row resolve.
        let resolved = if role == "llm" {
            match llm_override {
                Some(p) => Some(p.clone()),
                None => state.providers.resolve(&state.pg, role, user_id).await.ok().flatten(),
            }
        } else {
            state.providers.resolve(&state.pg, role, user_id).await.ok().flatten()
        };
        let Some(p) = resolved else {
            continue;
        };
        if !p.enabled {
            continue;
        }
        if let Some(v) = p.base_url {
            map.insert(format!("{role}_base_url"), v.into());
        }
        if let Some(v) = p.model {
            map.insert(format!("{role}_model"), v.into());
        }
        if let Some(v) = p.api_key {
            map.insert(format!("{role}_api_key"), v.into());
        }
    }
    // Bind the embed role to the ACTIVE index's model (provenance), so query + ingest
    // always embed consistently with the live vectors — independent of the "desired"
    // `provider_configs` embed row a pending migration carries, and ignoring any
    // per-user embed override (embed is deployment-wide). No provenance yet ⇒ leave
    // the resolved embed override as-is (un-seeded deploys behave exactly as before).
    if let Ok(Some(active)) = crate::embedding_index::active(&state.pg, state.message_key).await {
        map.insert("embed_model".into(), active.model.into());
        match active.base_url {
            Some(u) => { map.insert("embed_base_url".into(), u.into()); }
            None => { map.remove("embed_base_url"); }
        }
        match active.api_key {
            Some(k) => { map.insert("embed_api_key".into(), k.into()); }
            None => { map.remove("embed_api_key"); }
        }
    }
    map
}

/// Augment a provider-override map with the per-turn reasoning request, in the
/// unified shape every ML path understands:
/// - `llm_thinking` (`adaptive:<level>` / `adaptive` / `off`) — read by the native
///   Anthropic adapter, a no-op elsewhere (kept for back-compat).
/// - `llm_reasoning_enabled` / `llm_reasoning_level` / `llm_reasoning_trace` — read
///   by the OpenAI / Gemini / local translators in `llm.py` (+ the Gemini adapter).
///
/// No-op when `spec` is None (unattended/scheduler/workflow/voice turns keep the
/// provider defaults). The ML layer host-gates the final wire parameter, so an
/// override here can never push an invalid param onto a model that rejects it.
pub fn with_reasoning(
    mut map: ProviderOverrides,
    spec: Option<&crate::reasoning::ReasoningSpec>,
) -> ProviderOverrides {
    let Some(spec) = spec else { return map };
    map.insert("llm_reasoning_trace".into(), if spec.return_trace { "true" } else { "false" }.into());
    if spec.enabled {
        let level = spec.level.as_deref().unwrap_or("auto");
        map.insert("llm_reasoning_enabled".into(), "true".into());
        map.insert("llm_reasoning_level".into(), level.into());
        // Anthropic: `adaptive` (dynamic) when level is auto, else `adaptive:<level>`.
        let thinking = if level == "auto" { "adaptive".to_string() } else { format!("adaptive:{level}") };
        map.insert("llm_thinking".into(), thinking.into());
    } else {
        map.insert("llm_reasoning_enabled".into(), "false".into());
        map.insert("llm_thinking".into(), "off".into());
    }
    map
}

/// RAII latency timer for a backend→ML round-trip: records
/// `ml_request_duration_seconds{op}` on drop (covers success and error paths).
struct MlTimer {
    op: &'static str,
    start: std::time::Instant,
}

impl MlTimer {
    fn new(op: &'static str) -> Self {
        Self { op, start: std::time::Instant::now() }
    }
}

impl Drop for MlTimer {
    fn drop(&mut self) {
        metrics::histogram!("ml_request_duration_seconds", "op" => self.op)
            .record(self.start.elapsed().as_secs_f64());
    }
}

/// Chat messages are OpenAI-shape JSON objects ({role, content, [tool_calls],
/// [tool_call_id]}) so the tool-call loop can carry assistant-tool-call and
/// tool-result messages, not just {role, content}.
pub type Message = serde_json::Value;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Sampling {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    /// Reasoning-effort hint for thinking-capable providers (OpenAI gpt-5.x/o-series,
    /// Gemini 2.5). `None` for normal chat (wire unchanged); set to `"minimal"` for
    /// utility calls like chat-title naming so reasoning models stay fast and don't
    /// burn the whole token budget thinking. ML clamps it to each provider's floor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerateRequest {
    pub messages: Vec<Message>,
    pub sampling: Sampling,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tool schemas the answering model may call while streaming (currently only the
    /// library top-up). Omitted ⇒ a plain answer stream. Serialized only when present
    /// so a turn that advertises no tools stays byte-identical on the wire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    /// Provider overrides. Empty ⇒ omitted ⇒ ML uses its defaults.
    #[serde(default, skip_serializing_if = "omap_is_empty")]
    pub overrides: ProviderOverrides,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Usage {
    pub prompt_tokens: Option<i32>,
    pub completion_tokens: Option<i32>,
    /// Hidden reasoning tokens, normalised across providers (OpenAI
    /// `reasoning_tokens`, Anthropic `thinking_tokens`, Gemini `thoughtsTokenCount`).
    /// Billed even when the trace is hidden, so it is surfaced to the SPA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i32>,
}

/// One event from the generation stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GenEvent {
    Token {
        delta: String,
    },
    /// A reasoning-trace delta on the dedicated channel — routed to the SPA's
    /// reasoning panel, kept out of the answer.
    Reasoning {
        delta: String,
    },
    /// A tool call the model made mid-answer. Emitted only once the arguments have
    /// fully accumulated + parsed (never partial), followed by a `Done` carrying
    /// `finish_reason:"tool_calls"`. The caller runs the tool and continues the answer.
    ToolCall {
        #[serde(default)]
        id: Option<String>,
        name: String,
        #[serde(default)]
        arguments: serde_json::Value,
    },
    Done {
        #[serde(default)]
        finish_reason: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        usage: Usage,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub model_id: String,
    pub max_model_len: i64,
}

/// A live generation stream. Recv events via [`GenStream::recv`]; drop to cancel.
pub struct GenStream {
    rx: mpsc::Receiver<GenEvent>,
    reader: JoinHandle<()>,
}

impl GenStream {
    pub async fn recv(&mut self) -> Option<GenEvent> {
        self.rx.recv().await
    }
}

impl Drop for GenStream {
    fn drop(&mut self) {
        // Abort the reader → drops the upstream response → cancels the LLM.
        self.reader.abort();
    }
}

/// Start a streaming generation. Errors surface only on connect/non-200;
/// per-token errors arrive as [`GenEvent::Error`].
pub async fn generate(
    http: &reqwest::Client,
    base_url: &str,
    req: &GenerateRequest,
) -> Result<GenStream, AppError> {
    let url = format!("{}/generate", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(req)
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /generate connect: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!(
            "ml /generate returned {}",
            resp.status()
        )));
    }

    let (tx, rx) = mpsc::channel::<GenEvent>(64);
    let reader = tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            buf.extend_from_slice(&bytes);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = &line[..line.len() - 1];
                if line.is_empty() {
                    continue;
                }
                if let Ok(ev) = serde_json::from_slice::<GenEvent>(line) {
                    if tx.send(ev).await.is_err() {
                        return; // receiver dropped → stop reading, drop response
                    }
                }
            }
        }
        if !buf.is_empty() {
            if let Ok(ev) = serde_json::from_slice::<GenEvent>(&buf) {
                let _ = tx.send(ev).await;
            }
        }
    });

    Ok(GenStream { rx, reader })
}

// --- RAG -------------------------------------------------------------------

use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
pub struct Citation {
    pub doc_id: Option<Uuid>,
    pub chunk_index: Option<i32>,
    pub page_number: Option<i32>,
    pub clause_section_ref: Option<String>,
    pub quote_text: String,
}

/// One per-part synthesis slice: the part's own sub-answer scaffold +
/// only its [D#] blocks (turn-global indices), for the `per_part` synthesis mode. Empty list
/// on a non-numbered prompt → the backend uses unified synthesis.
#[derive(Debug, Clone, Deserialize)]
pub struct SynthPart {
    pub title: String,
    pub context: String,
    #[serde(default)]
    pub has_evidence: bool,
}

#[derive(Serialize)]
struct RetrieveRequest<'a> {
    prompt: &'a str,
    kb_ids: &'a [String],
    /// Source-ACL deny-list: `kb_documents.id`s to exclude
    /// from retrieval via a Qdrant `must_not doc_id`. Omitted from the wire when
    /// empty so a request with no denials serialises byte-identically to before the
    /// feature (ML defaults it to `[]`).
    #[serde(skip_serializing_if = "deny_is_empty")]
    deny_doc_ids: &'a [String],
    /// Stream NDJSON progress (A4) instead of one JSON dict. Always true here —
    /// chat is the sole caller and it surfaces live retrieval activity.
    stream: bool,
    #[serde(flatten)]
    overrides: &'a RagOverrides,
    #[serde(rename = "overrides", skip_serializing_if = "omap_is_empty")]
    provider_overrides: ProviderOverrides,
}

/// serde `skip_serializing_if` predicate for the borrowed `deny_doc_ids` slice.
fn deny_is_empty(v: &&[String]) -> bool {
    v.is_empty()
}

/// One NDJSON event from the streaming retrieval (A4, mirrors [`WebEvent`]):
/// progress lines while the agentic loop runs, then exactly one `Done` (or
/// `Error`). The whole loop stays server-side in Python — invisible here.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RetrieveEvent {
    Progress {
        stage: String,
        #[serde(default)]
        detail: Option<String>,
    },
    Done {
        context: String,
        citations: Vec<Citation>,
        /// Per-part synthesis slices; omitted/empty on non-numbered prompts.
        #[serde(default)]
        parts: Vec<SynthPart>,
        /// Retrieval telemetry — only the fields the backend acts on are typed; the
        /// rest of the Python `debug` dict is ignored (serde drops unknown keys).
        /// Decides whether to offer the model the `search_library` top-up tool.
        #[serde(default)]
        debug: RetrieveDebug,
    },
    Error { message: String },
}

/// The subset of the ML `debug` object the backend reads. `gap_needs_exhausted` /
/// `gap_stop_reason` say whether the iterative first pass gave up; `gap_unresolved`
/// lists the needs it could not satisfy (surfaced to the model as known gaps).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RetrieveDebug {
    #[serde(default)]
    pub gap_needs_exhausted: i64,
    #[serde(default)]
    pub gap_stop_reason: String,
    #[serde(default)]
    pub gap_unresolved: Vec<String>,
}

/// Live event stream for a retrieval. Dropping it aborts the reader (which
/// cancels the upstream HTTP body — same mechanics as `WebStream`/`GenStream`).
pub struct RetrieveStream {
    rx: mpsc::Receiver<RetrieveEvent>,
    reader: JoinHandle<()>,
}

impl RetrieveStream {
    pub async fn recv(&mut self) -> Option<RetrieveEvent> {
        self.rx.recv().await
    }
}

impl Drop for RetrieveStream {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Optional per-request RAG knobs sourced from the runtime config store (tweaked
/// in the super-admin panel). An absent field → the ML service uses its own
/// configured default. Flattened into the `/retrieve` body, keeping the clean
/// boundary — Rust says "retrieve with top_k = X".
#[derive(Serialize, Default, Clone)]
pub struct RagOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_retrieval: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_variants: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerank_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grade_skip_threshold: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_subqueries: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_parents: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_chunks: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_uncited_per_subq: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_per_subq_budget: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_hard_cap: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_crossref_reserve: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_lookup_max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub neighbor_span: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crossref_max_sections: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toc_max_sections: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub late_anchor_cap: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap_round_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap_rounds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap_reserve: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap_deadline_secs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap_diminishing_unseen: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gap_escalate: Option<bool>,
}

/// Build the RAG overrides from the runtime config (super-admin knobs). Unset
/// keys stay `None` so the ML service keeps its own default.
pub async fn rag_overrides(pg: &sqlx::PgPool) -> RagOverrides {
    use crate::config::runtime;
    async fn geti(pg: &sqlx::PgPool, key: &str) -> Option<i64> {
        runtime::get(pg, key).await.ok().flatten().and_then(|e| e.value.parse::<i64>().ok())
    }
    async fn getf(pg: &sqlx::PgPool, key: &str) -> Option<f64> {
        runtime::get(pg, key).await.ok().flatten().and_then(|e| e.value.parse::<f64>().ok())
    }
    RagOverrides {
        top_k: geti(pg, "rag.top_k").await,
        over_retrieval: geti(pg, "rag.over_retrieval").await,
        max_rounds: geti(pg, "rag.max_rounds").await,
        query_variants: geti(pg, "rag.query_variants").await,
        rerank_enabled: runtime::get(pg, "rag.rerank_enabled")
            .await
            .ok()
            .flatten()
            .map(|e| e.value == "true"),
        grade_skip_threshold: getf(pg, "rag.grade_skip_threshold").await,
        max_subqueries: geti(pg, "rag.max_subqueries").await,
        max_parents: geti(pg, "rag.max_parents").await,
        max_context_chunks: geti(pg, "rag.max_context_chunks").await,
        pool_uncited_per_subq: geti(pg, "rag.pool_uncited_per_subq").await,
        pool_per_subq_budget: geti(pg, "rag.pool_per_subq_budget").await,
        pool_hard_cap: geti(pg, "rag.pool_hard_cap").await,
        pool_crossref_reserve: geti(pg, "rag.pool_crossref_reserve").await,
        anchor_lookup_max: geti(pg, "rag.anchor_lookup_max").await,
        neighbor_span: geti(pg, "rag.neighbor_span").await,
        crossref_max_sections: geti(pg, "rag.crossref_max_sections").await,
        toc_max_sections: geti(pg, "rag.toc_max_sections").await,
        late_anchor_cap: geti(pg, "rag.late_anchor_cap").await,
        gap_round_enabled: runtime::get(pg, "rag.gap_round_enabled")
            .await
            .ok()
            .flatten()
            .map(|e| e.value == "true"),
        gap_rounds: geti(pg, "rag.gap_rounds").await,
        gap_reserve: geti(pg, "rag.gap_reserve").await,
        gap_deadline_secs: geti(pg, "rag.gap_deadline_secs").await,
        gap_diminishing_unseen: getf(pg, "rag.gap_diminishing_unseen").await,
        gap_escalate: runtime::get(pg, "rag.gap_escalate")
            .await
            .ok()
            .flatten()
            .map(|e| e.value == "true"),
    }
}

/// One streaming `retrieve` call over the server-resolved KB allow-list: Python
/// pre-filters the single collection by `knowledge_base_id IN <kb_ids>` then runs
/// the full agentic loop (invisible here), emitting progress
/// lines before the terminal `Done { context, citations }`. An empty allow-list
/// must be handled by the caller (fail-closed — don't call this). Dropping the
/// returned stream cancels the in-flight retrieval. Mirrors [`web_search_stream`].
pub async fn retrieve_stream(
    http: &reqwest::Client,
    base_url: &str,
    prompt: &str,
    kb_ids: &[String],
    deny_doc_ids: &[String],
    overrides: &RagOverrides,
    providers: ProviderOverrides,
    timeout: Option<std::time::Duration>,
) -> Result<RetrieveStream, AppError> {
    let url = format!("{}/retrieve", base_url.trim_end_matches('/'));
    let _t = MlTimer::new("retrieve");
    let mut req = http.post(url).json(&RetrieveRequest {
        prompt,
        kb_ids,
        deny_doc_ids,
        stream: true,
        overrides,
        provider_overrides: providers,
    });
    if let Some(d) = timeout {
        req = req.timeout(d);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /retrieve: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /retrieve returned {}", resp.status())));
    }

    let (tx, rx) = mpsc::channel::<RetrieveEvent>(64);
    let reader = tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            buf.extend_from_slice(&bytes);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = &line[..line.len() - 1];
                if line.is_empty() {
                    continue;
                }
                if let Ok(ev) = serde_json::from_slice::<RetrieveEvent>(line) {
                    if tx.send(ev).await.is_err() {
                        return;
                    }
                }
            }
        }
        if !buf.is_empty() {
            if let Ok(ev) = serde_json::from_slice::<RetrieveEvent>(&buf) {
                let _ = tx.send(ev).await;
            }
        }
    });

    Ok(RetrieveStream { rx, reader })
}

// --- Web search --------------------------------------------------------------------------------

/// A web citation returned by the ML pipeline — URL-shaped, distinct from the
/// document-anchored `Citation` (resolved decision 2). `snippet_only` marks
/// evidence taken from a search-result snippet without fetching the page.
#[derive(Debug, Clone, Deserialize)]
pub struct WebCitation {
    pub url: String,
    pub title: Option<String>,
    pub domain: String,
    pub published_date: Option<String>,
    pub fetched_at: Option<String>,
    pub quote_text: String,
    #[serde(default)]
    pub snippet_only: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebSearchResult {
    pub digest: String,
    pub citations: Vec<WebCitation>,
}

/// Runtime/admin + per-Agent web-search knobs, flattened into the request body
/// (the RagOverrides pattern). A present-but-empty list string means "list off"
/// and still overrides the ML service's env default; absent fields fall back.
#[derive(Serialize, Default, Clone, Debug)]
pub struct WebOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_allowlist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_blocklist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowlist_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub robots_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fetches: Option<i64>,
}

/// Build web overrides from the runtime config (the client-admin "Web search"
/// settings). Unset keys stay `None` so the ML service keeps its env defaults.
pub async fn web_overrides(pg: &sqlx::PgPool) -> WebOverrides {
    use crate::config::runtime;
    async fn gets(pg: &sqlx::PgPool, key: &str) -> Option<String> {
        runtime::get(pg, key).await.ok().flatten().map(|e| e.value)
    }
    WebOverrides {
        domain_allowlist: gets(pg, "web_search.allowlist").await,
        domain_blocklist: gets(pg, "web_search.blocklist").await,
        allowlist_only: gets(pg, "web_search.allowlist_only").await.map(|v| v == "true"),
        robots_policy: gets(pg, "web_search.robots_policy").await,
        max_fetches: None, // per-Agent, folded in by the dispatcher
    }
}

#[derive(Serialize)]
struct WebSearchRequest<'a> {
    query: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    recency: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<&'a str>,
    stream: bool,
    #[serde(flatten)]
    overrides: &'a WebOverrides,
    #[serde(rename = "overrides", skip_serializing_if = "omap_is_empty")]
    provider_overrides: ProviderOverrides,
}

/// One NDJSON event from the streaming web search (web-search flow doc):
/// progress lines while the loop runs, then exactly one `Done` (or `Error`).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebEvent {
    Progress {
        stage: String,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        round: Option<i32>,
        #[serde(default)]
        subq: Option<String>,
    },
    /// A synthesis token (deep path only): the digest is written by a streaming
    /// LLM call so the deep answer types into the chat live. Absent on the inline
    /// quick/standard path, which streams its answer in the chat turn instead.
    Token { delta: String },
    Done { digest: String, citations: Vec<WebCitation> },
    Error { message: String },
}

/// Live event stream for a web search. Dropping it aborts the reader (which
/// cancels the upstream HTTP body — same mechanics as `GenStream`/`CellStream`).
pub struct WebStream {
    rx: mpsc::Receiver<WebEvent>,
    reader: tokio::task::JoinHandle<()>,
}

impl WebStream {
    pub async fn recv(&mut self) -> Option<WebEvent> {
        self.rx.recv().await
    }
}

impl Drop for WebStream {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// One streaming `web_search` call. The whole pipeline (SERP → SSRF-guarded
/// paced fetch → extract → rerank → assemble) runs server-side in Python,
/// invisible here — the same clean boundary as `retrieve`; progress events let
/// the caller surface live agent activity. Callers MUST pass the egress gate
/// (`integrations::guard_egress`) before calling this.
pub async fn web_search_stream(
    http: &reqwest::Client,
    base_url: &str,
    query: &str,
    recency: Option<&str>,
    depth: Option<&str>,
    overrides: &WebOverrides,
    providers: ProviderOverrides,
    timeout: Option<std::time::Duration>,
) -> Result<WebStream, AppError> {
    let url = format!("{}/web_search", base_url.trim_end_matches('/'));
    let _t = MlTimer::new("web_search");
    let mut req = http
        .post(url)
        .json(&WebSearchRequest { query, recency, depth, stream: true, overrides, provider_overrides: providers });
    // The inline path relies on the 120 s Web tool timeout; the deep background
    // path runs minutes, so it pins a generous per-request timeout (the shared ML
    // client sets none).
    if let Some(d) = timeout {
        req = req.timeout(d);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /web_search: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /web_search returned {}", resp.status())));
    }

    let (tx, rx) = mpsc::channel::<WebEvent>(64);
    let reader = tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            buf.extend_from_slice(&bytes);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = &line[..line.len() - 1];
                if line.is_empty() {
                    continue;
                }
                if let Ok(ev) = serde_json::from_slice::<WebEvent>(line) {
                    if tx.send(ev).await.is_err() {
                        return;
                    }
                }
            }
        }
        if !buf.is_empty() {
            if let Ok(ev) = serde_json::from_slice::<WebEvent>(&buf) {
                let _ = tx.send(ev).await;
            }
        }
    });

    Ok(WebStream { rx, reader })
}

// --- Deep Research -----------------------------------------------------------------------------

/// One NDJSON event from the streaming research pipeline: progress lines while
/// the run executes (plan → collect → notes → outline → write → cohere → check
/// → deliver), then exactly one `Done` (or `Error`).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResearchEvent {
    Progress {
        phase: String,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        sources_read: Option<i64>,
        #[serde(default)]
        sections_done: Option<i64>,
        #[serde(default)]
        sections_total: Option<i64>,
        /// Full ordered section roadmap, sent once when the outline is settled.
        #[serde(default)]
        sections: Option<Vec<String>>,
    },
    /// A report-writing token: section headings + bodies stream as they are
    /// written so the report types into the chat live. The terminal `Done` still
    /// carries the authoritative `report_md` (post-coherence) for reconciliation.
    Token { delta: String },
    Done {
        title: String,
        report_md: String,
        /// Web citations in [W#] reference order (empty for a files-only run).
        citations: Vec<WebCitation>,
        /// Document-anchored citations in [D#] reference order (Phase 2 corpus
        /// modes). Absent on a Phase-1 web run ⇒ default empty (back-compat).
        #[serde(default)]
        doc_citations: Vec<Citation>,
        /// Citation-verification summary (present only when the run verified).
        /// Absent ⇒ None (an unverified run).
        #[serde(default)]
        verification: Option<ResearchVerification>,
    },
    Error { message: String },
}

/// One unsupported-but-surviving claim span in the FINAL report (char offsets),
/// for the groundedness pill + inline `<mark>` (same shape as a Mode-A span).
#[derive(Debug, Clone, Deserialize)]
pub struct ResearchVerificationSpan {
    pub start: i32,
    pub end: i32,
    pub text: String,
    #[serde(default)]
    pub label: String, // "not_mentioned" (contradicted claims are cut, no span)
    #[serde(default)]
    pub score: f64,
}

/// The in-pipeline verification result for a Deep Research report.
#[derive(Debug, Clone, Deserialize)]
pub struct ResearchVerification {
    pub score: f64,
    #[serde(default)]
    pub total: i32,
    #[serde(default)]
    pub supported: i32,
    #[serde(default)]
    pub contradicted: i32,
    #[serde(default)]
    pub not_mentioned: i32,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub spans: Vec<ResearchVerificationSpan>,
}

/// One document of the census inventory the backend sends for files/hybrid runs.
#[derive(Serialize, Debug, Clone)]
pub struct DocEntry {
    pub doc_id: Uuid,
    pub kb_id: Uuid,
    pub kb_name: String,
    pub path: String,
    pub mime: Option<String>,
    pub filename: String,
}

#[derive(Serialize)]
struct DeepResearchRequest<'a> {
    question: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<&'a str>,
    /// A user-defined template resolved by the backend and sent inline (the ML
    /// service holds no database). Absent for the built-ins, whose behaviour the
    /// service owns. Skipped when None so the built-in wire shape is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    template_spec: Option<&'a serde_json::Value>,
    source: &'a str,
    kb_ids: &'a [String],
    docs: &'a [DocEntry],
    #[serde(skip_serializing_if = "Option::is_none")]
    total_docs: Option<i64>,
    refinements: &'a [String],
    /// Run the in-pipeline citation verification + ground-or-cut.
    verify: bool,
    #[serde(flatten)]
    overrides: &'a WebOverrides,
    #[serde(flatten)]
    research: &'a ResearchOverrides,
    #[serde(rename = "overrides", skip_serializing_if = "omap_is_empty")]
    provider_overrides: ProviderOverrides,
}

/// Runtime Deep Research budget knobs (super-admin), parallel to `WebOverrides`/
/// `RagOverrides`. Flattened into the request; an absent field ⇒ the ML service
/// keeps its env default. `verify` is computed by the caller (NOT sent here) —
/// it is the boolean `features.groundedness && research.verify`.
#[derive(Serialize, Default, Clone)]
pub struct ResearchOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub research_max_minutes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub research_census_cap: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub research_notes_concurrency: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub research_deepen_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub research_deepen_concurrency: Option<i64>,
}

/// Whether DR verification should run: `features.groundedness` (boot) AND the
/// `research.verify` runtime knob. Plus the budget overrides. Read fresh at run
/// time (super-admin knobs take effect on the next run).
pub async fn research_overrides(pg: &sqlx::PgPool) -> ResearchOverrides {
    use crate::config::runtime;
    async fn getf(pg: &sqlx::PgPool, key: &str) -> Option<f64> {
        runtime::get(pg, key).await.ok().flatten().and_then(|e| e.value.parse::<f64>().ok())
    }
    async fn geti(pg: &sqlx::PgPool, key: &str) -> Option<i64> {
        runtime::get(pg, key).await.ok().flatten().and_then(|e| e.value.parse::<i64>().ok())
    }
    async fn getb(pg: &sqlx::PgPool, key: &str) -> Option<bool> {
        runtime::get(pg, key).await.ok().flatten().map(|e| e.value == "true")
    }
    ResearchOverrides {
        research_max_minutes: getf(pg, "research.max_minutes").await,
        research_census_cap: geti(pg, "research.census_cap").await,
        research_notes_concurrency: geti(pg, "research.notes_concurrency").await,
        research_deepen_enabled: getb(pg, "research.deepen_enabled").await,
        research_deepen_concurrency: geti(pg, "research.deepen_concurrency").await,
    }
}

/// The `research.verify` runtime knob (off by default). The caller AND-gates it
/// with `features.groundedness`.
pub async fn research_verify_enabled(pg: &sqlx::PgPool) -> bool {
    use crate::config::runtime;
    runtime::get(pg, "research.verify")
        .await
        .ok()
        .flatten()
        .map(|e| e.value == "true")
        .unwrap_or(false)
}

/// The full definitions of the built-in report templates, fetched from the
/// research service. Used only when a user duplicates a built-in into an editable
/// one — the picker itself is served from the backend's own metadata copy, so
/// this is never on a page-load path and is deliberately un-cached. If the ML
/// service is down the duplicate fails honestly rather than serving a stale copy.
pub async fn builtin_research_templates(
    http: &reqwest::Client,
    base_url: &str,
) -> Result<Vec<serde_json::Value>, AppError> {
    let url = format!("{}/research/templates", base_url.trim_end_matches('/'));
    let resp = http
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /research/templates: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!(
            "ml /research/templates returned {}",
            resp.status()
        )));
    }
    resp.json::<Vec<serde_json::Value>>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /research/templates decode: {e}")))
}

/// Live event stream for a research run. Dropping it aborts the reader (which
/// cancels the upstream HTTP body — `GenStream`/`WebStream` mechanics).
pub struct ResearchStream {
    rx: mpsc::Receiver<ResearchEvent>,
    reader: tokio::task::JoinHandle<()>,
}

impl ResearchStream {
    pub async fn recv(&mut self) -> Option<ResearchEvent> {
        self.rx.recv().await
    }
}

impl Drop for ResearchStream {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// One streaming `deep_research` call. The whole synthesis pipeline (collect →
/// memory bank → outline → write → cohere → checks) runs server-side in
/// Python, invisible here. Callers MUST pass the egress gate before this.
#[allow(clippy::too_many_arguments)]
pub async fn research_stream(
    http: &reqwest::Client,
    base_url: &str,
    question: &str,
    template: Option<&str>,
    template_spec: Option<&serde_json::Value>,
    source: &str,
    kb_ids: &[String],
    docs: &[DocEntry],
    total_docs: Option<i64>,
    refinements: &[String],
    verify: bool,
    overrides: &WebOverrides,
    research: &ResearchOverrides,
    providers: ProviderOverrides,
    timeout: Option<std::time::Duration>,
) -> Result<ResearchStream, AppError> {
    let url = format!("{}/deep_research", base_url.trim_end_matches('/'));
    let _t = MlTimer::new("deep_research");
    let mut req = http.post(url).json(&DeepResearchRequest {
        question,
        template,
        template_spec,
        source,
        kb_ids,
        docs,
        total_docs,
        refinements,
        verify,
        overrides,
        research,
        provider_overrides: providers,
    });
    if let Some(d) = timeout {
        req = req.timeout(d);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /deep_research: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /deep_research returned {}", resp.status())));
    }

    let (tx, rx) = mpsc::channel::<ResearchEvent>(64);
    let reader = tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            buf.extend_from_slice(&bytes);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = &line[..line.len() - 1];
                if line.is_empty() {
                    continue;
                }
                if let Ok(ev) = serde_json::from_slice::<ResearchEvent>(line) {
                    if tx.send(ev).await.is_err() {
                        return;
                    }
                }
            }
        }
        if !buf.is_empty() {
            if let Ok(ev) = serde_json::from_slice::<ResearchEvent>(&buf) {
                let _ = tx.send(ev).await;
            }
        }
    });

    Ok(ResearchStream { rx, reader })
}

/// One scope entry shown to the triage call (a library the user can read).
#[derive(Serialize)]
pub struct TriageScopeEntry {
    pub index: usize,
    pub name: String,
    pub kind: String,
    pub doc_count: i64,
}

#[derive(Serialize)]
struct TriageRequest<'a> {
    question: &'a str,
    source: &'a str,
    scope: &'a [TriageScopeEntry],
}

/// A triage option: a tappable chip. `scope_indices` reference entries in the
/// scope list the backend sent (mapped to kb_ids by the caller — LLM-emitted
/// ids are never trusted).
#[derive(Debug, Clone, Deserialize)]
pub struct TriageOption {
    pub label: String,
    #[serde(default)]
    pub scope_indices: Vec<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TriageQuestion {
    pub id: String,
    pub prompt: String,
    pub options: Vec<TriageOption>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TriageOut {
    #[serde(default)]
    pub ambiguous: bool,
    #[serde(default)]
    pub questions: Vec<TriageQuestion>,
}

/// Ambiguity triage for the plan gate. Side-effect-free; NEVER blocks the
/// interactive `prepare` — any non-200 / decode error / timeout degrades to
/// "no questions". The short timeout is the caller's responsibility.
pub async fn research_triage(
    http: &reqwest::Client,
    base_url: &str,
    question: &str,
    source: &str,
    scope: &[TriageScopeEntry],
    timeout: std::time::Duration,
) -> TriageOut {
    let url = format!("{}/research/triage", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .timeout(timeout)
        .json(&TriageRequest { question, source, scope })
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => r.json::<TriageOut>().await.unwrap_or_default(),
        Ok(r) => {
            tracing::debug!(status = %r.status(), "research triage non-200 (no chips)");
            TriageOut::default()
        }
        Err(e) => {
            tracing::debug!(error = %e, "research triage unreachable (no chips)");
            TriageOut::default()
        }
    }
}

// --- Groundedness verification (Mode A — live) -----------------------------

/// One unsupported span the verifier flagged in the answer. `start`/`end` are
/// char offsets into the answer text; `label` is `contradicted` (the source
/// disagrees) or `not_mentioned` (the source is silent); `score` is confidence.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GroundSpan {
    pub start: i32,
    pub end: i32,
    pub text: String,
    #[serde(default = "default_verdict")]
    pub label: String,
    #[serde(default)]
    pub score: f64,
}

fn default_verdict() -> String {
    "not_mentioned".into()
}

/// Result of a live groundedness check. `score` is the grounded fraction ∈ [0,1];
/// `None` means the verifier was disabled/unreachable (fail-open — no run recorded).
#[derive(Debug, Clone, Deserialize)]
pub struct VerifyLiveResult {
    #[serde(default)]
    pub spans: Vec<GroundSpan>,
    pub score: Option<f64>,
    #[serde(default)]
    pub total: i32,
    #[serde(default)]
    pub flagged: i32,
    #[serde(default)]
    pub model: String,
}

/// Groundedness dial from the runtime config (super-admin knobs). Absent fields →
/// the ML service uses its own defaults. Flattened into the verify request bodies.
#[derive(Serialize, Default, Clone)]
pub struct GroundednessOverrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strictness: Option<String>, // "strict" | "lenient"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hhem_filter: Option<bool>,
}

/// Build the groundedness overrides from runtime config: `groundedness.strict`
/// (bool; off = lenient), `groundedness.threshold` (float), `groundedness.hhem_filter`
/// (bool). Unset → None (the ML service keeps its own default).
pub async fn groundedness_overrides(pg: &sqlx::PgPool) -> GroundednessOverrides {
    use crate::config::runtime;
    let strictness = runtime::get(pg, "groundedness.strict")
        .await
        .ok()
        .flatten()
        .map(|e| if e.value == "false" { "lenient".to_string() } else { "strict".to_string() });
    let threshold = runtime::get(pg, "groundedness.threshold")
        .await
        .ok()
        .flatten()
        .and_then(|e| e.value.parse::<f64>().ok());
    let hhem_filter = runtime::get(pg, "groundedness.hhem_filter")
        .await
        .ok()
        .flatten()
        .map(|e| e.value == "true");
    GroundednessOverrides { strictness, threshold, hhem_filter }
}

/// Whether ground-or-cut repair is enabled (runtime knob `groundedness.repair`;
/// default off — regeneration quality is model-dependent).
pub async fn groundedness_repair_enabled(pg: &sqlx::PgPool) -> bool {
    crate::config::runtime::get(pg, "groundedness.repair")
        .await
        .ok()
        .flatten()
        .map(|e| e.value == "true")
        .unwrap_or(false)
}

#[derive(Serialize)]
struct VerifyRequest<'a> {
    context: &'a str,
    question: &'a str,
    answer: &'a str,
    #[serde(flatten)]
    overrides: &'a GroundednessOverrides,
    #[serde(rename = "overrides", skip_serializing_if = "omap_is_empty")]
    provider_overrides: ProviderOverrides,
}

/// Verify a streamed RAG answer against its retrieved context (Mode A). The ML
/// service flags unsupported spans and derives a groundedness score; the verifier
/// engine being down fails open there, so this only errors on transport/decode.
/// Called post-stream from a spawned task — never on the hot path.
pub async fn verify_live(
    http: &reqwest::Client,
    base_url: &str,
    context: &str,
    question: &str,
    answer: &str,
    overrides: &GroundednessOverrides,
    providers: ProviderOverrides,
) -> Result<VerifyLiveResult, AppError> {
    let url = format!("{}/verify", base_url.trim_end_matches('/'));
    let _t = MlTimer::new("verify");
    let resp = http
        .post(url)
        .json(&VerifyRequest { context, question, answer, overrides, provider_overrides: providers })
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /verify: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /verify returned {}", resp.status())));
    }
    resp.json::<VerifyLiveResult>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /verify decode: {e}")))
}

// --- Provider health probe ---------------------------------------------------

#[derive(Serialize)]
struct ProviderTestReq<'a> {
    role: &'a str,
    #[serde(rename = "overrides", skip_serializing_if = "omap_is_empty")]
    provider_overrides: ProviderOverrides,
}

/// Outcome of a single provider probe — mirrors the ML `/provider/test` body.
/// `Serialize` so a handler can return it straight to the SPA. Never carries the
/// api_key (the ML side strips it).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderTestResult {
    pub ok: bool,
    #[serde(default)]
    pub latency_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Probe one provider role through the ML service: a minimal real call with the
/// supplied `{role}_base_url/_model/_api_key` overrides. The key is sent to the
/// local ML service over the shared-secret channel and never logged here.
pub async fn test_provider(
    http: &reqwest::Client,
    base_url: &str,
    role: &str,
    overrides: ProviderOverrides,
) -> Result<ProviderTestResult, AppError> {
    let url = format!("{}/provider/test", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&ProviderTestReq { role, provider_overrides: overrides })
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /provider/test: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /provider/test returned {}", resp.status())));
    }
    resp.json::<ProviderTestResult>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /provider/test decode: {e}")))
}

// --- Groundedness verification (Mode B — Verify draft) ---------------------

/// One decomposed claim + its verdict against bound evidence.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimVerdictOut {
    pub text: String,
    pub verdict: String, // supported | contradicted | not_mentioned
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub section: String,
    #[serde(default)]
    pub had_citation: bool,
    /// `{start, end, text}` of the claim's verbatim span in the document, or
    /// `None` if it could not be located. Feeds highlight + repair.
    #[serde(default)]
    pub source_span: Option<serde_json::Value>,
}

/// Aggregate result of a draft verification. `score` = supported/total (None when
/// no verifiable claims were found / the verifier was unreachable).
#[derive(Debug, Clone, Deserialize)]
pub struct VerifyDraftResult {
    #[serde(default)]
    pub claims: Vec<ClaimVerdictOut>,
    pub score: Option<f64>,
    #[serde(default)]
    pub total: i32,
    #[serde(default)]
    pub supported: i32,
    #[serde(default)]
    pub contradicted: i32,
    #[serde(default)]
    pub not_mentioned: i32,
}

#[derive(Serialize)]
struct VerifyDraftRequest<'a> {
    path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime: Option<&'a str>,
    kb_ids: &'a [String],
    #[serde(flatten)]
    overrides: &'a GroundednessOverrides,
    #[serde(rename = "overrides", skip_serializing_if = "omap_is_empty")]
    provider_overrides: ProviderOverrides,
}

/// Decompose a draft (its extracted text) into claims and verify each against the
/// caller's KB allow-list. A throughput call — run from the background task only.
pub async fn verify_draft(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    mime: Option<&str>,
    kb_ids: &[String],
    overrides: &GroundednessOverrides,
    providers: ProviderOverrides,
) -> Result<VerifyDraftResult, AppError> {
    let url = format!("{}/verify-draft", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&VerifyDraftRequest { path, mime, kb_ids, overrides, provider_overrides: providers })
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /verify-draft: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /verify-draft returned {}", resp.status())));
    }
    resp.json::<VerifyDraftResult>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /verify-draft decode: {e}")))
}

// --- Ground-or-cut repair (groundedness) ------------------------------

/// One flagged claim handed to the repair engine.
#[derive(Debug, Clone, Serialize)]
pub struct RepairClaimInput {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_text: Option<String>,
    pub verdict: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

/// One claim's repair outcome. `action` ∈ regenerated | cut | kept; `replacement`
/// is the grounded rewrite (None for cut/kept); `reverify_*` is the verdict of the
/// new citation — a rewrite is only proposed when re-verified `supported`.
#[derive(Debug, Clone, Deserialize)]
pub struct RepairResult {
    #[serde(default)]
    pub source_text: Option<String>,
    #[serde(default)]
    pub claim_text: Option<String>,
    pub action: String,
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub citation_ref: Option<String>,
    #[serde(default)]
    pub reverify_verdict: String,
    #[serde(default)]
    pub reverify_score: f64,
}

#[derive(Serialize)]
struct RepairDraftRequest<'a> {
    claims: &'a [RepairClaimInput],
    kb_ids: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    strictness: Option<&'a str>,
}

/// Repair a finished run's flagged claims (regenerate-or-cut + re-verify). A
/// throughput call — run from the background task only.
pub async fn repair_draft(
    http: &reqwest::Client,
    base_url: &str,
    claims: &[RepairClaimInput],
    kb_ids: &[String],
    strictness: Option<&str>,
) -> Result<Vec<RepairResult>, AppError> {
    let url = format!("{}/repair-draft", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&RepairDraftRequest { claims, kb_ids, strictness })
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /repair-draft: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /repair-draft returned {}", resp.status())));
    }
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        results: Vec<RepairResult>,
    }
    Ok(resp
        .json::<R>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /repair-draft decode: {e}")))?
        .results)
}

#[derive(Serialize)]
struct IngestRequest<'a> {
    doc_id: &'a str,
    kb_id: &'a str,
    path: &'a str,
    mime: Option<&'a str>,
    dimension: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chunk_overlap: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pdfplumber: Option<bool>,
    /// Per-KB parent–child chunking; None ⇒ the ML default.
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_child: Option<bool>,
    #[serde(skip_serializing_if = "omap_is_empty")]
    overrides: ProviderOverrides,
    /// Dual-write target during a blue-green re-index ({dim, model, base_url, api_key}).
    #[serde(skip_serializing_if = "Option::is_none")]
    dual: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IngestResult {
    pub chunks: i64,
    /// The document's own date (ISO `YYYY-MM-DD`), best-effort; `None` if unknown.
    #[serde(default)]
    pub effective_date: Option<String>,
}

/// Ingest a document into the shared collection (extract→chunk→embed→upsert).
/// The backend supplies `kb_id`; Python stamps it as the immutable
/// `knowledge_base_id` on every chunk (no access grants in the payload).
#[allow(clippy::too_many_arguments)]
pub async fn ingest(
    http: &reqwest::Client,
    base_url: &str,
    doc_id: &str,
    kb_id: &str,
    path: &str,
    mime: Option<&str>,
    dimension: i32,
    chunk_size: Option<i64>,
    chunk_overlap: Option<i64>,
    pdfplumber: Option<bool>,
    providers: ProviderOverrides,
    dual: Option<serde_json::Value>,
    parent_child: Option<bool>,
) -> Result<IngestResult, AppError> {
    let url = format!("{}/ingest", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&IngestRequest { doc_id, kb_id, path, mime, dimension, chunk_size, chunk_overlap, pdfplumber, parent_child, overrides: providers, dual })
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /ingest: {e}")))?;
    if !resp.status().is_success() {
        // Carry the reason (e.g. "OCR required but unavailable: …") through so it
        // reaches the document's error status + the ingest-status WS frame.
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let detail: String = body.chars().take(300).collect();
        return Err(AppError::Other(anyhow!("ml /ingest returned {status}: {detail}")));
    }
    resp.json::<IngestResult>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /ingest decode: {e}")))
}

/// Purge a document's chunks from the shared collection (KB doc deletion).
pub async fn delete_doc(
    http: &reqwest::Client,
    base_url: &str,
    kb_id: &str,
    doc_id: &str,
) -> Result<(), AppError> {
    let url = format!("{}/delete-doc", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&serde_json::json!({ "kb_id": kb_id, "doc_id": doc_id }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /delete-doc: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /delete-doc returned {}", resp.status())));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmbedInfo {
    pub model: String,
    pub dimension: i32,
}

/// Probe the configured embed model's id + dimension (set at PK creation). Passes
/// the resolved provider overrides so a deployment that points `embed` at a cloud
/// API (DB provider config) is honoured, not just the ML `.env` default.
pub async fn embed_info(
    http: &reqwest::Client,
    base_url: &str,
    providers: ProviderOverrides,
) -> Result<EmbedInfo, AppError> {
    let url = format!("{}/embed-dimension", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&serde_json::json!({ "overrides": providers }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /embed-dimension: {e}")))?;
    resp.json::<EmbedInfo>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /embed-dimension decode: {e}")))
}

/// Ensure the single shared Qdrant collection exists (created once at the
/// deployment embedding dimension; payload-partitioned by `knowledge_base_id`).
pub async fn ensure_collection(
    http: &reqwest::Client,
    base_url: &str,
    dimension: i32,
) -> Result<(), AppError> {
    let url = format!("{}/collections", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&serde_json::json!({ "dimension": dimension }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /collections: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /collections returned {}", resp.status())));
    }
    Ok(())
}

// --- Tool-call loop ----------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatStep {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Serialize)]
struct ChatStepRequest<'a> {
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [serde_json::Value]>,
    sampling: &'a Sampling,
    #[serde(skip_serializing_if = "omap_is_empty")]
    overrides: ProviderOverrides,
}

/// One non-streaming tool-decision step: returns the model's content and/or
/// tool calls. Used by the tool loop; the final answer streams via [`generate`].
pub async fn chat_step(
    http: &reqwest::Client,
    base_url: &str,
    messages: &[Message],
    tools: Option<&[serde_json::Value]>,
    sampling: &Sampling,
    providers: ProviderOverrides,
) -> Result<ChatStep, AppError> {
    let url = format!("{}/chat-step", base_url.trim_end_matches('/'));
    let _t = MlTimer::new("chat_step");
    let resp = http
        .post(url)
        .json(&ChatStepRequest { messages, tools, sampling, overrides: providers })
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /chat-step: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /chat-step returned {}", resp.status())));
    }
    resp.json::<ChatStep>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /chat-step decode: {e}")))
}

#[derive(Debug, Clone, serde::Serialize)]
struct ClassifyRequest<'a> {
    prompt: &'a str,
    context: &'a str,
}

/// Result of the neutral moderation classifier — a domain `category` + a small
/// anomaly signal. It NEVER emits a verdict or a score (the score is computed in
/// Rust from structural facts). See `moderation`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ClassifyResult {
    pub category: String,
    #[serde(default)]
    pub anomaly: bool,
    #[serde(default)]
    pub confidence: f32,
}

/// Neutral prompt classification (moderation; spec). Same local in-perimeter
/// model; nothing leaves the perimeter. Off the hot path — callers spawn it.
pub async fn classify_prompt(
    http: &reqwest::Client,
    base_url: &str,
    prompt: &str,
    context: &str,
    providers: ProviderOverrides,
) -> Result<ClassifyResult, AppError> {
    let url = format!("{}/classify-prompt", base_url.trim_end_matches('/'));
    let mut body = serde_json::to_value(ClassifyRequest { prompt, context }).unwrap_or_default();
    if !providers.is_empty() {
        body["overrides"] = providers.into();
    }
    let resp = http
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /classify-prompt: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /classify-prompt returned {}", resp.status())));
    }
    resp.json::<ClassifyResult>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /classify-prompt decode: {e}")))
}

/// Read a document's extracted text (the `read_document` tool's Python side).
/// `prompt` is the task to read FOR: when given and the document is too large to
/// stuff, the Python side runs an exhaustive map-reduce focused on it instead of
/// truncating (.md). `None` = plain whole-document read.
pub async fn read_document(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    mime: Option<&str>,
    prompt: Option<&str>,
    providers: ProviderOverrides,
) -> Result<String, AppError> {
    let url = format!("{}/read-document", base_url.trim_end_matches('/'));
    let _t = MlTimer::new("read_document");
    let mut body = serde_json::json!({ "path": path, "mime": mime, "prompt": prompt });
    if !providers.is_empty() {
        body["overrides"] = providers.into();
    }
    let resp = http
        .post(url)
        .json(&body)
        // Bound extraction: a huge/slow document must error here rather than hang
        // past the client's upload timeout (which would surface as "Failed to fetch").
        .timeout(std::time::Duration::from_secs(240))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /read-document: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /read-document returned {}", resp.status())));
    }
    #[derive(Deserialize)]
    struct R {
        text: String,
    }
    Ok(resp
        .json::<R>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /read-document decode: {e}")))?
        .text)
}

// --- Tracked changes (DOCX) --------------------------------------------------

/// A find/replace edit to apply as a tracked change.
#[derive(Debug, Clone, Serialize)]
pub struct EditInput {
    pub find: String,
    pub replace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_after: Option<String>,
}

/// One applied tracked change (its `<w:del>` and `<w:ins>` share `w_id`).
#[derive(Debug, Clone, Deserialize)]
pub struct AppliedChange {
    pub w_id: String,
    pub find: String,
    pub replace: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EditError {
    pub index: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyResult {
    pub changes: Vec<AppliedChange>,
    #[serde(default)]
    pub errors: Vec<EditError>,
}

/// Apply find/replace edits as tracked changes; Python writes the new DOCX to
/// `out_path` and returns the per-change ids (tracked-changes flow –4).
pub async fn apply_tracked_changes(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    out_path: &str,
    edits: &[EditInput],
    author: &str,
) -> Result<ApplyResult, AppError> {
    let url = format!("{}/apply-tracked-changes", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&serde_json::json!({ "path": path, "out_path": out_path, "edits": edits, "author": author }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /apply-tracked-changes: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /apply-tracked-changes returned {}", resp.status())));
    }
    resp.json::<ApplyResult>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /apply-tracked-changes decode: {e}")))
}

/// Accept/reject one tracked change by `w_id`; Python writes the resolved DOCX
/// to `out_path`.
pub async fn resolve_tracked_change(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    out_path: &str,
    w_id: &str,
    action: &str,
) -> Result<(), AppError> {
    let url = format!("{}/resolve-tracked-change", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&serde_json::json!({ "path": path, "out_path": out_path, "w_id": w_id, "action": action }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /resolve-tracked-change: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /resolve-tracked-change returned {}", resp.status())));
    }
    Ok(())
}

/// Accept/reject all tracked changes (optionally filtered by author); returns
/// the resolved `w_id`s.
pub async fn resolve_all_tracked_changes(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    out_path: &str,
    action: &str,
    author_filter: Option<&str>,
) -> Result<Vec<String>, AppError> {
    let url = format!("{}/resolve-tracked-changes", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&serde_json::json!({
            "path": path, "out_path": out_path, "action": action, "author_filter": author_filter
        }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /resolve-tracked-changes: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /resolve-tracked-changes returned {}", resp.status())));
    }
    #[derive(Deserialize)]
    struct R {
        resolved: Vec<String>,
    }
    Ok(resp
        .json::<R>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /resolve-tracked-changes decode: {e}")))?
        .resolved)
}

// --- DOCX→PDF rendition ------------------------------------------------------

/// Whether the ML service can render DOCX→PDF (LibreOffice present).
pub async fn render_available(http: &reqwest::Client, base_url: &str) -> Result<bool, AppError> {
    let url = format!("{}/render/available", base_url.trim_end_matches('/'));
    let resp = http
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /render/available: {e}")))?;
    #[derive(Deserialize)]
    struct R {
        available: bool,
    }
    Ok(resp
        .json::<R>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /render/available decode: {e}")))?
        .available)
}

/// Render a DOCX to PDF in `out_dir`; returns the PDF path. `Unavailable` (503)
/// when LibreOffice is absent.
pub async fn render_pdf(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    out_dir: &str,
) -> Result<String, AppError> {
    let url = format!("{}/render", base_url.trim_end_matches('/'));
    let _t = MlTimer::new("render");
    let resp = http
        .post(url)
        .json(&serde_json::json!({ "path": path, "out_dir": out_dir }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /render: {e}")))?;
    if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        return Err(AppError::Unavailable("DOCX→PDF rendition unavailable".into()));
    }
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /render returned {}", resp.status())));
    }
    #[derive(Deserialize)]
    struct R {
        pdf_path: String,
    }
    Ok(resp
        .json::<R>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /render decode: {e}")))?
        .pdf_path)
}

// --- Tabular review ----------------------------------------------------------

/// A document under review (its current-version path on disk).
#[derive(Debug, Clone, Serialize)]
pub struct ReviewDoc {
    pub document_id: Uuid,
    pub path: String,
    pub mime: Option<String>,
}

/// An extraction column: a format-typed prompt run against each document.
/// `mechanism` selects how the document is fed to the model: `stuff` (whole doc,
/// truncation-guarded), `per_document_rag` (top-k chunks), or `map_section`.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewColumn {
    pub key: String,
    pub format: String,
    pub prompt: String,
    pub mechanism: String,
}

/// One streamed event from review generation.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReviewEvent {
    Cell {
        document_id: Uuid,
        column_key: String,
        #[serde(default)]
        status: String,
        #[serde(default)]
        value: serde_json::Value,
        #[serde(default)]
        reasoning: Option<String>,
        #[serde(default)]
        citations: serde_json::Value,
        #[serde(default)]
        error: Option<String>,
    },
    Done,
    Error {
        message: String,
    },
}

/// A live stream of cell results. Drop to abort (cancels the upstream pool).
pub struct CellStream {
    rx: mpsc::Receiver<ReviewEvent>,
    reader: JoinHandle<()>,
}

impl CellStream {
    pub async fn recv(&mut self) -> Option<ReviewEvent> {
        self.rx.recv().await
    }
}

impl Drop for CellStream {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// Start tabular cell generation; Python runs the bounded-concurrency pool and
/// streams one `Cell` event per (document × column), then `Done`.
pub async fn generate_review(
    http: &reqwest::Client,
    base_url: &str,
    documents: &[ReviewDoc],
    columns: &[ReviewColumn],
    providers: ProviderOverrides,
) -> Result<CellStream, AppError> {
    let url = format!("{}/generate-review", base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({ "documents": documents, "columns": columns });
    if !providers.is_empty() {
        body["overrides"] = providers.into();
    }
    let resp = http
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /generate-review connect: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /generate-review returned {}", resp.status())));
    }

    let (tx, rx) = mpsc::channel::<ReviewEvent>(64);
    let reader = tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            buf.extend_from_slice(&bytes);
            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = &line[..line.len() - 1];
                if line.is_empty() {
                    continue;
                }
                if let Ok(ev) = serde_json::from_slice::<ReviewEvent>(line) {
                    if tx.send(ev).await.is_err() {
                        return;
                    }
                }
            }
        }
        if !buf.is_empty() {
            if let Ok(ev) = serde_json::from_slice::<ReviewEvent>(&buf) {
                let _ = tx.send(ev).await;
            }
        }
    });

    Ok(CellStream { rx, reader })
}

// --- Memory recall index -----------------------------------------------------

/// Index a memory fact for relevance recall (best-effort; Postgres is truth).
pub async fn memory_upsert(
    http: &reqwest::Client,
    base_url: &str,
    scope_key: &str,
    fact_id: &str,
    content: &str,
    providers: ProviderOverrides,
) -> Result<(), AppError> {
    let url = format!("{}/memory/upsert", base_url.trim_end_matches('/'));
    http.post(url)
        .json(&serde_json::json!({ "scope_key": scope_key, "fact_id": fact_id, "content": content, "overrides": providers }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /memory/upsert: {e}")))?;
    Ok(())
}

/// Relevance-rank a scope's facts against `query`; returns fact ids (strings).
pub async fn memory_search(
    http: &reqwest::Client,
    base_url: &str,
    scope_key: &str,
    query: &str,
    limit: i64,
    providers: ProviderOverrides,
) -> Result<Vec<String>, AppError> {
    let url = format!("{}/memory/search", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&serde_json::json!({ "scope_key": scope_key, "query": query, "limit": limit, "overrides": providers }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /memory/search: {e}")))?;
    #[derive(Deserialize)]
    struct R {
        ids: Vec<String>,
    }
    Ok(resp
        .json::<R>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /memory/search decode: {e}")))?
        .ids)
}

/// Remove a memory fact from the recall index (best-effort).
pub async fn memory_delete(
    http: &reqwest::Client,
    base_url: &str,
    scope_key: &str,
    fact_id: &str,
) -> Result<(), AppError> {
    let url = format!("{}/memory/delete", base_url.trim_end_matches('/'));
    http.post(url)
        .json(&serde_json::json!({ "scope_key": scope_key, "fact_id": fact_id }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /memory/delete: {e}")))?;
    Ok(())
}

// --- Generated artefacts -----------------------------------------------------

/// Generate a DOCX/PDF/MD artefact at `out_path`; returns `(path, mime)`.
pub async fn generate_artefact(
    http: &reqwest::Client,
    base_url: &str,
    kind: &str,
    title: &str,
    content: &str,
    out_path: &str,
) -> Result<(String, String), AppError> {
    let url = format!("{}/generate-artefact", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&serde_json::json!({ "kind": kind, "title": title, "content": content, "out_path": out_path }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /generate-artefact: {e}")))?;
    if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        return Err(AppError::Unavailable("artefact generation unavailable".into()));
    }
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /generate-artefact returned {}", resp.status())));
    }
    #[derive(Deserialize)]
    struct R {
        path: String,
        mime: String,
    }
    let r = resp
        .json::<R>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /generate-artefact decode: {e}")))?;
    Ok((r.path, r.mime))
}

/// Render a review matrix to an `.xlsx` at `out_path`; returns the path.
pub async fn export_review(
    http: &reqwest::Client,
    base_url: &str,
    name: &str,
    columns: &serde_json::Value,
    rows: &serde_json::Value,
    out_path: &str,
) -> Result<String, AppError> {
    let url = format!("{}/export-review", base_url.trim_end_matches('/'));
    let resp = http
        .post(url)
        .json(&serde_json::json!({ "name": name, "columns": columns, "rows": rows, "out_path": out_path }))
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /export-review: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /export-review returned {}", resp.status())));
    }
    #[derive(Deserialize)]
    struct R {
        path: String,
    }
    Ok(resp
        .json::<R>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /export-review decode: {e}")))?
        .path)
}

/// Transcribe captured audio to text (voice dictation). Posts the raw audio to
/// the ML service, which fronts the STT engine (OpenAI-audio contract).
pub async fn transcribe(
    http: &reqwest::Client,
    base_url: &str,
    audio: &[u8],
    mime: &str,
    providers: ProviderOverrides,
) -> Result<String, AppError> {
    let url = format!("{}/transcribe", base_url.trim_end_matches('/'));
    let _t = MlTimer::new("transcribe");
    let mut builder = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, mime);
    // The body is raw audio, so provider overrides ride a header (keeps any
    // stt_api_key out of the URL/query). ML reads `X-PAI-Overrides` → set_overrides.
    if !providers.is_empty() {
        if let Ok(j) = serde_json::to_string(&providers) {
            builder = builder.header("x-pai-overrides", j);
        }
    }
    let resp = builder
        .body(audio.to_vec())
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /transcribe connect: {e}")))?;
    if resp.status().as_u16() == 503 {
        return Err(AppError::Unavailable("speech-to-text engine unavailable".into()));
    }
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /transcribe returned {}", resp.status())));
    }
    #[derive(Deserialize)]
    struct R {
        text: String,
    }
    Ok(resp
        .json::<R>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /transcribe decode: {e}")))?
        .text)
}

/// Synthesise speech for `text` (read-aloud). Returns the audio bytes + mime
/// from the TTS engine (via the ML service, OpenAI-audio contract).
pub async fn synthesize(
    http: &reqwest::Client,
    base_url: &str,
    text: &str,
    voice: Option<&str>,
    providers: ProviderOverrides,
) -> Result<(Vec<u8>, String), AppError> {
    let url = format!("{}/speech", base_url.trim_end_matches('/'));
    let _t = MlTimer::new("synthesize");
    let mut body = serde_json::json!({ "text": text, "voice": voice });
    if !providers.is_empty() {
        body["overrides"] = providers.into();
    }
    let resp = http
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /speech connect: {e}")))?;
    if resp.status().as_u16() == 503 {
        return Err(AppError::Unavailable("text-to-speech engine unavailable".into()));
    }
    if !resp.status().is_success() {
        return Err(AppError::Other(anyhow!("ml /speech returned {}", resp.status())));
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /speech bytes: {e}")))?
        .to_vec();
    Ok((bytes, mime))
}

/// Learn the served model id + context window (chat-turn token budgeting).
pub async fn model_info(http: &reqwest::Client, base_url: &str) -> Result<ModelInfo, AppError> {
    let url = format!("{}/model-info", base_url.trim_end_matches('/'));
    let resp = http
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /model-info connect: {e}")))?;
    resp.json::<ModelInfo>()
        .await
        .map_err(|e| AppError::Other(anyhow!("ml /model-info decode: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // GOLDEN regression: with no user-defined template, the `/deep_research` body
    // must serialise EXACTLY as before this feature — the `template_spec` field
    // absent, not `null`. A run with zero custom templates has to be byte-identical
    // to the pre-feature request. Asserted by serialisation, never by eye. The
    // overrides are bound locally so the request may borrow them.
    fn dr_json(template_spec: Option<&serde_json::Value>) -> String {
        let web = WebOverrides::default();
        let research = ResearchOverrides::default();
        let req = DeepResearchRequest {
            question: "q",
            template: Some("exploration"),
            template_spec,
            source: "web",
            kb_ids: &[],
            docs: &[],
            total_docs: None,
            refinements: &[],
            verify: false,
            overrides: &web,
            research: &research,
            provider_overrides: ProviderOverrides::new(),
        };
        serde_json::to_string(&req).unwrap()
    }

    #[test]
    fn deep_research_omits_template_spec_when_absent() {
        let json = dr_json(None);
        assert!(
            !json.contains("template_spec"),
            "built-in run must not carry a template_spec key: {json}"
        );
    }

    #[test]
    fn deep_research_includes_template_spec_when_present() {
        let spec = serde_json::json!({ "id": "x", "label": "Ours" });
        let json = dr_json(Some(&spec));
        assert!(json.contains("template_spec"), "custom run must carry the spec: {json}");
        assert!(json.contains("\"label\":\"Ours\""));
    }

    // A4: the NDJSON shapes the ml `/retrieve?stream=true` runner emits must decode
    // to each `RetrieveEvent` variant (tag = "type"). Guards the Rust↔Python wire
    // contract for the chat hot path.
    #[test]
    fn retrieve_event_decodes_each_variant() {
        let progress: RetrieveEvent =
            serde_json::from_str(r#"{"type":"progress","stage":"search","detail":"searching your library"}"#)
                .unwrap();
        assert!(matches!(progress, RetrieveEvent::Progress { detail: Some(_), .. }));

        // `detail` is optional.
        let bare: RetrieveEvent = serde_json::from_str(r#"{"type":"progress","stage":"assemble"}"#).unwrap();
        assert!(matches!(bare, RetrieveEvent::Progress { detail: None, .. }));

        let done: RetrieveEvent = serde_json::from_str(
            r#"{"type":"done","context":"[1] ctx","citations":[{"doc_id":null,"chunk_index":0,"page_number":null,"clause_section_ref":null,"quote_text":"q"}]}"#,
        )
        .unwrap();
        match done {
            RetrieveEvent::Done { context, citations, parts, debug } => {
                assert_eq!(context, "[1] ctx");
                assert_eq!(citations.len(), 1);
                assert!(parts.is_empty(), "parts defaults to empty when omitted");
                // `debug` defaults when the ML line omits it (older/streamed payloads).
                assert_eq!(debug.gap_needs_exhausted, 0);
                assert_eq!(debug.gap_stop_reason, "");
                assert!(debug.gap_unresolved.is_empty());
            }
            _ => panic!("expected Done"),
        }

        let err: RetrieveEvent = serde_json::from_str(r#"{"type":"error","message":"boom"}"#).unwrap();
        assert!(matches!(err, RetrieveEvent::Error { message } if message == "boom"));
    }
}
