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

//! Background `depth=deep` web search. The inline tool dispatcher enqueues a `web_search_deep` durable
//! task and returns immediately; this module is the task handler — it runs the
//! exhaustive, politely-paced ML loop with no tool-timeout pressure and posts the
//! digest + citations back into the chat as a fresh assistant message, notifying
//! the user's sockets. Modelled on `groundedness::verify_draft`.

use std::time::Duration;

use serde_json::json;
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::error::{AppError, Result};
use crate::ml::WebCitation;
use crate::state::AppState;
use crate::ws::protocol::{CitationOut, ServerFrame};

/// Generous per-request ceiling for the deep ML call (the shared ML client sets
/// none). Matches the agent-run kill-token TTL minted by the dispatcher.
const DEEP_ML_TIMEOUT_SECS: u64 = 1800;

/// Persist web citations. `message_id` is `Some` when the caller already knows
/// the assistant message (the deep background path) and `None` when the inline
/// dispatcher keys rows by turn for the chat orchestrator to link post-stream.
/// Shared by both paths so the parse/insert logic lives in one place.
pub async fn persist_web_citations(
    pool: &sqlx::PgPool,
    turn_id: Uuid,
    message_id: Option<Uuid>,
    citations: &[WebCitation],
) {
    if citations.is_empty() {
        return;
    }
    // One UNNEST insert instead of a row-per-round-trip loop (optimisation audit,
    // L4). Parse dates into parallel arrays first.
    let ids: Vec<Uuid> = citations.iter().map(|_| Uuid::now_v7()).collect();
    let urls: Vec<String> = citations.iter().map(|c| c.url.clone()).collect();
    // Strip NUL bytes from scraped text (Postgres rejects them in `text`, error
    // 22021) — one bad row would otherwise fail the whole batch (re-audit R5).
    let titles: Vec<Option<String>> =
        citations.iter().map(|c| c.title.as_ref().map(|t| t.replace('\0', ""))).collect();
    let domains: Vec<String> = citations.iter().map(|c| c.domain.clone()).collect();
    let published: Vec<Option<time::Date>> = citations
        .iter()
        .map(|c| {
            c.published_date.as_deref().and_then(|d| {
                time::Date::parse(d, &time::format_description::well_known::Iso8601::DATE).ok()
            })
        })
        .collect();
    let fetched: Vec<time::OffsetDateTime> = citations
        .iter()
        .map(|c| {
            c.fetched_at
                .as_deref()
                .and_then(|t| {
                    time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339).ok()
                })
                .unwrap_or_else(time::OffsetDateTime::now_utc)
        })
        .collect();
    let quotes: Vec<String> = citations.iter().map(|c| c.quote_text.replace('\0', "")).collect();
    let snippets: Vec<bool> = citations.iter().map(|c| c.snippet_only).collect();
    let res = sqlx::query!(
        r#"INSERT INTO web_citations
           (id, message_id, turn_id, url, title, domain, published_date, fetched_at, quote_text, snippet_only)
           SELECT id, $2, $3, url, title, domain, published_date, fetched_at, quote_text, snippet_only
           FROM UNNEST($1::uuid[], $4::text[], $5::text[], $6::text[], $7::date[], $8::timestamptz[], $9::text[], $10::bool[])
              AS t(id, url, title, domain, published_date, fetched_at, quote_text, snippet_only)"#,
        &ids,
        message_id,
        turn_id,
        &urls,
        &titles as &[Option<String>],
        &domains,
        &published as &[Option<time::Date>],
        &fetched,
        &quotes,
        &snippets,
    )
    .execute(pool)
    .await;
    // Best-effort, but never silent: a batch failure now discards ALL rows (the
    // old loop lost one), so it must at least be visible (re-audit R5).
    if let Err(e) = res {
        tracing::warn!(error = %e, rows = citations.len(), "web_citations batch insert failed");
    }
}

/// Build the WS citation payload directly from the ML result (the deep path
/// frame carries citations inline rather than via a DB round-trip).
fn citations_out(citations: &[WebCitation]) -> Vec<CitationOut> {
    citations
        .iter()
        .map(|c| CitationOut {
            quote_text: c.quote_text.clone(),
            url: Some(c.url.clone()),
            title: c.title.clone(),
            domain: Some(c.domain.clone()),
            published_date: c.published_date.clone(),
            fetched_at: c.fetched_at.clone(),
            snippet_only: Some(c.snippet_only),
            ..Default::default()
        })
        .collect()
}

struct DeepPayload {
    run_id: Option<Uuid>,
    chat_id: Uuid,
    turn_id: Uuid,
    user_id: Option<Uuid>,
    role: String,
    query: String,
    recency: Option<String>,
    /// Per-Agent fetch cap captured at enqueue time (min-clamps the ML budget).
    max_fetches: Option<i64>,
}

fn parse(payload: &serde_json::Value) -> Result<DeepPayload> {
    let uuid = |k: &str| payload.get(k).and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
    let chat_id = uuid("chat_id").ok_or_else(|| AppError::Validation("deep web search: missing chat_id".into()))?;
    let turn_id = uuid("turn_id").ok_or_else(|| AppError::Validation("deep web search: missing turn_id".into()))?;
    let query = payload
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|q| !q.trim().is_empty())
        .ok_or_else(|| AppError::Validation("deep web search: missing query".into()))?;
    Ok(DeepPayload {
        run_id: uuid("run_id"),
        chat_id,
        turn_id,
        user_id: uuid("user_id"),
        role: payload.get("role").and_then(|v| v.as_str()).unwrap_or("user").to_string(),
        query,
        recency: payload.get("recency").and_then(|v| v.as_str()).map(str::to_string),
        max_fetches: payload.get("max_fetches").and_then(|v| v.as_i64()),
    })
}

/// Notify the user's sockets that a message was posted outside any turn.
fn notify(state: &AppState, p: &DeepPayload, message_id: Uuid, citations: Vec<CitationOut>) {
    if let Some(uid) = p.user_id {
        state.hub.send_to_user(
            uid,
            ServerFrame::ChatMessagePosted { chat_id: p.chat_id, message_id, citations },
        );
    }
}

/// Task handler. Terminal on every outcome: a failure posts an honest message
/// and returns `Ok` (a retry would double-post). All `Err` returns happen BEFORE
/// the first DB write.
pub async fn run_deep(state: &AppState, payload: &serde_json::Value) -> Result<()> {
    let p = parse(payload)?;

    // Killed/expired before we started → do nothing (the kill already set the
    // run's terminal status). `alive` is only meaningful when agents are on.
    if let Some(run_id) = p.run_id {
        if state.boot.features.agents_enabled && !crate::agent::alive(state, run_id).await {
            return Ok(());
        }
    }

    // Runtime overrides resolve HERE (not at enqueue) so the latest admin
    // config applies; the per-Agent fetch cap rode the payload.
    let mut overrides = crate::ml::web_overrides(&state.pg).await;
    overrides.max_fetches = p.max_fetches;

    // Create the streaming assistant message UP FRONT so the deep run's synthesis
    // tokens have a target (the message types into the chat live) and a reload
    // mid-run resumes from the row. `ChatMessageStarted` makes the client open the
    // bubble; `ChatMessageToken` frames stream into it; `ChatMessagePosted`
    // finalises with citations.
    let message_id = crate::chat::start_assistant_message(&state.pg, p.chat_id).await?;
    if let Some(uid) = p.user_id {
        state.hub.send_to_user(
            uid,
            ServerFrame::ChatMessageStarted { chat_id: p.chat_id, message_id, agent: None },
        );
    }

    // Stream the deep run: coarse progress is broadcast to the user's sockets
    // (the originating turn is long gone), synthesis tokens stream into the
    // message, the terminal event carries the final digest + citations.
    let mut acc = String::new();
    let mut last_flush = std::time::Instant::now();
    let result: Result<crate::ml::WebSearchResult> = async {
        let mut stream = crate::ml::web_search_stream(
            &state.http,
            &state.boot.ml.base_url,
            &p.query,
            p.recency.as_deref(),
            Some("deep"),
            &overrides,
            crate::ml::provider_overrides(state, None).await,
            Some(Duration::from_secs(DEEP_ML_TIMEOUT_SECS)),
        )
        .await?;
        loop {
            match stream.recv().await {
                Some(crate::ml::WebEvent::Progress { stage, detail, round, .. }) => {
                    if let Some(uid) = p.user_id {
                        let mut text = detail.unwrap_or(stage);
                        if let Some(r) = round {
                            text = format!("round {r}: {text}");
                        }
                        state.hub.send_to_user(
                            uid,
                            ServerFrame::WebSearchProgress {
                                chat_id: p.chat_id,
                                turn_id: p.turn_id,
                                detail: text,
                            },
                        );
                    }
                }
                Some(crate::ml::WebEvent::Token { delta }) => {
                    acc.push_str(&delta);
                    if let Some(uid) = p.user_id {
                        state.hub.send_to_user(
                            uid,
                            ServerFrame::ChatMessageToken { chat_id: p.chat_id, message_id, delta },
                        );
                    }
                    // Throttled persist (mirrors the chat turn's ~750 ms flush) so a
                    // reload mid-stream resumes the partial answer from the row.
                    if last_flush.elapsed() >= Duration::from_millis(750) {
                        crate::chat::flush_assistant_message(&state.pg, message_id, &acc).await;
                        last_flush = std::time::Instant::now();
                    }
                }
                Some(crate::ml::WebEvent::Done { digest, citations }) => {
                    break Ok(crate::ml::WebSearchResult { digest, citations });
                }
                Some(crate::ml::WebEvent::Error { message }) => {
                    break Err(AppError::Other(anyhow::anyhow!("ml web search: {message}")));
                }
                None => {
                    break Err(AppError::Other(anyhow::anyhow!(
                        "the deep search stream ended unexpectedly"
                    )));
                }
            }
        }
    }
    .await;

    // A kill issued during the long ML call → settle the partial message (so it
    // stops showing as streaming) and discard the rest (mid-ML cancel of the ML
    // side itself is a deferred follow-up).
    if let Some(run_id) = p.run_id {
        if state.boot.features.agents_enabled && !crate::agent::alive(state, run_id).await {
            let _ = crate::chat::finish_assistant_message(&state.pg, message_id, &acc, None).await;
            return Ok(());
        }
    }

    match result {
        Ok(res) => {
            // The synthesised prose IS the answer (== the streamed tokens), so no
            // header — finalise the streamed message with the authoritative text.
            crate::chat::finish_assistant_message(&state.pg, message_id, &res.digest, None).await?;
            persist_web_citations(&state.pg, p.turn_id, Some(message_id), &res.citations).await;

            let mut ev = AuditEvent::action("web_search.results", &p.role);
            ev.actor_user_id = p.user_id;
            ev.resource_type = Some("integration".into());
            ev.payload = Some(json!({
                "kind": "web_search",
                "deep": true,
                "turn_id": p.turn_id,
                "message_id": message_id,
                "result_count": res.citations.len(),
                "urls": res.citations.iter().map(|c| c.url.as_str()).collect::<Vec<_>>(),
            }));
            let _ = audit::append(&state.pg, &ev).await;

            notify(state, &p, message_id, citations_out(&res.citations));
            if let Some(run_id) = p.run_id {
                crate::agent::finish(state, run_id, "completed").await;
            }
            Ok(())
        }
        Err(e) => {
            // Honest failure settles the already-streaming message; terminal (no retry).
            let content = if acc.trim().is_empty() {
                format!("Deep web search for \u{201c}{}\u{201d} could not be completed: {e}.", p.query)
            } else {
                format!("{acc}\n\n*The deep web search did not finish cleanly: {e}.*")
            };
            let _ = crate::chat::finish_assistant_message(&state.pg, message_id, &content, None).await;
            notify(state, &p, message_id, Vec::new());
            let mut ev = AuditEvent::action("web_search.deep_failed", &p.role);
            ev.actor_user_id = p.user_id;
            ev.resource_type = Some("integration".into());
            ev.outcome = audit::AuditOutcome::Failure;
            ev.outcome_reason = Some(e.to_string());
            ev.payload = Some(json!({ "kind": "web_search", "turn_id": p.turn_id }));
            let _ = audit::append(&state.pg, &ev).await;
            if let Some(run_id) = p.run_id {
                crate::agent::finish(state, run_id, "failed").await;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_requires_chat_turn_query() {
        let chat = Uuid::now_v7();
        let turn = Uuid::now_v7();
        // Complete payload parses.
        let ok = parse(&json!({
            "chat_id": chat, "turn_id": turn, "query": "latest rust",
            "user_id": null, "role": "user", "recency": "month",
        }))
        .expect("valid payload");
        assert_eq!(ok.chat_id, chat);
        assert_eq!(ok.turn_id, turn);
        assert_eq!(ok.query, "latest rust");
        assert_eq!(ok.recency.as_deref(), Some("month"));
        assert!(ok.run_id.is_none());

        // Missing chat_id / turn_id / query each rejected.
        assert!(parse(&json!({ "turn_id": turn, "query": "x" })).is_err());
        assert!(parse(&json!({ "chat_id": chat, "query": "x" })).is_err());
        assert!(parse(&json!({ "chat_id": chat, "turn_id": turn })).is_err());
        // Blank query rejected.
        assert!(parse(&json!({ "chat_id": chat, "turn_id": turn, "query": "  " })).is_err());
    }

    #[test]
    fn web_event_lines_parse() {
        use crate::ml::WebEvent;
        let p: WebEvent = serde_json::from_str(
            r#"{"type":"progress","stage":"serp","detail":"rust release","round":2,"subq":"q"}"#,
        )
        .unwrap();
        assert!(matches!(p, WebEvent::Progress { ref stage, round: Some(2), .. } if stage == "serp"));
        // Optionals absent.
        let p2: WebEvent = serde_json::from_str(r#"{"type":"progress","stage":"plan"}"#).unwrap();
        assert!(matches!(p2, WebEvent::Progress { detail: None, round: None, .. }));
        let d: WebEvent = serde_json::from_str(
            r#"{"type":"done","digest":"Web sources:","citations":[]}"#,
        )
        .unwrap();
        assert!(matches!(d, WebEvent::Done { ref digest, .. } if digest == "Web sources:"));
        // Deep-path synthesis token.
        let t: WebEvent = serde_json::from_str(r#"{"type":"token","delta":"hello "}"#).unwrap();
        assert!(matches!(t, WebEvent::Token { ref delta } if delta == "hello "));
        let e: WebEvent = serde_json::from_str(r#"{"type":"error","message":"boom"}"#).unwrap();
        assert!(matches!(e, WebEvent::Error { ref message } if message == "boom"));
    }

    #[test]
    fn citations_out_maps_web_fields() {
        let c = WebCitation {
            url: "https://example.com/a".into(),
            title: Some("A".into()),
            domain: "example.com".into(),
            published_date: Some("2026-05-01".into()),
            fetched_at: Some("2026-06-10T10:00:00+00:00".into()),
            quote_text: "quote".into(),
            snippet_only: true,
        };
        let out = citations_out(&[c]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url.as_deref(), Some("https://example.com/a"));
        assert_eq!(out[0].domain.as_deref(), Some("example.com"));
        assert_eq!(out[0].snippet_only, Some(true));
        assert!(out[0].doc_id.is_none(), "web citation carries no document anchor");
    }
}
