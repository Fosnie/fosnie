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

//! Chat turn driver. One turn: RBAC, persist user message,
//! audit, seven-layer compose, optional RAG (slot [5]), the bounded-parallel
//! tool-call loop (when the Agent has tools), then stream the final answer and
//! persist the assistant message + citations. Cancellation is graceful.

pub mod budget;
pub mod compose;

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use time::OffsetDateTime;
use tokio::sync::{mpsc, Notify, Semaphore};
use uuid::Uuid;

use crate::audit::{self, AuditEvent};
use crate::auth::rbac::{Permission, ResourceType};
use crate::auth::AuthContext;
use crate::error::{AppError, Result};
use crate::ml::{self, GenEvent, GenerateRequest, Sampling};
use crate::state::AppState;
use crate::ws::protocol::ServerFrame;

const MAX_TOOL_STEPS: usize = 5;

/// Per-agent run-control circuit breakers (agents). Read from
/// `agents.params`; sensible defaults when unset.
#[derive(Clone)]
struct RunControls {
    /// Hard cap on tool-loop iterations.
    max_steps: usize,
    /// Optional output-token budget for the whole run (None = unbounded).
    token_budget: Option<i64>,
    /// Wall-clock budget; also the TTL of the per-run kill token.
    wall_clock_secs: u64,
    /// How long an INTERACTIVE run waits at an approval card before auto-rejecting.
    approval_timeout_secs: u64,
    /// How long an UNATTENDED run may sit awaiting the owner's approval (long —
    /// a nightly run must survive until the owner wakes; never auto-reject early).
    unattended_approval_ttl_secs: u64,
}

impl Default for RunControls {
    fn default() -> Self {
        Self {
            max_steps: MAX_TOOL_STEPS,
            token_budget: None,
            wall_clock_secs: 300,
            approval_timeout_secs: 600,
            unattended_approval_ttl_secs: 86_400,
        }
    }
}

struct AgentConfig {
    system_prompt: Option<String>,
    sampling: Sampling,
    tool_concurrency: usize,
    tools: Vec<String>,
    controls: RunControls,
    /// Per-Agent web-search budget (params.web_depth_max / web_max_fetches).
    web: Option<crate::tools::WebBudget>,
}

/// Run one turn, reporting failures as a `chat.error` frame.
#[allow(clippy::too_many_arguments)]
/// A per-turn file attachment: extracted text injected into this turn's prompt, plus
/// (on a code-interpreter host, under the size cap) the raw bytes so the sandbox can
/// read the original file by name.
pub struct Attachment {
    /// Durable `chat_attachments` row id — used to backfill the message link after
    /// the user message persists, so the file renders under it / in the docs rail.
    pub id: Uuid,
    pub filename: String,
    pub text: String,
    pub bytes: Option<Vec<u8>>,
    /// MIME type (e.g. `image/jpeg`). Present for images → drives the inline vision path.
    pub mime: Option<String>,
}

#[allow(clippy::too_many_arguments)]
/// Prepare an in-place regenerate (also drives edit + restart-from-here): resolve
/// the anchoring **user** message, delete every message at/after the branch point,
/// optionally edit the anchor, and return `(anchor_user_msg_id, content)` for a
/// `run_turn(..., reuse_user_msg = Some(anchor), ...)`. Destructive + owner-gated.
///
/// - `from_message_id` is an **assistant** answer → anchor = the user message just
///   before it; the answer and anything after it are dropped.
/// - `from_message_id` is a **user** message → anchor = itself; its answer and
///   anything after are dropped; `edit` (if set) rewrites its content first.
///
/// Dependent rows (citations, feedback, reviews, attachments) fall away by
/// `ON DELETE CASCADE`; artefacts detach via `ON DELETE SET NULL`.
pub async fn regenerate_prepare(
    state: &AppState,
    ctx: &AuthContext,
    chat_id: Uuid,
    from_message_id: Uuid,
    edit: Option<String>,
) -> Result<(Uuid, String)> {
    // Regenerate mutates history — require ownership (or admin), not mere read.
    let owner = sqlx::query_scalar!("SELECT owner_user_id FROM chats WHERE id = $1", chat_id)
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("chat not found".into()))?;
    if ctx.user_id != Some(owner) && !ctx.is_admin() {
        return Err(AppError::Forbidden("not permitted to modify this chat".into()));
    }

    let from = sqlx::query!(
        r#"SELECT role::text AS "role!", sequence_number AS "seq!", content
           FROM messages WHERE id = $1 AND chat_id = $2"#,
        from_message_id, chat_id
    )
    .fetch_optional(&state.pg)
    .await?
    .ok_or_else(|| AppError::Validation("message not found".into()))?;

    let (anchor_id, anchor_content, delete_from_seq) = if from.role == "assistant" {
        // Anchor = the user turn immediately preceding this answer.
        let prev = sqlx::query!(
            r#"SELECT id, content FROM messages
               WHERE chat_id = $1 AND role = 'user' AND sequence_number < $2
               ORDER BY sequence_number DESC LIMIT 1"#,
            chat_id, from.seq
        )
        .fetch_optional(&state.pg)
        .await?
        .ok_or_else(|| AppError::Validation("no prompt to regenerate".into()))?;
        (prev.id, prev.content, from.seq) // drop the answer (>= its seq) + anything after
    } else {
        // Anchor = this user message; optionally edit it in place first.
        let content = if let Some(new_text) = edit {
            sqlx::query!("UPDATE messages SET content = $1 WHERE id = $2", new_text, from_message_id)
                .execute(&state.pg)
                .await?;
            new_text
        } else {
            from.content
        };
        (from_message_id, content, from.seq + 1) // keep the prompt, drop everything after it
    };

    sqlx::query!(
        "DELETE FROM messages WHERE chat_id = $1 AND sequence_number >= $2",
        chat_id, delete_from_seq
    )
    .execute(&state.pg)
    .await?;

    Ok((anchor_id, anchor_content))
}

pub async fn run_turn(
    state: &AppState,
    ctx: &AuthContext,
    turn_id: Uuid,
    chat_id: Option<Uuid>,
    project_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    content: String,
    attachments: Vec<Attachment>,
    kb_ids: Vec<Uuid>,
    unattended: bool,
    // Regenerate: reuse this existing user row as the turn's query instead of
    // inserting a new one (branch replace — see `ChatRegenerate`). `None` = normal send.
    reuse_user_msg: Option<Uuid>,
    reasoning: Option<crate::reasoning::ReasoningSpec>,
    // Per-turn LLM provider pick (composer dropdown, multi-LLM). `None` = the chat's
    // remembered provider, else the deployment default.
    llm_provider_id: Option<Uuid>,
    tx: &mpsc::Sender<ServerFrame>,
    cancel: Arc<Notify>,
) {
    if let Err(e) =
        run_turn_inner(state, ctx, turn_id, chat_id, project_id, agent_id, content, attachments, kb_ids, unattended, reuse_user_msg, reasoning, llm_provider_id, tx, cancel).await
    {
        let _ = tx
            .send(ServerFrame::ChatError { turn_id: Some(turn_id), message: e.to_string() })
            .await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_turn_inner(
    state: &AppState,
    ctx: &AuthContext,
    turn_id: Uuid,
    chat_id: Option<Uuid>,
    project_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    content: String,
    attachments: Vec<Attachment>,
    kb_ids: Vec<Uuid>,
    unattended: bool,
    reuse_user_msg: Option<Uuid>,
    reasoning: Option<crate::reasoning::ReasoningSpec>,
    llm_provider_id: Option<Uuid>,
    tx: &mpsc::Sender<ServerFrame>,
    cancel: Arc<Notify>,
) -> Result<()> {
    // New chats start with a generic placeholder; the real title is named by the
    // LLM in a background task below (keeps TTFT untouched — no naming call before
    // the first token).
    let (chat_id, chat_project_id, chat_agent_id, chat_mode, created) =
        resolve_chat(state, ctx, chat_id, project_id, agent_id, "New chat").await?;
    if created {
        let _ = tx.send(ServerFrame::ChatCreated { chat_id }).await;
        // Title the chat from the prompt off the hot path. On any failure the chat
        // keeps the "New chat" placeholder. The `title = 'New chat'` guard avoids
        // clobbering a user rename that raced this call.
        let st = state.clone();
        let prompt = content.clone();
        let uid = ctx.user_id;
        tokio::spawn(async move {
            if let Some(owner) = uid {
                if let Some(title) = name_chat(&st, Some(owner), &prompt).await {
                    let _ = sqlx::query!(
                        "UPDATE chats SET title = $2 WHERE id = $1 AND title = 'New chat'",
                        chat_id,
                        title
                    )
                    .execute(&st.pg)
                    .await;
                    st.hub.send_invalidate(&[owner], vec![vec!["chats".into()]]);
                }
            }
        });
    }

    // Resolve the effective LLM provider ONCE for this turn (multi-LLM): an explicit
    // composer pick wins, else the chat's remembered provider, else the deployment
    // default. A visible explicit pick is persisted to the chat so it sticks — and a
    // later regenerate reuses it, since the regenerate frame carries no per-turn field.
    if let Some(rid) = llm_provider_id {
        if crate::providers::visible_llm(&state.pg, ctx.user_id, rid).await.unwrap_or(false) {
            let _ = sqlx::query!("UPDATE chats SET llm_provider_id = $2 WHERE id = $1", chat_id, rid)
                .execute(&state.pg)
                .await;
        }
    }
    let llm_sel = crate::providers::resolve_llm(&state.pg, state.message_key, ctx.user_id, Some(chat_id), llm_provider_id)
        .await
        .ok()
        .flatten();

    // Per-turn Library attachments (e.g. an automation's chosen Libraries):
    // materialise as chat_kb_links so the RAG allow-list below picks them up this
    // very turn. The intersection (∩ caller-can-read) still governs access — an
    // attachment the user cannot read contributes nothing (fail-closed).
    if !kb_ids.is_empty() {
        // One UNNEST insert instead of a per-kb round-trip on the pre-stream path
        //.
        sqlx::query!(
            "INSERT INTO chat_kb_links (chat_id, kb_id, attached_by) \
             SELECT $1, kb_id, $3 FROM UNNEST($2::uuid[]) AS t(kb_id) \
             ON CONFLICT (chat_id, kb_id) DO NOTHING",
            chat_id,
            &kb_ids,
            ctx.user_id,
        )
        .execute(&state.pg)
        .await?;
    }

    // Persist the user message — typed text + a compact attachment marker; the
    // full attachment text is injected into the prompt below (model only). On a
    // regenerate (`reuse_user_msg`) the user row already exists and its trailing
    // answer has been deleted, so we skip the insert entirely and reuse the row —
    // `load_history` below then picks it up as this turn's query. Attachment TEXT
    // is not re-injected on regenerate (the durable rows stay linked and render).
    let user_msg_id = if let Some(id) = reuse_user_msg {
        id
    } else {
        let stored_content = if attachments.is_empty() {
            content.clone()
        } else {
            let names = attachments.iter().map(|a| a.filename.as_str()).collect::<Vec<_>>().join(", ");
            format!("{content}\n\n[attached: {names}]")
        };
        let user_seq = next_seq(&state.pg, chat_id).await?;
        let new_id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO messages (id, chat_id, role, sequence_number, content) VALUES ($1, $2, 'user', $3, $4)",
            new_id, chat_id, user_seq, stored_content
        )
        .execute(&state.pg)
        .await?;
        // Link the durable attachment rows to this message (and chat) so they render
        // under it and in the docs rail, live + after reload. Scoped to the caller's own
        // still-unlinked uploads. Mirrors the artefact `message_id` backfill below.
        if !attachments.is_empty() {
            let ids: Vec<Uuid> = attachments.iter().map(|a| a.id).collect();
            let _ = sqlx::query!(
                "UPDATE chat_attachments SET message_id = $1, chat_id = $2 \
                 WHERE id = ANY($3) AND owner_user_id = $4 AND message_id IS NULL",
                new_id,
                chat_id,
                &ids,
                ctx.user_id,
            )
            .execute(&state.pg)
            .await;
        }
        new_id
    };
    audit_event(state, ctx, "chat.message.sent", chat_id, json!({ "message_id": user_msg_id }));

    // Parallelise the independent per-turn loads: none
    // consumes another's output, so one try_join! replaces five serial round-trips
    // before the first token. `start_run` (needs the agent), RAG `retrieve` (needs
    // the allow-list) and history compaction stay sequential after the join.
    let (agent, allow, skills, memory_facts, history) = tokio::try_join!(
        load_agent(&state.pg, chat_agent_id),
        crate::kb::retrieval_allowlist(&state.pg, ctx, chat_id, chat_project_id, chat_agent_id),
        load_skills(&state.pg, chat_agent_id),
        crate::http::memory::recall(state, ctx, chat_project_id, &content),
        load_history(&state.pg, chat_id),
    )?;

    // An *agentic* run = a configured Agent that can take a gated (state-changing /
    // egress) action. Such runs get a durable `agent_runs` row + a per-run kill-token
    // (so in-loop tools are guarded and the run is revocable/auditable). Plain-chat
    // agents (no gated tools) skip this entirely — no overhead.
    // `generate_artefact` is excluded: it is hidden from the LLM (never a model tool
    // call) and only the deterministic drafter-fallback uses it, so it must NOT make
    // an agent agentic — otherwise every default agent would spin up an agent_run and
    // pause the auto-draft for approval (which blocks `chat.completed`).
    // MCP tools in scope this turn (FEATURE B1) — namespaced, RBAC-filtered. Computed
    // once here so it also decides agenticity (MCP tools are gated-by-default, so they
    // need a run for the kill-token + HITL), then folded into the tool defs below.
    // Per-agent MCP scoping: only servers assigned to this agent (namespaced `slug__*`
    // entries in its tool list) are in scope this turn — so one agent's servers never
    // leak into another's catalogue.
    let mcp_allowed = crate::mcp::allowed_slugs(&agent.tools);
    let mcp_defs = crate::mcp::session_tool_defs(state, ctx, &mcp_allowed).await;
    let agentic = (chat_agent_id.is_some()
        && agent.tools.iter().any(|t| crate::tools::gated(t) && t != "generate_artefact"))
        || !mcp_defs.is_empty();
    let run_id: Option<Uuid> = if agentic {
        Some(
            crate::agent::start_run(
                state, chat_agent_id, ctx.user_id, ctx.role.as_str(), Some(chat_id), turn_id,
                chat_project_id, None, agent.controls.wall_clock_secs,
            )
            .await?,
        )
    } else {
        None
    };

    // Layer [5] RAG. Authorisation is the INTERSECTION allow-list (Libraries
    //): KBs attached to this context (agent-bound ∪ project-linked ∪
    // chat-linked) ∩ KBs this user may personally read — resolved fresh from
    // Postgres, so grant/attach/detach take effect on this very turn. The single
    // `retrieve` pre-filters the shared collection by `knowledge_base_id IN
    // <allow-list>`. Fail-closed: an empty allow-list means NO retrieval.
    let mut rag_citations: Vec<ml::Citation> = Vec::new();
    let mut rag_context: Option<String> = None;
    // per-part synthesis slices (empty ⇒ unified synthesis).
    let mut rag_parts: Vec<ml::SynthPart> = Vec::new();
    // the retrieval Coverage summary, captured from the `summary`
    // progress event and persisted as a completed Agent-activity step (not transient).
    let mut rag_summary: Option<String> = None;
    // Iterative-retrieval gap signals from the Done `debug` object — decide whether to
    // offer the model the `search_library` top-up tool. Default = "no gaps" so a turn
    // without retrieval never advertises the tool.
    let mut rag_gap_debug: ml::RetrieveDebug = ml::RetrieveDebug::default();
    // the turn's KB allow-list + source-ACL deny-list, hoisted out of the retrieval block
    // so the search_library tool + its dispatch can reuse the exact same scoping.
    let mut turn_kb_ids: Vec<String> = Vec::new();
    let mut turn_deny_doc_ids: Vec<String> = Vec::new();
    let mut cancelled = false;
    // per-turn phase timer (debug-level table emitted at turn end).
    let mut phases = TurnPhases::new();
    if !allow.is_empty() {
        let retrieve_t = std::time::Instant::now();
        let kb_ids: Vec<String> = allow.iter().map(|id| id.to_string()).collect();
        // Retrieval-time deny-list: KB documents in scope
        // whose connected-source ACL (under an `enforce` mapping) excludes this
        // caller. Passed to ML as a Qdrant `must_not doc_id` filter so RAG never
        // surfaces a chunk the workspace read-path would 404 — the ethical wall
        // extends to retrieval. Core default is an empty list (byte-identical); the
        // Enterprise seam consults its materialised KB entitlements and fails closed
        // on its own error. This narrows *within* the allow-list, never widens it.
        let deny_ids = state.rbac.denied_kb_doc_ids(&state.pg, ctx, &allow).await?;
        let deny_doc_ids: Vec<String> = deny_ids.iter().map(|id| id.to_string()).collect();
        // Hoist for the model-driven search_library tool (same allow-list + deny-list scoping).
        turn_kb_ids = kb_ids.clone();
        turn_deny_doc_ids = deny_doc_ids.clone();
        // Audit every retrieval with the resolved allow-list (an investigator can prove which
        // knowledge bases were in scope) and how many documents the source-ACL deny-list hid.
        audit_event(
            state,
            ctx,
            "rag.retrieve",
            chat_id,
            json!({ "kb_ids": kb_ids, "denied_count": deny_doc_ids.len() }),
        );
        let rag_ov = ml::rag_overrides(&state.pg).await;
        // Display gate: the retrieval Coverage summary (part/section/expansion counts) is useful
        // when tuning retrieval but noise for an ordinary user, so it is shown only when the
        // operator turns diagnostics on. Off by default; read live so it toggles without a restart.
        let show_diagnostics = crate::config::runtime::get(&state.pg, "rag.show_diagnostics")
            .await
            .ok()
            .flatten()
            .map(|e| e.value == "true")
            .unwrap_or(false);
        // Stream retrieval progress as a `retrieve` tool so the client shows live
        // "Searching your library…" activity before the first token. This phase can run for
        // minutes under load, so it MUST honour cancel: the select below races the stream against
        // the turn's cancel Notify. On cancel we drop the stream (cancelling the upstream retrieve)
        // and abort the turn cleanly below — we never fall through to generation, so the consumed
        // Notify permit isn't stolen from a later loop.
        let _ = tx
            .send(ServerFrame::ChatTool { turn_id, name: "retrieve".into(), phase: "started".into(), detail: None })
            .await;
        match ml::retrieve_stream(
            &state.http,
            &state.boot.ml.base_url,
            &content,
            &kb_ids,
            &deny_doc_ids,
            &rag_ov,
            ml::provider_overrides_with_llm(state, ctx.user_id, llm_sel.as_ref()).await,
            Some(retrieve_timeout(&state.pg).await),
        )
        .await
        {
            Ok(mut stream) => loop {
                tokio::select! {
                    ev = stream.recv() => match ev {
                        Some(ml::RetrieveEvent::Progress { stage, detail }) => {
                            // The terminal `summary` event is the Coverage line. It is captured and
                            // forwarded on a PERSISTENT `summary` phase (not the transient
                            // `progress` label) so it survives as a completed activity step — but
                            // only when diagnostics are on. With diagnostics off it is dropped
                            // entirely (no capture, no frame), so no Coverage step reaches the UI;
                            // the human progress steps below are always shown.
                            if stage == "summary" {
                                if show_diagnostics {
                                    if let Some(text) = detail.clone() {
                                        rag_summary = Some(text);
                                    }
                                    let _ = tx
                                        .send(ServerFrame::ChatTool {
                                            turn_id,
                                            name: "retrieve".into(),
                                            phase: "summary".into(),
                                            detail: Some(detail.unwrap_or(stage)),
                                        })
                                        .await;
                                }
                            } else {
                                let _ = tx
                                    .send(ServerFrame::ChatTool {
                                        turn_id,
                                        name: "retrieve".into(),
                                        phase: "progress".into(),
                                        detail: Some(detail.unwrap_or(stage)),
                                    })
                                    .await;
                            }
                        }
                        Some(ml::RetrieveEvent::Done { context, citations, parts, debug }) => {
                            if !context.trim().is_empty() {
                                rag_context = Some(context);
                                rag_citations = citations;
                                rag_parts = parts;
                            }
                            rag_gap_debug = debug;
                            break;
                        }
                        Some(ml::RetrieveEvent::Error { message }) => {
                            tracing::warn!(error = %message, "retrieve failed; proceeding without RAG context");
                            break;
                        }
                        None => break,
                    },
                    _ = cancel.notified() => { cancelled = true; break; }
                }
            },
            Err(e) => tracing::warn!(error = %e, "retrieve stream failed; proceeding without RAG context"),
        }
        let _ = tx
            .send(ServerFrame::ChatTool { turn_id, name: "retrieve".into(), phase: "finished".into(), detail: None })
            .await;
        phases.mark("retrieve", retrieve_t.elapsed());
    }

    // Cancelled during retrieval — abort cleanly before composing/generating. No
    // assistant row exists yet, so report the interruption with no message id.
    if cancelled {
        let _ = tx.send(ServerFrame::ChatInterrupted { turn_id, message_id: None }).await;
        return Ok(());
    }

    // Seven-layer compose with the stable-prefix budget allocator: size [1]–[4] first,
    // then split the remainder between [5] RAG (trimmed to fit) and [6] history (compacted
    // to fit). The
    // prefix is never trimmed for a lower slot — that is what keeps the prompt
    // cache warm across turns.
    let prompt = effective_prompt(&agent.system_prompt, &state.boot.default_system_prompt);
    // `skills` and `memory_facts` were loaded in the join above.

    // [1]–[4] prefix WITHOUT RAG, measured and reserved first (no [5] note here — it
    // measures only the stable cacheable prefix).
    let prefix = compose::build_system(&prompt, ctx, &skills, &memory_facts, None, unattended, false);

    // [5] general-knowledge fallback: when nothing was retrieved and the workspace mode
    // permits it (general/legal, not Deep Research), let the agent answer from general
    // knowledge instead of stalling on a strict "work only from documents" prompt.
    let gk_fallback = rag_context.is_none() && chat_mode != "research";
    // Unknown budget (ML offline) → a large sentinel disables trimming/compaction
    // (the old best-effort behaviour), so a turn never fails on budgeting.
    let budget_total = context_budget(state).await.unwrap_or(i64::MAX / 8);
    let answer_reserve = (budget_total / 4).clamp(64, 1024);
    let rag_tokens = rag_context.as_deref().map(est_tokens).unwrap_or(0);
    let alloc = budget::allocate(budget_total, est_tokens(&prefix), rag_tokens, answer_reserve);

    // Trim [5] RAG to its budget (rare guard; chunks are already capped upstream).
    let rag_trimmed = rag_context.as_deref().map(|r| trim_to_tokens(r, alloc.rag_budget));
    let system =
        compose::build_system(&prompt, ctx, &skills, &memory_facts, rag_trimmed.as_deref(), unattended, gk_fallback);

    // `history` was loaded in the join above; compact it to the budget now.
    // Notify the user as the chat approaches the budget (compaction still runs).
    maybe_warn_context(chat_id, est_tokens(&system), &history, budget_total, answer_reserve, tx).await;
    let history =
        compact_history(state, chat_id, turn_id, history, alloc.history_budget, &agent.sampling, tx).await;
    let mut messages = compose::build_messages(system, history);

    // Only advertise tools this host can actually run (capability gate) — a
    // disabled capability (e.g. code_interpreter on a macOS host) is never shown.
    // Also honour per-group feature flags (Tier-2 #8): code-interpreter is hidden
    // from a user whose group has it disabled, even where the host supports it.
    // `generate_artefact` is NOT advertised to the LLM: small models misuse it
    // (echo its schema, leave content empty). It is a platform marker — the
    // turn's answer is saved as the artefact post-hoc (the fallback below).
    let ci_ok = state.features.enabled_for(state, ctx, "code_interpreter").await;
    // Per-deployment native-tool overrides: a tool an
    // admin switched off is dropped here (so it also leaves `ci_files` detection
    // consistent), and its description is replaced inside `defs`. An empty map
    // leaves the defs byte-identical to the code default (prefix-cache safety).
    let tool_overrides = crate::tools::load_overrides(&state.pg).await.unwrap_or_default();
    let enabled_tools: Vec<String> = agent
        .tools
        .iter()
        .filter(|t| t.as_str() != "generate_artefact")
        .filter(|t| crate::tools::host_enabled(t, &state.boot.features))
        .filter(|t| t.as_str() != "code_interpreter" || ci_ok)
        .filter(|t| tool_overrides.get(t.as_str()).map(|o| o.enabled).unwrap_or(true))
        .cloned()
        .collect();
    let mut tool_defs = crate::tools::defs(&enabled_tools, &tool_overrides);
    tool_defs.extend(mcp_defs); // namespaced MCP tools ride the same defs/dispatch rails
    // Custom HTTP tools the Agent selected — enabled +
    // approved only. Appended AFTER MCP so an agent with no custom tools keeps a
    // byte-identical layout (empty-vec extend is a no-op → prefix-cache safe).
    let (custom_defs, custom_tools) =
        crate::tools::custom::load_enabled_custom(&state.pg, &agent.tools).await;
    tool_defs.extend(custom_defs);

    // Code-interpreter input files = this turn's attachments that carry bytes, but only
    // when the tool is actually advertised. The sandbox writes each into its working dir
    // by name, so the model's code can read the original file (e.g. the real .xlsx).
    let ci_on = enabled_tools.iter().any(|t| t == "code_interpreter");
    let ci_files: Vec<crate::code_interpreter::InputFile> = if ci_on {
        attachments
            .iter()
            .filter_map(|a| {
                a.bytes.as_ref().map(|b| crate::code_interpreter::InputFile {
                    name: sandbox_name(&a.filename),
                    bytes: b.clone(),
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    // Inject per-turn attachment text into the current (last) user message — for the
    // model only; the persisted message keeps just the `[attached: …]` marker. When the
    // file is in the sandbox and its text is large, inject a compact preview and let the
    // model read the full data with code (size-adaptive).
    if !attachments.is_empty() {
        // An image goes to a vision-capable model as an image part; for any other
        // attachment (and for images when the model can't see) the extracted/OCR'd
        // text is injected as before. Resolve the model's vision capability only
        // when there is actually an image with bytes to send.
        let is_image = |a: &Attachment| a.bytes.is_some() && a.mime.as_deref().is_some_and(|m| m.starts_with("image/"));
        let vision_on = attachments.iter().any(is_image) && {
            // Match the vision probe to the turn's SELECTED llm (multi-LLM), so an
            // image goes as an image part only when the chosen model can see it.
            let (b, m) = match &llm_sel {
                Some(p) => (p.base_url.clone(), p.model.clone()),
                None => (None, None),
            };
            crate::vision::detect(b.as_deref(), m.as_deref(), None)
        };

        let (images, texts): (Vec<&Attachment>, Vec<&Attachment>) = if vision_on {
            attachments.iter().partition(|a| is_image(a))
        } else {
            (Vec::new(), attachments.iter().collect())
        };

        let blocks = texts
            .iter()
            .map(|a| attachment_block(&sandbox_name(&a.filename), &a.text, ci_on && a.bytes.is_some()))
            .collect::<Vec<_>>()
            .join("\n\n");

        if let Some(last) = messages.last_mut() {
            if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                let prev = last.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
                let text_part = if blocks.is_empty() { prev } else { format!("{prev}\n\n{blocks}") };
                if images.is_empty() {
                    last["content"] = json!(text_part);
                } else {
                    // OpenAI-shape multimodal content: one text part + an image_url part
                    // per image (data URL). The ML adapters translate image_url to the
                    // native Anthropic/Gemini image block.
                    let mut parts: Vec<Value> = Vec::with_capacity(1 + images.len());
                    parts.push(json!({ "type": "text", "text": text_part }));
                    for a in &images {
                        if let Some(bytes) = a.bytes.as_ref() {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                            let mime = a.mime.as_deref().unwrap_or("image/png");
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": { "url": format!("data:{mime};base64,{b64}") }
                            }));
                        }
                    }
                    last["content"] = json!(parts);
                }
            }
        }
    }

    let mut interrupted = false;
    let mut activity = Activity::default();
    // The auto-RAG retrieve isn't a tool-loop call, so `observe` below won't
    // record it — persist it here so the "Searched the library" step survives a
    // reload (else activity is empty -> NULL -> the inline step list vanishes).
    if !allow.is_empty() {
        activity.tools.push("retrieve".to_string());
    }

    // Persist the assistant row UP FRONT (empty), so a reload / return mid-turn
    // finds it and shows the answer as it streams — the DB row is the resume
    // channel. Its content + activity are UPDATEd incrementally below; the terminal
    // timestamp (completed | interrupted) is set at the end. `chat.started` hands the
    // client the real message id immediately so live tokens + the row reconcile.
    let asst_seq = next_seq(&state.pg, chat_id).await?;
    let asst_id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO messages (id, chat_id, role, sequence_number, content, turn_id) VALUES ($1, $2, 'assistant', $3, '', $4)",
        asst_id, chat_id, asst_seq, turn_id
    )
    .execute(&state.pg)
    .await?;
    let _ = tx.send(ServerFrame::ChatStarted { turn_id, chat_id, message_id: asst_id }).await;

    // Tool-call loop (only if the Agent has tools).
    let mut ready_answer: Option<String> = None;
    let mut loop_usage: Option<ml::Usage> = None;
    // The loop ran at reduced reasoning (a heavy reasoning model) → don't reuse its
    // low-reasoning terminal content as the answer; regenerate at full reasoning below,
    // keeping `ready_answer` only as an empty-answer fallback.
    let mut loop_capped = false;

    // Model-driven RAG top-up gating: advertise the `search_library` tool so the MAIN model can
    // search the library again when the automatic first pass fell short. `gaps_only` (default)
    // offers it ONLY when the iterative pass left unresolved gaps → healthy turns keep their
    // byte-identical fast path; `always` offers it on every retrieval turn; `off` never. Injected
    // here (not via `agent.tools`) because it is a dynamic, gap-driven system tool — but the
    // deployment `tool_overrides` kill-switch wins, and a no-KB turn never gets it (fail-closed).
    let mut rag_tool_ctx: Option<crate::tools::RagToolCtx> = None;
    let mut model_search_commentary = true;
    // Under the `midstream` locus the tool is withheld from the pre-stream tool loop and handed
    // to the streaming answer instead; this carries its schema through to the stream.
    let mut midstream_tool_def: Option<Value> = None;
    let mut model_search_locus = "loop".to_string();
    if !turn_kb_ids.is_empty()
        && tool_overrides.get("search_library").map(|o| o.enabled).unwrap_or(true)
    {
        let cfg = |k: &'static str| crate::config::runtime::get(&state.pg, k);
        let mode = cfg("rag.model_search_mode")
            .await
            .ok()
            .flatten()
            .map(|e| e.value)
            .unwrap_or_else(|| "gaps_only".to_string());
        let gaps_left = rag_gap_debug.gap_needs_exhausted > 0
            || (!rag_gap_debug.gap_stop_reason.is_empty()
                && rag_gap_debug.gap_stop_reason != "sufficient");
        let advertise = crate::tools::advertise_search_library(&mode, gaps_left);
        if advertise {
            let geti = |v: Option<crate::config::runtime::ConfigEntry>, d: u64| {
                v.and_then(|e| e.value.parse::<u64>().ok()).unwrap_or(d)
            };
            let max_calls = geti(cfg("rag.model_search_max_calls").await.ok().flatten(), 4) as u32;
            let deadline = geti(cfg("rag.model_search_deadline_secs").await.ok().flatten(), 20);
            model_search_commentary = cfg("rag.model_search_show_commentary")
                .await
                .ok()
                .flatten()
                .map(|e| e.value == "true")
                .unwrap_or(true);
            let sl_def = crate::tools::search_library_def(&rag_gap_debug.gap_unresolved);
            // Locus decides WHERE the tool runs. `loop` (default) advertises it in the pre-stream
            // tool loop — a non-streamed step before the answer. `midstream` withholds it from
            // that loop and hands it to the streaming answer, so the model can search between
            // segments of one bubble, saving a step on gap-turns. Only unified synthesis carries
            // tools to the stream (checked below); per_part ignores this.
            model_search_locus = cfg("rag.model_search_locus")
                .await
                .ok()
                .flatten()
                .map(|e| e.value)
                .unwrap_or_else(|| "loop".to_string());
            if model_search_locus == "midstream" {
                midstream_tool_def = Some(sl_def);
            } else {
                tool_defs.push(sl_def);
            }
            rag_tool_ctx = Some(crate::tools::RagToolCtx::new(
                turn_kb_ids.clone(),
                turn_deny_doc_ids.clone(),
                max_calls,
                deadline,
                rag_context.as_deref(),
                rag_citations.len(),
            ));
        }
    }

    if !tool_defs.is_empty() {
        let outcome = run_tool_loop(
            state, ctx, turn_id, chat_id, chat_project_id, &agent, run_id, &tool_defs, &ci_files, &custom_tools, rag_tool_ctx.as_ref(), &mut messages, &mut activity, reasoning.as_ref(), llm_sel.as_ref(), tx, &cancel, &mut phases, model_search_commentary,
        )
        .await?;
        interrupted = outcome.interrupted;
        ready_answer = outcome.ready_answer;
        loop_usage = outcome.final_usage;
        loop_capped = outcome.capped;
    }

    // `search_library` citations (from the pre-stream loop AND any mid-stream calls) are merged
    // into the turn's list after streaming, just before persistence — see below — so both land in
    // the single ChatCitations frame. `rag_tool_ctx` is kept alive until then.

    // attach the retrieval Coverage summary AFTER the tool loop, so `observe`'s
    // wholesale `steps` reassignment can't clobber it. Persisted + rendered as a step.
    activity.coverage = rag_summary.clone();

    // Final answer: stream it (tools omitted — the model now just answers).
    let mut acc = String::new();
    // Reasoning trace streamed on the dedicated channel (kept out of `acc`/the
    // answer); folded into the persisted content as `<think>…</think>` so a reload
    // reconstructs it via `splitThink`.
    let mut reasoning_acc = String::new();
    let mut prompt_tokens: Option<i32> = None;
    let mut completion_tokens: Option<i32> = None;
    let mut reasoning_tokens: Option<i32> = None;
    let mut model_used: Option<String> = None;
    let mut errored: Option<String> = None;
    // Terminal finish reason of the streamed answer (`stop` | `length` | …). Kept so
    // an empty-but-`length` answer (a reasoning model that spent the whole
    // `max_completion_tokens` budget thinking) is surfaced + retried instead of
    // persisting a silent blank, and so the audit event records the real reason.
    let mut finish_reason: Option<String> = None;

    // `detached` = the socket went away mid-stream. We keep generating + persisting
    // (the turn is NOT killed) and just stop pushing frames, so a reopen resumes the
    // answer from the DB row. Content is flushed to the row on a ~750 ms throttle
    // (inside `stream_generate`).
    let mut detached = false;

    // Reuse the answer the tool loop already produced. When the model finished a
    // tool-using turn it returned a clean final answer (qwen3 splits reasoning out
    // of `content`); the loop hands it back as `ready_answer`. Use it verbatim and
    // skip the streaming reroll below — that second pass can come back empty on a
    // small model (whole budget spent inside <think>), which left the turn blank.
    // EXCEPTION: when the loop ran at reduced reasoning (`loop_capped`), the terminal
    // content is low-reasoning — skip reuse so the streaming path regenerates a
    // full-reasoning answer; `ready_answer` stays as an empty-answer fallback.
    if !interrupted && !loop_capped {
        if let Some(answer) = ready_answer.clone() {
            // Sanitise before it ever reaches the user: this reuse path is exactly how a
            // tool-call/UUID leak reached the answer. A complete
            // string in hand → safe to scrub (no token-fragmentation risk).
            acc = strip_tool_leak(&answer);
            if !detached && tx.send(ServerFrame::ChatToken { turn_id, delta: acc.clone() }).await.is_err() {
                detached = true;
            }
            // No final stream to carry usage; take the terminating step's usage so
            // the audit event + token metrics aren't zeroed. `model_used` stays None
            // (the tool-step response carries no model id).
            prompt_tokens = loop_usage.as_ref().and_then(|u| u.prompt_tokens);
            completion_tokens = loop_usage.as_ref().and_then(|u| u.completion_tokens);
            reasoning_tokens = loop_usage.as_ref().and_then(|u| u.reasoning_tokens);
        }
    }

    // Otherwise (no captured answer — e.g. the agent answered with no tool calls):
    // stream the answer live, preserving TTFT.
    let generate_t = std::time::Instant::now();
    if !interrupted && acc.is_empty() {
        // The sampling for this pass — a retry (below) bumps `max_tokens` on it.
        let mut sampling = agent.sampling.clone();
        // on a retrieval turn, cap the FINAL answer's reasoning effort
        // to `rag.answer_reasoning_effort` (default medium). Clamps DOWN only — a lower
        // per-turn choice is respected — because the per-sub-question mini-answers already
        // did the local work, so heavy synthesis reasoning is mostly wasted latency.
        let answer_reasoning: Option<crate::reasoning::ReasoningSpec> = if rag_context.is_some() {
            let effort = crate::config::runtime::get(&state.pg, "rag.answer_reasoning_effort")
                .await
                .ok()
                .flatten()
                .map(|e| e.value)
                .unwrap_or_else(|| "medium".to_string());
            reasoning.as_ref().map(|r| r.clamped_to(&effort))
        } else {
            reasoning.clone()
        };
        // `per_part` splits the reasoning-heavy mega-synthesis into one
        // small generate per numbered part (each over its own slice) — the fix for medium/local
        // models. Backend-only knob (ML always emits the slices; empty ⇒ unified fallback).
        let synthesis_mode = crate::config::runtime::get(&state.pg, "rag.synthesis_mode")
            .await
            .ok()
            .flatten()
            .map(|e| e.value)
            .unwrap_or_else(|| "unified".to_string());
        if synthesis_mode == "per_part" && !rag_parts.is_empty() {
            let out = synthesize_per_part(
                state, ctx, &prompt, &memory_facts, unattended, gk_fallback,
                answer_reasoning.as_ref(), llm_sel.as_ref(), &sampling, &rag_parts, turn_id, asst_id, tx, &cancel,
            )
            .await;
            acc = out.acc;
            reasoning_acc = out.reasoning;
            prompt_tokens = out.prompt_tokens;
            completion_tokens = out.completion_tokens;
            reasoning_tokens = out.reasoning_tokens;
            model_used = out.model;
            finish_reason = out.finish_reason;
            detached = out.detached;
            interrupted = out.cancelled;
        } else {
        // root fix: the final synthesis sends NO `tools`, so the
        // layer-[2] skills advertisement can only leak as literal `<read_skill …/>` text.
        // Rebuild the system message WITHOUT skills for this streamed pass; the tool loop
        // (already finished) kept its own skills-bearing prefix.
        {
            let synth_system =
                compose::build_system(&prompt, ctx, &[], &memory_facts, rag_trimmed.as_deref(), unattended, gk_fallback);
            if let Some(first) = messages.first_mut() {
                first["content"] = json!(synth_system);
            }
        }
        // Segmented streaming answer. Usually one pass, but a segment can end three ways and
        // each folds back into the SAME bubble and DB row (one `acc`, one `asst_id`):
        //   • empty at the token cap → retry ONCE with a larger cap (a reasoning model that spent
        //     its whole budget thinking) before giving up — good turns keep their fast first token;
        //   • tool_calls             → run the library top-up and continue the answer;
        //   • length                 → auto-continue the truncated answer, bounded by `max_cont`.
        let max_cont = crate::config::runtime::get(&state.pg, "chat.answer_max_continuations")
            .await.ok().flatten().and_then(|e| e.value.parse::<usize>().ok()).unwrap_or(4);
        // One total-budget deadline spanning every segment AND tool execution of this answer, so a
        // model that keeps searching can't extend the turn without bound.
        let answer_deadline = tokio::time::Instant::now() + synthesis_max_total(&state.pg).await;
        // Tools reach the stream only under the midstream locus and unified synthesis.
        let mut stream_tools: Option<Vec<Value>> =
            if model_search_locus == "midstream" && synthesis_mode == "unified" {
                midstream_tool_def.clone().map(|d| vec![d])
            } else {
                None
            };
        let base_messages = messages.clone();
        // `next_messages = Some(..)` replays a tool-result set before continuing; `None` means the
        // first pass (base messages) or a truncation continuation (rebuilt from the answer so far).
        let mut next_messages: Option<Vec<Value>> = None;
        let mut first_pass = true;
        let mut empty_retry_used = false;
        let mut cont = 0usize;
        let mut empty_tool_streak: u8 = 0;
        loop {
            reasoning_acc.clear();
            let seg_messages = match next_messages.take() {
                Some(m) => m,
                None if first_pass => base_messages.clone(),
                None => continuation_messages(&base_messages, &acc),
            };
            let req = GenerateRequest {
                messages: seg_messages,
                sampling: sampling.clone(),
                model: None,
                tools: stream_tools.clone(),
                overrides: ml::with_reasoning(ml::provider_overrides_with_llm(state, ctx.user_id, llm_sel.as_ref()).await, answer_reasoning.as_ref()),
            };
            let seg_start_len = acc.len();
            let term = stream_generate(state, &req, turn_id, asst_id, tx, &cancel, &mut acc, &mut reasoning_acc, &mut detached, first_pass, Some(answer_deadline)).await?;
            finish_reason = term.finish.clone();
            model_used = term.model.clone().or(model_used);
            if first_pass {
                prompt_tokens = term.prompt_tokens;
                completion_tokens = term.completion_tokens;
                reasoning_tokens = term.reasoning_tokens;
            } else {
                prompt_tokens = sum_opt(prompt_tokens, term.prompt_tokens);
                completion_tokens = sum_opt(completion_tokens, term.completion_tokens);
                reasoning_tokens = sum_opt(reasoning_tokens, term.reasoning_tokens);
            }
            if term.errored.is_some() { errored = term.errored.clone(); }
            if term.interrupted { interrupted = true; }

            // Empty-at-cap retry (first pass only, once).
            if first_pass && !empty_retry_used {
                if let Some(bumped) = retry_cap(interrupted, errored.is_some(), acc.is_empty(), finish_reason.as_deref(), sampling.max_tokens) {
                    tracing::warn!(prev = ?sampling.max_tokens, bumped, "empty answer hit max_tokens while reasoning; retrying with a higher cap");
                    sampling.max_tokens = Some(bumped);
                    empty_retry_used = true;
                    continue; // redo the first segment (stays first_pass)
                }
            }
            first_pass = false;
            if interrupted || errored.is_some() || detached { break; }

            // Mid-stream tool call: run the library top-up, replay the results, continue the answer.
            if finish_reason.as_deref() == Some("tool_calls")
                && stream_tools.is_some()
                && !term.tool_calls.is_empty()
            {
                let seg_had_text = acc.len() > seg_start_len;
                empty_tool_streak = if seg_had_text { 0 } else { empty_tool_streak + 1 };
                let mut calls_json: Vec<Value> = Vec::new();
                let mut tool_msgs: Vec<Value> = Vec::new();
                for tc in &term.tool_calls {
                    let id = tc.id.clone().unwrap_or_else(|| format!("call_{}", tc.name));
                    calls_json.push(json!({
                        "id": id, "type": "function",
                        "function": { "name": tc.name, "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into()) }
                    }));
                    // Cancel-aware: the tool dispatch is not itself cancel-wrapped, so race it here.
                    let result = tokio::select! {
                        r = crate::tools::dispatch(state, ctx, chat_project_id, chat_id, turn_id, tx, None, rag_tool_ctx.as_ref(), &[], &custom_tools, &tc.name, &tc.arguments) =>
                            r.unwrap_or_else(|e| format!("error: {e}")),
                        _ = cancel.notified() => { interrupted = true; "error: cancelled".to_string() }
                    };
                    tool_msgs.push(json!({ "role": "tool", "tool_call_id": id, "content": result }));
                    if interrupted { break; }
                }
                activity.observe(&term.tool_calls);
                if interrupted { break; }
                // The answer so far becomes the assistant turn that made the calls; the tool
                // results follow; the model then continues the same answer.
                let mut next = base_messages.clone();
                next.push(json!({ "role": "assistant", "content": acc.clone(), "tool_calls": calls_json }));
                next.extend(tool_msgs);
                next_messages = Some(next);
                // Terminality: once the per-turn search budget is spent, or the model produced two
                // straight empty tool-only segments, the continuation carries NO tools → it answers.
                let budget_spent = match rag_tool_ctx.as_ref() {
                    Some(rc) => rc.state.lock().await.calls >= rc.max_calls,
                    None => true,
                };
                if budget_spent || empty_tool_streak >= 2 {
                    stream_tools = None;
                }
                continue;
            }

            // Truncated non-empty answer → auto-continue (bounded); converges over a shorter remainder.
            if is_truncation(finish_reason.as_deref()) && !acc.trim().is_empty() && cont < max_cont {
                cont += 1;
                tracing::warn!(cont, finish = ?finish_reason, "answer truncated at token cap; auto-continuing");
                continue; // next_messages stays None → rebuilt from `acc`
            }
            break;
        }
        // Still truncated after the continuation budget → surface it rather than a silent stop.
        if is_truncation(finish_reason.as_deref()) && !acc.is_empty() && !interrupted && errored.is_none() {
            let notice = "\n\n_[Response truncated at the model's token limit.]_";
            acc.push_str(notice);
            if !detached {
                let _ = tx.send(ServerFrame::ChatToken { turn_id, delta: notice.to_string() }).await;
            }
        }
        } // end unified synthesis branch
    }
    phases.mark("generate", generate_t.elapsed());
    tracing::debug!(target: "chat::phases", "chat turn phase summary\n{}", phases.summary());

    // Final guard: never persist a silent blank. If the answer is empty for a
    // non-interrupted, non-errored turn (e.g. a reasoning model exhausted its cap
    // even after the retry above), fall back to the tool loop's terminal content
    // (a reduced-reasoning answer beats none) and, failing that, a visible notice —
    // so the turn reads as a completed message rather than "(no response)".
    if !interrupted && errored.is_none() && acc.is_empty() {
        acc = match ready_answer.as_deref().map(strip_tool_leak).filter(|s| !s.trim().is_empty()) {
            Some(fallback) => fallback,
            None => match finish_reason.as_deref() {
                Some("length") => "The model reached its token limit while reasoning and produced no answer. Try a shorter prompt, or raise the agent's maximum tokens.".to_string(),
                _ => "The model returned an empty response.".to_string(),
            },
        };
        if !detached {
            let _ = tx.send(ServerFrame::ChatToken { turn_id, delta: acc.clone() }).await;
        }
    }

    // Finalise the assistant row created up front: write the full content + activity
    // + token usage, and the terminal timestamp. An errored turn is terminal too
    // (interrupted_at) so the SPA's `streaming` flag flips off and polling stops.
    let now = OffsetDateTime::now_utc();
    let errored_now = errored.is_some();
    let completed_at = (!interrupted && !errored_now).then_some(now);
    let interrupted_at = (interrupted || errored_now).then_some(now);
    // Persist the answer with any reasoning trace folded back in as a `<think>`
    // block, so a reload reconstructs the reasoning panel (the live trace arrived
    // out-of-band on the dedicated channel and isn't part of `acc`).
    // Scrub tool-call JSON / internal UUIDs from the durable copy too,
    // so a reload never shows plumbing even if a future path streamed it. The
    // reasoning trace is already separated, so fold the CLEANED answer only.
    let clean_acc = strip_tool_leak(&acc);
    let persisted_content = if reasoning_acc.is_empty() {
        clean_acc
    } else {
        format!("<think>{reasoning_acc}</think>{clean_acc}")
    };
    sqlx::query!(
        r#"UPDATE messages
           SET content = $2, completed_at = $3, interrupted_at = $4,
               prompt_tokens = $5, completion_tokens = $6, activity = $7
           WHERE id = $1"#,
        asst_id, persisted_content, completed_at, interrupted_at, prompt_tokens, completion_tokens, activity.to_json()
    )
    .execute(&state.pg)
    .await?;

    // Link any artefacts this turn produced to the assistant message, so the UI
    // can render them inline under the answer (not as a chat-wide panel).
    let _ = sqlx::query!(
        "UPDATE generated_artefacts SET message_id = $1 WHERE turn_id = $2 AND message_id IS NULL",
        asst_id,
        turn_id
    )
    .execute(&state.pg)
    .await;

    // Same pattern for web citations: the web_search dispatcher inserted them
    // keyed by turn (it doesn't know the message id); link them now —
    // unconditionally, so interrupted turns still attach their sources.
    let _ = sqlx::query!(
        "UPDATE web_citations SET message_id = $1 WHERE turn_id = $2 AND message_id IS NULL",
        asst_id,
        turn_id
    )
    .execute(&state.pg)
    .await;

    // Drafter fallback: if the Agent can produce documents (it advertises
    // `generate_artefact`), the user actually ASKED for one this turn
    // (`wants_artefact`), but no artefact was produced — save the written answer
    // itself as the document, in the format the user requested (`artefact_kind`:
    // pdf/docx/md). Intent-gated so an ordinary answer is never silently filed; the
    // real tool is hidden from the model (it misuses it), so the answer IS the draft.
    if !interrupted
        && errored.is_none()
        && acc.trim().len() > 40
        && agent.tools.iter().any(|t| t == "generate_artefact")
        && wants_artefact(&content)
    {
        let has_artefact = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM generated_artefacts WHERE turn_id = $1) AS "e!""#,
            turn_id
        )
        .fetch_one(&state.pg)
        .await
        .unwrap_or(false);
        if !has_artefact {
            // A meaningful title (from the draft's first line, else LLM-named) + the
            // body with that title line stripped, so the rendered heading isn't doubled.
            // Strip the `<think>` block first — the drafted document is the ANSWER,
            // never the reasoning.
            let drafted = strip_think(&acc);
            let (title, body) = derive_title_body(state, &drafted, &content).await;
            // The format the user asked for (pdf/docx/md); doubles as the file ext.
            let kind = artefact_kind(&content);
            match run_id {
                // ── Agentic run: the artefact write is a GATED state-changing action.
                // Pause for human approval; on approve we generate the EXACT drafted
                // content (verbatim — never re-inferred). Durable: the pending call is
                // persisted, so an approval survives a crash (the `agent_resume` task).
                Some(rid) => {
                    // Carry the assistant message id so the approved artefact links
                    // to it and renders INLINE under the answer (not only in the rail).
                    let pending = json!({ "kind": kind, "title": title, "content": body.trim(), "message_id": asst_id.to_string() });
                    crate::agent::request_approval(
                        state, rid, ctx.user_id, ctx.role.as_str(), "generate_artefact", &pending, 0,
                    )
                    .await?;
                    let frame = ServerFrame::AgentApproval {
                        run_id: rid,
                        turn_id,
                        tool: "generate_artefact".into(),
                        summary: format!("Generate “{title}” as a downloadable document?"),
                        args: pending,
                    };
                    let _ = tx.send(frame.clone()).await;
                    if let Some(uid) = ctx.user_id {
                        state.hub.send_to_user(uid, frame);
                    }

                    if unattended {
                        // No human present: leave the run `awaiting_approval`; the owner
                        // approves later and the durable `agent_resume` task generates it
                        // (slice F notifies the owner). Do not block a scheduler slot.
                    } else {
                        // Interactive: await the decision (bounded by the agent's
                        // approval timeout), then act. Single-winner via the CAS.
                        let rx = state.approvals.register(rid);
                        let decided = tokio::time::timeout(
                            std::time::Duration::from_secs(agent.controls.approval_timeout_secs),
                            rx,
                        )
                        .await;
                        state.approvals.forget(rid);
                        match decided {
                            // Approved (REST already CAS-approved before resolving us):
                            // run the approved action verbatim (idempotent single-winner).
                            Ok(Ok(true)) => {
                                if let Err(e) = crate::agent::execute_approved(state, rid).await {
                                    tracing::warn!(error = %e, "approved artefact generation failed");
                                }
                            }
                            // Rejected by REST — REST owns the finish; nothing to do.
                            Ok(Ok(false)) => {}
                            // Timeout or sender dropped: try to win the reject CAS. If we
                            // win, it's genuinely rejected; if we lose, a concurrent
                            // approve won and its durable resume will generate it.
                            _ => {
                                if crate::agent::decide(state, rid, false).await.unwrap_or(false) {
                                    crate::agent::finish(state, rid, "rejected").await;
                                }
                            }
                        }
                    }
                }
                // ── Non-agentic agent that merely lists generate_artefact: keep the
                // existing immediate behaviour (the drafted answer becomes the artefact).
                None => {
                    let artefact_id = Uuid::now_v7();
                    // Store the RELATIVE suffix; resolve for the ML call only.
                    let rel = format!("{chat_id}/{artefact_id}.{kind}");
                    let out_path = crate::storage::resolve_file(&state.boot.storage.artefacts_dir, &rel)
                        .to_string_lossy()
                        .to_string();
                    match ml::generate_artefact(&state.http, &state.boot.ml.base_url, kind, &title, body.trim(), &out_path).await {
                        Ok((_path, mime)) => {
                            let _ = sqlx::query!(
                                "INSERT INTO generated_artefacts (id, chat_id, turn_id, message_id, kind, title, disk_path, mime, created_by) \
                                 VALUES ($1, $2, $3, $4, ($5::text)::artefact_kind, $6, $7, $8, $9)",
                                artefact_id, chat_id, turn_id, asst_id, kind, title, rel, mime, ctx.user_id,
                            )
                            .execute(&state.pg)
                            .await;
                            let mut ev = AuditEvent::action("artefact.generated", ctx.role.as_str());
                            ev.actor_user_id = ctx.user_id;
                            ev.resource_type = Some("artefact".into());
                            ev.resource_id = Some(artefact_id);
                            ev.payload = Some(json!({ "chat_id": chat_id, "kind": kind, "title": title, "auto": true }));
                            // Post-stream → durable append (R4c); artefact provenance.
                            let _ = audit::append(&state.pg, &ev).await;
                        }
                        Err(e) => tracing::warn!(error = %e, "auto-artefact generation failed"),
                    }
                }
            }
        }
    } else if let Some(rid) = run_id {
        // Agentic run that produced no artefact this turn (read-only answer): close it.
        crate::agent::complete_if_running(state, rid).await;
    }

    if let Some(message) = errored {
        let _ = tx.send(ServerFrame::ChatError { turn_id: Some(turn_id), message }).await;
    } else if interrupted {
        // Post-stream → durable synchronous append: off the TTFT
        // path, and a terminal turn event must not be droppable.
        let mut event = AuditEvent::action("chat.message.interrupted", ctx.role.as_str());
        event.actor_user_id = ctx.user_id;
        event.resource_type = Some("chat".into());
        event.resource_id = Some(chat_id);
        event.payload = Some(json!({ "message_id": asst_id }));
        audit::append(&state.pg, &event).await?;
        let _ = tx.send(ServerFrame::ChatInterrupted { turn_id, message_id: Some(asst_id) }).await;
    } else {
        // The Agent version live at this turn — ties the answer to the exact
        // configuration that produced it (Tier-2 #7 version history).
        let agent_version: Option<i32> = if let Some(aid) = chat_agent_id {
            sqlx::query_scalar!("SELECT MAX(version_number) FROM agent_versions WHERE agent_id = $1", aid)
                .fetch_one(&state.pg)
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        // Compliance evidence (FEATURE A2): capture the full per-interaction record
        // (PII encrypted with the asker's subject key) and bind its content hash into
        // the audit chain below. Off the TTFT path; a capture failure must never fail
        // the turn — log and continue without the hash.
        let evidence_hash = {
            let retrieval_meta = (!rag_citations.is_empty()).then(|| {
                json!(rag_citations
                    .iter()
                    .map(|c| json!({
                        "doc_id": c.doc_id,
                        "page_number": c.page_number,
                        "clause_section_ref": c.clause_section_ref,
                    }))
                    .collect::<Vec<_>>())
            });
            let pt = prompt_tokens.map(|t| t as i32);
            let ct = completion_tokens.map(|t| t as i32);
            let input = crate::audit::EvidenceInput {
                interaction_id: asst_id,
                trace_id: Some(turn_id),
                subject_id: ctx.user_id,
                prompt: Some(content.clone()),
                output: Some(acc.clone()),
                retrieval: rag_context.clone(),
                retrieval_meta,
                model_name: model_used.clone(),
                citation_coverage: None,
                // Real terminal reason of the streamed answer; the tool-loop reuse
                // path leaves it unset (the model stopped naturally) → record "stop".
                finish_reason: finish_reason.clone().or_else(|| Some("stop".into())),
                prompt_tokens: pt,
                completion_tokens: ct,
                total_tokens: pt.zip(ct).map(|(p, c)| p + c),
                ..Default::default()
            };
            state.evidence.capture(state, input).await
        };

        let mut event = AuditEvent::action("chat.assistant.completed", ctx.role.as_str());
        event.actor_user_id = ctx.user_id;
        event.resource_type = Some("chat".into());
        event.resource_id = Some(chat_id);
        event.model_agent_traceability =
            Some(json!({ "model": model_used, "agent_id": chat_agent_id, "agent_version": agent_version }));
        event.token_usage = Some(json!({ "prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens }));
        event.payload = Some(json!({
            "message_id": asst_id, "tools": agent.tools, "evidence_content_hash": evidence_hash,
        }));
        // Post-stream → durable synchronous append: the token-usage
        // rollups (power/users_admin) and feedback model attribution read THIS row
        // — it must never be queue-droppable. Off the TTFT path, so no cost.
        audit::append(&state.pg, &event).await?;

        // Observability: same source as the usage-analytics rollups → reconciles.
        let model_label = model_used.clone().unwrap_or_else(|| "unknown".into());
        metrics::counter!("chat_turns_total").increment(1);
        metrics::counter!("llm_tokens_total", "kind" => "prompt", "model" => model_label.clone())
            .increment(prompt_tokens.unwrap_or(0).max(0) as u64);
        metrics::counter!("llm_tokens_total", "kind" => "completion", "model" => model_label)
            .increment(completion_tokens.unwrap_or(0).max(0) as u64);

        // Fold in every `search_library` citation now — both the pre-stream tool loop's and any
        // mid-stream calls' — so all documents (auto-RAG + top-ups) land in the single
        // ChatCitations frame below (the client replaces its list per frame; a second frame would
        // clobber the first). Draining the context here, after streaming, is what lets a mid-stream
        // call contribute.
        if let Some(rc) = rag_tool_ctx {
            let st = rc.state.into_inner();
            if !st.tool_citations.is_empty() {
                rag_citations.extend(st.tool_citations);
            }
        }

        let mut citations_out: Vec<crate::ws::protocol::CitationOut> = Vec::new();
        if !rag_citations.is_empty() {
            persist_citations(&state.pg, asst_id, &rag_citations).await?;
            citations_out.extend(rag_citations.iter().map(|c| crate::ws::protocol::CitationOut {
                doc_id: c.doc_id,
                document_id: None, // RAG citation → knowledge_docs, not a workspace document
                version_id: None,  // unversioned base
                quote_text: c.quote_text.clone(),
                page_number: c.page_number,
                clause_section_ref: c.clause_section_ref.clone(),
                risk: risk_of(&c.quote_text, c.clause_section_ref.as_deref()),
                ..Default::default()
            }));
        }
        // Web citations the web_search tool persisted this turn (already linked
        // to the message above). Merged into the SAME frame — the client
        // replaces the citation list per frame, so two frames would clobber.
        if let Ok(rows) = sqlx::query!(
            r#"SELECT url, title, domain, published_date, fetched_at, quote_text, snippet_only
               FROM web_citations WHERE turn_id = $1 ORDER BY created_at, id"#,
            turn_id
        )
        .fetch_all(&state.pg)
        .await
        {
            citations_out.extend(rows.into_iter().map(|r| crate::ws::protocol::CitationOut {
                quote_text: r.quote_text,
                url: Some(r.url),
                title: r.title,
                domain: Some(r.domain),
                published_date: r.published_date.map(|d| d.to_string()),
                fetched_at: r
                    .fetched_at
                    .format(&time::format_description::well_known::Rfc3339)
                    .ok(),
                snippet_only: Some(r.snippet_only),
                ..Default::default()
            }));
        }
        if !citations_out.is_empty() {
            let _ = tx
                .send(ServerFrame::ChatCitations { turn_id, message_id: asst_id, citations: citations_out })
                .await;
        }
        let _ = tx.send(ServerFrame::ChatCompleted { turn_id, chat_id, message_id: asst_id, reasoning_tokens }).await;

        // Live groundedness (Mode A): if
        // this answer drew on retrieved sources and the feature is on, verify its
        // faithfulness against that context. Spawned post-stream so it never blocks
        // the turn/TTFT; fails open if the verifier is unavailable. Only RAG answers
        // are checked — a no-source answer has nothing to ground against.
        if state.features.enabled_for(state, ctx, "groundedness").await {
            if let (Some(uid), Some(context)) = (ctx.user_id, rag_context.clone()) {
                let st = state.clone();
                let role = ctx.role.as_str().to_string();
                let question = content.clone();
                // Ground the ANSWER only — strip the `<think>` block, otherwise the
                // reasoning (not drawn from sources) is scored as unsupported.
                let answer = strip_think(&acc);
                tokio::spawn(async move {
                    crate::groundedness::verify_message(
                        &st, uid, role, chat_id, turn_id, asst_id, question, answer, context,
                    )
                    .await;
                });
            }
        }
    }

    // Moderation (accountability, not refusal).
    // Off the hot path: the answer is already streamed + completed above. Spawned so it
    // never blocks generation/TTFT; it self-gates on the (off-by-default) feature config.
    if let Some(uid) = ctx.user_id {
        let st = state.clone();
        let prompt = content.clone();
        tokio::spawn(async move {
            st.moderation.on_turn_completed(&st, uid, chat_id, asst_id, chat_project_id, prompt).await;
        });
    }

    Ok(())
}

/// Run tool-decision rounds until the model stops calling tools (or the cap).
/// Mutates `messages` with assistant-tool-call and tool-result turns. Returns
/// `true` if cancelled mid-loop.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
/// What the agent did this turn — the latest `track_steps` plan + the distinct
/// tools it used, in first-seen order. Persisted on the assistant message so the
/// inline activity timeline survives a reload.
#[derive(Default)]
struct Activity {
    steps: Vec<crate::ws::protocol::StepOut>,
    tools: Vec<String>,
    /// the retrieval Coverage summary, rendered as a completed
    /// activity step (set after the tool loop so `observe` can't clobber it).
    coverage: Option<String>,
}

impl Activity {
    /// Fold one tool-loop step's calls into the accumulator: capture the latest
    /// `track_steps` checklist; record every other tool name once (order kept).
    fn observe(&mut self, calls: &[crate::ml::ToolCall]) {
        for tc in calls {
            if tc.name == "track_steps" {
                if let Some(arr) = tc.arguments.get("steps").and_then(|v| v.as_array()) {
                    self.steps = arr
                        .iter()
                        .filter_map(|s| {
                            let title = s.get("title").and_then(|v| v.as_str())?.trim();
                            if title.is_empty() {
                                return None;
                            }
                            let status = s.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
                            Some(crate::ws::protocol::StepOut {
                                title: title.to_string(),
                                status: status.to_string(),
                            })
                        })
                        .collect();
                }
            } else if !self.tools.contains(&tc.name) {
                self.tools.push(tc.name.clone());
            }
        }
    }

    /// JSON to store on the message, or `None` when nothing happened.
    fn to_json(&self) -> Option<Value> {
        if self.steps.is_empty() && self.tools.is_empty() && self.coverage.is_none() {
            None
        } else {
            Some(json!({ "steps": self.steps, "tools": self.tools, "coverage": self.coverage }))
        }
    }
}

/// What the agentic tool loop produced. `ready_answer` is the model's clean final
/// answer, captured at the moment it stopped calling tools — the caller uses it
/// directly and skips the streaming reroll (which a small model can return empty).
struct ToolLoopOutcome {
    interrupted: bool,
    ready_answer: Option<String>,
    final_usage: Option<ml::Usage>,
    /// The tool loop ran at REDUCED reasoning: its terminal `content`
    /// is low-reasoning, so the caller should regenerate a full-reasoning final answer
    /// rather than reuse it verbatim (`ready_answer` is kept only as an empty-answer
    /// fallback). False when reasoning was off / at/below the cap / a local model.
    capped: bool,
}

/// The Agent's system prompt for layer [1], falling back to the platform default when
/// it is absent OR blank. The agent editor saves a cleared field as `Some("")`, not
/// `None`, so a `None`-only fallback would leave the model rudderless on an empty prompt.
fn effective_prompt(agent_prompt: &Option<String>, default: &str) -> String {
    match agent_prompt {
        Some(p) if !p.trim().is_empty() => p.clone(),
        _ => default.to_string(),
    }
}

/// A safe basename for an attachment inside the sandbox working dir — strip any path
/// component so the guest writes a plain file (the model references it by this name).
fn sandbox_name(filename: &str) -> String {
    filename.rsplit(['/', '\\']).next().unwrap_or(filename).replace(['/', '\\'], "_")
}

/// The prompt block for one attachment. When the file is in the code-interpreter sandbox
/// AND its text is large, inject a COMPACT preview (filename + a "read it with code" note
/// + the first lines) so the model reads the full data via the tool instead of drowning
/// in 25k tokens of stuffed rows. Otherwise the full text, with a one-line sandbox note
/// when applicable. Size-adaptive per the product decision.
fn attachment_block(name: &str, text: &str, in_sandbox: bool) -> String {
    const COMPACT_OVER_CHARS: usize = 8_000;
    const PREVIEW_LINES: usize = 20;
    if in_sandbox {
        let note = format!(
            "[Attached document: {name} — also available to the code_interpreter tool as \
             ./{name} in its working directory; read it with code for precise analysis.]"
        );
        if text.chars().count() > COMPACT_OVER_CHARS {
            let preview = text.lines().take(PREVIEW_LINES).collect::<Vec<_>>().join("\n");
            return format!("{note}\nPreview (first {PREVIEW_LINES} lines; the full data is in the file):\n{preview}");
        }
        return format!("{note}\n{text}");
    }
    format!("[Attached document: {name}]\n{text}")
}

/// The answer to reuse from a terminating tool-loop step (the model stopped calling
/// tools): its `content` — but only when a tool actually ran this turn AND the
/// content is non-empty after trim. A step-1 answer with no tools returns `None` so
/// it streams live via the normal path (TTFT), and an empty terminating content
/// falls through to the streaming path too.
fn reuse_answer(any_tool_ran: bool, content: String) -> Option<String> {
    // Guard: a terminating `content` that still looks like a
    // tool call (a call mis-emitted as text) must NEVER become the answer — return
    // None so the live generate() path produces a real answer instead of echoing JSON.
    if any_tool_ran && !content.trim().is_empty() && !looks_like_tool_call(content.trim()) {
        Some(content)
    } else {
        None
    }
}

/// The larger completion cap to retry the answer with when it came back empty
/// *solely* because the model hit its token limit while reasoning (`length` +
/// empty answer) — an OpenAI reasoning model that spent the whole
/// `max_completion_tokens` budget thinking. Returns `None` when a retry is not
/// warranted: an interrupt, a real error, a non-empty answer, or any finish reason
/// other than `length`. The new cap is at least 32768 and at least double the
/// previous cap, so a hard multi-part prompt gets meaningful headroom.
/// A generation stopped because it hit the output-token cap, not because it finished.
/// Reported differently per provider path: chat-completions `length`, OpenAI Responses API
/// `max_output_tokens`, Anthropic `max_tokens`. Truncation is silent (the answer just ends),
/// so every truncation-recovery path must recognise all three.
fn is_truncation(finish: Option<&str>) -> bool {
    matches!(finish, Some("length") | Some("max_output_tokens") | Some("max_tokens"))
}

fn retry_cap(interrupted: bool, errored: bool, acc_empty: bool, finish: Option<&str>, prev: Option<i32>) -> Option<i32> {
    if interrupted || errored || !acc_empty || !is_truncation(finish) {
        return None;
    }
    Some(prev.unwrap_or(8192).saturating_mul(2).max(32768))
}

/// The instruction appended when auto-continuing a truncated answer.
const CONTINUE_NUDGE: &str =
    "Continue the answer from exactly where it stopped. Do not repeat any text already \
     written, do not restart, and add no preamble — continue mid-sentence if necessary.";

/// Terminal state of one streamed generation pass.
#[derive(Default)]
struct GenTerminal {
    finish: Option<String>,
    model: Option<String>,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    reasoning_tokens: Option<i32>,
    errored: Option<String>,
    interrupted: bool,
    /// Tool calls the model made in this segment (empty unless `finish == "tool_calls"`).
    /// The caller executes them and continues the answer in a follow-up pass.
    tool_calls: Vec<ml::ToolCall>,
}

/// Stream ONE generation, appending answer deltas to `acc` and reasoning to `reasoning_acc`,
/// pushing `ChatToken`/`ChatReasoning` frames (unless detached), with the leading-tool-tag
/// gate (first pass only) and the ~750 ms durable flush. Shared by the initial synthesis pass,
/// the empty-retry, the truncation auto-continue, and per_part — so all stream handling lives
/// in one place. Returns the terminal finish/usage; honours cancel.
#[allow(clippy::too_many_arguments)]
/// Max wall-clock gap between streamed generation events before an answer/part is treated as
/// STALLED (robustness). A live generation keeps emitting token/reasoning deltas
/// that reset the gap, so only a genuinely wedged provider stream (no bytes at all) trips it — one
/// stalled per_part stream can then no longer hang the whole turn behind the httpx 600 s read
/// timeout. `0` disables the guard (effectively a day).
async fn synthesis_idle_timeout(pg: &sqlx::PgPool) -> std::time::Duration {
    let secs = crate::config::runtime::get(pg, "rag.synthesis_idle_timeout_secs")
        .await
        .ok()
        .flatten()
        .and_then(|e| e.value.parse::<u64>().ok())
        .unwrap_or(120);
    std::time::Duration::from_secs(if secs == 0 { 86_400 } else { secs })
}

/// Total reqwest timeout (incl. stream read) for the `/retrieve` call. The iterative-retrieval
/// phase (ml `rag.gap_deadline_secs`) runs on TOP of ordinary retrieval, so a fixed 120 s ceiling
/// would kill the turn mid-loop while progress is still live. Budget = 120 s base + the gap
/// deadline, so the ML-side deadline trips first (fail-soft, honest known-gaps) and the Rust
/// timeout stays a genuine backstop. Default gap deadline mirrors ml/app/config.py (60 s).
async fn retrieve_timeout(pg: &sqlx::PgPool) -> std::time::Duration {
    let gap = crate::config::runtime::get(pg, "rag.gap_deadline_secs")
        .await
        .ok()
        .flatten()
        .and_then(|e| e.value.parse::<u64>().ok())
        .unwrap_or(60);
    std::time::Duration::from_secs(120 + gap)
}

/// TOTAL wall-clock budget for one answer/part generation (robustness). Unlike the
/// idle gap, reasoning deltas do NOT reset this — so it is the guard against a REASONING-RUNAWAY
/// (a reasoning model that streams summary deltas for minutes without ever emitting an output token:
/// the idle gap never trips because the stream keeps "breathing", yet the part never finishes and,
/// in per_part, its full channel buffer wedges the in-order consumer and holds a concurrency permit
/// so later parts never even start). Generous by default — a heavy legal part reasons+answers in
/// well under this. `0` disables (effectively a day).
async fn synthesis_max_total(pg: &sqlx::PgPool) -> std::time::Duration {
    let secs = crate::config::runtime::get(pg, "rag.synthesis_part_max_secs")
        .await
        .ok()
        .flatten()
        .and_then(|e| e.value.parse::<u64>().ok())
        .unwrap_or(300);
    std::time::Duration::from_secs(if secs == 0 { 86_400 } else { secs })
}

async fn stream_generate(
    state: &AppState,
    req: &GenerateRequest,
    turn_id: Uuid,
    asst_id: Uuid,
    tx: &mpsc::Sender<ServerFrame>,
    cancel: &Arc<Notify>,
    acc: &mut String,
    reasoning_acc: &mut String,
    detached: &mut bool,
    lead_gate: bool,
    // Shared total-budget deadline. `None` ⇒ this segment gets its own fresh budget (the
    // single-pass callers); `Some` ⇒ one deadline spanning every segment + tool execution of a
    // multi-segment answer, so mid-stream tool calls can't extend the turn without bound.
    deadline_arg: Option<tokio::time::Instant>,
) -> Result<GenTerminal, crate::error::AppError> {
    let frame_counter = metrics::counter!("ws_token_frames_total");
    let frame_bytes = metrics::counter!("ws_token_frame_bytes_total");
    let mut out = GenTerminal::default();
    let mut head_open = lead_gate;
    let mut head_buf = String::new();
    let mut last_flush = std::time::Instant::now();
    let gen_start = std::time::Instant::now();
    let mut ttft_recorded = false;
    let mut stream = ml::generate(&state.http, &state.boot.ml.base_url, req).await?;
    let idle = synthesis_idle_timeout(&state.pg).await;
    let deadline = match deadline_arg {
        Some(d) => d,
        None => tokio::time::Instant::now() + synthesis_max_total(&state.pg).await,
    };
    loop {
        // Trip on whichever is sooner: the idle gap (silent stall) or the TOTAL budget (a reasoning
        // runaway that streams summary deltas for minutes but never an output token — the idle gap
        // alone never catches it because each delta resets it).
        let step = idle.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        if step.is_zero() {
            tracing::warn!("generation exceeded total budget; ending soft");
            out.finish = Some("stall_timeout".to_string());
            break;
        }
        tokio::select! {
            ev = stream.recv() => match ev {
                Some(GenEvent::Token { delta }) => {
                    if !ttft_recorded {
                        metrics::histogram!("llm_ttft_seconds").record(gen_start.elapsed().as_secs_f64());
                        ttft_recorded = true;
                    }
                    acc.push_str(&delta);
                    let delta_len = delta.len();
                    let send_str: Option<String> = if head_open {
                        head_buf.push_str(&delta);
                        match lead_gate_ready(&head_buf, 4096) {
                            Some(clean) => {
                                head_open = false;
                                if clean.trim_start().len() != head_buf.trim_start().len() {
                                    tracing::warn!("stripped a leading tool-shaped fragment from the streamed answer");
                                }
                                (!clean.is_empty()).then_some(clean)
                            }
                            None => None,
                        }
                    } else {
                        Some(delta)
                    };
                    if !*detached {
                        if let Some(s) = send_str {
                            if tx.send(ServerFrame::ChatToken { turn_id, delta: s }).await.is_err() {
                                *detached = true;
                            }
                        }
                    }
                    frame_counter.increment(1);
                    frame_bytes.increment(delta_len as u64);
                    if last_flush.elapsed() >= std::time::Duration::from_millis(750) {
                        let _ = sqlx::query!("UPDATE messages SET content = $1 WHERE id = $2", acc.as_str(), asst_id)
                            .execute(&state.pg)
                            .await;
                        last_flush = std::time::Instant::now();
                    }
                }
                Some(GenEvent::Reasoning { delta }) => {
                    if !ttft_recorded {
                        metrics::histogram!("llm_ttft_seconds").record(gen_start.elapsed().as_secs_f64());
                        ttft_recorded = true;
                    }
                    reasoning_acc.push_str(&delta);
                    if !*detached && tx.send(ServerFrame::ChatReasoning { turn_id, delta }).await.is_err() {
                        *detached = true;
                    }
                }
                Some(GenEvent::ToolCall { id, name, arguments }) => {
                    // Collect — the caller runs it and continues the answer. Not framed to the
                    // client as tokens; the `ChatTool` activity frames come from the dispatch.
                    out.tool_calls.push(ml::ToolCall { id, name, arguments });
                }
                Some(GenEvent::Done { usage, model, finish_reason: fr }) => {
                    out.prompt_tokens = usage.prompt_tokens;
                    out.completion_tokens = usage.completion_tokens;
                    out.reasoning_tokens = usage.reasoning_tokens;
                    out.model = model;
                    out.finish = fr;
                    metrics::histogram!("llm_generation_seconds").record(gen_start.elapsed().as_secs_f64());
                    break;
                }
                Some(GenEvent::Error { message }) => { out.errored = Some(message); break; }
                None => break,
            },
            _ = cancel.notified() => { out.interrupted = true; break; }
            // No stream event for `step` (the idle gap, or the remaining total budget) — the stream
            // is wedged or running away. End soft with the partial answer already streamed;
            // "stall_timeout" is NOT a truncation reason, so the caller does not auto-continue a
            // dead stream. Dropping `stream` aborts the request.
            _ = tokio::time::sleep(step) => {
                tracing::warn!(step_s = step.as_secs(), "generation stalled / over budget; ending soft");
                out.finish = Some("stall_timeout".to_string());
                break;
            }
        }
    }
    drop(stream);
    Ok(out)
}

/// Build the message array for a truncation-continuation pass: the base synthesis messages,
/// then the answer-so-far as an assistant turn, then the continue instruction.
fn continuation_messages(base: &[Value], answer_so_far: &str) -> Vec<Value> {
    let mut msgs = base.to_vec();
    msgs.push(json!({"role": "assistant", "content": answer_so_far}));
    msgs.push(json!({"role": "user", "content": CONTINUE_NUDGE}));
    msgs
}

async fn run_tool_loop(
    state: &AppState,
    ctx: &AuthContext,
    turn_id: Uuid,
    chat_id: Uuid,
    project_id: Option<Uuid>,
    agent: &AgentConfig,
    run_id: Option<Uuid>,
    tool_defs: &[Value],
    ci_files: &[crate::code_interpreter::InputFile],
    custom_tools: &std::collections::HashMap<String, crate::tools::custom::CustomToolRow>,
    rag_ctx: Option<&crate::tools::RagToolCtx>,
    messages: &mut Vec<Value>,
    activity: &mut Activity,
    reasoning: Option<&crate::reasoning::ReasoningSpec>,
    llm_sel: Option<&crate::ext::ResolvedProvider>,
    tx: &mpsc::Sender<ServerFrame>,
    cancel: &Arc<Notify>,
    phases: &mut TurnPhases,
    show_commentary: bool,
) -> Result<ToolLoopOutcome> {
    let sem = Arc::new(Semaphore::new(agent.tool_concurrency.max(1)));
    let mut tokens_used: i64 = 0;
    // Signatures of (tool, args) already run this turn — a degenerate ReAct loop
    // re-issues an identical call after a failure/timeout; we refuse the repeat.
    let mut seen_calls: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Did we dispatch at least one tool call this turn? Only then is the model's
    // terminating `content` the post-tool answer worth reusing (a step-1 answer
    // with no tools streams live via the normal path, preserving TTFT).
    let mut any_tool_ran = false;

    // Run the tool-deciding steps at REDUCED reasoning: a heavy
    // reasoning model otherwise burns minutes per non-streaming step → 300s timeout.
    // The user's full effort is reserved for the streamed final answer. No-op on
    // non-reasoning/local models.
    let loop_reasoning: Option<crate::reasoning::ReasoningSpec> =
        reasoning.map(|r| r.capped_for_scaffolding());
    let capped = reasoning.map(|r| r.is_capped_for_scaffolding()).unwrap_or(false);

    // when the loop runs at reduced reasoning its terminal content is
    // DISCARDED (the streamed final answer regenerates at full effort below), so there is
    // no point paying for a full-length answer on every tool-deciding step — cap their
    // output. 1024 is ample for tool-call arguments and for the intentionally-short
    // empty-answer fallback. When NOT capped the terminal content IS reused as the answer
    // (reuse_answer), so the agent's own budget is kept untouched.
    let loop_sampling = {
        let mut s = agent.sampling.clone();
        if capped {
            const SCAFFOLD_CAP: i32 = 1024;
            s.max_tokens = Some(s.max_tokens.map_or(SCAFFOLD_CAP, |m| m.min(SCAFFOLD_CAP)));
        }
        s
    };

    let max_steps = agent.controls.max_steps;
    for step_idx in 0..max_steps {
        // live progress: a moving "Thinking · step k of N" while a multi-step tool
        // loop is actually iterating, so the UI never sits on a static label during
        // the LLM wait. `name:"reasoning"` is not a real tool — the frontend renders
        // the detail without recording a tool. Skip step 0: on a plain single-answer
        // turn the model returns its answer on the first pass (no tool call), so this
        // frame would open an "Agent activity" panel that shows a frozen "step 1 of N"
        // and then vanishes. Only emit once a tool has run and the loop iterates.
        if step_idx > 0 {
            let _ = tx
                .send(ServerFrame::ChatTool {
                    turn_id,
                    name: "reasoning".into(),
                    phase: "progress".into(),
                    detail: Some(step_progress_label(step_idx + 1, max_steps)),
                })
                .await;
        }
        // Token-budget circuit breaker: stop the loop and let the model answer with
        // what it has, rather than spinning past the run's budget.
        if let Some(budget) = agent.controls.token_budget {
            if tokens_used >= budget {
                tracing::debug!(tokens_used, budget, "agent token budget reached; stopping tool loop");
                return Ok(ToolLoopOutcome { interrupted: false, ready_answer: None, final_usage: None, capped });
            }
        }
        let step_t = std::time::Instant::now();
        let step = tokio::select! {
            r = ml::chat_step(&state.http, &state.boot.ml.base_url, messages, Some(tool_defs), &loop_sampling, ml::with_reasoning(ml::provider_overrides_with_llm(state, ctx.user_id, llm_sel).await, loop_reasoning.as_ref())) => r?,
            _ = cancel.notified() => return Ok(ToolLoopOutcome { interrupted: true, ready_answer: None, final_usage: None, capped }),
        };
        phases.mark(format!("step {} llm", step_idx + 1), step_t.elapsed());
        tokens_used += step.usage.completion_tokens.unwrap_or(0) as i64
            + step.usage.prompt_tokens.unwrap_or(0) as i64;
        if let Some(rid) = run_id {
            let _ = sqlx::query!(
                "UPDATE agent_runs SET token_used = $2, step_count = step_count + 1, updated_at = now() WHERE id = $1",
                rid, tokens_used as i32,
            )
            .execute(&state.pg)
            .await;
        }
        if step.tool_calls.is_empty() {
            // The model stopped calling tools — `content` is its final answer (the
            // qwen3 reasoning parser already splits reasoning out of `content`
            // upstream). Hand it back so the caller can use it and skip the
            // streaming reroll, which on a small model can return empty (whole
            // budget spent inside <think>). Only reuse it when a tool actually ran
            // and the answer is non-empty; otherwise let the streaming path answer.
            let ready_answer = reuse_answer(any_tool_ran, step.content);
            return Ok(ToolLoopOutcome { interrupted: false, ready_answer, final_usage: Some(step.usage), capped });
        }

        any_tool_ran = true;

        // Record the assistant's tool-call message.
        let calls_json: Vec<Value> = step
            .tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id.clone().unwrap_or_else(|| format!("call_{}", tc.name)),
                    "type": "function",
                    "function": { "name": tc.name, "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into()) }
                })
            })
            .collect();
        messages.push(json!({ "role": "assistant", "content": step.content, "tool_calls": calls_json }));

        // Surface the model's brief commentary next to a library top-up as a transient
        // `reasoning` detail (not persisted chat text). Only when a search_library call is
        // actually happening this step, so ordinary tool turns are untouched.
        if show_commentary && step.tool_calls.iter().any(|tc| tc.name == "search_library") {
            let comment = step.content.trim();
            if !comment.is_empty() {
                let snippet: String = comment.chars().take(120).collect();
                let _ = tx
                    .send(ServerFrame::ChatTool {
                        turn_id,
                        name: "reasoning".into(),
                        phase: "progress".into(),
                        detail: Some(snippet),
                    })
                    .await;
            }
        }

        // Classify each call: a side-effecting MCP call (FEATURE B1) takes a sequential
        // human-approval gate (the approvals registry is keyed by run_id); native +
        // read-only-MCP calls auto-run bounded-parallel as before. "Is MCP" is just
        // metadata to `run_one_call` — same gates, timeout, audit, result envelope.
        let mut auto: Vec<&ml::ToolCall> = Vec::new();
        let mut gated: Vec<&ml::ToolCall> = Vec::new();
        let mut by_id: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for tc in &step.tool_calls {
            // Loop guard: refuse an EXACT repeat of a call already made this turn
            // (the model loops on a tool that failed/timed out). Hand back a firm
            // "stop and answer" result instead of re-dispatching (no second wait).
            let id = tc.id.clone().unwrap_or_else(|| format!("call_{}", tc.name));
            let sig = format!("{}|{}", tc.name, serde_json::to_string(&tc.arguments).unwrap_or_default());
            if !seen_calls.insert(sig) {
                by_id.insert(id, format!(
                    "error: '{}' was already called with these exact arguments earlier in this turn and did not help. Do NOT call it again — answer the user now using the context you already have.",
                    tc.name
                ));
                continue;
            }
            // A gated call pauses for human approval: a side-effecting MCP tool, or
            // a custom tool that is side-effecting or a script.
            let hitl = run_id.is_some()
                && ((crate::mcp::is_namespaced(&tc.name)
                    && match crate::mcp::split(&tc.name) {
                        Some((s, t)) => crate::mcp::is_side_effecting(state, s, t).await,
                        None => false,
                    })
                    || custom_tools
                        .get(&tc.name)
                        .map(|r| r.kind == "script" || r.side_effecting)
                        .unwrap_or(false));
            if hitl {
                gated.push(tc);
            } else {
                auto.push(tc);
            }
        }
        let auto_results = futures_util::future::join_all(auto.iter().map(|tc| {
            let sem = sem.clone();
            async move {
                let _permit = sem.acquire().await.expect("semaphore");
                run_one_call(state, ctx, run_id, project_id, chat_id, turn_id, tx, agent, ci_files, custom_tools, rag_ctx, tc).await
            }
        }))
        .await;
        for (id, r) in auto_results {
            by_id.insert(id, r);
        }
        for tc in gated {
            let (id, r) = gated_call(
                state, ctx, run_id.expect("gated implies a run"), project_id, chat_id, turn_id, tx, agent, custom_tools, tc,
            )
            .await;
            by_id.insert(id, r);
        }

        // Append results in the model's original call order.
        for tc in &step.tool_calls {
            let id = tc.id.clone().unwrap_or_else(|| format!("call_{}", tc.name));
            let result = by_id.remove(&id).unwrap_or_else(|| "error: no tool result".into());
            messages.push(json!({ "role": "tool", "tool_call_id": id, "content": result }));
        }

        // Fold this step's tool calls into the persisted activity (latest plan +
        // tools used) for the inline activity timeline.
        activity.observe(&step.tool_calls);
    }
    Ok(ToolLoopOutcome { interrupted: false, ready_answer: None, final_usage: None, capped })
}

/// Dispatch one tool call (native or MCP) with the shared gates, timeout, frames and
/// audit. "Is MCP" is metadata: a namespaced name routes to `mcp::dispatch` (which
/// enforces egress + per-server RBAC), everything else to `tools::dispatch`.
#[allow(clippy::too_many_arguments)]
async fn run_one_call(
    state: &AppState,
    ctx: &AuthContext,
    run_id: Option<Uuid>,
    project_id: Option<Uuid>,
    chat_id: Uuid,
    turn_id: Uuid,
    tx: &mpsc::Sender<ServerFrame>,
    agent: &AgentConfig,
    ci_files: &[crate::code_interpreter::InputFile],
    custom_tools: &std::collections::HashMap<String, crate::tools::custom::CustomToolRow>,
    rag_ctx: Option<&crate::tools::RagToolCtx>,
    tc: &ml::ToolCall,
) -> (String, String) {
    let id = tc.id.clone().unwrap_or_else(|| format!("call_{}", tc.name));
    let _ = tx
        .send(ServerFrame::ChatTool { turn_id, name: tc.name.clone(), phase: "started".into(), detail: None })
        .await;
    audit_tool(state, ctx, chat_id, &tc.name, "invoked");
    let started = OffsetDateTime::now_utc();
    let is_mcp = crate::mcp::is_namespaced(&tc.name);
    let custom_row = custom_tools.get(&tc.name);

    // Pre-dispatch gates: the per-run kill-token + constrained delegation (native
    // tools); MCP enforces its own RBAC + egress inside `mcp::dispatch`.
    let blocked = if let Some(rid) = run_id {
        if !crate::agent::alive(state, rid).await {
            Some("error: agent run halted (killed, expired, or agents disabled)".to_string())
        } else if let Err(e) = crate::tools::tool_permitted(state, ctx, &tc.name, project_id).await {
            Some(format!("error: {e}"))
        } else {
            None
        }
    } else {
        None
    };
    let result = if let Some(b) = blocked {
        b
    } else {
        let to = if is_mcp {
            std::time::Duration::from_secs(120)
        } else if let Some(r) = custom_row {
            std::time::Duration::from_secs(r.timeout_secs.map(|s| s.max(1) as u64).unwrap_or(120))
        } else {
            crate::tools::timeout_for(&tc.name, &state.boot.tool_timeout_secs)
        };
        let fut = async {
            if is_mcp {
                crate::mcp::dispatch(state, ctx, chat_id, &tc.name, &tc.arguments).await
            } else {
                crate::tools::dispatch(state, ctx, project_id, chat_id, turn_id, tx, agent.web.as_ref(), rag_ctx, ci_files, custom_tools, &tc.name, &tc.arguments).await
            }
        };
        match tokio::time::timeout(to, fut).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => format!("error: {e}"),
            Err(_) => format!("error: tool '{}' timed out. Do NOT call it again — answer from the context you already have.", tc.name),
        }
    };
    let ms = (OffsetDateTime::now_utc() - started).whole_milliseconds();
    audit_tool(state, ctx, chat_id, &tc.name, "completed");
    let _ = tx
        .send(ServerFrame::ChatTool { turn_id, name: tc.name.clone(), phase: "finished".into(), detail: None })
        .await;
    // Observability: a per-tool call counter + latency
    // histogram covering native + MCP (the durable-resume path counts separately
    // in `agent::execute_pending`). Cardinality is bounded by the closed native
    // set + admin-registered MCP tools.
    let kind = if is_mcp { "mcp" } else if custom_row.is_some() { "custom" } else { "native" };
    let status = if result.starts_with("error:") { "error" } else { "ok" };
    metrics::counter!("tool_calls_total", "tool" => tc.name.clone(), "kind" => kind, "status" => status)
        .increment(1);
    metrics::histogram!("tool_call_duration_seconds", "tool" => tc.name.clone(), "kind" => kind)
        .record(ms as f64 / 1000.0);
    tracing::debug!(tool = %tc.name, ms, "tool dispatched");
    (id, result)
}

/// A side-effecting gated call (MCP or custom): pause for explicit human approval
/// (reusing the agent-run approval gate), then run it verbatim on approve. The
/// pending call is persisted, so an unattended approval resumes durably via
/// `agent::execute_pending` (FEATURE B1 #4).
#[allow(clippy::too_many_arguments)]
async fn gated_call(
    state: &AppState,
    ctx: &AuthContext,
    run_id: Uuid,
    project_id: Option<Uuid>,
    chat_id: Uuid,
    turn_id: Uuid,
    tx: &mpsc::Sender<ServerFrame>,
    agent: &AgentConfig,
    custom_tools: &std::collections::HashMap<String, crate::tools::custom::CustomToolRow>,
    tc: &ml::ToolCall,
) -> (String, String) {
    let id = tc.id.clone().unwrap_or_else(|| format!("call_{}", tc.name));
    if let Err(e) = crate::agent::request_approval(
        state, run_id, ctx.user_id, ctx.role.as_str(), &tc.name, &tc.arguments, 0,
    )
    .await
    {
        return (id, format!("error: {e}"));
    }
    let summary = if custom_tools.contains_key(&tc.name) {
        format!("Run custom tool `{}`?", tc.name)
    } else {
        format!("Run MCP tool `{}`?", tc.name)
    };
    let frame = ServerFrame::AgentApproval {
        run_id,
        turn_id,
        tool: tc.name.clone(),
        summary,
        args: tc.arguments.clone(),
    };
    let _ = tx.send(frame.clone()).await;
    if let Some(uid) = ctx.user_id {
        state.hub.send_to_user(uid, frame);
    }
    let rx = state.approvals.register(run_id);
    let decided = tokio::time::timeout(
        std::time::Duration::from_secs(agent.controls.approval_timeout_secs),
        rx,
    )
    .await;
    state.approvals.forget(run_id);
    match decided {
        // Approved (REST CAS-approved before resolving us): run it, then return the run
        // to `running` so the loop continues and `complete_if_running` finalises it.
        Ok(Ok(true)) => {
            // Gated tools are MCP/custom side-effecting calls, never code_interpreter or
            // search_library → no CI files, no RAG-tool context.
            let (_id, r) = run_one_call(state, ctx, Some(run_id), project_id, chat_id, turn_id, tx, agent, &[], custom_tools, None, tc).await;
            crate::agent::mark_running(state, run_id).await;
            (id, r)
        }
        Ok(Ok(false)) => (id, "error: the user declined this tool call".into()),
        _ => {
            if crate::agent::decide(state, run_id, false).await.unwrap_or(false) {
                crate::agent::finish(state, run_id, "rejected").await;
            }
            (id, "error: tool approval timed out".into())
        }
    }
}

async fn load_agent(pool: &sqlx::PgPool, agent_id: Option<Uuid>) -> Result<AgentConfig> {
    let Some(id) = agent_id else {
        return Ok(AgentConfig {
            system_prompt: None,
            sampling: Sampling { temperature: Some(0.7), ..Default::default() },
            tool_concurrency: 4,
            tools: Vec::new(),
            controls: RunControls::default(),
            web: None,
        });
    };
    let row = sqlx::query!(
        "SELECT system_prompt, params FROM agents WHERE id = $1 AND archived_at IS NULL",
        id
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Validation("agent not found".into()))?;
    let mut tools: Vec<String> =
        sqlx::query_scalar!("SELECT tool_name FROM agent_tools WHERE agent_id = $1", id)
            .fetch_all(pool)
            .await?;
    // Every agent gets a baseline set without enabling it (read helpers + artefacts);
    // host-capability + per-turn gating still apply downstream.
    for &t in crate::tools::DEFAULT_TOOLS {
        if !tools.iter().any(|x| x == t) {
            tools.push(t.to_string());
        }
    }

    let p = row.params;
    let f = |k: &str| p.get(k).and_then(|v| v.as_f64()).map(|x| x as f32);
    let sampling = Sampling {
        temperature: f("temperature").or(Some(0.7)),
        top_p: f("top_p"),
        max_tokens: p.get("max_tokens").and_then(|v| v.as_i64()).map(|x| x as i32),
        frequency_penalty: f("frequency_penalty"),
        presence_penalty: f("presence_penalty"),
        reasoning_effort: None,
    };
    let tool_concurrency = p.get("tool_concurrency").and_then(|v| v.as_u64()).unwrap_or(4) as usize;

    let d = RunControls::default();
    let controls = RunControls {
        max_steps: p
            .get("max_steps")
            .and_then(|v| v.as_u64())
            .map(|x| (x as usize).clamp(1, 50))
            .unwrap_or(d.max_steps),
        token_budget: p.get("token_budget").and_then(|v| v.as_i64()),
        wall_clock_secs: p.get("wall_clock_secs").and_then(|v| v.as_u64()).unwrap_or(d.wall_clock_secs),
        approval_timeout_secs: p
            .get("approval_timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(d.approval_timeout_secs),
        unattended_approval_ttl_secs: p
            .get("unattended_approval_ttl_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(d.unattended_approval_ttl_secs),
    };

    // Per-Agent web-search budget: a tightening cap, never a widening one
    // (the dispatcher clamps the requested depth + the ML fetch budget).
    let web_depth_max = p
        .get("web_depth_max")
        .and_then(|v| v.as_str())
        .filter(|d| ["quick", "standard", "deep"].contains(d))
        .map(str::to_string);
    let web_max_fetches = p.get("web_max_fetches").and_then(|v| v.as_i64()).filter(|n| *n > 0);
    let web = (web_depth_max.is_some() || web_max_fetches.is_some())
        .then_some(crate::tools::WebBudget { depth_max: web_depth_max, max_fetches: web_max_fetches });

    Ok(AgentConfig { system_prompt: Some(row.system_prompt), sampling, tool_concurrency, tools, controls, web })
}

/// Slot-[2] skill metadata for the chat's Agent (always-resident name + description).
async fn load_skills(
    pool: &sqlx::PgPool,
    agent_id: Option<Uuid>,
) -> Result<Vec<compose::SkillMeta>> {
    // Default skills (`is_default`) apply to EVERY agent — and to a no-agent chat —
    // without an explicit binding; attached skills add to them. UNION dedups when a
    // default is also explicitly attached. `agent_id` binds as NULL when absent, so
    // the attached branch then matches nothing.
    // `s.enabled` gates both branches: an admin-disabled skill never enters slot [2].
    let rows = sqlx::query!(
        r#"SELECT s.id AS "id!", s.name AS "name!", s.description AS "description!"
           FROM skills s WHERE s.is_default AND s.enabled
           UNION
           SELECT s.id, s.name, s.description
           FROM agent_skills a JOIN skills s ON s.id = a.skill_id
           WHERE a.agent_id = $1 AND s.enabled
           ORDER BY 2"#,
        agent_id
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| compose::SkillMeta { id: r.id, name: r.name, description: r.description })
        .collect())
}

/// Returns (chat_id, project_id, agent_id, created).
async fn resolve_chat(
    state: &AppState,
    ctx: &AuthContext,
    chat_id: Option<Uuid>,
    project_id: Option<Uuid>,
    agent_id: Option<Uuid>,
    title: &str,
) -> Result<(Uuid, Option<Uuid>, Option<Uuid>, String, bool)> {
    let owner = ctx
        .user_id
        .ok_or_else(|| AppError::Forbidden("a chat needs a Keycloak user owner".into()))?;

    match chat_id {
        Some(id) => {
            let row = sqlx::query!(
                "SELECT owner_user_id, project_id, agent_id, mode FROM chats WHERE id = $1 AND archived_at IS NULL",
                id
            )
            .fetch_optional(&state.pg)
            .await?
            .ok_or_else(|| AppError::Validation("chat not found".into()))?;
            if row.owner_user_id != owner {
                state.rbac.require(&state.pg, ctx, ResourceType::Chat, id, Permission::Write).await?;
            }
            Ok((id, row.project_id, row.agent_id, row.mode, false))
        }
        None => {
            let id = Uuid::now_v7();
            sqlx::query!(
                "INSERT INTO chats (id, owner_user_id, project_id, agent_id, title) VALUES ($1, $2, $3, $4, $5)",
                id, owner, project_id, agent_id, title
            )
            .execute(&state.pg)
            .await?;
            // New chats from a normal turn take the column default mode ('general').
            Ok((id, project_id, agent_id, "general".to_string(), true))
        }
    }
}

/// Strip leading markdown/heading markers and emphasis wrappers from a title line.
fn strip_title_markers(line: &str) -> String {
    let mut t = line.trim();
    for p in ["######", "#####", "####", "###", "##", "#", ">", "-", "*", "•"] {
        if let Some(rest) = t.strip_prefix(p) {
            t = rest.trim_start();
            break;
        }
    }
    for p in ["Subject:", "Title:", "SUBJECT:", "TITLE:"] {
        if let Some(rest) = t.strip_prefix(p) {
            t = rest.trim_start();
            break;
        }
    }
    t.trim_matches(|c| c == '*' || c == '_' || c == '"' || c == '#').trim().to_string()
}

/// Is this line an intended heading? — a markdown `#`, a wholly-bold `**…**`/`__…__`
/// line, or a `Subject:`/`Title:` label. (Plain prose / meta narration is not.) Lines
/// that mention a tool name or contain code ticks are excluded — those are the model
/// narrating, not titling.
fn is_heading_like(line: &str) -> bool {
    let l = line.trim();
    let ll = l.to_lowercase();
    if l.contains('`') || ll.contains("generate_artefact") || ll.contains("artefact") {
        return false;
    }
    l.starts_with('#')
        || (l.starts_with("**") && l.ends_with("**") && l.chars().count() > 4)
        || (l.starts_with("__") && l.ends_with("__") && l.chars().count() > 4)
        || ["Subject:", "Title:", "SUBJECT:", "TITLE:"].iter().any(|p| l.starts_with(p))
}

/// Scan the first dozen lines of the drafted answer for a heading-like line and use
/// it as the title; the body is everything AFTER it (so a meta preamble like "I'll use
/// the tool… Here's the result:" is dropped, and the heading isn't duplicated). Returns
/// `None` (→ caller LLM-names) when there's no usable heading.
fn extract_title_body(answer: &str) -> Option<(String, String)> {
    // A producing verb / request opener means it's the echoed prompt, not a title.
    const CMD: [&str; 14] = [
        "draft", "write", "generate", "create", "produce", "prepare", "make", "compose",
        "give", "please", "can", "could", "i", "kindly",
    ];
    let lines: Vec<&str> = answer.lines().collect();
    let mut seen = 0usize;
    for (idx, raw) in lines.iter().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        seen += 1;
        if seen > 12 {
            break;
        }
        if !is_heading_like(raw) {
            continue;
        }
        let cand = strip_title_markers(raw);
        let n = cand.chars().count();
        // A real title is a few words — not one token (often a stray symbol/tool name).
        if !(3..=80).contains(&n) || cand.split_whitespace().count() < 2 {
            continue;
        }
        let firstword = cand.split_whitespace().next().unwrap_or("").to_lowercase();
        if CMD.contains(&firstword.as_str()) {
            continue;
        }
        let body = lines[idx + 1..].join("\n").trim_start().to_string();
        return Some((cand, body));
    }
    None
}

/// Last-resort title: the user's request, truncated (the pre-LLM behaviour).
fn fallback_title(prompt: &str) -> String {
    let p = prompt.trim();
    if p.is_empty() {
        "Document".into()
    } else {
        p.chars().take(60).collect()
    }
}

/// Cheap best-effort LLM naming — used only when first-line extraction fails. Returns
/// a concise Title-Case title, or `None` on any error/empty (caller falls back).
async fn name_artefact(state: &AppState, content: &str) -> Option<String> {
    let head: String = content.chars().take(1500).collect();
    let messages = vec![
        json!({ "role": "system", "content": "You name documents. Reply with ONLY a concise title in Title Case, 3 to 8 words. No quotation marks, no trailing punctuation, no preamble." }),
        json!({ "role": "user", "content": format!("Document:\n{head}\n\nTitle:") }),
    ];
    // Budget 512 (not ~24): a reasoning model spends hidden reasoning tokens before
    // the title, so a tiny cap returns empty content. `reasoning_effort=minimal`
    // keeps it fast on providers that honour it (ML clamps + only sends where valid).
    let sampling = crate::ml::Sampling {
        max_tokens: Some(512),
        reasoning_effort: Some("minimal".into()),
        ..Default::default()
    };
    let step = crate::ml::chat_step(&state.http, &state.boot.ml.base_url, &messages, None, &sampling, crate::ml::provider_overrides(state, None).await)
        .await
        .ok()?;
    let line = step.content.trim().lines().next().unwrap_or("").to_string();
    let t = strip_title_markers(&line);
    (t.chars().count() >= 3).then(|| t.chars().take(80).collect())
}

/// Derive a meaningful (title, body) for the artefact: extract a title from the
/// drafted answer's first line, else make one cheap LLM naming call, else the prompt.
async fn derive_title_body(state: &AppState, answer: &str, prompt: &str) -> (String, String) {
    if let Some((title, body)) = extract_title_body(answer) {
        return (title, body);
    }
    let title = match name_artefact(state, answer.trim()).await {
        Some(t) => t,
        None => fallback_title(prompt),
    };
    (title, answer.trim().to_string())
}

/// Heuristic: did the user actually ask for a downloadable document this turn?
/// Gates the drafter fallback so an ordinary answer is never silently saved as a
/// file. Deliberately an allow-list (a producing verb AND a document noun, or an
/// explicit "downloadable"/"as a <format>" phrase) — conservative by design, so it
/// errs towards NOT creating an unasked-for artefact. British English.
fn wants_artefact(prompt: &str) -> bool {
    let p = prompt.to_lowercase();
    if p.contains("downloadable")
        || p.contains("as a pdf")
        || p.contains("as a docx")
        || p.contains("as a document")
        || p.contains("as a word")
    {
        return true;
    }
    const VERBS: [&str; 8] =
        ["draft", "write", "generate", "create", "produce", "prepare", "export", "make"];
    const NOUNS: [&str; 9] = [
        "document", "memo", "letter", "report", "contract", "agreement", "brief", "pdf", "docx",
    ];
    VERBS.iter().any(|v| p.contains(v)) && NOUNS.iter().any(|n| p.contains(n))
}

/// The requested artefact format from the user's prompt — `pdf`/`docx`/`md`
/// (Python's generator writes all three). Default markdown. The string doubles as
/// the file extension.
fn artefact_kind(prompt: &str) -> &'static str {
    let p = prompt.to_lowercase();
    if p.contains("pdf") {
        "pdf"
    } else if p.contains("docx") || p.contains("word") {
        "docx"
    } else {
        "md"
    }
}

/// Insert an EMPTY, still-streaming assistant message from OUTSIDE a turn — used
/// by background jobs (deep web search / Deep Research) that stream a result back
/// into the chat. `completed_at` is left NULL so the messages API reports it as
/// `streaming` (a reload resumes from the row). The job streams tokens in and then
/// calls [`finish_assistant_message`]. Returns the new message id.
/// A background job (deep web search / Deep Research) streams tokens into it and
/// then calls [`finish_assistant_message`]. Returns the new message id.
pub(crate) async fn start_assistant_message(pool: &sqlx::PgPool, chat_id: Uuid) -> Result<Uuid> {
    let seq = next_seq(pool, chat_id).await?;
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO messages (id, chat_id, role, sequence_number, content) \
         VALUES ($1, $2, 'assistant', $3, '')",
        id, chat_id, seq
    )
    .execute(pool)
    .await?;
    Ok(id)
}

/// Persist accumulated content WITHOUT settling the message (the ~750 ms mid-stream
/// flush, mirroring the chat turn's throttle) so a reload mid-run resumes the
/// partial answer. Best-effort: a transient failure is retried by the next flush.
pub(crate) async fn flush_assistant_message(pool: &sqlx::PgPool, id: Uuid, content: &str) {
    let _ = sqlx::query!("UPDATE messages SET content = $1 WHERE id = $2", content, id)
        .execute(pool)
        .await;
}

/// Settle a streamed background message: write the final content and stamp
/// `completed_at` so the `streaming` flag flips off and polling stops.
pub(crate) async fn finish_assistant_message(
    pool: &sqlx::PgPool,
    id: Uuid,
    content: &str,
    activity: Option<serde_json::Value>,
) -> Result<()> {
    // `COALESCE` keeps any existing activity when the caller passes None, so a
    // background sender that has nothing to record stays behaviour-identical.
    sqlx::query!(
        "UPDATE messages SET content = $1, completed_at = now(), activity = COALESCE($3, activity) \
         WHERE id = $2",
        content, id, activity
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn next_seq(pool: &sqlx::PgPool, chat_id: Uuid) -> Result<i32> {
    let n: i32 = sqlx::query_scalar!(
        r#"SELECT COALESCE(MAX(sequence_number), 0) + 1 AS "n!" FROM messages WHERE chat_id = $1"#,
        chat_id
    )
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Rough token estimate (chars/4) — enough for a budget decision, not billing.
fn est_tokens(s: &str) -> i64 {
    (s.len() / 4) as i64
}

/// A history message with its sequence number — needed to track which turns have
/// already been folded into the rolling summary (the compaction watermark).
struct HistMsg {
    seq: i32,
    role: String,
    content: String,
}

impl HistMsg {
    fn to_value(&self) -> Value {
        compose::msg(&self.role, &self.content)
    }
}

/// The model's usable context window in tokens: the configured override, else
/// learned from the inference server. `None` when neither is known.
async fn context_budget(state: &AppState) -> Option<i64> {
    if state.boot.max_context_tokens > 0 {
        return Some(state.boot.max_context_tokens);
    }
    ml::model_info(&state.http, &state.boot.ml.base_url).await.ok().map(|mi| mi.max_model_len)
}

/// Truncate `s` to roughly `max_tokens` (chars ≈ tokens × 4) on a char boundary.
/// A coarse guard for the rare case [5] RAG context overflows its slot budget.
fn trim_to_tokens(s: &str, max_tokens: i64) -> String {
    let max_chars = (max_tokens.max(0) as usize).saturating_mul(4);
    if s.len() <= max_chars {
        return s.to_string();
    }
    let mut end = max_chars;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push_str("\n[context truncated to fit the budget]");
    out
}

/// Strip `<think>…</think>` reasoning blocks from a model answer, returning just
/// the answer text. The reasoning is re-embedded into the streamed content by the
/// ML service (so the frontend `splitThink` can show a CoT panel and a reload
/// re-splits the persisted message), but downstream consumers that treat the text
/// as the *answer* — groundedness scoring, the drafter-fallback document — must not
/// see the reasoning. Mirrors the frontend `splitThink`: folds out every closed
/// block, drops an unclosed trailing `<think>` and any orphan tags.
fn strip_think(content: &str) -> String {
    if !content.contains("<think>") && !content.contains("</think>") {
        return content.to_string();
    }
    let mut answer = String::new();
    let mut rest = content;
    loop {
        match rest.find("<think>") {
            None => {
                answer.push_str(rest);
                break;
            }
            Some(open) => {
                answer.push_str(&rest[..open]);
                let after = &rest[open + "<think>".len()..];
                match after.find("</think>") {
                    // Unclosed trailing block → the rest is reasoning; drop it.
                    None => break,
                    Some(close) => rest = &after[close + "</think>".len()..],
                }
            }
        }
    }
    // Drop any orphan tags a small model may emit without a partner.
    answer.replace("<think>", "").replace("</think>", "").trim().to_string()
}

/// True when `s` (a trimmed whole string or a `{…}` slice) parses as a tool-call-shaped
/// JSON object — top-level `name`+`arguments`, an OpenAI `{"function": {…}}` wrapper, or
/// a `tool_calls` array. Used to spot a tool call mis-emitted as text.
fn looks_like_tool_call(s: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(s.trim()) else {
        return false;
    };
    let Some(o) = v.as_object() else {
        return false;
    };
    (o.contains_key("name") && o.contains_key("arguments"))
        || o.get("function").map(serde_json::Value::is_object).unwrap_or(false)
        || o.contains_key("tool_calls")
}

/// Walk from the `{` at `start` to its matching `}` (string- and escape-aware). Returns
/// `(index_of_close, true)`, or `(len, false)` if the braces never balance.
fn match_brace(b: &[u8], start: usize) -> (usize, bool) {
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    let mut j = start;
    while j < b.len() {
        let c = b[j];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return (j, true);
            }
        }
        j += 1;
    }
    (b.len(), false)
}

/// Remove tool-call-shaped JSON objects from `content` (the belt-and-suspenders half of
/// `strip_tool_leak`). Scans on ASCII braces only — `{`/`}`/`"` are char boundaries, so
/// the kept slices stay valid UTF-8.
fn strip_tool_call_json(content: &str) -> String {
    let b = content.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut seg_start = 0;
    let mut i = 0;
    while i < n {
        if b[i] != b'{' {
            i += 1;
            continue;
        }
        let (end, ok) = match_brace(b, i);
        if !ok {
            break; // unbalanced tail — keep the remainder verbatim
        }
        if looks_like_tool_call(&content[i..=end]) {
            out.push_str(&content[seg_start..i]);
            seg_start = end + 1;
        }
        i = end + 1;
    }
    out.push_str(&content[seg_start..]);
    out
}

/// Length (in chars) of a canonical `8-4-4-4-12` hex UUID starting at `c[i]`, else None.
fn uuid_at(c: &[char], i: usize) -> Option<usize> {
    let groups = [8usize, 4, 4, 4, 12];
    let mut p = i;
    for (gi, &g) in groups.iter().enumerate() {
        if gi > 0 {
            if p >= c.len() || c[p] != '-' {
                return None;
            }
            p += 1;
        }
        for _ in 0..g {
            if p >= c.len() || !c[p].is_ascii_hexdigit() {
                return None;
            }
            p += 1;
        }
    }
    Some(p - i)
}

/// Drop bare internal UUID tokens (whitespace-delimited, not inside a URL/path). A UUID
/// embedded in a link is preceded by a non-space char, so it fails the guard and is kept.
fn strip_bare_uuids(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if let Some(len) = uuid_at(&c, i) {
            let before_ok = i == 0 || c[i - 1].is_whitespace();
            let after = i + len;
            let after_ok = after >= c.len()
                || c[after].is_whitespace()
                || matches!(c[after], '.' | ',' | ';' | ')' | ']' | '}' | '!' | '?' | '"' | '\'');
            if before_ok && after_ok {
                i = after; // drop the token
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// Collapse runs of spaces to one and trim (tidies gaps left by a removed JSON span).
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        let is_space = ch == ' ';
        if is_space && prev_space {
            continue;
        }
        prev_space = is_space;
        out.push(ch);
    }
    out.trim().to_string()
}

/// Belt-and-suspenders scrub (+): strip any
/// tool-call-shaped JSON, bare internal UUIDs, and a leading tool-shaped XML tag
/// (`<read_skill …/>`) from model output before it reaches the user or the DB. A normal
/// answer (no `{`, `-`, or `<`) short-circuits unchanged.
fn strip_tool_leak(content: &str) -> String {
    if !content.contains('{') && !content.contains('-') && !content.contains('<') {
        return content.to_string();
    }
    let no_lead = strip_leading_tool_tags(content);
    collapse_spaces(&strip_bare_uuids(&strip_tool_call_json(&no_lead)))
}

/// Tool-call XML tag names the model may parrot as literal text when a prompt advertises
/// skills but the request sends no `tools`. Lower-cased for matching.
const TOOL_TAG_NAMES: &[&str] = &[
    "read_skill", "tool", "tool_call", "tool_use", "invoke", "function", "function_call",
    "skill", "use_skill", "antml:invoke", "antml:function_calls",
];

/// If `s` begins (after optional leading whitespace) with a tool-shaped XML tag, return the
/// byte length from the START of `s` through the end of that tag — a self-closing `<t/>` or a
/// paired `<t …>…</t>` (or, if not yet closed, just the opening tag). Only the allow-listed
/// tool names match, so ordinary markup a real answer might contain is preserved. None when
/// the head is not a tool tag or the tag has not closed yet.
fn leading_tool_tag_len(s: &str) -> Option<usize> {
    let ws = s.len() - s.trim_start().len();
    let rest = &s[ws..];
    if !rest.starts_with('<') {
        return None;
    }
    let gt = rest.find('>')?; // opening tag not closed yet → caller waits
    let inner = rest[1..gt].trim_start_matches('/').trim_start();
    let name_end = inner
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ':'))
        .unwrap_or(inner.len());
    let name = inner[..name_end].to_ascii_lowercase();
    if !TOOL_TAG_NAMES.contains(&name.as_str()) {
        return None;
    }
    // Self-closing `…/>` → done at the opening tag.
    if rest[..=gt].trim_end().ends_with("/>") {
        return Some(ws + gt + 1);
    }
    // Paired: remove through the matching close tag if present.
    let close = format!("</{name}");
    let lower = rest.to_ascii_lowercase();
    if let Some(cpos) = lower.find(&close) {
        if let Some(cgt) = rest[cpos..].find('>') {
            return Some(ws + cpos + cgt + 1);
        }
    }
    // Opening tag only (close not streamed yet) — remove just the opening tag.
    Some(ws + gt + 1)
}

/// Strip any run of leading tool-shaped XML tags (handles two consecutive tags). Leaves the
/// rest untouched; does NOT scan the interior (that is `strip_tool_call_json`'s job).
fn strip_leading_tool_tags(s: &str) -> String {
    let mut rest = s;
    while let Some(len) = leading_tool_tag_len(rest) {
        rest = &rest[len..];
    }
    if std::ptr::eq(rest, s) { s.to_string() } else { rest.trim_start().to_string() }
}

/// Leading-fragment gate for the streamed synthesis. Given the buffered
/// head, decide whether to flush (returning the cleaned text to send, possibly empty) or keep
/// buffering (`None`) until a leading tool tag closes and real content appears. A `cap` bounds
/// how long TTFT is delayed for a pathological head.
fn lead_gate_ready(buf: &str, cap: usize) -> Option<String> {
    let t = buf.trim_start();
    if t.is_empty() {
        return (buf.len() >= cap).then(String::new);
    }
    if t.starts_with('<') {
        // Incomplete leading tag (no `>` yet) → wait for it to close, unless we hit the cap.
        if !t.contains('>') {
            return (buf.len() >= cap).then(|| strip_leading_tool_tags(buf));
        }
        let stripped = strip_leading_tool_tags(buf);
        if stripped.trim_start().is_empty() {
            // Only (complete) tool tags so far — wait for real content, or give up at the cap.
            return (buf.len() >= cap).then_some(stripped);
        }
        return Some(stripped);
    }
    // Starts with real text (not a tag) → flush as-is.
    Some(buf.to_string())
}

/// Aggregate result of a per-part synthesis pass.
struct PerPartOut {
    acc: String,
    reasoning: String,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    reasoning_tokens: Option<i32>,
    model: Option<String>,
    finish_reason: Option<String>,
    detached: bool,
    cancelled: bool,
}

/// A synthesis-refusal pattern — the final model claiming absence. Used only for telemetry
/// a part with evidence that still refuses is logged for calibration.
fn looks_like_refusal(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    ["not found", "no relevant", "not contained", "no information", "not present in", "absent from"]
        .iter()
        .any(|p| t.contains(p))
}

fn sum_opt(a: Option<i32>, b: Option<i32>) -> Option<i32> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0) + y.unwrap_or(0)),
    }
}

/// Per-part synthesis: generate each numbered part over ONLY its own
/// slice (own sub-answers + own [D#] blocks), concurrently (bounded), and stream them to the
/// client in part order — parts compute in parallel (wall-clock ≈ slowest part) but the user
/// sees them sequentially with part-1 TTFT preserved. Each part's system omits skills (like
/// the unified fix) and reuses the same reasoning clamp; the original prompt is background
/// only, so a part never sees another part's sub-answers (no contamination).
#[allow(clippy::too_many_arguments)]
async fn synthesize_per_part(
    state: &AppState,
    ctx: &AuthContext,
    prompt: &str,
    memory_facts: &[String],
    unattended: bool,
    gk_fallback: bool,
    answer_reasoning: Option<&crate::reasoning::ReasoningSpec>,
    llm_sel: Option<&crate::ext::ResolvedProvider>,
    sampling: &Sampling,
    parts: &[ml::SynthPart],
    turn_id: Uuid,
    asst_id: Uuid,
    tx: &mpsc::Sender<ServerFrame>,
    cancel: &Arc<Notify>,
) -> PerPartOut {
    // Bound concurrent generates so a 5-6 part prompt doesn't fire every heavy synthesis at
    // once; parts still stream in order so a small bound keeps the pipeline full.
    let sem = Arc::new(Semaphore::new(3));
    let base_ov = ml::with_reasoning(ml::provider_overrides_with_llm(state, ctx.user_id, llm_sel).await, answer_reasoning);
    let max_cont = crate::config::runtime::get(&state.pg, "chat.answer_max_continuations")
        .await.ok().flatten().and_then(|e| e.value.parse::<usize>().ok()).unwrap_or(4);
    let idle = synthesis_idle_timeout(&state.pg).await;
    let max_total = synthesis_max_total(&state.pg).await;

    // Spawn one generate task per part, each forwarding its GenEvents to an ordered channel.
    let mut receivers: Vec<mpsc::Receiver<ml::GenEvent>> = Vec::with_capacity(parts.len());
    for part in parts {
        let system = compose::build_system(prompt, ctx, &[], memory_facts, Some(&part.context), unattended, gk_fallback);
        let user = format!(
            "Full request (for context only — do not answer the other parts): {prompt}\n\nAnswer ONLY this part: {}",
            part.title
        );
        let messages: Vec<ml::Message> =
            vec![json!({"role": "system", "content": system}), json!({"role": "user", "content": user})];
        let req = ml::GenerateRequest { messages, sampling: sampling.clone(), model: None, tools: None, overrides: base_ov.clone() };
        let (ptx, prx) = mpsc::channel::<ml::GenEvent>(64);
        receivers.push(prx);
        let http = state.http.clone();
        let base_url = state.boot.ml.base_url.clone();
        let sem = sem.clone();
        tokio::spawn(async move {
            let _permit = sem.acquire_owned().await;
            // Bound BOTH the stream setup and every inter-event gap by `idle`: a wedged provider
            // stream (no bytes) trips the timeout, which breaks the loop → drops `s` → aborts the
            // ML/OpenAI request and RELEASES the permit (no zombie holding a synthesis slot), and
            // forwards an Error the consumer fail-softs on. A live stream resets `idle` each event.
            let started = tokio::time::timeout(idle, ml::generate(&http, &base_url, &req)).await;
            let mut s = match started {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => { let _ = ptx.send(ml::GenEvent::Error { message: e.to_string() }).await; return; }
                Err(_) => {
                    let _ = ptx.send(ml::GenEvent::Error { message: "generation did not start (stalled)".into() }).await;
                    return;
                }
            };
            // `deadline` is the TOTAL budget — reasoning deltas reset `idle` but NOT this, so a
            // reasoning-runaway (minutes of summary deltas, zero output tokens) is bounded here.
            let deadline = tokio::time::Instant::now() + max_total;
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    let _ = ptx.send(ml::GenEvent::Error {
                        message: format!("generation exceeded {}s budget (reasoning runaway?)", max_total.as_secs()),
                    }).await;
                    break; // drop `s` → abort ML request, free permit
                }
                // Trip on WHICHEVER is sooner: the idle gap (silent stall) or the total budget.
                match tokio::time::timeout(idle.min(remaining), s.recv()).await {
                    Ok(Some(ev)) => { if ptx.send(ev).await.is_err() { break; } } // consumer gone
                    Ok(None) => break,                                            // stream ended
                    Err(_) => {
                        let msg = if remaining <= idle {
                            format!("generation exceeded {}s budget (reasoning runaway?)", max_total.as_secs())
                        } else {
                            format!("generation stalled (no output for {}s)", idle.as_secs())
                        };
                        let _ = ptx.send(ml::GenEvent::Error { message: msg }).await;
                        break; // drop `s` → abort ML request, free permit
                    }
                }
            }
        });
    }

    let mut out = PerPartOut {
        acc: String::new(),
        reasoning: String::new(),
        prompt_tokens: None,
        completion_tokens: None,
        reasoning_tokens: None,
        model: None,
        finish_reason: None,
        detached: false,
        cancelled: false,
    };

    for (idx, (mut rx, part)) in receivers.into_iter().zip(parts.iter()).enumerate() {
        if out.cancelled {
            break;
        }
        let header = if idx == 0 { format!("## {}\n\n", part.title) } else { format!("\n\n## {}\n\n", part.title) };
        out.acc.push_str(&header);
        if !out.detached && tx.send(ServerFrame::ChatToken { turn_id, delta: header }).await.is_err() {
            out.detached = true;
        }
        let mut part_text = String::new();
        let mut part_finish: Option<String> = None;
        let mut part_failed: Option<String> = None;
        let mut head_open = true;
        let mut head_buf = String::new();
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Some(ml::GenEvent::Token { delta }) => {
                        part_text.push_str(&delta);
                        out.acc.push_str(&delta);
                        // lead-gate: strip a leading tool-shaped fragment before the first flush.
                        let send: Option<String> = if head_open {
                            head_buf.push_str(&delta);
                            match lead_gate_ready(&head_buf, 4096) {
                                Some(clean) => { head_open = false; (!clean.is_empty()).then_some(clean) }
                                None => None,
                            }
                        } else {
                            Some(delta)
                        };
                        if !out.detached {
                            if let Some(s) = send {
                                if tx.send(ServerFrame::ChatToken { turn_id, delta: s }).await.is_err() {
                                    out.detached = true;
                                }
                            }
                        }
                    }
                    Some(ml::GenEvent::Reasoning { delta }) => {
                        out.reasoning.push_str(&delta);
                        if !out.detached && tx.send(ServerFrame::ChatReasoning { turn_id, delta }).await.is_err() {
                            out.detached = true;
                        }
                    }
                    Some(ml::GenEvent::Done { usage, model, finish_reason }) => {
                        out.prompt_tokens = sum_opt(out.prompt_tokens, usage.prompt_tokens);
                        out.completion_tokens = sum_opt(out.completion_tokens, usage.completion_tokens);
                        out.reasoning_tokens = sum_opt(out.reasoning_tokens, usage.reasoning_tokens);
                        if model.is_some() { out.model = model; }
                        part_finish = finish_reason.clone();
                        if out.finish_reason.is_none() { out.finish_reason = finish_reason; }
                        break;
                    }
                    Some(ml::GenEvent::ToolCall { .. }) => {
                        // per_part never advertises tools, so a tool call here is unexpected —
                        // ignore it rather than mishandle it (mid-stream tools are unified-only).
                    }
                    Some(ml::GenEvent::Error { message }) => {
                        // The part's task hit an error or the idle-stall guard — record it and
                        // break; the post-loop emits a fail-soft notice and the NEXT part proceeds.
                        part_failed = Some(message);
                        break;
                    }
                    None => break,
                },
                _ = cancel.notified() => { out.cancelled = true; break; }
            }
        }
        // auto-continue a part truncated at the token cap (same mechanism as unified),
        // so a long part finishes even at high/xhigh reasoning effort.
        let mut cont = 0;
        while is_truncation(part_finish.as_deref()) && !part_text.trim().is_empty()
            && !out.cancelled && !out.detached && cont < max_cont
        {
            cont += 1;
            tracing::warn!(part = %part.title, cont, "per-part answer truncated; auto-continuing");
            let system = compose::build_system(prompt, ctx, &[], memory_facts, Some(&part.context), unattended, gk_fallback);
            let user = format!(
                "Full request (for context only — do not answer the other parts): {prompt}\n\nAnswer ONLY this part: {}",
                part.title
            );
            let base = vec![json!({"role": "system", "content": system}), json!({"role": "user", "content": user})];
            let req = ml::GenerateRequest {
                messages: continuation_messages(&base, &part_text),
                sampling: sampling.clone(),
                model: None,
                tools: None,
                overrides: base_ov.clone(),
            };
            match stream_generate(state, &req, turn_id, asst_id, tx, cancel, &mut out.acc, &mut out.reasoning, &mut out.detached, false, None).await {
                Ok(term) => {
                    part_finish = term.finish;
                    out.prompt_tokens = sum_opt(out.prompt_tokens, term.prompt_tokens);
                    out.completion_tokens = sum_opt(out.completion_tokens, term.completion_tokens);
                    out.reasoning_tokens = sum_opt(out.reasoning_tokens, term.reasoning_tokens);
                    if term.model.is_some() { out.model = term.model; }
                    if term.interrupted { out.cancelled = true; }
                }
                Err(e) => { tracing::warn!(part = %part.title, error = %e, "per-part continuation failed"); break; }
            }
        }
        if let Some(err) = &part_failed {
            // Fail-soft: a stalled/errored part gets a visible notice (keeping any partial text
            // already streamed) and the loop moves on — one bad part never blocks the whole answer.
            let notice = if part_text.trim().is_empty() {
                "_(No answer was generated for this part — generation stalled or failed.)_".to_string()
            } else {
                "\n\n_(This part was cut short — generation stalled or failed.)_".to_string()
            };
            tracing::warn!(part = %part.title, error = %err, "per-part synthesis fail-soft");
            out.acc.push_str(&notice);
            if !out.detached {
                let _ = tx.send(ServerFrame::ChatToken { turn_id, delta: notice }).await;
            }
        } else if part_text.trim().is_empty() {
            let notice = "_(No answer was generated for this part.)_";
            out.acc.push_str(notice);
            if !out.detached {
                let _ = tx.send(ServerFrame::ChatToken { turn_id, delta: notice.to_string() }).await;
            }
        } else if part.has_evidence && looks_like_refusal(&part_text) {
            // telemetry (NOT a block): a part with retrieved evidence that still refuses.
            tracing::warn!(part = %part.title, "synthesis_refusal_with_evidence");
        }
        // Flush the growing answer to the durable row after each part (parts are few).
        let _ = sqlx::query!("UPDATE messages SET content = $1 WHERE id = $2", out.acc, asst_id)
            .execute(&state.pg)
            .await;
    }
    if out.finish_reason.is_none() && !out.cancelled {
        out.finish_reason = Some("stop".to_string());
    }
    out
}

/// per-turn wall-clock phase table. Debug instrument: monotonic
/// marks accumulated over a turn and rendered once at `chat::phases` DEBUG. Phases
/// overlap under concurrency, so per-phase rows are wall time, not additive; TOTAL is
/// the real turn wall from `start`.
struct TurnPhases {
    start: std::time::Instant,
    rows: Vec<(String, std::time::Duration)>,
}

impl TurnPhases {
    fn new() -> Self {
        Self { start: std::time::Instant::now(), rows: Vec::new() }
    }

    fn mark(&mut self, label: impl Into<String>, dt: std::time::Duration) {
        self.rows.push((label.into(), dt));
    }

    fn summary(&self) -> String {
        let width = self.rows.iter().map(|(l, _)| l.len()).max().unwrap_or(5).max(5);
        let mut s = String::from("(phases overlap under concurrency; per-phase wall, not additive)\n");
        for (l, d) in &self.rows {
            s.push_str(&format!("  {l:<width$}  {:9.1} ms\n", d.as_secs_f64() * 1000.0));
        }
        s.push_str(&format!("  {:<width$}  {:9.1} ms  (turn wall)", "TOTAL", self.start.elapsed().as_secs_f64() * 1000.0));
        s
    }
}

/// The moving tool-loop progress label shown while the model decides its next step
/// User-safe: no ids/paths.
fn step_progress_label(k: usize, n: usize) -> String {
    format!("Thinking · step {k} of {n}")
}

const WARN_THRESHOLD_PCT: i64 = 75;

/// Emit a `context.warning` once the prompt is using ≥75% of the budget, so the
/// UI can prompt the user to start a new chat. Compaction still runs regardless.
async fn maybe_warn_context(
    chat_id: Uuid,
    system_tokens: i64,
    history: &[HistMsg],
    budget: i64,
    answer_reserve: i64,
    tx: &mpsc::Sender<ServerFrame>,
) {
    if budget <= 0 {
        return;
    }
    let hist: i64 = history.iter().map(|m| est_tokens(&m.content)).sum();
    let pct = ((system_tokens + hist + answer_reserve) * 100 / budget).clamp(0, 100);
    if pct >= WARN_THRESHOLD_PCT {
        let _ = tx.send(ServerFrame::ContextWarning { chat_id, usage_pct: pct as u32 }).await;
    }
}

/// Compact history into `[rolling summary] + [verbatim tail]` so [6] fits
/// `history_budget`. The summary is **incremental and
/// persisted** (`chat_summaries`): each turn folds only the turns that have
/// newly aged out of the verbatim window into the existing summary, so older
/// turns are never re-summarised and the summary survives a reload. It is
/// **never** written to the persistent memory store. Best-effort: a failed
/// summary call leaves the turns verbatim rather than dropping them.
async fn compact_history(
    state: &AppState,
    chat_id: Uuid,
    turn_id: Uuid,
    history: Vec<HistMsg>,
    history_budget: i64,
    sampling: &Sampling,
    tx: &mpsc::Sender<ServerFrame>,
) -> Vec<Value> {
    const KEEP_MIN: usize = 2;

    // Load any persisted rolling summary + the watermark it already covers.
    let (mut summary, mut watermark) =
        load_summary(&state.pg, chat_id).await.unwrap_or((String::new(), i32::MIN));

    // The verbatim window is everything newer than the watermark.
    let mut verbatim: Vec<HistMsg> = history.into_iter().filter(|m| m.seq > watermark).collect();

    // Fold the OLDEST verbatim turns into the summary until the rest fit budget.
    let tok = |v: &[HistMsg]| -> i64 { v.iter().map(|m| est_tokens(&m.content)).sum() };
    let mut to_fold: Vec<HistMsg> = Vec::new();
    while tok(&verbatim) > history_budget && verbatim.len() > KEEP_MIN {
        to_fold.push(verbatim.remove(0));
    }

    if !to_fold.is_empty() {
        let joined: String = to_fold
            .iter()
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n");
        let (sys, usr) = if summary.is_empty() {
            (
                "Summarise this earlier conversation concisely, preserving names, facts, \
                 and decisions. Output only the summary.".to_string(),
                joined,
            )
        } else {
            (
                "Update the running summary so it also covers the new earlier turns. \
                 Preserve names, facts, and decisions from BOTH. Output only the updated \
                 summary.".to_string(),
                format!("[Running summary]\n{summary}\n\n[New earlier turns]\n{joined}"),
            )
        };
        let sum_msgs =
            vec![json!({ "role": "system", "content": sys }), json!({ "role": "user", "content": usr })];
        match ml::chat_step(&state.http, &state.boot.ml.base_url, &sum_msgs, None, sampling, ml::provider_overrides(state, None).await).await {
            Ok(s) if !s.content.trim().is_empty() => {
                summary = s.content;
                watermark = to_fold.last().map(|m| m.seq).unwrap_or(watermark);
                let _ = upsert_summary(&state.pg, chat_id, &summary, watermark).await;
                let _ = tx.send(ServerFrame::ChatCompacted { turn_id, summarised: to_fold.len() as u32 }).await;
            }
            _ => {
                // Summary failed — keep the turns verbatim (better an over-long
                // prompt than dropped context). Restore them at the front.
                to_fold.reverse();
                for m in to_fold {
                    verbatim.insert(0, m);
                }
            }
        }
    }

    // Assemble: the rolling summary (if any) as a leading system note, then the
    // verbatim tail. build_messages prepends the real seven-layer system message.
    let mut out = Vec::with_capacity(verbatim.len() + 1);
    if !summary.is_empty() {
        out.push(json!({ "role": "system", "content": format!("[Earlier conversation summary]\n{summary}") }));
    }
    out.extend(verbatim.iter().map(HistMsg::to_value));
    out
}

async fn load_summary(pool: &sqlx::PgPool, chat_id: Uuid) -> Option<(String, i32)> {
    sqlx::query!("SELECT summary, up_to_sequence FROM chat_summaries WHERE chat_id = $1", chat_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|r| (r.summary, r.up_to_sequence))
}

async fn upsert_summary(pool: &sqlx::PgPool, chat_id: Uuid, summary: &str, watermark: i32) -> Result<()> {
    sqlx::query!(
        "INSERT INTO chat_summaries (chat_id, summary, up_to_sequence, updated_at) \
         VALUES ($1, $2, $3, now()) \
         ON CONFLICT (chat_id) DO UPDATE \
         SET summary = EXCLUDED.summary, up_to_sequence = EXCLUDED.up_to_sequence, updated_at = now()",
        chat_id, summary, watermark
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn load_history(pool: &sqlx::PgPool, chat_id: Uuid) -> Result<Vec<HistMsg>> {
    let rows = sqlx::query!(
        r#"SELECT sequence_number AS "seq!", role::text AS "role!", content
           FROM messages
           WHERE chat_id = $1 AND role IN ('user', 'assistant')
           ORDER BY sequence_number ASC"#,
        chat_id
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| HistMsg { seq: r.seq, role: r.role, content: r.content }).collect())
}

/// Enqueue a pre-stream/in-loop chat audit event for the writer task — off the
/// chat-turn await path (L6). Infallible by design (`try_send` drops with a
/// metric under saturation); events that must be durable use `audit::append`
/// directly at their call sites.
fn audit_event(state: &AppState, ctx: &AuthContext, action: &str, chat_id: Uuid, payload: Value) {
    let mut event = AuditEvent::action(action, ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("chat".into());
    event.resource_id = Some(chat_id);
    event.payload = Some(payload);
    audit::enqueue(&state.audit_tx, event);
}

fn audit_tool(state: &AppState, ctx: &AuthContext, chat_id: Uuid, name: &str, action: &str) {
    let mut event = AuditEvent::action(format!("tool.{action}"), ctx.role.as_str());
    event.actor_user_id = ctx.user_id;
    event.resource_type = Some("tool".into());
    event.resource_id = Some(chat_id);
    event.payload = Some(json!({ "tool": name }));
    audit::enqueue(&state.audit_tx, event);
}

/// Cheap risk classification for Legal-workspace citations: `amber` if the quote
/// or clause ref mentions a risk-bearing term, else `ok`. A placeholder for a
/// proper ML signal; the UI shows it only in Legal mode.
fn risk_of(quote: &str, clause: Option<&str>) -> Option<String> {
    const KW: [&str; 12] = [
        "liab", "indemn", "terminat", "penalt", "breach", "damages", "default", "forfeit",
        "dispute", "warrant", "arbitrat", "jurisdiction",
    ];
    let hay = format!("{} {}", quote.to_lowercase(), clause.unwrap_or("").to_lowercase());
    Some(if KW.iter().any(|k| hay.contains(k)) { "amber" } else { "ok" }.into())
}

/// Persist document-anchored citations for a posted message. `pub(crate)` so
/// background posters (Deep Research) reuse the exact same insert shape.
pub(crate) async fn persist_citations(pool: &sqlx::PgPool, message_id: Uuid, citations: &[ml::Citation]) -> Result<()> {
    if citations.is_empty() {
        return Ok(());
    }
    // One UNNEST insert instead of a row-per-round-trip loop.
    // Build a parallel array per column; `additional_metadata` is serialised
    // and cast back to jsonb per row.
    let ids: Vec<Uuid> = citations.iter().map(|_| Uuid::now_v7()).collect();
    let doc_ids: Vec<Option<Uuid>> = citations.iter().map(|c| c.doc_id).collect();
    let quotes: Vec<String> = citations.iter().map(|c| c.quote_text.clone()).collect();
    let pages: Vec<Option<i32>> = citations.iter().map(|c| c.page_number).collect();
    let clauses: Vec<Option<String>> = citations.iter().map(|c| c.clause_section_ref.clone()).collect();
    let metas: Vec<String> = citations
        .iter()
        .map(|c| json!({ "chunk_index": c.chunk_index }).to_string())
        .collect();
    sqlx::query!(
        "INSERT INTO citations \
         (id, message_id, doc_id, quote_text, page_number, clause_section_ref, additional_metadata) \
         SELECT id, $2, doc_id, quote_text, page_number, clause_section_ref, meta::jsonb \
         FROM UNNEST($1::uuid[], $3::uuid[], $4::text[], $5::int4[], $6::text[], $7::text[]) \
            AS t(id, doc_id, quote_text, page_number, clause_section_ref, meta)",
        &ids,
        message_id,
        &doc_ids as &[Option<Uuid>],
        &quotes,
        &pages as &[Option<i32>],
        &clauses as &[Option<String>],
        &metas,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Load persisted citations for a chat's messages, grouped by `message_id`, in the same
/// RAG-then-web order the live `ChatCitations` frame uses — so a reload renders the
/// Sources list identically (the live frame is otherwise the only delivery, which is why
/// sources vanished on reload). Best-effort per source: a query error yields no rows for
/// that source rather than failing the history load.
pub(crate) async fn load_citations(
    pool: &sqlx::PgPool,
    chat_id: Uuid,
) -> std::collections::HashMap<Uuid, Vec<crate::ws::protocol::CitationOut>> {
    use crate::ws::protocol::CitationOut;
    let mut by_msg: std::collections::HashMap<Uuid, Vec<CitationOut>> = std::collections::HashMap::new();

    // RAG (document) citations — byte-identical to the live mapping in the turn.
    if let Ok(rows) = sqlx::query!(
        r#"SELECT c.message_id, c.doc_id, c.quote_text, c.page_number, c.clause_section_ref
           FROM citations c JOIN messages m ON m.id = c.message_id
           WHERE m.chat_id = $1 ORDER BY c.created_at, c.id"#,
        chat_id
    )
    .fetch_all(pool)
    .await
    {
        for r in rows {
            by_msg.entry(r.message_id).or_default().push(CitationOut {
                doc_id: r.doc_id,
                quote_text: r.quote_text.clone(),
                page_number: r.page_number,
                clause_section_ref: r.clause_section_ref.clone(),
                risk: risk_of(&r.quote_text, r.clause_section_ref.as_deref()),
                ..Default::default()
            });
        }
    }

    // Web citations — appended after RAG (the live path merges both into one frame).
    if let Ok(rows) = sqlx::query!(
        r#"SELECT w.message_id, w.url, w.title, w.domain, w.published_date, w.fetched_at, w.quote_text, w.snippet_only
           FROM web_citations w JOIN messages m ON m.id = w.message_id
           WHERE m.chat_id = $1 ORDER BY w.created_at, w.id"#,
        chat_id
    )
    .fetch_all(pool)
    .await
    {
        for r in rows {
            let Some(mid) = r.message_id else { continue };
            by_msg.entry(mid).or_default().push(CitationOut {
                quote_text: r.quote_text,
                url: Some(r.url),
                title: r.title,
                domain: Some(r.domain),
                published_date: r.published_date.map(|d| d.to_string()),
                fetched_at: r.fetched_at.format(&time::format_description::well_known::Rfc3339).ok(),
                snippet_only: Some(r.snippet_only),
                ..Default::default()
            });
        }
    }
    by_msg
}

/// Cheap best-effort LLM naming of a chat from the user's opening prompt. Returns
/// a concise Title-Case title, or `None` on any error/empty (caller keeps the
/// "New chat" placeholder). Mirrors [`name_artefact`].
async fn name_chat(state: &AppState, user_id: Option<Uuid>, content: &str) -> Option<String> {
    let head: String = content.chars().take(1500).collect();
    if head.trim().is_empty() {
        return None;
    }
    let messages = vec![
        json!({ "role": "system", "content": "You name chat conversations. Reply with ONLY a concise title in Title Case, 3 to 6 words, capturing the user's intent. British English. No quotation marks, no trailing punctuation, no preamble." }),
        json!({ "role": "user", "content": format!("First message:\n{head}\n\nTitle:") }),
    ];
    // Budget 512 (not ~24): a reasoning model spends hidden reasoning tokens before
    // the title, so a tiny cap returns empty content. `reasoning_effort=minimal`
    // keeps it fast on providers that honour it (ML clamps + only sends where valid).
    let sampling = crate::ml::Sampling {
        max_tokens: Some(512),
        reasoning_effort: Some("minimal".into()),
        ..Default::default()
    };
    let step = crate::ml::chat_step(&state.http, &state.boot.ml.base_url, &messages, None, &sampling, crate::ml::provider_overrides(state, user_id).await)
        .await
        .ok()?;
    let line = step.content.trim().lines().next().unwrap_or("").to_string();
    let t = strip_title_markers(&line);
    (t.chars().count() >= 3).then(|| t.chars().take(80).collect())
}

#[cfg(test)]
mod think_tests {
    use super::strip_think;

    #[test]
    fn no_think_block_is_passthrough() {
        assert_eq!(strip_think("Just the answer."), "Just the answer.");
    }

    #[test]
    fn strips_closed_block_keeps_answer() {
        assert_eq!(strip_think("<think>reasoning here</think>The answer."), "The answer.");
    }

    #[test]
    fn drops_unclosed_trailing_block() {
        // Budget exhausted mid-think: nothing after <think> survives.
        assert_eq!(strip_think("<think>still reasoning"), "");
        assert_eq!(strip_think("preamble <think>cut off"), "preamble");
    }

    #[test]
    fn folds_multiple_blocks_and_orphan_tags() {
        assert_eq!(strip_think("<think>a</think>X<think>b</think>Y"), "XY");
        assert_eq!(strip_think("answer</think>"), "answer");
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::{attachment_block, sandbox_name};

    #[test]
    fn sandbox_name_is_a_safe_basename() {
        assert_eq!(sandbox_name("playlist.xlsx"), "playlist.xlsx");
        assert_eq!(sandbox_name("dir/sub/data.csv"), "data.csv");
        assert_eq!(sandbox_name("a\\b\\c.xlsx"), "c.xlsx");
    }

    #[test]
    fn not_in_sandbox_injects_full_text() {
        let big = "x\n".repeat(10_000); // > compact threshold
        let out = attachment_block("playlist.xlsx", &big, false);
        assert!(out.starts_with("[Attached document: playlist.xlsx]"));
        assert!(out.contains(&big)); // full text retained
        assert!(!out.contains("code_interpreter"));
    }

    #[test]
    fn in_sandbox_large_text_is_compact() {
        // Well over the 8k-char compact threshold.
        let big = (0..2000).map(|i| format!("row {i} ................")).collect::<Vec<_>>().join("\n");
        let out = attachment_block("playlist.xlsx", &big, true);
        assert!(out.contains("./playlist.xlsx")); // told where the file is
        assert!(out.contains("Preview (first 20 lines"));
        assert!(out.contains("row 0") && out.contains("row 19"));
        assert!(!out.contains("row 50")); // truncated — full data is in the sandbox file
    }

    #[test]
    fn in_sandbox_small_text_is_kept_full() {
        let small = "Title\tArtist\nTrack\tArtist";
        let out = attachment_block("p.xlsx", small, true);
        assert!(out.contains("./p.xlsx") && out.contains(small)); // note + full text (small)
    }
}

#[cfg(test)]
mod prompt_tests {
    use super::effective_prompt;

    #[test]
    fn blank_or_missing_prompt_falls_back_to_default() {
        let d = "You are a helpful assistant.";
        assert_eq!(effective_prompt(&None, d), d);
        assert_eq!(effective_prompt(&Some("".into()), d), d); // editor saves "" for a cleared field
        assert_eq!(effective_prompt(&Some("   \n".into()), d), d);
        // A real prompt is kept verbatim.
        assert_eq!(effective_prompt(&Some("Be terse.".into()), d), "Be terse.");
    }
}

#[cfg(test)]
mod reuse_answer_tests {
    use super::reuse_answer;

    #[test]
    fn reuses_post_tool_answer() {
        // A tool ran and the model gave a real answer -> reuse it, skip the reroll.
        assert_eq!(
            reuse_answer(true, "The UK VAT threshold is £90,000.".into()),
            Some("The UK VAT threshold is £90,000.".to_string())
        );
    }

    #[test]
    fn no_tool_ran_streams_live() {
        // Step-1 answer with no tool calls -> None, so it streams live (TTFT).
        assert_eq!(reuse_answer(false, "An immediate answer.".into()), None);
    }

    #[test]
    fn empty_terminating_content_falls_through() {
        // A tool ran but the terminating step had no content -> None (streaming path).
        assert_eq!(reuse_answer(true, String::new()), None);
        assert_eq!(reuse_answer(true, "   \n ".into()), None);
    }

    #[test]
    fn terminal_tool_call_json_is_not_reused() {
        // A tool call mis-emitted as text must NOT become the answer.
        let json = r#"{"name":"read_skill","arguments":{"skill_id":"5c111000-0000-0000-0000-000000000007"}}"#;
        assert_eq!(super::reuse_answer(true, json.into()), None);
    }
}

#[cfg(test)]
mod tool_leak_tests {
    use super::{is_truncation, lead_gate_ready, looks_like_refusal, looks_like_tool_call, retry_cap, strip_tool_leak, sum_opt};

    #[test]
    fn truncation_reasons_recognised_across_providers() {
        // chat-completions "length", OpenAI Responses "max_output_tokens", Anthropic "max_tokens".
        assert!(is_truncation(Some("length")));
        assert!(is_truncation(Some("max_output_tokens")));
        assert!(is_truncation(Some("max_tokens")));
        assert!(!is_truncation(Some("stop")));
        assert!(!is_truncation(None));
    }

    #[test]
    fn retry_cap_fires_on_responses_truncation_when_empty() {
        // Empty answer truncated on the Responses path (the case that used to be missed).
        assert!(retry_cap(false, false, true, Some("max_output_tokens"), Some(8192)).is_some());
        // Non-empty truncation is NOT a cap-retry (it's handled by auto-continue instead).
        assert!(retry_cap(false, false, false, Some("length"), Some(8192)).is_none());
        // Clean stop, interrupt, or error never retries.
        assert!(retry_cap(false, false, true, Some("stop"), Some(8192)).is_none());
        assert!(retry_cap(true, false, true, Some("length"), Some(8192)).is_none());
    }

    const UUID: &str = "5c111000-0000-0000-0000-000000000007";

    #[test]
    fn strips_text_emitted_tool_call() {
        let s = format!(r#"I'll read it. {{"name":"read_skill","arguments":{{"skill_id":"{UUID}"}}}}"#);
        let out = strip_tool_leak(&s);
        assert!(!out.contains("read_skill"), "tool JSON survived: {out:?}");
        assert!(!out.contains(UUID), "uuid survived: {out:?}");
        assert_eq!(out, "I'll read it.");
    }

    #[test]
    fn strips_bare_uuid_but_keeps_prose() {
        assert_eq!(strip_tool_leak(&format!("See skill {UUID} now")), "See skill now");
        // A UUID inside a URL/path is preceded by a non-space char → kept.
        let url = format!("http://x/{UUID}");
        assert_eq!(strip_tool_leak(&url), url);
    }

    #[test]
    fn leaves_normal_answer_untouched() {
        let a = "The company must file form AA01 with the registrar.";
        assert_eq!(strip_tool_leak(a), a);
        // Prose with a lone brace is not a tool call.
        let b = "Apply the rule {see clause 4}.";
        assert_eq!(strip_tool_leak(b), b);
    }

    #[test]
    fn refusal_detector_and_sum() {
        // telemetry: a refusal phrase is caught (case-insensitive),
        // a real answer is not.
        assert!(looks_like_refusal("The material is NOT FOUND in the supplied sources."));
        assert!(looks_like_refusal("No relevant provision exists."));
        assert!(!looks_like_refusal("Under s.994 a member may petition the court."));
        // Usage aggregation across parts: None+None stays None; otherwise sums.
        assert_eq!(sum_opt(None, None), None);
        assert_eq!(sum_opt(Some(10), None), Some(10));
        assert_eq!(sum_opt(Some(10), Some(5)), Some(15));
    }

    #[test]
    fn strips_leading_read_skill_xml() {
        // a leading self-closing tool tag is removed; prose survives.
        let s = format!("<read_skill id=\"{UUID}\"/>Under section 994 a member may petition.");
        assert_eq!(strip_tool_leak(&s), "Under section 994 a member may petition.");
        // A non-tool tag a real answer might contain is preserved.
        let keep = "<b>Bold</b> point about s.171.";
        assert_eq!(strip_tool_leak(keep), keep);
    }

    #[test]
    fn lead_gate_buffers_two_leading_xml_tags_then_flushes_text() {
        // A stream: "<read_skill …/>" then "<tool …/>" then real text → only the text shows.
        let cap = 4096;
        assert_eq!(lead_gate_ready("<read_skill", cap), None, "incomplete tag: wait");
        assert_eq!(lead_gate_ready("<read_skill id=\"a\"/>", cap), None, "only a tool tag: wait");
        assert_eq!(lead_gate_ready("<read_skill id=\"a\"/><tool/>", cap), None, "two tool tags: wait");
        assert_eq!(
            lead_gate_ready("<read_skill id=\"a\"/><tool/>Hello world", cap),
            Some("Hello world".to_string()),
            "real text after the tags → flush cleaned",
        );
        // A head that starts with real text flushes immediately (no TTFT penalty).
        assert_eq!(lead_gate_ready("Hello", cap), Some("Hello".to_string()));
    }

    #[test]
    fn looks_like_tool_call_discriminates() {
        assert!(looks_like_tool_call(r#"{"name":"x","arguments":{}}"#));
        assert!(looks_like_tool_call(r#"{"function":{"name":"x"}}"#));
        assert!(!looks_like_tool_call(r#"{"clause":"4","note":"applies"}"#));
        assert!(!looks_like_tool_call("The clause {a} applies"));
    }
}

#[cfg(test)]
mod turn_phase_tests {
    use super::{step_progress_label, TurnPhases};
    use std::time::Duration;

    #[test]
    fn summary_lists_rows_and_total() {
        let mut p = TurnPhases::new();
        p.mark("retrieve", Duration::from_millis(120));
        p.mark("generate", Duration::from_millis(80));
        let s = p.summary();
        assert!(s.contains("retrieve"));
        assert!(s.contains("generate"));
        assert!(s.contains("TOTAL"));
    }

    #[test]
    fn step_label_is_user_safe() {
        assert_eq!(step_progress_label(1, 6), "Thinking · step 1 of 6");
        assert_eq!(step_progress_label(6, 6), "Thinking · step 6 of 6");
    }
}

#[cfg(test)]
mod retry_cap_tests {
    use super::retry_cap;

    #[test]
    fn length_and_empty_retries_with_headroom() {
        // No prior cap -> the 32768 floor.
        assert_eq!(retry_cap(false, false, true, Some("length"), None), Some(32768));
        // Small prior cap -> the floor still wins over 2x.
        assert_eq!(retry_cap(false, false, true, Some("length"), Some(8192)), Some(32768));
        // Large prior cap -> double it (beats the floor).
        assert_eq!(retry_cap(false, false, true, Some("length"), Some(32768)), Some(65536));
    }

    #[test]
    fn non_empty_answer_never_retries() {
        // The model answered — even if it also hit the cap — so keep the answer.
        assert_eq!(retry_cap(false, false, false, Some("length"), Some(8192)), None);
    }

    #[test]
    fn normal_stop_never_retries() {
        // Empty but a clean stop (not a budget cut) -> the notice path, no retry.
        assert_eq!(retry_cap(false, false, true, Some("stop"), Some(8192)), None);
        assert_eq!(retry_cap(false, false, true, None, Some(8192)), None);
    }

    #[test]
    fn interrupt_or_error_never_retries() {
        assert_eq!(retry_cap(true, false, true, Some("length"), Some(8192)), None);
        assert_eq!(retry_cap(false, true, true, Some("length"), Some(8192)), None);
    }
}

#[cfg(test)]
mod title_tests {
    use super::{extract_title_body, is_heading_like, strip_title_markers};

    #[test]
    fn strips_markers_and_emphasis() {
        assert_eq!(strip_title_markers("**Confidentiality Memo — NDA**"), "Confidentiality Memo — NDA");
        assert_eq!(strip_title_markers("# Heading"), "Heading");
        assert_eq!(strip_title_markers("Subject: Annual Report"), "Annual Report");
    }

    #[test]
    fn picks_bold_heading_after_meta_preamble_and_drops_it() {
        let answer = "I'll use the generate_artefact tool. Here's the result:\n\n\
                      **Confidentiality Memo: NDA Clauses**\n\nThe parties agree…\nMore body.";
        let (title, body) = extract_title_body(answer).expect("a heading is found");
        assert_eq!(title, "Confidentiality Memo: NDA Clauses");
        assert!(body.starts_with("The parties agree"), "preamble dropped, body = {body:?}");
        assert!(!body.contains("generate_artefact"), "meta preamble must be dropped");
    }

    #[test]
    fn markdown_heading_is_used() {
        let (title, body) = extract_title_body("# Quarterly Review\n\nBody text here.").unwrap();
        assert_eq!(title, "Quarterly Review");
        assert_eq!(body, "Body text here.");
    }

    #[test]
    fn rejects_prompt_echo_and_plain_prose() {
        // First line is the echoed instruction, no heading anywhere → None (→ LLM-name).
        assert!(extract_title_body("Draft a short memo about NDA clauses as a PDF").is_none());
        assert!(extract_title_body("Here is some plain prose with no heading at all.").is_none());
    }

    #[test]
    fn rejects_tool_name_and_single_token_headings() {
        // A bold line that is just the tool name / one token is not a title.
        assert!(!is_heading_like("**generate_artefact**"));
        assert!(extract_title_body("**generate_artefact**\n\nbody").is_none());
        assert!(extract_title_body("**Memo**\n\nbody").is_none(), "single-word heading rejected");
        assert!(!is_heading_like("Run `generate_artefact` now"));
    }
}

#[cfg(test)]
mod compaction_tests {
    use super::*;
    use crate::config::BootConfig;
    use crate::{cache, db};
    use std::sync::Arc;

    async fn state_or_skip() -> Option<(AppState, sqlx::PgPool)> {
        let db_url = std::env::var("DATABASE_URL").ok()?;
        let redis_url =
            std::env::var("PAI__REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".into());
        let ml_url =
            std::env::var("PAI__ML__BASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());
        let pg = db::connect(&db_url, 5).await.ok()?;
        let redis = cache::create_pool(&redis_url).ok()?;
        let mut boot = BootConfig { database_url: db_url, redis_url, ..BootConfig::default() };
        boot.ml.base_url = ml_url;
        Some((AppState::new(pg.clone(), redis, Arc::new(boot)), pg))
    }

    fn msg(seq: i32, role: &str) -> HistMsg {
        HistMsg { seq, role: role.into(), content: format!("turn {seq}: ") + &"word ".repeat(40) }
    }

    /// The rolling summary is persisted and only the turns that NEWLY aged out
    /// of the verbatim window are folded — the watermark advances each pass and
    /// earlier turns are never re-summarised.
    #[tokio::test]
    async fn incremental_summary_persists_and_advances_watermark() {
        let Some((state, pg)) = state_or_skip().await else {
            eprintln!("skip: DATABASE_URL unset");
            return;
        };
        if ml::model_info(&state.http, &state.boot.ml.base_url).await.is_err() {
            eprintln!("skip: ML unavailable");
            return;
        }

        let owner: Uuid =
            sqlx::query_scalar("SELECT id FROM users LIMIT 1").fetch_one(&pg).await.unwrap();
        let chat_id = Uuid::now_v7();
        sqlx::query("INSERT INTO chats (id, owner_user_id, title) VALUES ($1, $2, 'compaction test')")
            .bind(chat_id)
            .bind(owner)
            .execute(&pg)
            .await
            .unwrap();

        let (tx, _rx) = mpsc::channel::<ServerFrame>(64);
        let sampling = Sampling::default();

        // Tiny budget forces the oldest turns to fold; the newest stay verbatim.
        let h1: Vec<HistMsg> =
            (1..=6).map(|i| msg(i, if i % 2 == 1 { "user" } else { "assistant" })).collect();
        let out1 = compact_history(&state, chat_id, Uuid::now_v7(), h1, 40, &sampling, &tx).await;
        assert_eq!(out1.first().unwrap()["role"], "system");
        assert!(out1.first().unwrap()["content"].as_str().unwrap().contains("Earlier conversation summary"));

        let (s1, w1) = load_summary(&pg, chat_id).await.expect("summary persisted");
        assert!(!s1.is_empty());
        assert!(w1 >= 1, "watermark advanced past the folded turns");

        // More turns arrive: only the new overflow (seq > watermark) folds.
        let h2: Vec<HistMsg> =
            (1..=10).map(|i| msg(i, if i % 2 == 1 { "user" } else { "assistant" })).collect();
        let _ = compact_history(&state, chat_id, Uuid::now_v7(), h2, 40, &sampling, &tx).await;
        let (_s2, w2) = load_summary(&pg, chat_id).await.expect("summary persisted");
        assert!(w2 > w1, "incremental fold advanced the watermark further ({w1} -> {w2})");

        let _ = sqlx::query("DELETE FROM chats WHERE id = $1").bind(chat_id).execute(&pg).await;
    }
}
