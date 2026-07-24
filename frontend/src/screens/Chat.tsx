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

import { confirmDialog, toast } from "@/components/dialogs";
import { Dropzone } from "@/components/Dropzone";
import { Composer, type ComposerHandle } from "@/components/Composer";
import { forwardRef, Fragment, useEffect, useMemo, useRef, useState } from "react";
import { Virtuoso } from "react-virtuoso";
import { useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { approveAgentRun, attachChatLibrary, cancelAgentRun, chatAttachmentUrl, createAgent, deleteChat, downloadChatAttachment, exportChat, rejectAgentRun, renameChat, revokeShare, shareChat, useAgents, useChatArtefacts, useChatLinks, useChatMessages, useChats, useGroupChats, useLibraries, useMyShares, useProjects, useResearchChats, useWhoami, type Artefact, type ChatAttachmentMeta, type ChatShare, type MsgActivity, type MsgGroundedness } from "@/api/client";
import { ArtefactChip } from "@/components/artefacts/ArtefactChip";
import { ArtefactPanelHost } from "@/components/artefacts/ArtefactPanel";
import { useArtefactActions } from "@/components/artefacts/useArtefactActions";
import { useArtefactPanel } from "@/components/artefacts/useArtefactPanel";
import { AgentActivity } from "@/components/agentActivity";
import { PlanPanel } from "@/components/PlanPanel";
import { TurnSummary } from "@/components/TurnSummary";
import { applyResolved, type PendingApproval } from "@/screens/approvalState";
import { FolderMenu } from "@/components/FolderMenu";
import { rememberPrefix } from "@/components/FolderApproval";
import { cancelLocalCall } from "@/shell/folders";
import { useChatWorkspace } from "@/api/client";
import { Groundedness } from "@/components/groundedness";
import { useFeedback } from "@/components/feedback";
import { useActiveProject } from "@/app/ProjectContext";
import { Icon } from "@/components/icons";
import { useReadAloud } from "@/voice/useVoice";
import { useLiveVoice } from "@/voice/useLiveVoice";
import { NeuralBackground } from "@/components/NeuralBackground";
import { MessageMarkdown } from "@/components/MessageMarkdown";
import { wsStore } from "@/ws/store";
import type { Citation, ReasoningSpec, ServerFrame } from "@/ws/protocol";
import { CitationPanel } from "@/components/CitationPanel";
import { getMessageActions, getMessageOverlay, type MsgActionCtx } from "@/ext/registry";
import { CiteChip } from "@/components/CiteChip";
import { CodeBlock } from "@/components/code";
import { Reasoning, Thinking, splitThink } from "@/components/reasoning";
import { ResearchRoadmapPanel, ResearchSteps, currentLabel, stagesFor, stepLabel } from "@/components/ResearchRoadmap";
import { AgentPicker, useAgentSelection, agentsForMode } from "@/components/AgentPicker";
import { useWorkmode } from "@/app/WorkmodeContext";

// Virtuoso list wrapper: the column geometry lives in
// design.css `.thread-virtual` (single source of truth alongside .thread-inner);
// Virtuoso's injected `style` owns the vertical offsets.
const ThreadList = forwardRef<HTMLDivElement, { style?: React.CSSProperties; children?: React.ReactNode }>(
  function ThreadList({ style, children }, ref) {
    return (
      <div ref={ref} style={style} className="thread-virtual">
        {children}
      </div>
    );
  },
);

interface Msg {
  id: string;
  role: "user" | "assistant";
  content: string;
  pending?: boolean;
  error?: boolean;
  citations?: Citation[];
  agent?: string;
  time?: string;
  /** ISO timestamp the message was sent/created — drives the "time ago" hover tip.
   * Real value from the DB (`MessageOut.created_at`) on reload; set locally on send. */
  createdAt?: string;
  startedAt?: number;
  /** Agent activity (track_steps plan + tools used), shown inline + persisted. */
  activity?: MsgActivity;
  /** Live groundedness verdict (score + flagged spans) of a RAG answer; persisted. */
  groundedness?: MsgGroundedness | null;
  /** Human sign-off on this turn (approved | changes_requested | rejected); drives the badge. */
  reviewDecision?: string | null;
  /** Reasoning trace streamed on the dedicated `chat.reasoning` channel (live).
   * On reload it's reconstructed from the persisted `<think>` block via splitThink. */
  reasoning?: string;
  /** Hidden reasoning tokens billed this turn (from chat.completed). */
  reasoningTokens?: number;
  /** Files the user attached to this message — rendered under the bubble + in the rail. */
  attachments?: ChatAttachmentMeta[];
  /** The turn that produced this message, so a folder change made during it can
   * be listed and undone from its inline summary (desktop only). */
  turnId?: string;
}

// An image attachment under a user message. The bytes route is credential-gated,
// so the element is handed an object URL fetched with the caller's credentials
// rather than a link to the endpoint; the URL is revoked when the message
// scrolls out of the list. Clicking saves the file.
function AttachmentImage({ att }: { att: ChatAttachmentMeta }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let held: string | null = null;
    let cancelled = false;
    chatAttachmentUrl(att.id)
      .then((u) => {
        if (cancelled) URL.revokeObjectURL(u);
        else {
          held = u;
          setUrl(u);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      if (held) URL.revokeObjectURL(held);
    };
  }, [att.id]);
  if (!url) return <div className="ed-hint mono">Loading image…</div>;
  return (
    <button
      className="msg-attach-img"
      title={att.filename}
      onClick={() => downloadChatAttachment(att.id, att.filename).catch((e) => toast((e as Error).message))}
    >
      <img src={url} alt={att.filename} loading="lazy" />
    </button>
  );
}

export function Chat() {
  const { chatId } = useParams();
  const nav = useNavigate();
  const qc = useQueryClient();
  const agents = useAgents();
  const chats = useChats();
  // Research chats are a separate server list (GET /api/chats excludes mode=research);
  // the header/title + DR source/KB must resolve from it too, else a DR chat is never
  // found → header shows "New chat" and the generated title never lands.
  const researchChats = useResearchChats(true);
  const history = useChatMessages(chatId);
  const artefacts = useChatArtefacts(chatId);
  const { active, setActive } = useActiveProject();
  const projects = useProjects();
  const libs = useLibraries();
  const chatLibs = useChatLinks(chatId);
  const agentName = (id: string | null) => agents.data?.find((a) => a.id === id)?.name ?? "Assistant";

  // Only agents tagged for the active workmode (general/research) appear here.
  const { mode } = useWorkmode();
  const visibleAgents = useMemo(() => agentsForMode(agents.data ?? [], mode), [agents.data, mode]);
  const { agentId, setAgentId, defaultAgentId, pinDefaultAgent } = useAgentSelection(visibleAgents);
  const [messages, setMessages] = useState<Msg[]>([]);
  const [sending, setSending] = useState(false);
  // Download/convert/verify an artefact — shared with the artefact panel, which
  // is where all of them but the download are offered.
  const artefactActions = useArtefactActions(chatId);
  const [notice, setNotice] = useState<string | null>(null);
  // The tool currently executing this turn — a live indicator inside the active
  // message's activity block (steps/tools persist on the message itself).
  const [runningTool, setRunningTool] = useState<string | null>(null);
  // Live progress detail for a streaming tool ("round 2: reading example.com").
  const [runningToolDetail, setRunningToolDetail] = useState<string | null>(null);
  // Coarse status of a background deep web search in this chat.
  const [deepStatus, setDeepStatus] = useState<string | null>(null);
  const [deepRunId, setDeepRunId] = useState<string | null>(null);
  // Live Deep Research roadmap (right-docked panel): the report's full section
  // list (known after the outline event) + how many are written. Cleared when the
  // report posts (chat.message_posted).
  const [deepRoadmap, setDeepRoadmap] = useState<{ sections: string[]; done: number; phase: string; detail?: string; sourcesRead?: number } | null>(null);
  // An agent run paused on a gated action, awaiting the user's approve/reject.
  // `detail` (present for a folder action) carries what a client can render as a
  // change to agree to rather than a sentence to wave through.
  // `state` tracks whether this gate is still open ('pending'), was approved, or
  // was closed some other way (rejected here, or settled on another device). A
  // resolved card stays visible with its buttons spent, so a decision taken
  // elsewhere is not a card that silently vanishes.
  const [approval, setApproval] = useState<PendingApproval | null>(null);
  // Live output of a command running in a connected folder, keyed by the turn,
  // so it can be shown as it arrives and the run stopped mid-way.
  const [terminalOut, setTerminalOut] = useState<string>("");
  // The folder this chat is bound to, for the restore block's "show in folder".
  const boundWorkspace = useChatWorkspace(chatId ?? undefined);
  // A folder chosen in the composer on a brand-new chat, before it has an id to
  // bind to. It rides the first `chat.send`; from then the binding lives server-side.
  const [pendingWorkspace, setPendingWorkspace] = useState<string | null>(null);

  // "Actions only": hide the assistant's prose and leave the plan, tool cards,
  // approvals, summaries and artefacts — for reviewing what a long agentic turn
  // actually did without the wall of text. Per-chat and in memory only (kept out
  // of storage on purpose: it is a viewing mode, not a preference).
  const [actionsOnly, setActionsOnly] = useState(false);
  useEffect(() => { setActionsOnly(false); }, [chatId]);

  const [showNewAgent, setShowNewAgent] = useState(false);
  const [citation, setCitation] = useState<Citation | null>(null);
  // Generic per-message overlay slot (extension registry) — Review & Approve's
  // drawer is one such overlay, opened by its registered message action.
  const [overlay, setOverlay] = useState<{ key: string; props: Record<string, unknown> } | null>(null);
  const openOverlay = (key: string, props: Record<string, unknown>) => setOverlay({ key, props });
  const [codeRuns, setCodeRuns] = useState<{ code: string; stdout: string; stderr: string; exit_code: number }[]>([]);
  const fb = useFeedback();
  const [copiedId, setCopiedId] = useState<string | null>(null);
  // User-message inline edit: the message being edited + its working draft.
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [attaching, setAttaching] = useState(false);
  const [sharing, setSharing] = useState(false);
  const composerRef = useRef<ComposerHandle>(null);

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
    return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  }

  // "time ago" for the user-message hover tip: coarse buckets, then an absolute date.
  function relTime(iso?: string): { rel: string; abs: string } | null {
    if (!iso) return null;
    const t = Date.parse(iso);
    if (Number.isNaN(t)) return null;
    const s = Math.max(0, Math.round((Date.now() - t) / 1000));
    let rel: string;
    if (s < 45) rel = "just now";
    else if (s < 3600) rel = `${Math.round(s / 60)} min ago`;
    else if (s < 86400) rel = `${Math.round(s / 3600)} h ago`;
    else if (s < 604800) rel = `${Math.round(s / 86400)} d ago`;
    else rel = new Date(t).toLocaleDateString("en-GB", { day: "2-digit", month: "short", year: "numeric" });
    const abs = new Date(t).toLocaleString("en-GB", { day: "2-digit", month: "short", hour: "2-digit", minute: "2-digit" });
    return { rel, abs };
  }

  function copyMsg(id: string, text: string) {
    navigator.clipboard?.writeText(text);
    setCopiedId(id);
    setTimeout(() => setCopiedId((c) => (c === id ? null : c)), 1200);
  }

  async function renameTo(title: string) {
    if (!chatId) return;
    try {
      await renameChat(chatId, title);
      await qc.invalidateQueries({ queryKey: ["chats"] });
    } catch (e) {
      toast(`Rename failed: ${(e as Error).message}`);
    }
  }
  async function archiveCurrent() {
    if (!chatId) return;
    if (!(await confirmDialog({ title: "Archive this chat?", body: "History is retained; the chat is hidden from the list.", danger: true, confirmLabel: "Archive" }))) return;
    try {
      await deleteChat(chatId);
      await qc.invalidateQueries({ queryKey: ["chats"] });
      nav("/");
    } catch (e) {
      toast(`Archive failed: ${(e as Error).message}`);
    }
  }

  const who = useWhoami();
  const voiceOn = !!who.data?.capabilities.voice;
  const codeOn = !!who.data?.capabilities.code_interpreter;
  const groundednessOn = !!who.data?.capabilities.groundedness;
  const readAloud = useReadAloud();
  const chatRef = useRef<string | null>(chatId ?? null);
  const createdLocally = useRef<string | null>(null);

  // Reasoning spec for the sends that bypass the composer (EmptyChat suggestions,
  // Regenerate). The composer owns the live control; here we read the same persisted
  // level so these sends still honour the chosen effort.
  function currentReasoning(): ReasoningSpec | null {
    const rc = who.data?.capabilities.reasoning;
    const mode = rc?.mode ?? "toggle";
    if (mode === "none") return null;
    const lvl = localStorage.getItem(`pai.thinking:${chatRef.current ?? "draft"}`) ??
      localStorage.getItem("pai.thinking:draft") ?? "off";
    const canDisable = rc?.can_disable ?? true;
    if (lvl === "off" && canDisable) return { enabled: false, level: null, return_trace: true };
    const level = lvl === "on" || lvl === "off" ? "auto" : lvl;
    return { enabled: true, level, return_trace: true };
  }
  const turnId = useRef<string | null>(null);
  const pendingId = useRef<string | null>(null);
  // Citations for messages posted OUTSIDE a turn (e.g. a deep web search result).
  // The messages API returns no citations, so the frame carries them inline; we
  // stash them here and the history-seed effect attaches them by id.
  const postedCitations = useRef<Map<string, Citation[]>>(new Map());
  const bottom = useRef<HTMLDivElement | null>(null);
  // The scroll container for the virtualised message list (L5c). A callback ref
  // into state so Virtuoso receives the element once it mounts.
  const [threadEl, setThreadEl] = useState<HTMLDivElement | null>(null);

  // Token-stream typewriter: `tokenBuf`
  // holds received-but-not-yet-shown chars. Instead of dumping the whole buffer per
  // frame — which lurches when a provider streams big chunks with network gaps
  // (Anthropic compat) rather than token-by-token (Ollama) — drain a CAPPED number of
  // chars per animation frame so display rate is decoupled from arrival bursts. The
  // cap is adaptive (grows with backlog) so latency stays bounded and it never lags.
  // `flushTokens` dumps the whole remainder at turn boundaries so no text is delayed.
  const tokenBuf = useRef("");
  const rafId = useRef<number | null>(null);
  const TYPE_MIN_CHARS = 2; // chars/frame when nearly caught up
  const TYPE_DIV = 6; // backlog divisor — accelerates when a big chunk lands
  // Move up to `take` chars from the queue into the pending message. Returns whether
  // the queue still has more (so the loop can reschedule).
  const drainSome = useRef((take: number) => {
    const buf = tokenBuf.current;
    if (!buf) return false;
    const n = Math.min(buf.length, take);
    const slice = buf.slice(0, n);
    tokenBuf.current = buf.slice(n);
    // Capture the target id NOW: the updater runs after React defers
    // it, by which time a boundary handler (chat.started/completed) may have
    // reassigned pendingId.current to an id no message carries yet — reading it
    // lazily would silently drop the buffered text.
    const target = pendingId.current;
    setMessages((p) => p.map((m) => (m.id === target ? { ...m, content: m.content + slice } : m)));
    return tokenBuf.current.length > 0;
  });
  // Continuous per-frame drain loop; self-reschedules while the queue is non-empty.
  const tick = useRef(() => {
    const take = Math.max(TYPE_MIN_CHARS, Math.ceil(tokenBuf.current.length / TYPE_DIV));
    const more = drainSome.current(take);
    rafId.current = more ? requestAnimationFrame(tick.current) : null;
  });
  const ensureDraining = useRef(() => {
    if (rafId.current == null) rafId.current = requestAnimationFrame(tick.current);
  });
  const flushTokens = useRef(() => {
    if (rafId.current != null) {
      cancelAnimationFrame(rafId.current);
      rafId.current = null;
    }
    // Dump the entire remaining queue at once (turn boundary) — final text must not
    // trail behind the typewriter.
    drainSome.current(tokenBuf.current.length);
  });

  // Background-message typewriter: a deep web search / Deep Research result streams
  // into the chat OUTSIDE any live turn (via chat.message_started/_token/_posted).
  // It has its OWN buffer + target so it never mixes with a concurrent live turn's
  // tokens; same adaptive per-frame drain as the chat typewriter above.
  const bgBuf = useRef("");
  const bgRafId = useRef<number | null>(null);
  const bgTarget = useRef<string | null>(null);
  const bgDrain = useRef((take: number) => {
    const buf = bgBuf.current;
    if (!buf) return false;
    const n = Math.min(buf.length, take);
    const slice = buf.slice(0, n);
    bgBuf.current = buf.slice(n);
    const target = bgTarget.current;
    setMessages((p) => p.map((m) => (m.id === target ? { ...m, content: m.content + slice } : m)));
    return bgBuf.current.length > 0;
  });
  const bgTick = useRef(() => {
    const take = Math.max(TYPE_MIN_CHARS, Math.ceil(bgBuf.current.length / TYPE_DIV));
    const more = bgDrain.current(take);
    bgRafId.current = more ? requestAnimationFrame(bgTick.current) : null;
  });
  const bgEnsure = useRef(() => {
    if (bgRafId.current == null) bgRafId.current = requestAnimationFrame(bgTick.current);
  });
  const bgFlush = useRef(() => {
    if (bgRafId.current != null) {
      cancelAnimationFrame(bgRafId.current);
      bgRafId.current = null;
    }
    bgDrain.current(bgBuf.current.length);
  });

  // Live / streaming voice: a call-mode session whose turns flow through the SAME
  // chat-turn as a typed send. On `voice.final` we set up the optimistic bubbles
  // exactly like `sendText` — minus the `chat.send`, since the server already
  // started the turn — so the relayed `chat.*` frames render the answer + citations
  // + groundedness, and the conversation persists in the normal message list.
  function onVoiceUserFinal(text: string) {
    const content = text.trim();
    if (!content) return;
    const nowIso = new Date().toISOString();
    const userMsg: Msg = { id: crypto.randomUUID(), role: "user", content, createdAt: nowIso };
    const pid = crypto.randomUUID();
    pendingId.current = pid;
    const now = new Date().toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
    setMessages((p) => [
      ...p.map((m) => (m.role === "assistant" && m.pending ? { ...m, pending: false } : m)),
      userMsg,
      { id: pid, role: "assistant", content: "", pending: true, agent: agentName(agentId), time: now, createdAt: nowIso, startedAt: Date.now() },
    ]);
    setSending(true);
    setRunningTool(null);
    setRunningToolDetail(null);
    scrollToBottom();
  }
  const live = useLiveVoice({
    chatId: chatRef.current,
    agentId,
    projectId: active?.id ?? null,
    onUserFinal: onVoiceUserFinal,
    pttDefault: who.data?.voice_live_opts?.ptt_default,
    silenceMs: who.data?.voice_live_opts?.silence_threshold_ms,
  });

  // Route change: switching chats / starting a new one.
  useEffect(() => {
    chatRef.current = chatId ?? null;
    setCodeRuns([]);
    if (!chatId || chatId !== createdLocally.current) {
      setMessages([]); // existing chat → seeded by history effect; new chat → empty
      // Once we leave the just-created chat it's a normal existing chat: clear the
      // flag so a later REVISIT resets + reseeds from history instead of showing the
      // stale in-memory messages of whatever chat we came from (the cross-chat bleed).
      createdLocally.current = null;
    }
  }, [chatId]);

  // Seed history for an existing chat (not the one we just created in this view).
  // Non-destructive: carry over live-only fields (citations, agent, time) from
  // the current state by id, so reconciling to canonical message ids after a
  // turn (the artefact-inline fix) doesn't drop a RAG turn's citations — the
  // messages API returns only id/role/content.
  useEffect(() => {
    if (chatId && history.data && chatId !== createdLocally.current) {
      setMessages((prev) => {
        const byId = new Map(prev.map((m) => [m.id, m]));
        const mapped = history.data!.map((m) => {
          const live = byId.get(m.id);
          // A still-writing assistant turn (streaming) renders as pending and is
          // polled until it settles — this is what resumes an answer after a reload.
          // Keep the longer of live vs DB content ONLY while it is still streaming, so
          // the live stream stays smooth; once settled, the DB row is authoritative
          // (a finalised background message reconciles its streamed draft to the
          // canonical report_md, which can be shorter than the draft).
          const content =
            live && (live.pending || m.streaming) && live.content.length > m.content.length
              ? live.content
              : m.content;
          return {
            id: m.id,
            role: m.role as Msg["role"],
            content,
            pending: m.streaming || live?.pending,
            startedAt: live?.startedAt,
            // Prefer the DB-loaded citations (survive a reload); fall back to live
            // frame state or a background-posted stash while the chat is open.
            citations: m.citations ?? live?.citations ?? postedCitations.current.get(m.id),
            agent: live?.agent,
            time: live?.time,
            createdAt: m.created_at ?? live?.createdAt,
            activity: m.activity ?? live?.activity,
            groundedness: m.groundedness ?? live?.groundedness,
            reviewDecision: m.review_decision,
            attachments: m.attachments ?? live?.attachments,
          };
        });
        // Preserve a live, still-streaming assistant bubble that hasn't landed in
        // history yet (race between its early INSERT and a poll refetch).
        const pid = pendingId.current;
        const liveP = pid ? byId.get(pid) : undefined;
        if (liveP && liveP.role === "assistant" && !history.data!.some((m) => m.id === pid)) {
          mapped.push(liveP as (typeof mapped)[number]);
        }
        return mapped;
      });
    }
  }, [chatId, history.data]);

  // A turn's server-derived activity (the files it changed and commands it ran,
  // for the end-of-turn summary) lives only on the persisted message — no live
  // frame carries it. Merge it onto the matching message once history has it, so
  // the summary appears the moment the turn settles rather than only after a
  // reload. This is needed because the full reseed above is skipped for a chat we
  // created in this view; here we touch only settled (non-pending) messages and
  // only the activity, so a still-streaming turn is never disturbed.
  useEffect(() => {
    if (!history.data) return;
    const byId = new Map(history.data.map((m) => [m.id, m.activity] as const));
    setMessages((prev) => {
      let changed = false;
      const next = prev.map((m) => {
        if (m.pending) return m;
        const dbActivity = byId.get(m.id);
        if (!dbActivity || dbActivity === m.activity) return m;
        // Only fill in the server-only side (files/commands); keep whatever the
        // live frames already put on steps/tools if the DB somehow lacks them.
        changed = true;
        return { ...m, activity: { ...m.activity, ...dbActivity } };
      });
      return changed ? next : prev;
    });
  }, [history.data]);

  // Streaming follow is owned solely by Virtuoso `followOutput`:
  // a scroll-on-every-[messages]-change effect here would re-pin to the bottom
  // each frame and stop the user scrolling up mid-stream. Sending jumps to the
  // bottom explicitly via `scrollToBottom`.
  function scrollToBottom() {
    requestAnimationFrame(() => bottom.current?.scrollIntoView());
  }

  useEffect(() => {
    return wsStore.onFrame((f: ServerFrame) => {
      switch (f.type) {
        case "chat.created": {
          const id = (f as { chat_id: string }).chat_id;
          createdLocally.current = id;
          chatRef.current = id;
          nav(`/c/${id}`, { replace: true });
          qc.invalidateQueries({ queryKey: ["chats"] });
          break;
        }
        case "chat.started": {
          // The assistant row exists (real message_id). Adopt it immediately so live
          // tokens and the persisted/resumed row reconcile on one id.
          const s = f as { turn_id: string; message_id: string };
          turnId.current = s.turn_id;
          flushTokens.current(); // drain any buffered tokens onto the old id first
          const prev = pendingId.current;
          pendingId.current = s.message_id;
          setMessages((p) => p.map((m) => (m.id === prev ? { ...m, id: s.message_id, turnId: s.turn_id } : m)));
          break;
        }
        case "chat.token": {
          const t = f as { turn_id: string; delta: string };
          turnId.current = t.turn_id;
          // Queue the delta; the typewriter loop drains it at a steady rate (L5a +
          // smoothing). Decouples display from bursty provider chunks.
          tokenBuf.current += t.delta;
          ensureDraining.current();
          break;
        }
        case "chat.reasoning": {
          // Reasoning trace on the dedicated channel — appended straight onto the
          // message's `reasoning` (the panel renders/auto-scrolls it live), kept out
          // of the answer text. Persisted server-side folded into `<think>`.
          const t = f as { turn_id: string; delta: string };
          turnId.current = t.turn_id;
          const target = pendingId.current;
          setMessages((p) => p.map((m) => (m.id === target ? { ...m, reasoning: (m.reasoning ?? "") + t.delta } : m)));
          break;
        }
        case "chat.tool": {
          const t = f as { name: string; phase: string; detail?: string };
          if (t.name === "track_steps") break; // the plan, not a "tool used"
          if (t.name === "reasoning") {
            // a moving "Thinking · step k of N" during the tool
            // loop — detail only, never a running tool nor a recorded tool.
            setRunningToolDetail(t.detail ?? null);
            break;
          }
          if (t.phase === "summary") {
            // the retrieval Coverage line — persist it on the message
            // as a completed activity step (NOT the transient running label), so it
            // survives the turn finishing and a reload.
            setMessages((p) => p.map((m) =>
              m.id !== pendingId.current ? m : { ...m, activity: { ...m.activity, coverage: t.detail ?? null } }
            ));
            break;
          }
          if (t.phase === "started") {
            setRunningTool(t.name);
            setRunningToolDetail(null);
          } else if (t.phase === "progress") {
            // Live detail from a streaming tool (e.g. "round 2: reading example.com").
            setRunningToolDetail(t.detail ?? null);
            // A command running in a folder streams its output here; keep it so
            // the panel shows it building up rather than only the last line.
            if (t.name === "desktop.terminal_run" && t.detail) {
              setTerminalOut((prev) => (prev + t.detail).slice(-8000));
            }
          } else {
            setRunningTool(null);
            setRunningToolDetail(null);
            // Record the finished tool on the active message's activity (deduped).
            setMessages((p) => p.map((m) => {
              if (m.id !== pendingId.current) return m;
              const tools = m.activity?.tools ?? [];
              return tools.includes(t.name) ? m : { ...m, activity: { ...m.activity, tools: [...tools, t.name] } };
            }));
          }
          break;
        }
        case "web_search.progress": {
          // Coarse status of a background deep web search; cleared when the
          // result posts back (chat.message_posted).
          const w = f as { chat_id: string; detail: string };
          if (w.chat_id === chatRef.current) setDeepStatus(`Deep web search — ${w.detail}`);
          break;
        }
        case "research.progress": {
          // Deep Research run status — the client composes the line:
          // "write · section 3/8 · 42 sources". Cleared on chat.message_posted.
          const r = f as { chat_id: string; run_id?: string; phase: string; detail?: string; sources_read?: number; sections_done?: number; sections_total?: number; sections?: string[] };
          if (r.chat_id !== chatRef.current) break;
          const bits = [r.phase];
          if (r.detail) bits.push(r.detail);
          if (r.sections_done != null && r.sections_total != null) bits.push(`section ${r.sections_done + 1}/${r.sections_total}`);
          if (r.sources_read != null) bits.push(`${r.sources_read} sources`);
          setDeepStatus(`Deep research — ${bits.join(" · ")}`);
          if (r.run_id) setDeepRunId(r.run_id); // enables the Stop button
          // Roadmap: track the current macro-phase from the FIRST event so the panel
          // shows from second one; accumulate the section list when the outline event
          // carries it; tick `done` from each write event's sections_done.
          setDeepRoadmap((prev) => ({
            phase: r.phase,
            sections: r.sections ?? prev?.sections ?? [],
            done: r.sections_done ?? prev?.done ?? 0,
            detail: r.detail,
            sourcesRead: r.sources_read,
          }));
          break;
        }
        case "chat.steps": {
          // Live checklist from the track_steps tool (#13) — full list each time;
          // stored on the active message so it persists + renders inline.
          const s = f as { steps: { title: string; status: string }[] };
          setMessages((p) => p.map((m) => (m.id === pendingId.current ? { ...m, activity: { ...m.activity, steps: s.steps } } : m)));
          break;
        }
        case "agent.approval": {
          const a = f as { run_id: string; tool: string; summary: string; detail?: Record<string, unknown> | null };
          setApproval({ runId: a.run_id, tool: a.tool, summary: a.summary, detail: a.detail ?? null, state: "pending" });
          if (a.tool === "desktop.terminal_run") setTerminalOut("");
          break;
        }
        case "agent.approval.resolved": {
          // The gate was decided — here, or on another device the user has open.
          // Settle the pending card in place (approved, or closed) rather than
          // leaving it asking a question that already has an answer.
          const r = f as { run_id: string; approved: boolean };
          setApproval((prev) => applyResolved(prev, r.run_id, r.approved));
          break;
        }
        case "chat.completed": {
          const c = f as { message_id: string; chat_id?: string; reasoning_tokens?: number };
          flushTokens.current(); // apply the final buffered tokens before the id-swap
          // Capture the pending id BEFORE reassigning the ref: the setMessages
          // updater runs later (React defers it), so reading pendingId.current
          // inside it would see the new value and the id-swap would never match.
          const prevPid = pendingId.current;
          pendingId.current = c.message_id;
          setMessages((p) => p.map((m) => {
            if (m.id === prevPid) return { ...m, id: c.message_id, pending: false, reasoningTokens: c.reasoning_tokens };
            return m.role === "assistant" && m.pending ? { ...m, pending: false } : m; // clear any straggler
          }));
          turnId.current = null;
          setSending(false);
          setRunningTool(null);
          setRunningToolDetail(null);
          setApproval(null);
          qc.invalidateQueries({ queryKey: ["chats"] });
          // Refetch artefacts AND messages for THIS chat (frame id is robust for
          // a just-created chat). Reconciling the message history to canonical DB
          // ids is what lets an artefact match its message inline: the rail lists
          // all artefacts, but the inline chip is keyed by message_id. The short
          // follow-up catches a server-side auto-artefact written right before
          // this frame — so the inline chip appears without a page reload.
          const cid = c.chat_id ?? chatRef.current ?? undefined;
          if (cid) {
            qc.invalidateQueries({ queryKey: ["chat-messages", cid] });
            qc.invalidateQueries({ queryKey: ["artefacts", cid] });
            window.setTimeout(() => qc.invalidateQueries({ queryKey: ["artefacts", cid] }), 1200);
            // Show whatever this turn produced. Arming rather than opening here is
            // deliberate: the refetch above has only been queued, so the artefact
            // does not exist client-side yet.
            panel.armAutoOpen(cid);
          }
          break;
        }
        case "chat.citations": {
          const c = f as { message_id: string; citations: Citation[] };
          setMessages((p) => p.map((m) => (m.id === c.message_id ? { ...m, citations: c.citations } : m)));
          break;
        }
        case "chat.message_started": {
          // A background job (deep web search / Deep Research) opened a streaming
          // assistant message — insert a pending bubble keyed by message_id so the
          // following chat.message_token frames type into it.
          const s = f as { chat_id: string; message_id: string; agent?: string };
          if (s.chat_id !== chatRef.current) break;
          bgTarget.current = s.message_id;
          bgBuf.current = "";
          const now = new Date().toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
          setMessages((p) =>
            p.some((m) => m.id === s.message_id)
              ? p
              : [...p, { id: s.message_id, role: "assistant", content: "", pending: true, agent: s.agent, time: now, startedAt: Date.now() }],
          );
          scrollToBottom();
          break;
        }
        case "chat.message_token": {
          // Token for a streaming background message — route to its own typewriter.
          const t = f as { chat_id: string; message_id: string; delta: string };
          if (t.message_id !== bgTarget.current) break;
          bgBuf.current += t.delta;
          bgEnsure.current();
          break;
        }
        case "chat.message_posted": {
          // A background job finalised its message. Drain the typewriter, settle the
          // bubble, stash + attach citations, then refetch so the authoritative
          // content (the reconciled report_md) replaces the streamed draft.
          const p = f as { chat_id: string; message_id: string; citations: Citation[] };
          if (p.citations?.length) postedCitations.current.set(p.message_id, p.citations);
          if (p.message_id === bgTarget.current) {
            bgFlush.current();
            bgTarget.current = null;
          }
          setMessages((prev) =>
            prev.map((m) =>
              m.id === p.message_id ? { ...m, pending: false, citations: p.citations ?? m.citations } : m,
            ),
          );
          setDeepStatus(null); // the result has landed — clear the status line
          setDeepRunId(null);
          setDeepRoadmap(null); // live panel goes; the persisted "Research steps" recap takes over
          qc.invalidateQueries({ queryKey: ["chat-messages", p.chat_id] });
          qc.invalidateQueries({ queryKey: ["chats"] });
          break;
        }
        case "chat.groundedness": {
          // Post-stream faithfulness verdict (Mode A); arrives after chat.completed
          // so the message already carries its canonical id. Stored on the message
          // (persisted server-side too) → renders the score pill + flagged claims.
          const g = f as { message_id: string; score: number | null; total: number; flagged: number; spans: { start: number; end: number; text: string; label: string }[] };
          setMessages((p) => p.map((m) => (m.id === g.message_id ? { ...m, groundedness: { score: g.score, total: g.total, flagged: g.flagged, spans: g.spans } } : m)));
          break;
        }
        case "chat.interrupted":
          flushTokens.current();
          setMessages((p) => p.map((m) => (m.role === "assistant" && m.pending ? { ...m, pending: false } : m)));
          turnId.current = null;
          setSending(false);
          setRunningTool(null);
          setRunningToolDetail(null);
          setApproval(null);
          // Reconcile message ids + any partial-turn artefact (same as completed).
          if (chatRef.current) {
            qc.invalidateQueries({ queryKey: ["chat-messages", chatRef.current] });
            qc.invalidateQueries({ queryKey: ["artefacts", chatRef.current] });
          }
          break;
        case "chat.error": {
          const e = f as { message: string };
          flushTokens.current();
          setMessages((p) => p.map((m) => {
            if (m.id === pendingId.current) return { ...m, pending: false, error: true, content: m.content || `Error: ${e.message}` };
            return m.role === "assistant" && m.pending ? { ...m, pending: false } : m;
          }));
          turnId.current = null;
          setSending(false);
          setApproval(null);
          break;
        }
        case "chat.compacted": {
          const c = f as { summarised: number };
          setNotice(`Earlier history was compacted (${c.summarised} messages summarised).`);
          break;
        }
        case "context.warning": {
          const c = f as { chat_id: string; usage_pct: number };
          if (c.chat_id === chatRef.current) {
            setNotice(`This chat is using ~${c.usage_pct}% of the context window — older turns are being summarised. Start a new chat to keep full detail.`);
          }
          break;
        }
        case "code.result": {
          const c = f as unknown as { chat_id: string; code: string; stdout: string; stderr: string; exit_code: number };
          if (c.chat_id === chatRef.current) {
            setCodeRuns((p) => [...p, { code: c.code, stdout: c.stdout, stderr: c.stderr, exit_code: c.exit_code }]);
          }
          break;
        }
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function decideApproval(ok: boolean) {
    if (!approval) return;
    const rid = approval.runId;
    setApproval(null);
    try {
      if (ok) await approveAgentRun(rid);
      else await rejectAgentRun(rid);
    } catch (e) {
      // A 409 means the gate was already decided (a timeout, or the user on
      // another device got there first). That is not an error to report — the
      // decision stands, and the resolved frame has already settled the card.
      if ((e as { status?: number }).status === 409) return;
      setNotice((e as Error).message);
    }
  }

  // "Always allow this command here": agree the prefix for the folder this
  // approval is about, before approving the run, so the next identical command
  // does not stop to ask. The folder id rides on the approval detail.
  async function allowPrefixForApproval(prefix: string) {
    const wsId = approval?.detail?.workspace_id as string | undefined;
    if (!wsId) throw new Error("this action is not tied to a folder");
    await rememberPrefix(wsId, prefix);
  }

  // Core send: build the optimistic bubbles + emit the chat.send frame with the
  // composer's per-turn extras (attachments + reasoning). Called by <Composer>.
  function sendWith(content: string, extras: { attachments: ChatAttachmentMeta[]; reasoning: ReasoningSpec | null; llmProviderId?: string | null }) {
    if (!content || !agentId || sending) return;
    const nowIso = new Date().toISOString();
    const userMsg: Msg = {
      id: crypto.randomUUID(),
      role: "user",
      content,
      createdAt: nowIso,
      attachments: extras.attachments.length ? extras.attachments : undefined,
    };
    const pid = crypto.randomUUID();
    pendingId.current = pid;
    const now = new Date().toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
    setMessages((p) => [
      ...p.map((m) => (m.role === "assistant" && m.pending ? { ...m, pending: false } : m)), // resolve any straggler
      userMsg,
      { id: pid, role: "assistant", content: "", pending: true, agent: agentName(agentId), time: now, createdAt: nowIso, startedAt: Date.now() },
    ]);
    setSending(true);
    // Remember which artefacts predate this turn, so the panel can open the one
    // the turn produces (and may claim the panel again).
    panel.beginTurn();
    setRunningTool(null); // a new turn starts fresh; the new message holds its own activity
    setRunningToolDetail(null);
    wsStore.send({
      type: "chat.send",
      content,
      agent_id: agentId,
      chat_id: chatRef.current,
      project_id: chatRef.current ? null : (active?.id ?? null),
      attachment_ids: extras.attachments.map((a) => a.id),
      reasoning: extras.reasoning,
      // Per-turn LLM provider pick (multi-LLM); backend persists it to the chat so it
      // sticks (and a regenerate reuses it). Absent on composer-less sends → the chat's
      // stored provider / deployment default.
      llm_provider_id: extras.llmProviderId ?? null,
      // The folder chosen in the composer, carried so a new chat's first message
      // already works in it — the chat is created by this send, and could not be
      // bound to a folder before it existed. Only sent when nothing is bound yet;
      // an existing chat's binding lives server-side.
      workspace_id: chatRef.current ? null : (pendingWorkspace ?? null),
    });
    if (!chatRef.current && pendingWorkspace) {
      // It rode this send; the chat now exists and will carry it. Clear the
      // one-shot so a later turn does not re-bind it over a switch.
      setPendingWorkspace(null);
    }
    scrollToBottom();
  }
  // Sends that bypass the composer (EmptyChat suggestion, Regenerate): no per-turn
  // attachments; reasoning read from the persisted level.
  function sendText(raw: string) {
    sendWith(raw.trim(), { attachments: [], reasoning: currentReasoning() });
  }
  function cancel() {
    if (turnId.current) wsStore.send({ type: "chat.cancel", turn_id: turnId.current });
  }
  // In-place regenerate — drives the answer's Regenerate button, plus user-message
  // "Restart from here" and edit-and-resubmit. `fromId` is the assistant answer to
  // replace OR the user message to restart from; `editedContent` (user anchor only)
  // rewrites the prompt first. Mirrors sendWith's optimistic bubbles: it drops the
  // stale answer + everything after the anchor and appends one pending assistant, so
  // the backend's branch-replace is reflected immediately (no prompt duplication).
  function regenerate(fromId: string, editedContent?: string) {
    if (!chatRef.current || sending) return;
    setMessages((p) => {
      const i = p.findIndex((m) => m.id === fromId);
      if (i < 0) return p;
      const anchorIsUser = p[i].role === "user";
      // Keep up to and including the anchoring user turn; drop the rest.
      const keepUpto = anchorIsUser ? i : i - 1;
      if (keepUpto < 0) return p;
      const kept = p.slice(0, keepUpto + 1).map((m, idx) =>
        anchorIsUser && idx === keepUpto && editedContent !== undefined ? { ...m, content: editedContent } : m,
      );
      const pid = crypto.randomUUID();
      pendingId.current = pid;
      const now = new Date().toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
      return [
        ...kept,
        { id: pid, role: "assistant", content: "", pending: true, agent: agentName(agentId), time: now, createdAt: new Date().toISOString(), startedAt: Date.now() },
      ];
    });
    setSending(true);
    setRunningTool(null);
    setRunningToolDetail(null);
    wsStore.send({
      type: "chat.regenerate",
      chat_id: chatRef.current,
      from_message_id: fromId,
      content: editedContent ?? null,
    });
    scrollToBottom();
  }
  function saveEdit(id: string) {
    const text = editDraft.trim();
    setEditingId(null);
    if (text) regenerate(id, text);
  }

  const empty = messages.length === 0;
  // Prefer the persisted (LLM-generated) title once it lands; the ["chats"] WS
  // invalidate refreshes it live. Until then fall back to the first user message.
  // Resolve the current chat from BOTH lists — a research chat is absent from the
  // default ["chats"] list, so the header/title + DR params must fall back to the
  // research list (["chats","research"]).
  const currentChat = chatId
    ? chats.data?.find((c) => c.id === chatId) ?? researchChats.data?.find((c) => c.id === chatId)
    : undefined;
  const persistedTitle = currentChat?.title;
  // Attached libraries for a Deep Research chat (corpus runs), captured at run time
  // — shown next to the live roadmap and under the finished report.
  const researchParams = currentChat?.research_params;
  const researchKbNames = researchParams?.kb_names ?? [];
  const researchSource = researchParams?.source;
  const chatTitle =
    persistedTitle && persistedTitle !== "New chat"
      ? persistedTitle
      : (messages.find((m) => m.role === "user")?.content.slice(0, 80) ?? "New chat");
  const ready = !!agentId;

  // Artefacts grouped by the assistant message that produced them (rendered
  // inline under that answer); the rail lists them all for quick download.
  const artefactList = artefacts.data ?? [];
  const artByMsg = useMemo(() => {
    const m = new Map<string, Artefact[]>();
    for (const a of artefactList) {
      if (!a.message_id) continue;
      const arr = m.get(a.message_id) ?? [];
      arr.push(a);
      m.set(a.message_id, arr);
    }
    return m;
  }, [artefactList]);
  const [docsOpen, setDocsOpen] = useState(false);
  // User-attached files across the chat (flattened from the messages), for the rail.
  const attachList = useMemo(() => {
    const seen = new Set<string>();
    const out: ChatAttachmentMeta[] = [];
    for (const m of messages) {
      for (const a of m.attachments ?? []) {
        if (seen.has(a.id)) continue;
        seen.add(a.id);
        out.push(a);
      }
    }
    return out;
  }, [messages]);
  const [attachOpen, setAttachOpen] = useState(false);
  // Which artefact is open beside the chat (URL-backed, so it is shareable).
  const panel = useArtefactPanel(chatId, artefactList, artefacts.isLoading);

  return (
    <Dropzone
      className={"general-main" + (panel.isOpen ? " panel-open" : "") + (panel.isOpen && panel.docked ? " panel-docked" : "")}
      onFiles={(f) => composerRef.current?.addFiles(f)}
    >
      <div className="chat-col">
      {/* Live Deep Research roadmap — right-docked from the first progress event. */}
      {deepRoadmap && (
        <ResearchRoadmapPanel
          stages={stagesFor(researchSource)}
          phase={deepRoadmap.phase}
          sections={deepRoadmap.sections}
          done={deepRoadmap.done}
          sources={researchKbNames}
        />
      )}
      {/* Top bar — agent picker + new + connection + sign-out marker */}
      <header className="topbar">
        <div className="topbar-l">
          <AgentPicker agents={visibleAgents} value={agentId} defaultId={defaultAgentId} onChange={setAgentId} onSetDefault={pinDefaultAgent} onNew={() => setShowNewAgent(true)} canCreate={["client_admin", "super_admin"].includes(who.data?.role ?? "")} />
          {!chatId && active && <span className="conn mono">new chat in “{active.name}”</span>}
        </div>
        <div className="topbar-r">
          {codeOn && <span className="conn mono"><Icon.Code size={13} /> sandbox</span>}
        </div>
      </header>

      {sharing && chatId && (
        <ShareChatModal chatId={chatId} onClose={() => setSharing(false)} onShared={() => { setSharing(false); setNotice("Chat shared."); }} />
      )}

      {fb.modal}

      {/* Thread / empty state — hero is the new-chat landing only; never flash it
          while an existing chat's history is loading (chatId set). The chat header
          and notice banner live INSIDE the scroller so they pass under the absolute
          glass top bar as the thread scrolls. */}
      {empty && !sending && !chatId ? (
        <EmptyChat
          ready={ready}
          onPick={sendText}
          setup={
            who.data && !who.data.llm_configured
              ? {
                  canAdmin: ["client_admin", "super_admin"].includes(who.data.role ?? ""),
                  onProviders: () => nav("/admin/providers"),
                }
              : undefined
          }
        />
      ) : (
        <div className="thread" ref={setThreadEl}>
          {chatId && !empty && (
            <ChatHeader
              title={chatTitle}
              onRename={renameTo}
              onExport={(fmt) => exportChat(chatId, fmt).catch((e) => toast(`Export failed: ${(e as Error).message}`))}
              onArchive={archiveCurrent}
              onShare={() => setSharing(true)}
              actionsOnly={actionsOnly}
              onToggleActionsOnly={() => setActionsOnly((v) => !v)}
            />
          )}
          {notice && (
            <div className="border-b border-gold-dark/40 bg-gold/10 px-6 py-2 text-xs text-gold-light">
              {notice} <button onClick={() => setNotice(null)} className="underline">dismiss</button>
            </div>
          )}
          {/* The live turn's plan, pinned above the stream so "where are we" is
              always in view. Sourced from the pending message's own steps; it
              vanishes when the turn ends and the finished message carries the plan. */}
          {sending ? (() => {
            const pending = messages.find((m) => m.pending && m.role === "assistant");
            const steps = pending?.activity?.steps ?? [];
            return steps.length ? <PlanPanel steps={steps} /> : null;
          })() : null}
          <Virtuoso
            customScrollParent={threadEl ?? undefined}
            data={messages}
            followOutput="auto"
            computeItemKey={(_, m) => m.id}
            components={{ List: ThreadList }}
            itemContent={(_, m) => {
              if (m.role === "user") {
                const atts = m.attachments ?? [];
                // Hide the plaintext "[attached: …]" marker when real chips show it.
                const text = atts.length ? m.content.replace(/\n*\[attached:[^\]]*\]\s*$/, "") : m.content;
                const editing = editingId === m.id;
                const ago = relTime(m.createdAt);
                if (editing) {
                  return (
                    <div className="msg user fade-up">
                      <div className="msg-edit">
                        <textarea
                          className="msg-edit-area"
                          value={editDraft}
                          autoFocus
                          onChange={(e) => setEditDraft(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) { e.preventDefault(); saveEdit(m.id); }
                            if (e.key === "Escape") { e.preventDefault(); setEditingId(null); }
                          }}
                        />
                        <div className="msg-edit-actions">
                          <button className="btn-ghost" onClick={() => setEditingId(null)}>Cancel</button>
                          <button className="btn-primary" onClick={() => saveEdit(m.id)} disabled={!editDraft.trim()}>Save &amp; submit</button>
                        </div>
                      </div>
                    </div>
                  );
                }
                return (
                  <div className="msg user fade-up">
                    {text && (
                      <div className="bubble user">
                        {text}
                        {ago && (
                          <span className="msg-time-tip" aria-hidden="true">
                            {ago.rel} · {ago.abs}
                          </span>
                        )}
                      </div>
                    )}
                    <div className="msg-actions user">
                      <button onClick={() => copyMsg(m.id, m.content)} title={copiedId === m.id ? "Copied" : "Copy"}><Icon.Copy size={15} /></button>
                      <button onClick={() => { setEditDraft(text); setEditingId(m.id); }} title="Edit message"><Icon.Edit size={15} /></button>
                      <button onClick={() => regenerate(m.id)} title="Restart from here"><Icon.Refresh size={15} /></button>
                    </div>
                    {atts.length > 0 && (
                      <div className="msg-attachments">
                        {atts.map((a) =>
                          a.mime.startsWith("image/") ? (
                            <AttachmentImage key={a.id} att={a} />
                          ) : (
                            <button
                              key={a.id}
                              className="skill-chip"
                              onClick={() => downloadChatAttachment(a.id, a.filename).catch((e) => toast((e as Error).message))}
                              title={`Download ${a.filename}`}
                            >
                              <Icon.Doc size={13} />
                              <span style={{ maxWidth: "14rem", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                                {a.filename}
                              </span>
                              <span className="mono" style={{ color: "var(--ink-3)", fontSize: "0.7rem" }}>{fmtBytes(a.byte_size)}</span>
                              <Icon.Download size={12} />
                            </button>
                          ),
                        )}
                      </div>
                    )}
                  </div>
                );
              }
              const split = splitThink(m.content);
              // Prefer the live dedicated reasoning channel; fall back to the
              // `<think>` block folded into the persisted content (reload path).
              const reasoning = m.reasoning ?? split.reasoning;
              const answer = split.answer;
              const live = !!m.pending && !answer && !m.error; // still reasoning, no answer yet
              const copyText = answer || reasoning || m.content;
              return (
                <div className="msg ai fade-up">
                  <div className="ai-avatar"><Icon.Agents size={14} /></div>
                  <div className="ai-body">
                    <div className="ai-name mono">
                      {m.agent ?? agentName(agentId)}
                      {m.time && <span className="ai-time"> · {m.time}</span>}
                    </div>

                    {m.error ? (
                      <div className="ai-text" style={{ color: "var(--red)" }}>{m.content}</div>
                    ) : (
                      <>
                        {!actionsOnly && reasoning && reasoning.trim() && (
                          <Reasoning reasoning={reasoning} startedAt={m.startedAt} live={live} tokens={m.reasoningTokens} />
                        )}
                        {/* Pre-token wait: pending with no reasoning and no answer yet —
                            show an animated placeholder so the bubble isn't blank. For a
                            Deep Research run, surface the live DR phase ("Deep research —
                            outline · 0/8") instead of the generic "Reasoning…". */}
                        {m.pending && !answer && !(reasoning && reasoning.trim()) && (() => {
                          const mine = m.id === pendingId.current || m.id === bgTarget.current;
                          // Deep Research → the macro-stage label; deep web search →
                          // its coarse status; otherwise the generic thinking dots.
                          if (mine && deepRoadmap) {
                            return <div className="ai-text mono deep-phase"><span className="think-dots"><span /><span /><span /></span> {currentLabel(deepRoadmap.phase, deepRoadmap.sections, deepRoadmap.done)}</div>;
                          }
                          if (mine && deepStatus) {
                            return <div className="ai-text mono deep-phase"><span className="think-dots"><span /><span /><span /></span> {deepStatus}</div>;
                          }
                          return <Thinking startedAt={m.startedAt} />;
                        })()}
                        {answer && !actionsOnly && (
                          <MessageMarkdown
                            answer={answer}
                            pending={m.pending}
                            groundednessOn={groundednessOn}
                            spans={m.groundedness?.spans}
                          />
                        )}
                        {/* Completed turn that produced no answer text (e.g. tools ran
                            but nothing was said): show a terminal placeholder so the
                            bubble — and the live-voice overlay that mirrors it — never
                            sits on an empty "…". */}
                        {!m.pending && !answer && (
                          <div className="ai-text mono" style={{ opacity: 0.55 }}>(no response)</div>
                        )}
                      </>
                    )}

                    {(m.activity?.steps?.length || m.activity?.tools?.length || (m.pending && (runningTool || runningToolDetail || approval))) ? (
                      <AgentActivity
                        activity={m.activity}
                        live={!!m.pending}
                        startedAt={m.startedAt}
                        runningTool={m.pending ? runningTool : null}
                        runningDetail={m.pending ? runningToolDetail : null}
                        approval={m.pending && approval ? { tool: approval.tool, summary: approval.summary, detail: approval.detail, state: approval.state } : null}
                        onApprove={() => decideApproval(true)}
                        onReject={() => decideApproval(false)}
                        onAllowPrefix={allowPrefixForApproval}
                        terminalOut={m.pending && runningTool === "desktop.terminal_run" ? terminalOut : null}
                        onKillTerminal={() => { if (turnId.current) void cancelLocalCall(turnId.current).catch(() => {}); }}
                      />
                    ) : null}

                    {/* "What I did": the files this turn changed and the commands it
                        ran, once it has finished. Renders nothing for a turn with no
                        side effects. Carries the restore block on the desktop. */}
                    {!m.pending ? (
                      <TurnSummary
                        activity={m.activity}
                        turnId={m.turnId}
                        startedAt={m.startedAt}
                        workspaceId={boundWorkspace.data?.id}
                      />
                    ) : null}

                    {groundednessOn && m.groundedness ? <Groundedness groundedness={m.groundedness} messageId={m.id} /> : null}

                    {/* Deep Research recap: attached libraries (corpus runs) +
                        the section roadmap + phase timeline, persisted on the
                        message so it survives a reload. */}
                    {m.activity?.research_roadmap?.sections?.length ? (
                      <>
                        {researchKbNames.length > 0 && (
                          <div className="roadmap-sources mono" style={{ marginTop: 8 }}>
                            Sources: {researchKbNames.map((s) => `“${s}”`).join(", ")}
                          </div>
                        )}
                        <ResearchSteps
                          sections={m.activity.research_roadmap.sections}
                          phases={m.activity.research_roadmap.phases}
                        />
                      </>
                    ) : null}

                    {/* Artefacts are listed as chips; the document itself is read in
                        the panel beside the chat, which is also where the convert,
                        page and verify actions live. */}
                    {artByMsg.get(m.id)?.length ? (
                      <div className="msg-artefacts">
                        {artByMsg.get(m.id)!.map((a) => (
                          <ArtefactChip
                            key={a.id}
                            artefact={a}
                            selected={panel.selectedId === a.id}
                            onOpen={panel.open}
                            onDownload={artefactActions.download}
                          />
                        ))}
                      </div>
                    ) : null}

                    {!m.pending && !m.error && (
                      <div className="msg-actions">
                        <button onClick={() => copyMsg(m.id, copyText)} title={copiedId === m.id ? "Copied" : "Copy"}><Icon.Copy size={15} /></button>
                        <button onClick={() => regenerate(m.id)} title="Regenerate"><Icon.Refresh size={15} /></button>
                        {voiceOn && (
                          <button
                            onClick={() => (readAloud.speakingId === m.id ? readAloud.stop() : readAloud.play(m.id, copyText))}
                            disabled={readAloud.busy && readAloud.speakingId !== m.id}
                            title="Read aloud"
                          >
                            {readAloud.speakingId === m.id ? <Icon.Pause size={15} /> : <Icon.Play size={15} />}
                          </button>
                        )}
                        <span className="msg-actions-sep" />
                        <button className={"fb" + (fb.feedback[m.id] === "up" ? " on" : "")} onClick={() => fb.rate(m.id, "up")} title="Good response"><Icon.Like size={15} /></button>
                        <button className={"fb" + (fb.feedback[m.id] === "down" ? " on down" : "")} onClick={() => fb.rate(m.id, "down")} title="Needs work"><Icon.Dislike size={15} /></button>
                        {getMessageActions().map((a) => {
                          const ctx: MsgActionCtx = {
                            msg: { id: m.id, reviewDecision: m.reviewDecision },
                            who: who.data,
                            chatId,
                            openOverlay,
                          };
                          return a.predicate(ctx) ? <Fragment key={a.key}>{a.render(ctx)}</Fragment> : null;
                        })}
                      </div>
                    )}

                    {m.citations && m.citations.length > 0 && (
                      <details className="cites-wrap">
                        <summary className="cites-summary">
                          <Icon.Quote size={12} />
                          <span>{m.citations.length} source{m.citations.length === 1 ? "" : "s"}</span>
                          <Icon.Chevron size={13} className="cites-chev" />
                        </summary>
                        <div className="cites">
                          {m.citations.map((c, i) => (
                            <CiteChip key={i} c={c} onOpen={() => setCitation(c)} />
                          ))}
                        </div>
                      </details>
                    )}
                  </div>
                </div>
              );
            }}
          />

          {/* Agent activity (steps + tools + approval) renders inline under its
              assistant message. Code-interpreter runs + the scroll anchor sit after
              the virtualised list, still within the `.thread` scroll container. */}
          <div className="thread-inner" style={{ paddingTop: 0 }}>
            {codeRuns.map((r, i) => (
              <details key={i} className="self-start max-w-[80ch] rounded-sm border border-line bg-navy-light/60 text-xs">
                <summary className="mono cursor-pointer px-3 py-1.5 text-slate-lightest">ran Python · exit {r.exit_code}</summary>
                <div className="space-y-2 px-3 pb-2">
                  <CodeBlock code={r.code} lang="python" />
                  {r.stdout && <pre className="overflow-x-auto whitespace-pre-wrap rounded bg-navy-lighter/40 p-2 text-slate">{r.stdout}</pre>}
                  {r.stderr && <pre className="overflow-x-auto whitespace-pre-wrap rounded bg-navy-lighter/40 p-2 text-urgency-red">{r.stderr}</pre>}
                </div>
              </details>
            ))}
            <div ref={bottom} />
          </div>
        </div>
      )}

      {/* Corner rail — generated documents + user-attached files for this chat
          (agent activity now lives inline under each message). */}
      {(artefactList.length > 0 || attachList.length > 0) && (
        <div className="doc-rail">
          {artefactList.length > 0 && (
            <div className="rail-group">
              <button className="doc-rail-toggle" onClick={() => setDocsOpen((v) => !v)} title="Generated documents">
                <Icon.Doc size={14} />
                <span>{artefactList.length} doc{artefactList.length > 1 ? "s" : ""}</span>
                <Icon.Chevron size={13} style={{ transform: docsOpen ? "rotate(180deg)" : "none", transition: "transform .15s" }} />
              </button>
              {docsOpen && (
                <div className="doc-rail-list">
                  <div className="doc-rail-head mono">Generated documents</div>
                  {artefactList.map((a) => (
                    <button
                      key={a.id}
                      className="doc-rail-item"
                      title={`Open ${a.title}`}
                      onClick={() => panel.open(a)}
                    >
                      <Icon.Doc size={14} />
                      <span className="doc-rail-name">{a.title}</span>
                      <span className="artefact-kind mono">{a.kind}</span>
                    </button>
                  ))}
                </div>
              )}
            </div>
          )}
          {attachList.length > 0 && (
            <div className="rail-group">
              <button className="doc-rail-toggle" onClick={() => setAttachOpen((v) => !v)} title="Attached files">
                <Icon.Doc size={14} />
                <span>{attachList.length} file{attachList.length > 1 ? "s" : ""}</span>
                <Icon.Chevron size={13} style={{ transform: attachOpen ? "rotate(180deg)" : "none", transition: "transform .15s" }} />
              </button>
              {attachOpen && (
                <div className="doc-rail-list">
                  <div className="doc-rail-head mono">Attached files</div>
                  {attachList.map((a) => (
                    <button
                      key={a.id}
                      className="doc-rail-item"
                      title={`Download ${a.filename}`}
                      onClick={() => downloadChatAttachment(a.id, a.filename).catch((e) => toast((e as Error).message))}
                    >
                      <Icon.Doc size={14} />
                      <span className="doc-rail-name">{a.filename}</span>
                      <span className="artefact-kind mono">{fmtBytes(a.byte_size)}</span>
                      <Icon.Download size={13} />
                    </button>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      )}

      {/* Composer — floats over the thread (chat scrolls under the glass) */}
      <Composer
        ref={composerRef}
        setAgentId={setAgentId}
        chatId={chatId ?? null}
        ready={ready}
        sending={sending}
        placeholder={ready ? `Message ${agentName(agentId)}…` : "Select or create an agent first"}
        agentName={agentName(agentId)}
        live={live}
        messages={messages}
        onCancel={cancel}
        onSubmit={sendWith}
        statusSlot={deepRunId && deepRoadmap ? (
          <div className="composer-wrap" style={{ paddingBottom: 0 }}>
            <div className="mono deep-status glass glass--pill">
              <span className="think-dots"><span /><span /><span /></span>
              <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                {stepLabel(deepRoadmap.phase, deepRoadmap.detail, deepRoadmap.sourcesRead)}
              </span>
              <button
                className="btn btn-line"
                style={{ marginLeft: "auto", padding: "1px 8px", fontSize: "0.7rem" }}
                onClick={() => {
                  const id = deepRunId;
                  setDeepStatus("Deep research — stopping…");
                  setDeepRunId(null);
                  void cancelAgentRun(id).catch(() => {});
                }}
              >
                Stop
              </button>
            </div>
          </div>
        ) : null}
        groundingSlot={(close) => (
          <>
            <div className="divider" style={{ margin: "5px 0" }} />
            <div className="menu-label mono">Ground in a project</div>
            <button className="menu-item" onClick={() => { setActive(null); close(); }}>{!active && <Icon.Check size={14} />} None</button>
            {(projects.data ?? []).map((p) => (
              <button key={p.id} className="menu-item" onClick={() => { setActive(p); close(); }}>
                {active?.id === p.id && <Icon.Check size={14} />} {p.name}
              </button>
            ))}
            {chatId && !active && (libs.data?.length ?? 0) > 0 && (
              <>
                <div className="divider" style={{ margin: "5px 0" }} />
                <div className="menu-label mono">Attach a library (this chat)</div>
                {(libs.data ?? []).map((k) => {
                  const on = (chatLibs.data ?? []).some((l) => l.id === k.id);
                  return (
                    <button
                      key={k.id}
                      className="menu-item"
                      disabled={attaching || on}
                      onClick={async () => {
                        setAttaching(true);
                        try {
                          await attachChatLibrary(chatId, k.id);
                          await qc.invalidateQueries({ queryKey: ["chat-kb-links", chatId] });
                          close();
                        } catch (e) {
                          toast(`Attach failed: ${(e as Error).message}`);
                        } finally {
                          setAttaching(false);
                        }
                      }}
                    >
                      {on ? <Icon.Check size={14} /> : <Icon.Layers size={14} />} {k.name}
                    </button>
                  );
                })}
              </>
            )}
            <FolderMenu chatId={chatId ?? null} pending={pendingWorkspace} setPending={setPendingWorkspace} close={close} />
          </>
        )}
      />

      {/* Modals / overlays */}
      {showNewAgent && (
        <NewAgentModal
          onClose={() => setShowNewAgent(false)}
          onCreated={async (id) => {
            setShowNewAgent(false);
            await qc.invalidateQueries({ queryKey: ["agents"] });
            setAgentId(id);
          }}
        />
      )}
      {citation && <CitationPanel citation={citation} onClose={() => setCitation(null)} />}
      {overlay && (() => {
        const O = getMessageOverlay(overlay.key);
        return O ? <O.component {...overlay.props} onClose={() => setOverlay(null)} /> : null;
      })()}
      </div>

      {/* The selected artefact: a column beside the thread on a wide viewport,
          a drawer over it otherwise. */}
      <ArtefactPanelHost
        open={panel.isOpen}
        artefact={panel.selected}
        loading={panel.pending}
        missing={panel.missing}
        mode={panel.mode}
        actions={artefactActions}
        groundednessOn={groundednessOn}
        onClose={panel.close}
        onInteract={panel.markInteracted}
      />
    </Dropzone>
  );
}

// ── Chat header: serif title (double-click to rename) + export/archive menu ──
function ChatHeader({
  title, onRename, onExport, onArchive, onShare, actionsOnly, onToggleActionsOnly,
}: {
  title: string;
  onRename: (t: string) => void;
  onExport: (fmt: "md" | "json" | "pdf") => void;
  onArchive: () => void;
  onShare: () => void;
  actionsOnly: boolean;
  onToggleActionsOnly: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [val, setVal] = useState(title);
  const [menu, setMenu] = useState(false);
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => setVal(title), [title]);
  useEffect(() => {
    const onDoc = (e: MouseEvent) => { if (ref.current && !ref.current.contains(e.target as Node)) setMenu(false); };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, []);
  const commit = () => {
    setEditing(false);
    const t = val.trim();
    if (t && t !== title) onRename(t); else setVal(title);
  };
  return (
    <div className="chat-header">
      {editing ? (
        <input
          className="chat-rename"
          autoFocus
          value={val}
          onChange={(e) => setVal(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => { if (e.key === "Enter") commit(); if (e.key === "Escape") { setVal(title); setEditing(false); } }}
        />
      ) : (
        <h2 className="chat-h-title serif" onDoubleClick={() => setEditing(true)} title="Double-click to rename">{title}</h2>
      )}
      <div className="chat-h-actions" ref={ref}>
        <button
          className={"icon-btn" + (actionsOnly ? " active" : "")}
          title={actionsOnly ? "Show everything" : "Actions only: hide prose, keep the plan, tools and results"}
          aria-pressed={actionsOnly}
          onClick={onToggleActionsOnly}
        >
          <Icon.Filter size={15} />
        </button>
        <button className="icon-btn" title="Rename" onClick={() => setEditing(true)}><Icon.Edit size={15} /></button>
        <div className="menu-wrap">
          <button className="icon-btn" title="More" onClick={() => setMenu((m) => !m)}><Icon.Dots size={16} /></button>
          {menu && (
            <div className="menu fade-up glass glass--menu">
              <button className="menu-item" onClick={() => { setMenu(false); onShare(); }}><Icon.Send size={15} /> Share to a chat…</button>
              <div className="divider" style={{ margin: "5px 0" }} />
              <div className="menu-label mono">Export</div>
              <button className="menu-item" onClick={() => { setMenu(false); onExport("md"); }}><Icon.Download size={15} /> Markdown (.md)</button>
              <button className="menu-item" onClick={() => { setMenu(false); onExport("json"); }}><Icon.Download size={15} /> JSON (.json)</button>
              <button className="menu-item" onClick={() => { setMenu(false); onExport("pdf"); }}><Icon.Download size={15} /> PDF (.pdf)</button>
              <div className="divider" style={{ margin: "5px 0" }} />
              <button className="menu-item danger" onClick={() => { setMenu(false); onArchive(); }}><Icon.Trash size={15} /> Archive chat</button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

// ── Share this chat into a group/project/DM chat ──
function ShareChatModal({ chatId, onClose, onShared }: { chatId: string; onClose: () => void; onShared: () => void }) {
  const qc = useQueryClient();
  const chats = useGroupChats();
  const myShares = useMyShares();
  const [mode, setMode] = useState<"share" | "manage">("share");
  const [busy, setBusy] = useState(false);
  const [target, setTarget] = useState("");

  async function submit() {
    if (!target || busy) return;
    setBusy(true);
    try { await shareChat(chatId, target); onShared(); }
    catch (e) { toast(`Share failed: ${(e as Error).message}`); setBusy(false); }
  }

  async function revoke(s: ChatShare) {
    if (!(await confirmDialog({ title: `Stop sharing "${s.chat_title}"?`, body: `It will no longer appear in ${s.group_chat_name}.`, danger: true, confirmLabel: "Stop sharing" }))) return;
    try { await revokeShare(s.chat_id, s.group_chat_id); qc.invalidateQueries({ queryKey: ["my-shares"] }); }
    catch (e) { toast(`Revoke failed: ${(e as Error).message}`); }
  }
  const section = (label: string, kind: string) => {
    const list = (chats.data ?? []).filter((c) => c.kind === kind);
    if (!list.length) return null;
    return (
      <>
        <div className="menu-label mono" style={{ margin: "8px 0 4px" }}>{label}</div>
        {list.map((c) => (
          <button key={c.id} className={"kb-opt" + (target === c.id ? " on" : "")} onClick={() => setTarget(c.id)}>
            <span className="kb-check">{target === c.id && <Icon.Check size={13} />}</span>
            <Icon.Team size={15} /><span className="kb-name">{c.name ?? c.kind}</span>
          </button>
        ))}
      </>
    );
  };

  return (
    <div className="modal-scrim" onClick={onClose}>
      <div className="modal" style={{ width: 520, maxWidth: "100%" }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <div>
            <div className="eyebrow">Share</div>
            <h2 className="serif modal-title">{mode === "share" ? "Share this chat" : "Your shared chats"}</h2>
          </div>
          <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
            <button
              className="btn btn-ghost"
              style={{ fontSize: 12, padding: "4px 10px" }}
              onClick={() => { const m = mode === "share" ? "manage" : "share"; setMode(m); if (m === "manage") myShares.refetch(); }}
            >
              {mode === "share" ? "Manage my shares" : "← Share"}
            </button>
            <button className="icon-btn" onClick={onClose}><Icon.Close size={18} /></button>
          </div>
        </div>
        <div className="modal-body">
          {mode === "share" ? (
            <>
              <label className="form-label">Send a link to…</label>
              {chats.isLoading ? (
                <p className="ed-hint mono">Loading…</p>
              ) : (chats.data?.length ?? 0) === 0 ? (
                <p className="ed-hint mono">No team chats to share into yet.</p>
              ) : (
                <div className="kb-list scroll">
                  {section("Projects", "project")}
                  {section("Groups", "group")}
                  {section("Direct messages", "dm")}
                </div>
              )}
            </>
          ) : (
            <>
              <label className="form-label">Chats you've shared — revoke to cut access.</label>
              {myShares.isLoading ? (
                <p className="ed-hint mono">Loading…</p>
              ) : (myShares.data?.length ?? 0) === 0 ? (
                <p className="ed-hint mono">You haven't shared any chats.</p>
              ) : (
                <div className="kb-list scroll" style={{ display: "grid", gap: 8 }}>
                  {myShares.data!.map((s) => (
                    <div key={s.chat_id + s.group_chat_id} className="list-row">
                      <Icon.Team size={15} />
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <div className="truncate">{s.chat_title}</div>
                        <div className="ed-hint mono" style={{ fontSize: 11 }}>→ {s.group_chat_name}</div>
                      </div>
                      <button className="icon-btn" title="Revoke" onClick={() => revoke(s)}><Icon.Close size={15} /></button>
                    </div>
                  ))}
                </div>
              )}
            </>
          )}
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>Close</button>
          {mode === "share" && (
            <button className="btn btn-gold" onClick={submit} disabled={busy || !target}>{busy ? "Sharing…" : "Share"}</button>
          )}
        </div>
      </div>
    </div>
  );
}

// ── First-run onboarding checklist ──
// Shown on the empty chat only while no LLM provider is configured (whoami
// `llm_configured=false`). A checklist, not a wizard: three steps that fade away
// the moment a model is connected (with `--profile local` the backend seeds a
// provider on boot, so this never appears). Admins get a direct link to Providers;
// everyone else is told to ask an administrator.
function OnboardingChecklist({ canAdmin, onProviders }: { canAdmin: boolean; onProviders: () => void }) {
  return (
    <div className="onboard-card anim-on fade-in">
      <div className="eyebrow">Get started</div>
      <ol className="onboard-steps">
        <li className="onboard-step">
          <span className="onboard-num">1</span>
          <div className="onboard-body">
            <div className="onboard-title">Connect a model</div>
            <div className="onboard-hint">
              {canAdmin
                ? "Add an LLM provider (OpenAI, OpenRouter, Anthropic, or a local engine). Or relaunch with the local profile — then it is already wired up."
                : "Ask an administrator to add a model provider, or relaunch with the local profile."}
            </div>
            {canAdmin && (
              <button className="btn btn-gold onboard-cta" onClick={onProviders}>
                <Icon.Wrench size={14} /> Open Providers
              </button>
            )}
          </div>
        </li>
        <li className="onboard-step">
          <span className="onboard-num">2</span>
          <div className="onboard-body">
            <div className="onboard-title">Upload a document</div>
            <div className="onboard-hint">Add a file to a Project's Knowledge so answers can cite your own material.</div>
          </div>
        </li>
        <li className="onboard-step">
          <span className="onboard-num">3</span>
          <div className="onboard-body">
            <div className="onboard-title">Ask a question</div>
            <div className="onboard-hint">Type below and start the conversation.</div>
          </div>
        </li>
      </ol>
    </div>
  );
}

// ── Custom agent dropdown (replaces native <select>) ──
// Everyday-AI prompt starters. We show four at random per mount so the empty
// screen feels alive rather than serving the same static list every time. Only
// shown once a provider is configured (i.e. the GET STARTED checklist is gone).
const SUGGESTION_POOL = [
  "Summarise a document for me.",
  "Draft a professional email.",
  "Explain a concept in plain language.",
  "Help me brainstorm ideas.",
  "Rewrite this to sound more polished.",
  "Turn my notes into a clear summary.",
  "Compare two options and recommend one.",
  "Draft a plan for a project.",
  "Give me feedback on an idea.",
  "Create a checklist for a task.",
  "Proofread and improve my writing.",
  "Answer a question from my documents.",
];

function EmptyChat({ ready, onPick, setup }: { ready: boolean; onPick: (t: string) => void; setup?: { canAdmin: boolean; onProviders: () => void } }) {
  // Pick once per mount so the four stay put across re-renders but reshuffle on
  // each fresh visit to the empty screen.
  const [picks] = useState(() => [...SUGGESTION_POOL].sort(() => Math.random() - 0.5).slice(0, 4));
  return (
    <div className="empty stagger anim-on relative z-0">
      <div className="absolute inset-0 -z-10 opacity-70 pointer-events-none"><NeuralBackground /></div>
      <div className="eyebrow fade-up">General workspace</div>
      <h1 className="serif empty-title fade-up">What should we work on?</h1>
      <p className="empty-sub fade-up">{ready ? "Pick a starting point, or just start typing below." : "Select or create an agent to begin."}</p>
      {setup && <OnboardingChecklist canAdmin={setup.canAdmin} onProviders={setup.onProviders} />}
      {!setup && ready && (
        <div className="suggest-grid fade-up">
          {picks.map((s, i) => (
            <button key={i} className="suggest" onClick={() => onPick(s)}>
              <Icon.ChevronR size={14} />
              <span>{s}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// ── New-agent modal ──
// Minimal agent creation with a tool selector (full editor — params, Project
// Knowledge, Skills — lives in the Agents screen). `edit_document` is what lets
// an agent propose tracked changes on workspace documents.
const AGENT_TOOLS: { name: string; label: string; hint: string }[] = [
  { name: "edit_document", label: "Edit document", hint: "propose tracked changes on a workspace doc" },
  { name: "read_document", label: "Read document", hint: "read a document's text" },
  { name: "list_documents", label: "List documents", hint: "see the project's documents" },
  { name: "read_table_cells", label: "Read table cells", hint: "read a tabular review's results" },
  { name: "generate_artefact", label: "Generate artefact", hint: "create a new document" },
];

function NewAgentModal({ onClose, onCreated }: { onClose: () => void; onCreated: (id: string) => void }) {
  const [name, setName] = useState("");
  const [sys, setSys] = useState("You are a helpful assistant.");
  const [tools, setTools] = useState<Set<string>>(new Set());
  const [submitting, setSubmitting] = useState(false);

  function toggle(t: string) {
    setTools((prev) => {
      const next = new Set(prev);
      if (next.has(t)) next.delete(t); else next.add(t);
      return next;
    });
  }

  async function submit() {
    if (!name.trim() || submitting) return;
    setSubmitting(true);
    try {
      const { id } = await createAgent(name.trim(), sys, [...tools]);
      onCreated(id);
    } catch (e) {
      toast(`Create agent failed: ${(e as Error).message}`);
      setSubmitting(false);
    }
  }

  return (
    <div className="modal-scrim" onClick={onClose}>
      <div className="modal" style={{ width: 520, maxWidth: "100%" }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <div>
            <div className="eyebrow">Agents</div>
            <h2 className="serif modal-title">New agent</h2>
          </div>
          <button className="icon-btn" onClick={onClose}><Icon.Close size={18} /></button>
        </div>
        <div className="modal-body">
          <label className="form-label">Name</label>
          <input className="field" value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Legal editor" />

          <label className="form-label">System prompt</label>
          <textarea className="field code-field" rows={3} value={sys} onChange={(e) => setSys(e.target.value)} />

          <label className="form-label">Tools</label>
          <div className="ed-section" style={{ padding: 14 }}>
            {AGENT_TOOLS.map((t) => (
              <div key={t.name} className={"tool-row" + (tools.has(t.name) ? "" : " ")}>
                <span className="tool-ic"><Icon.Wrench size={15} /></span>
                <div className="tool-info">
                  <span className="tool-name">{t.label}</span>
                  <span className="tool-desc">{t.hint}</span>
                </div>
                <button className={"toggle" + (tools.has(t.name) ? " on" : "")} onClick={() => toggle(t.name)} aria-label={t.label}>
                  <span className="toggle-knob" />
                </button>
              </div>
            ))}
          </div>
        </div>
        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>Cancel</button>
          <button className="btn btn-gold" onClick={submit} disabled={!name.trim() || submitting}>
            {submitting ? "Creating…" : "Create agent"}
          </button>
        </div>
      </div>
    </div>
  );
}

// Enterprise per-message actions/overlays (Review & Approve) register from the
// private edition's entry via the message-action registry — never from this Core
// screen. The Core render loop (`getMessageActions`/`getMessageOverlay`) shows
// whatever is registered.
