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

//! Agent completions: the caller addresses one of their agents and gets the
//! whole pipeline behind it — retrieval over their libraries, the agent's tools
//! and skills, its prompt. The point of the compatibility surface: talking to
//! your own knowledge from any client that speaks this protocol.
//!
//! **History belongs to the platform, not the request.** The pipeline builds a
//! conversation from what it has stored, so the `messages` array is not
//! replayed: the final user message is taken as the question and any earlier
//! turns in the array are ignored. Continuity instead runs through the
//! `X-Fosnie-Chat-Id` response header — send back what a previous response
//! returned and the exchange continues; omit it and a new conversation starts.
//! Honest, and it keeps one turn's meaning identical whether it arrived from
//! here or from the application.
//!
//! Conversations created here are marked with their origin and kept out of the
//! chat lists: this is machine traffic, and it would drown a person's sidebar.

use std::sync::Arc;

use axum::Json;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::auth::AuthContext;
use crate::ml;
use crate::state::AppState;
use crate::ws::protocol::ServerFrame;

use super::chat::{CHAT_ID_HEADER, ChatCompletionRequest, Emit, completion, completion_id, now_unix};
use super::error::ApiError;
use super::models;
use super::sse::{StreamState, TurnGuard, citations_value, collect, stream_response};

/// Frames arrive faster than they are rendered when a local model is quick, and
/// this drain does a database read at the end of a turn, so the buffer matches
/// the interactive transport's rather than the (much simpler) scheduler drain.
const FRAME_BUFFER: usize = 256;

/// How much of the question becomes the conversation title.
const TITLE_LEN: usize = 60;

pub struct Source {
    rx: mpsc::Receiver<ServerFrame>,
    state: AppState,
    citations: Vec<Value>,
    ended: bool,
}

pub async fn run(
    state: &AppState,
    ctx: &AuthContext,
    req: ChatCompletionRequest,
    agent_id: Uuid,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let uid = models::user_id(ctx)?;
    require_visible_agent(state, ctx, uid, agent_id).await?;

    let content = req.last_user_text().ok_or_else(|| {
        ApiError::bad_request("'messages' must end with a user message containing text")
    })?;

    let chat_id = match continued_chat(headers)? {
        Some(id) => {
            verify_api_chat(state, uid, id).await?;
            id
        }
        None => create_chat(state, uid, agent_id, &content).await?,
    };

    let (tx, rx) = mpsc::channel::<ServerFrame>(FRAME_BUFFER);
    let cancel = Arc::new(Notify::new());
    let turn_id = Uuid::now_v7();

    // Spawned rather than awaited: the response head (and its conversation
    // header) has to reach the client before the first token.
    {
        let st = state.clone();
        let c = ctx.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            crate::chat::run_turn(
                &st,
                &c,
                turn_id,
                Some(chat_id),
                None,
                Some(agent_id),
                content,
                Vec::new(),
                Vec::new(),
                // Unattended: no one is watching, so the turn must not wait on a
                // person. See the approval handling in the drain below.
                true,
                None,
                None,
                None,
                &tx,
                cancel,
            )
            .await;
            // Dropping the sender closes the channel, which ends the stream.
        });
    }

    let source = Source { rx, state: state.clone(), citations: Vec::new(), ended: false };
    let guard = TurnGuard::new(cancel, None);
    let id = completion_id();
    let created = now_unix();
    let mut st = StreamState::new(guard, source, id.clone(), req.model.clone(), created);

    let mut response = if req.stream {
        stream_response(st)
    } else {
        let agg = collect(&mut st).await?;
        Json(completion(&id, &req.model, created, &agg)).into_response()
    };
    if let Ok(v) = axum::http::HeaderValue::from_str(&chat_id.to_string()) {
        response.headers_mut().insert(CHAT_ID_HEADER, v);
    }
    Ok(response)
}

/// The agent must be one this caller can see. The pipeline itself does not
/// re-check, so without this a key could drive any agent by guessing its id.
async fn require_visible_agent(
    state: &AppState,
    ctx: &AuthContext,
    uid: Uuid,
    agent_id: Uuid,
) -> Result<(), ApiError> {
    let visible = sqlx::query_scalar!(
        r#"SELECT EXISTS(
               SELECT 1 FROM agents
               WHERE id = $1 AND archived_at IS NULL
                 AND ($2 OR created_by IS NULL OR created_by = $3)
           ) AS "e!""#,
        agent_id,
        ctx.is_admin(),
        uid,
    )
    .fetch_one(&state.pg)
    .await?;
    if visible {
        Ok(())
    } else {
        Err(ApiError::model_not_found(&format!("{}{}", models::AGENT_PREFIX, agent_id)))
    }
}

fn continued_chat(headers: &HeaderMap) -> Result<Option<Uuid>, ApiError> {
    let Some(raw) = headers.get(CHAT_ID_HEADER) else { return Ok(None) };
    let s = raw
        .to_str()
        .map_err(|_| ApiError::bad_request("malformed conversation header"))?
        .trim();
    if s.is_empty() {
        return Ok(None);
    }
    Uuid::parse_str(s)
        .map(Some)
        .map_err(|_| ApiError::bad_request("malformed conversation header"))
}

/// A continued conversation must be one this surface created for this user.
///
/// Without the origin check a key could post into the owner's ordinary
/// conversations, which is a write into the application's UI from a credential
/// meant for machine traffic. Refusing as "not found" keeps a caller from
/// probing which ids exist.
async fn verify_api_chat(state: &AppState, uid: Uuid, chat_id: Uuid) -> Result<(), ApiError> {
    let ok = sqlx::query_scalar!(
        r#"SELECT EXISTS(
               SELECT 1 FROM chats
               WHERE id = $1 AND owner_user_id = $2 AND origin = 'api' AND archived_at IS NULL
           ) AS "e!""#,
        chat_id,
        uid,
    )
    .fetch_one(&state.pg)
    .await?;
    if ok {
        Ok(())
    } else {
        Err(ApiError::not_found("that conversation does not exist for this key"))
    }
}

/// Create the conversation up front rather than letting the pipeline do it.
///
/// Two reasons. The origin has to be right from the first instant, and the
/// conversation id has to be known before the response head is written — it
/// travels as a header, and a streamed body is far too late.
async fn create_chat(
    state: &AppState,
    uid: Uuid,
    agent_id: Uuid,
    content: &str,
) -> Result<Uuid, ApiError> {
    let id = Uuid::now_v7();
    let title = title_from(content);
    sqlx::query!(
        "INSERT INTO chats (id, owner_user_id, agent_id, title, origin) \
         VALUES ($1, $2, $3, $4, 'api')",
        id,
        uid,
        agent_id,
        title,
    )
    .execute(&state.pg)
    .await?;
    Ok(id)
}

/// A title from the question itself. The application titles a new conversation
/// with a background model call, which is worth it for something a person will
/// scan in a sidebar and wasted on traffic that never appears in one.
fn title_from(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "API conversation".into();
    }
    let mut out: String = flat.chars().take(TITLE_LEN).collect();
    if flat.chars().count() > TITLE_LEN {
        out.push('…');
    }
    out
}

impl super::sse::EmitSource for Source {
    async fn next(&mut self) -> Option<Emit> {
        next_emit(self).await
    }
}

/// Translate pipeline frames into the completion vocabulary.
async fn next_emit(src: &mut Source) -> Option<Emit> {
    if src.ended {
        return None;
    }
    loop {
        let frame = src.rx.recv().await?;
        match frame {
            ServerFrame::ChatToken { delta, .. } => {
                return Some(Emit::Delta { content: delta, reasoning: String::new() });
            }
            ServerFrame::ChatReasoning { delta, .. } => {
                return Some(Emit::Delta { content: String::new(), reasoning: delta });
            }
            ServerFrame::ChatCitations { citations, .. } => {
                // Held back for the terminal chunk: a client that understands
                // the field wants the whole set at once, and one that does not
                // must never see it interleaved with answer text.
                src.citations = citations
                    .iter()
                    .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
                    .collect();
            }
            ServerFrame::ChatCompleted { message_id, .. } => {
                src.ended = true;
                let usage = persisted_usage(&src.state, message_id).await;
                return Some(Emit::Final {
                    finish_reason: "stop".into(),
                    usage,
                    citations: citations_value(&src.citations),
                });
            }
            ServerFrame::ChatInterrupted { .. } => {
                src.ended = true;
                return Some(Emit::Final {
                    finish_reason: "stop".into(),
                    usage: ml::Usage::default(),
                    citations: citations_value(&src.citations),
                });
            }
            ServerFrame::ChatError { message, .. } => {
                src.ended = true;
                return Some(Emit::Failed(ApiError::unavailable(message)));
            }
            ServerFrame::AgentApproval { run_id, tool, .. } => {
                // A tool needing a person's approval cannot get one here. The
                // request is refused on their behalf so the turn continues with
                // an honest refusal instead of stalling until a timeout.
                auto_decline(&src.state, run_id, &tool).await;
            }
            // Everything else describes progress for a live interface: tool
            // phases, retrieval steps, groundedness, document edits. None of it
            // has a place in a completion, and a tool phase in particular must
            // not be dressed up as a client tool call — the tool belongs to the
            // agent and the caller has nothing to run.
            _ => {}
        }
    }
}

/// Decline a pending approval and release whatever is waiting on it.
///
/// Three steps, all needed: the decision is recorded (a compare-and-set, so a
/// person approving from the application at the same moment still wins), the
/// waiting turn is released, and the run is closed. Releasing the waiter is the
/// load-bearing part — the turn blocks on it, and would otherwise sit there
/// until the agent's approval timeout expires.
async fn auto_decline(state: &AppState, run_id: Uuid, tool: &str) {
    tracing::info!(%run_id, %tool, "declining a tool approval on an unattended API request");
    if crate::agent::decide(state, run_id, false).await.unwrap_or(false) {
        crate::agent::audit_run(
            state,
            None,
            crate::auth::PlatformRole::User.as_str(),
            "agent.rejected",
            run_id,
            json!({ "reason": "no person is present on an API request" }),
        )
        .await;
        state.approvals.resolve(run_id, false);
        crate::agent::finish(state, run_id, "rejected").await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BootConfig;
    use crate::state::AppStateBuilder;

    /// Needs Postgres + Redis (the run's state is durable and its kill-token
    /// lives in the cache); returns `None` so the test skips without them.
    async fn state() -> Option<AppState> {
        let db_url = std::env::var("DATABASE_URL").ok()?;
        let redis_url =
            std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
        let pg = crate::db::connect(&db_url, 2).await.ok()?;
        let redis = crate::cache::create_pool(&redis_url).ok()?;
        let boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
        Some(AppStateBuilder::new(pg, redis, std::sync::Arc::new(boot)).build())
    }

    /// A run parked at a gated tool call, exactly as the pipeline leaves it.
    async fn parked_run(state: &AppState) -> Uuid {
        let run_id = crate::agent::start_run(
            state, None, None, "user", None, Uuid::now_v7(), None, None, 60,
        )
        .await
        .expect("run started");
        crate::agent::request_approval(
            state, run_id, None, "user", "some_tool", &json!({}), 0,
        )
        .await
        .expect("parked at the gate");
        run_id
    }

    async fn run_status(state: &AppState, run_id: Uuid) -> String {
        sqlx::query_scalar::<_, String>(
            "SELECT status::text FROM agent_runs WHERE id = $1",
        )
        .bind(run_id)
        .fetch_one(&state.pg)
        .await
        .expect("run row")
    }

    /// An approval request on this surface is declined, and the waiting turn is
    /// released with a **negative** decision.
    ///
    /// The sign is the whole point. `resolve(run_id, true)` would compile, pass a
    /// smoke test, and hand an unattended request the one thing the approval gate
    /// exists to withhold: it would execute the gated tool with nobody present to
    /// approve it. So this asserts the value the waiter actually receives, not
    /// merely that it was woken.
    #[tokio::test]
    async fn an_approval_request_is_declined_and_the_waiter_released() {
        let Some(state) = state().await else {
            eprintln!("skip: DATABASE_URL unset");
            return;
        };
        let run_id = parked_run(&state).await;

        // Stand in for the turn blocked inside the tool gate.
        let waiter = state.approvals.register(run_id);

        let (tx, rx) = mpsc::channel::<ServerFrame>(8);
        tx.send(ServerFrame::AgentApproval {
            run_id,
            turn_id: Uuid::now_v7(),
            tool: "some_tool".into(),
            summary: "Run some_tool?".into(),
            args: json!({}),
        })
        .await
        .unwrap();
        drop(tx); // nothing follows: the stream ends after the frame is handled

        let mut src = Source { rx, state: state.clone(), citations: Vec::new(), ended: false };
        let emitted = next_emit(&mut src).await;

        assert!(
            emitted.is_none(),
            "the approval request is not rendered to the client: it is not a completion event, \
             and a caller has nothing to answer it with"
        );
        assert_eq!(
            waiter.await.expect("the waiting turn is released, not left to time out"),
            false,
            "the decision handed to the gate must be a refusal"
        );
        assert_eq!(run_status(&state, run_id).await, "rejected");
        assert!(
            !crate::agent::decide(&state, run_id, true).await.unwrap(),
            "the decision is consumed, so a later approval cannot revive the run"
        );
    }

    /// A person approving from the application at the same instant wins, and the
    /// surface does not then report a refusal that never happened.
    #[tokio::test]
    async fn a_run_already_decided_elsewhere_is_left_alone() {
        let Some(state) = state().await else {
            eprintln!("skip: DATABASE_URL unset");
            return;
        };
        let run_id = parked_run(&state).await;
        assert!(crate::agent::decide(&state, run_id, true).await.unwrap(), "approved first");

        let waiter = state.approvals.register(run_id);
        auto_decline(&state, run_id, "some_tool").await;

        assert_eq!(
            run_status(&state, run_id).await,
            "approved",
            "the earlier decision stands: the compare-and-set found nothing to decline"
        );
        // The waiter is left for whoever owns the real decision to resolve.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), waiter).await.is_err(),
            "no spurious decision is delivered"
        );
    }
}

/// Token counts as stored on the finished message.
///
/// The row is written before the completion frame is sent, so this read is
/// always current. Absent counts mean the provider reported none; zero is the
/// honest answer rather than a guess.
async fn persisted_usage(state: &AppState, message_id: Uuid) -> ml::Usage {
    match sqlx::query!(
        "SELECT prompt_tokens, completion_tokens FROM messages WHERE id = $1",
        message_id
    )
    .fetch_optional(&state.pg)
    .await
    {
        Ok(Some(r)) => ml::Usage {
            prompt_tokens: r.prompt_tokens,
            completion_tokens: r.completion_tokens,
            reasoning_tokens: None,
        },
        _ => ml::Usage::default(),
    }
}
