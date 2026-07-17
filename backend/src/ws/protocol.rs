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

//! WebSocket wire protocol — JSON envelope `{ version, type, … }` with a `type`
//! discriminator. One multiplexed socket
//! per user. This slice carries the chat-token-stream class + cancel + presence;
//! team-messaging replay is a later slice.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 1;

/// Frames the client sends. Unknown fields (e.g. `version`) are ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ClientFrame {
    #[serde(rename = "chat.send")]
    ChatSend {
        #[serde(default)]
        chat_id: Option<Uuid>,
        content: String,
        #[serde(default)]
        agent_id: Option<Uuid>,
        #[serde(default)]
        project_id: Option<Uuid>,
        /// Per-turn file attachments (ids from POST /api/chat-attachments). Their
        /// extracted text is injected into this turn's prompt, then dropped.
        #[serde(default)]
        attachment_ids: Vec<Uuid>,
        /// Per-turn extended-thinking effort, legacy wire form (`adaptive:<level>` /
        /// `off`). Superseded by `reasoning`; kept so an older client keeps working.
        #[serde(default)]
        thinking: Option<String>,
        /// Per-turn capability-aware reasoning request (enabled/level/return_trace).
        /// Preferred over `thinking`; when absent the legacy `thinking` is used.
        #[serde(default)]
        reasoning: Option<crate::reasoning::ReasoningSpec>,
        /// Per-turn LLM provider pick (composer dropdown, multi-LLM). When set and
        /// visible it wins for this turn and is persisted to `chats.llm_provider_id`
        /// so the chat remembers it (and a regenerate reuses it). Absent ⇒ the chat's
        /// stored provider, else the deployment default.
        #[serde(default)]
        llm_provider_id: Option<Uuid>,
    },
    #[serde(rename = "chat.cancel")]
    ChatCancel { turn_id: Uuid },
    /// Regenerate an answer in place (also drives edit + restart-from-here).
    /// `from_message_id` is the assistant answer to replace, OR a user message to
    /// restart from. The anchoring user message is REUSED (never re-inserted) and
    /// every message at/after the deletion point is dropped (branch replace).
    /// `content`, when set on a user anchor, edits that message before re-running.
    #[serde(rename = "chat.regenerate")]
    ChatRegenerate {
        chat_id: Uuid,
        from_message_id: Uuid,
        #[serde(default)]
        content: Option<String>,
    },
    /// In-band session refresh: the client pushes a (renewed) token to keep the
    /// socket's session + resume window alive past the original token's expiry.
    #[serde(rename = "auth")]
    Auth { token: String },
    /// Send a team-chat message over the multiplexed socket (same reliable path
    /// as `POST /api/group-chats/{id}/messages`).
    #[serde(rename = "group.send")]
    GroupSend {
        chat_id: Uuid,
        content: String,
        #[serde(default)]
        mentions: Option<serde_json::Value>,
    },
    /// Dictation: base64-encoded captured audio → server transcribes → replies
    /// with a `voice.transcript` frame (the SPA then sends it as `chat.send`).
    #[serde(rename = "voice.transcribe")]
    VoiceTranscribe {
        audio_base64: String,
        mime: String,
        #[serde(default)]
        chat_id: Option<Uuid>,
    },
    /// Read-aloud: text → server synthesises → replies with a `voice.audio` frame.
    #[serde(rename = "voice.speak")]
    VoiceSpeak {
        text: String,
        #[serde(default)]
        voice: Option<String>,
    },
    /// Live voice: open a streaming session on this
    /// socket. The orchestrator adopts `chat_id` (or the first turn creates one);
    /// the live turn persists like any chat. `mode` is `"ptt"` | `"vad"`; `aec`
    /// reports whether the browser has echo cancellation on (gates barge-in).
    #[serde(rename = "voice.stream.start")]
    VoiceStreamStart {
        #[serde(default)]
        chat_id: Option<Uuid>,
        #[serde(default)]
        project_id: Option<Uuid>,
        #[serde(default)]
        agent_id: Option<Uuid>,
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        aec: bool,
    },
    /// Live voice: one captured PCM16-mono frame (base64), with a monotonic seq.
    #[serde(rename = "voice.audio.chunk")]
    VoiceAudioChunk { audio_base64: String, seq: u64 },
    /// Live voice: the user spoke over the assistant — cancel the in-flight reply
    /// (LLM + TTS) and return to listening.
    #[serde(rename = "voice.barge_in")]
    VoiceBargeIn,
    /// Live voice: close the streaming session.
    #[serde(rename = "voice.stream.end")]
    VoiceStreamEnd,
    /// Streaming dictation (composer mic): open an STT-only session. PCM rides the
    /// same `voice.audio.chunk`; transcripts come back as `voice.partial` (interim)
    /// and `voice.transcript` (settled).
    #[serde(rename = "voice.dictate.start")]
    VoiceDictateStart,
    /// Streaming dictation: close the session.
    #[serde(rename = "voice.dictate.stop")]
    VoiceDictateStop,
    #[serde(rename = "ping")]
    Ping,
}

/// Frames the server sends. Serialised wrapped in [`Envelope`] to carry `version`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ServerFrame {
    #[serde(rename = "hello")]
    Hello {
        socket_id: Uuid,
        user_id: Uuid,
        resume_token: String,
    },
    #[serde(rename = "chat.created")]
    ChatCreated { chat_id: Uuid },
    /// The assistant message row for this turn exists (empty, streaming). Sent
    /// before the first token so the client adopts the real `message_id` — live
    /// tokens and the persisted row reconcile, and a reload/return mid-turn resumes
    /// the answer from the DB. Replayable (low-volume).
    #[serde(rename = "chat.started")]
    ChatStarted { turn_id: Uuid, chat_id: Uuid, message_id: Uuid },
    #[serde(rename = "chat.token")]
    ChatToken { turn_id: Uuid, delta: String },
    /// A reasoning-trace delta on the dedicated channel —
    /// the client routes it to the message's reasoning panel, separate from the
    /// answer. High-volume and NOT replayable; the trace is persisted folded into
    /// the message content (`<think>…</think>`), so a reload reconstructs it.
    #[serde(rename = "chat.reasoning")]
    ChatReasoning { turn_id: Uuid, delta: String },
    #[serde(rename = "chat.completed")]
    ChatCompleted {
        turn_id: Uuid,
        chat_id: Uuid,
        message_id: Uuid,
        /// Hidden reasoning tokens billed for this turn (normalised across
        /// providers), so the SPA can show the reasoning cost. Absent when zero.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_tokens: Option<i32>,
    },
    #[serde(rename = "chat.interrupted")]
    ChatInterrupted {
        turn_id: Uuid,
        message_id: Option<Uuid>,
    },
    #[serde(rename = "chat.citations")]
    ChatCitations {
        turn_id: Uuid,
        message_id: Uuid,
        citations: Vec<CitationOut>,
    },
    /// A message posted into a chat OUTSIDE the current turn — a background job's
    /// result (e.g. a `depth=deep` web search). The
    /// client refetches the chat's messages; citations ride inline because the
    /// messages API does not return them and a separate `chat.citations` frame
    /// would race the refetch.
    #[serde(rename = "chat.message_posted")]
    ChatMessagePosted {
        chat_id: Uuid,
        message_id: Uuid,
        citations: Vec<CitationOut>,
    },
    /// A background job (deep web search / Deep Research) has created an empty,
    /// streaming assistant message and is about to stream its answer into it.
    /// The client inserts a pending bubble keyed by `message_id` so the following
    /// `chat.message_token` frames type into it — the background-job analogue of
    /// `chat.started`. Replayable (low-volume); `chat.message_posted` finalises.
    #[serde(rename = "chat.message_started")]
    ChatMessageStarted {
        chat_id: Uuid,
        message_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// A token delta for a streaming background message (keyed by `message_id`,
    /// not the live turn). The client routes it to that bubble's typewriter. Like
    /// `chat.token`, high-volume and NOT replayable — the content is durable via
    /// the DB row, so a reload resumes from there.
    #[serde(rename = "chat.message_token")]
    ChatMessageToken {
        chat_id: Uuid,
        message_id: Uuid,
        delta: String,
    },
    #[serde(rename = "chat.error")]
    ChatError {
        turn_id: Option<Uuid>,
        message: String,
    },
    /// Coarse status of a background deep web search, broadcast to the user's
    /// sockets (the originating turn finished long ago). The UI shows a subtle
    /// status line, cleared when `chat.message_posted` delivers the result.
    #[serde(rename = "web_search.progress")]
    WebSearchProgress {
        chat_id: Uuid,
        turn_id: Uuid,
        detail: String,
    },
    /// Live status of a Deep Research run: phase +
    /// optional detail and counters; the client composes the status line
    /// ("Deep research — write · section 3/8 · 42 sources"). Cleared when
    /// `chat.message_posted` delivers the report.
    #[serde(rename = "research.progress")]
    ResearchProgress {
        chat_id: Uuid,
        run_id: Uuid,
        phase: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sources_read: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sections_done: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        sections_total: Option<i64>,
        /// Full ordered section roadmap, sent once when the outline is settled, so
        /// the client renders the roadmap and ticks sections off as they write.
        #[serde(skip_serializing_if = "Option::is_none")]
        sections: Option<Vec<String>>,
    },
    #[serde(rename = "chat.tool")]
    ChatTool {
        turn_id: Uuid,
        name: String,
        phase: String, // "started" | "progress" | "finished"
        /// Live progress detail (phase "progress" — e.g. "round 2: reading
        /// example.com" from a streaming web search).
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// An agent run paused on a gated (state-changing / egress) action and needs
    /// the human to approve or reject before it proceeds.
    /// Resolved via `POST /api/agent-runs/{run_id}/approve|reject`.
    #[serde(rename = "agent.approval")]
    AgentApproval {
        run_id: Uuid,
        turn_id: Uuid,
        tool: String,
        summary: String,
        args: serde_json::Value,
    },
    /// A live step checklist the model registered via the `track_steps` tool
    /// (#13). The full list is sent each update (replace, don't merge).
    #[serde(rename = "chat.steps")]
    ChatSteps { turn_id: Uuid, steps: Vec<StepOut> },
    /// Post-stream groundedness result for a RAG answer (Mode A). `score` is the
    /// grounded fraction
    /// ∈ [0,1] (`None` when the verifier was unreachable — UI shows nothing);
    /// `spans` are the unsupported char ranges in the answer. Pushed once, after
    /// `chat.completed`, from a spawned task — never blocks the turn.
    #[serde(rename = "chat.groundedness")]
    ChatGroundedness {
        turn_id: Uuid,
        message_id: Uuid,
        score: Option<f64>,
        total: i32,
        flagged: i32,
        spans: Vec<GroundSpanOut>,
    },
    /// Progress of a "Verify draft" (groundedness Mode B) background job, pushed to
    /// the requester. `status` ∈ queued | running | succeeded | error.
    #[serde(rename = "verification.status")]
    VerificationStatus {
        run_id: Uuid,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        progress: Option<String>,
    },
    /// A draft verification finished: the headline score + claim counts. The full
    /// per-claim report is fetched via `GET /api/verification-runs/{id}`.
    #[serde(rename = "verification.complete")]
    VerificationComplete {
        run_id: Uuid,
        score: Option<f64>,
        total: i32,
        supported: i32,
        contradicted: i32,
        not_mentioned: i32,
    },
    /// Ground-or-cut repair finished for a run: counts of proposed rewrites
    /// (`regenerated`), deletions (`cut`), and unchanged (`kept`). The viewer
    /// refetches its tracked-change proposals on this. `error` is set when repair
    /// could not run (e.g. a non-DOCX document) so the UI clears instead of hanging.
    #[serde(rename = "repair.complete")]
    RepairComplete {
        run_id: Uuid,
        document_id: Uuid,
        regenerated: i64,
        cut: i64,
        kept: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "doc.edited")]
    DocEdited {
        turn_id: Uuid,
        document_id: Uuid,
        version_id: Uuid,
        changes: Vec<EditChangeOut>,
    },
    #[serde(rename = "tabular.cell")]
    TabularCellUpdated {
        review_id: Uuid,
        document_id: Uuid,
        column_key: String,
        status: String, // "done" | "error"
    },
    #[serde(rename = "tabular.complete")]
    TabularReviewComplete { review_id: Uuid },
    #[serde(rename = "chat.compacted")]
    ChatCompacted { turn_id: Uuid, summarised: u32 },
    /// The chat is approaching the context budget (~70–80%); the UI can warn the
    /// user and offer to start a new chat. Compaction proceeds automatically.
    #[serde(rename = "context.warning")]
    ContextWarning { chat_id: Uuid, usage_pct: u32 },
    #[serde(rename = "code.result")]
    CodeResult {
        chat_id: Uuid,
        code: String,
        stdout: String,
        stderr: String,
        exit_code: i32,
    },
    #[serde(rename = "group.message")]
    GroupMessage {
        chat_id: Uuid,
        id: Uuid,
        seq: i32,
        sender_user_id: Option<Uuid>,
        message_type: String, // "user" | "system"
        content: String,
        created_at: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        attachments: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        shared_resources: Option<serde_json::Value>,
    },
    /// A reaction was toggled on a message (live update for chat members).
    #[serde(rename = "group.reaction")]
    GroupReaction {
        chat_id: Uuid,
        message_id: Uuid,
        emoji: String,
        user_id: Uuid,
        added: bool,
    },
    /// Lookahead reminder that one of the user's automations is about to run
    /// (Tier-2 #16). Pushed once per occurrence, shortly before it is due.
    #[serde(rename = "automation.reminder")]
    AutomationReminder {
        automation_id: Uuid,
        name: String,
        due_at: String,
        in_seconds: i64,
    },
    /// Live per-document ingestion progress, pushed to the uploader as the
    /// background pipeline advances (uploading → extracting → indexing → ready,
    /// or → error). Postgres stays the source of truth; a dropped frame is fine.
    #[serde(rename = "ingest.status")]
    IngestStatus {
        doc_id: Uuid,
        kb_id: Uuid,
        status: String, // "extracting" | "indexing" | "ready" | "error"
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "presence")]
    Presence { user_id: Uuid, status: String },
    /// Dictation result for a `voice.transcribe` request.
    #[serde(rename = "voice.transcript")]
    VoiceTranscript { text: String },
    /// Read-aloud audio (base64) for a `voice.speak` request — also the
    /// streaming-voice seam for the deferred live mode.
    #[serde(rename = "voice.audio")]
    VoiceAudio { audio_base64: String, mime: String },
    /// Live voice: the conversation state changed (idle/listening/capturing/
    /// thinking/speaking/interrupted/error) — drives the SPA's state visuals.
    #[serde(rename = "voice.state")]
    VoiceLiveState { state: String },
    /// Live voice: a stabilising partial transcript (shown muted). Emitted only when
    /// a streaming-STT engine is present.
    #[serde(rename = "voice.partial")]
    VoicePartial { text: String },
    /// Live voice: the settled final transcript for an utterance (this turn's input).
    #[serde(rename = "voice.final")]
    VoiceFinal { text: String },
    /// Live voice: one synthesised audio chunk of the reply, played in `seq` order.
    #[serde(rename = "voice.tts.chunk")]
    VoiceTtsChunk { audio_base64: String, mime: String, seq: u64 },
    /// Live voice: the reply's audio is fully synthesised (end of this turn's speech).
    #[serde(rename = "voice.tts.end")]
    VoiceTtsEnd,
    /// Live voice: a session-level error (the SPA drops to the text path).
    #[serde(rename = "voice.error")]
    VoiceError { message: String },
    /// Read-cache hint: tell the recipient's client to drop the given React-Query
    /// keys (after a group / membership / grant write) so open views refresh without
    /// a reload. Each inner vec is one query key, e.g. `["group", "<id>"]`.
    #[serde(rename = "invalidate")]
    Invalidate { keys: Vec<Vec<String>> },
    #[serde(rename = "pong")]
    Pong,
}

/// One step in a `track_steps` checklist (#13). Status ∈ pending | running |
/// done | skipped.
#[derive(Debug, Clone, Serialize)]
pub struct StepOut {
    pub title: String,
    pub status: String,
}

/// One unsupported span in a verified answer (groundedness Mode A). `start`/`end`
/// are char offsets into the answer text; `label` is `contradicted` | `not_mentioned`.
#[derive(Debug, Clone, Serialize)]
pub struct GroundSpanOut {
    pub start: i32,
    pub end: i32,
    pub text: String,
    pub label: String,
}

/// A citation surfaced to the client. Unified contract: RAG citations carry
/// `doc_id` (knowledge_docs) with `version_id = None`; legal-workspace citations
/// carry `document_id` + `version_id` (version-pinned); web citations carry
/// `url` (+ title/domain/dates) and no document anchor. The client branches on
/// which fields are present.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CitationOut {
    pub doc_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<Uuid>,
    pub quote_text: String,
    pub page_number: Option<i32>,
    pub clause_section_ref: Option<String>,
    /// Risk classification for the Legal workspace: `"amber"` (flagged clause) or
    /// `"ok"`. A keyword heuristic today (an ML signal can replace it later); the
    /// UI surfaces it only in Legal mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    // --- Web-citation fields (present only when `url` is set) ---------------
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet_only: Option<bool>,
}

/// One proposed tracked change, surfaced live so the UI can render accept/reject
/// cards (tracked-changes flow).
#[derive(Debug, Clone, Serialize)]
pub struct EditChangeOut {
    pub w_id: String,
    pub find: String,
    pub replace: String,
}

/// Outgoing wrapper adding the protocol `version` to every frame.
#[derive(Serialize)]
struct Envelope<'a> {
    version: u32,
    #[serde(flatten)]
    frame: &'a ServerFrame,
}

impl ServerFrame {
    pub fn to_json(&self) -> String {
        serde_json::to_string(&Envelope {
            version: PROTOCOL_VERSION,
            frame: self,
        })
        .expect("ServerFrame serialises")
    }
}
