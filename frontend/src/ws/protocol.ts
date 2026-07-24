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

// TS mirror of backend/src/ws/protocol.rs. Frames are JSON; the server wraps
// outgoing frames in an Envelope { version, ...frame }. We send the same shape.

export interface Citation {
  doc_id: string | null;
  document_id?: string | null;
  version_id?: string | null;
  quote_text: string;
  page_number: number | null;
  clause_section_ref?: string | null;
  /** Legal-workspace risk flag: "amber" (flagged) or "ok". Shown only in Legal mode. */
  risk?: "amber" | "ok" | null;
  // Web-citation fields (present only when `url` is set — no document anchor).
  url?: string | null;
  title?: string | null;
  domain?: string | null;
  published_date?: string | null;
  fetched_at?: string | null;
  /** Evidence came from the search-result snippet; the page itself was not fetched. */
  snippet_only?: boolean | null;
}

/** One unsupported span in a verified answer (groundedness Mode A); char offsets into the answer. */
export interface GroundSpan {
  start: number;
  end: number;
  text: string;
  /** "contradicted" (source disagrees) | "not_mentioned" (source silent). */
  label: string;
}

/** Per-turn capability-aware reasoning request. */
export interface ReasoningSpec {
  enabled: boolean;
  level?: string | null;
  return_trace: boolean;
}

export type ClientFrame =
  | { type: "chat.send"; chat_id?: string | null; content: string; agent_id?: string | null; project_id?: string | null; attachment_ids?: string[]; thinking?: string | null; reasoning?: ReasoningSpec | null; llm_provider_id?: string | null; workspace_id?: string | null }
  | { type: "chat.cancel"; turn_id: string }
  // Regenerate an answer in place (also drives edit + restart-from-here). The
  // anchoring user message is reused; every message at/after the deletion point
  // is dropped. `content` set on a user anchor edits it before re-running.
  | { type: "chat.regenerate"; chat_id: string; from_message_id: string; content?: string | null }
  | { type: "group.send"; chat_id: string; content: string; mentions?: unknown }
  | { type: "auth"; token: string }
  // Live / streaming voice. PCM16 LE 16 kHz mono, base64,
  // 20–40 ms per `voice.audio.chunk`. The turn fires server-side; no `chat.send`.
  | { type: "voice.stream.start"; chat_id?: string | null; project_id?: string | null; agent_id?: string | null; mode?: string; aec?: boolean }
  | { type: "voice.audio.chunk"; audio_base64: string; seq: number }
  | { type: "voice.barge_in" }
  | { type: "voice.stream.end" }
  // Streaming dictation (composer mic): STT-only. PCM rides voice.audio.chunk;
  // transcripts return as voice.partial (interim) + voice.transcript (settled).
  | { type: "voice.dictate.start" }
  | { type: "voice.dictate.stop" }
  // Optional opening frame: which client this is. Nothing is enforced on it; a
  // connection that omits it is treated as the web application.
  | { type: "client.hello"; client_kind: string; client_version: string; capabilities: string[] }
  | { type: "ping" }
  // Forward compatibility: a frame type this build does not know about is still
  // a valid thing to send. The server ignores unrecognised types rather than
  // failing the connection.
  | { type: string; [k: string]: unknown };

export type ServerFrame =
  | { type: "hello"; socket_id: string; user_id: string; resume_token: string; server_version?: string; features?: string[] }
  | { type: "chat.created"; chat_id: string }
  | { type: "chat.started"; turn_id: string; chat_id: string; message_id: string }
  | { type: "chat.token"; turn_id: string; delta: string }
  | { type: "chat.reasoning"; turn_id: string; delta: string }
  | { type: "chat.completed"; turn_id: string; chat_id: string; message_id: string; reasoning_tokens?: number }
  | { type: "chat.interrupted"; turn_id: string; message_id: string | null }
  | { type: "chat.citations"; turn_id: string; message_id: string; citations: Citation[] }
  | { type: "chat.message_posted"; chat_id: string; message_id: string; citations: Citation[] }
  | { type: "chat.message_started"; chat_id: string; message_id: string; agent?: string }
  | { type: "chat.message_token"; chat_id: string; message_id: string; delta: string }
  | { type: "chat.error"; turn_id: string | null; message: string; chat_id?: string }
  | { type: "chat.tool"; turn_id: string; name: string; phase: string; detail?: string }
  | { type: "web_search.progress"; chat_id: string; turn_id: string; detail: string }
  | { type: "research.progress"; chat_id: string; run_id: string; phase: string; detail?: string; sources_read?: number; sections_done?: number; sections_total?: number; sections?: string[] }
  | { type: "chat.steps"; turn_id: string; steps: { title: string; status: string }[] }
  | { type: "chat.groundedness"; turn_id: string; message_id: string; score: number | null; total: number; flagged: number; spans: GroundSpan[] }
  | { type: "verification.status"; run_id: string; status: string; progress?: string }
  | { type: "verification.complete"; run_id: string; score: number | null; total: number; supported: number; contradicted: number; not_mentioned: number }
  | { type: "repair.complete"; run_id: string; document_id: string; regenerated: number; cut: number; kept: number; error?: string }
  | { type: "agent.approval"; run_id: string; turn_id: string; tool: string; summary: string; args: Record<string, unknown>; detail?: Record<string, unknown> | null }
  | { type: "agent.approval.resolved"; run_id: string; approved: boolean }
  | { type: "chat.compacted"; turn_id: string; summarised: number }
  | { type: "context.warning"; chat_id: string; usage_pct: number }
  | { type: "ingest.status"; doc_id: string; kb_id: string; status: string; error?: string }
  | { type: "automation.reminder"; automation_id: string; name: string; due_at: string; in_seconds: number }
  | { type: "presence"; user_id: string; status: string }
  | { type: "invalidate"; keys: string[][] }
  | { type: "pong" }
  // Live / streaming voice. The transcript + answer ride the relayed chat.* frames;
  // these carry the live state, the partial/final transcript, and the spoken reply.
  | { type: "voice.state"; state: string; retrieving?: boolean }
  | { type: "voice.partial"; text: string }
  | { type: "voice.final"; text: string }
  | { type: "voice.transcript"; text: string }
  | { type: "voice.tts.chunk"; audio_base64: string; mime: string; seq: number }
  | { type: "voice.tts.end" }
  | { type: "voice.error"; message: string }
  // frames the spine doesn't act on yet (docs/tabular/messaging)
  | { type: string; [k: string]: unknown };

export type WsStatus = "idle" | "connecting" | "open" | "closed";
