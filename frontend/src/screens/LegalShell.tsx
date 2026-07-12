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

import { toast } from "@/components/dialogs";
import { useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { MessageMarkdown } from "@/components/MessageMarkdown";
import {
  approveAgentRun,
  createKnowledge,
  downloadArtefact,
  rejectAgentRun,
  uploadDocument,
  uploadWorkspaceDoc,
  useAgents,
  useChatArtefacts,
  useChatMessages,
  useProjectDocs,
  useProjects,
  useReviews,
  useWhoami,
  useWorkspaceDocs,
  type Artefact,
  type ChatAttachmentMeta,
  type ProjectSummary,
} from "@/api/client";
import { useActiveProject } from "@/app/ProjectContext";
import { Icon } from "@/components/icons";
import { useFeedback } from "@/components/feedback";
import { AgentPicker, useAgentSelection, agentsForMode } from "@/components/AgentPicker";
import { Composer, type ComposerHandle } from "@/components/Composer";
import { useLiveVoice } from "@/voice/useLiveVoice";
import { CitationPanel } from "@/components/CitationPanel";
import { CreateReviewModal } from "@/components/CreateReviewModal";
import { Reasoning, splitThink } from "@/components/reasoning";
import { wsStore } from "@/ws/store";
import type { Citation, ReasoningSpec, ServerFrame } from "@/ws/protocol";

// The Legal CHAT surface (reached via New chat / `/` and `/c/:id` in Legal mode).
// A tab strip — Assistant / Tabular Review / Document Viewer / Documents — over the
// active matter (the active legal project, if one is selected). Opening a project
// itself goes to the ProjectWorkspace window (app/App.tsx), not here.

type TabId = "assistant" | "tabular" | "viewer" | "docs";

const LEGAL_TABS: { id: TabId; label: string; Glyph: typeof Icon.Scale }[] = [
  { id: "assistant", label: "Assistant", Glyph: Icon.Scale },
  { id: "tabular", label: "Tabular Review", Glyph: Icon.Grid },
  { id: "viewer", label: "Document Viewer", Glyph: Icon.Doc },
  { id: "docs", label: "Documents", Glyph: Icon.Docs },
];

export function LegalShell() {
  const { projectId } = useParams();
  const { active } = useActiveProject();
  const projects = useProjects();
  const matter = projects.data?.find((p) => p.id === projectId) ?? active ?? null;
  const [tab, setTab] = useState<TabId>("assistant");

  // "New chat" (sidebar) routes to `/` with a `newChat` marker — when we're already
  // on `/` the route doesn't remount, so snap back to the Assistant tab explicitly.
  const loc = useLocation();
  const newChat = (loc.state as { newChat?: number } | null)?.newChat;
  useEffect(() => {
    if (newChat) setTab("assistant");
  }, [newChat]);

  return (
    <div className="legal-shell">
      <div className="legal-tabs">
        <div className="legal-tabs-l">
          {LEGAL_TABS.map(({ id, label, Glyph }) => (
            <button key={id} className={"legal-tab" + (tab === id ? " on" : "")} onClick={() => setTab(id)}>
              <Glyph size={15} /> {label}
            </button>
          ))}
        </div>
      </div>

      <div className="legal-body">
        {tab === "assistant" ? (
          <LegalAssistant matter={matter} />
        ) : !matter ? (
          <NoMatter />
        ) : tab === "tabular" ? (
          <LegalTabular matter={matter} />
        ) : tab === "viewer" ? (
          <LegalViewer matter={matter} />
        ) : (
          <LegalDocs matter={matter} />
        )}
      </div>
    </div>
  );
}

function NoMatter() {
  return (
    <div className="empty">
      <div className="eyebrow">Legal workspace</div>
      <h1 className="serif empty-title">Open a matter</h1>
      <p className="empty-sub">
        Choose a <b>matter</b> from the Projects list to use its tabular reviews and
        documents. The Assistant works with or without a matter.
      </p>
    </div>
  );
}

// ── Matter-grounded assistant: two-pane chat with a cited-source rail ──
interface LMsg {
  id: string;
  role: "user" | "assistant";
  content: string;
  pending?: boolean;
  error?: boolean;
  citations?: Citation[];
  attachments?: ChatAttachmentMeta[];
  agent?: string;
  time?: string;
  startedAt?: number;
}

function LegalAssistant({ matter }: { matter: ProjectSummary | null }) {
  const qc = useQueryClient();
  const nav = useNavigate();
  const { chatId } = useParams();
  const agents = useAgents();
  const who = useWhoami();
  const docs = useWorkspaceDocs(matter?.id);
  const history = useChatMessages(chatId);
  const artefacts = useChatArtefacts(chatId);
  // The legal shell only ever offers legal-tagged agents.
  const visibleAgents = useMemo(() => agentsForMode(agents.data ?? [], "legal"), [agents.data]);
  const { agentId, setAgentId, defaultAgentId, pinDefaultAgent } = useAgentSelection(visibleAgents);
  const [messages, setMessages] = useState<LMsg[]>([]);
  const [sending, setSending] = useState(false);
  const [sel, setSel] = useState(0);
  const [citation, setCitation] = useState<Citation | null>(null);
  // Inline HITL gate: an action-taking agent (e.g. Legal Drafter generating a
  // document) pauses for approval; show the card in-thread, not only the sidebar.
  const [approval, setApproval] = useState<{ runId: string; tool: string; summary: string } | null>(null);
  const fb = useFeedback();

  // Generated artefacts grouped by the assistant message that produced them, so a
  // download chip renders inline under that answer (matches the General chat).
  const artByMsg = useMemo(() => {
    const m = new Map<string, Artefact[]>();
    for (const a of artefacts.data ?? []) {
      if (!a.message_id) continue;
      const arr = m.get(a.message_id) ?? [];
      arr.push(a);
      m.set(a.message_id, arr);
    }
    return m;
  }, [artefacts.data]);

  async function decideApproval(ok: boolean) {
    if (!approval) return;
    const rid = approval.runId;
    setApproval(null);
    try {
      if (ok) await approveAgentRun(rid);
      else await rejectAgentRun(rid);
    } catch (e) {
      toast((e as Error).message);
    }
  }

  const chatRef = useRef<string | null>(null);
  const createdLocally = useRef<string | null>(null);
  const pendingId = useRef<string | null>(null);
  const turnId = useRef<string | null>(null);
  const bottom = useRef<HTMLDivElement | null>(null);
  const composerRef = useRef<ComposerHandle>(null);

  // Token-stream coalescing (re-audit R17, same shape as Chat.tsx): buffer
  // deltas and apply once per animation frame instead of a render per token.
  // The target id is captured at apply time, never read lazily in the updater.
  const tokenBuf = useRef("");
  const rafId = useRef<number | null>(null);
  const applyTokenBuffer = useRef(() => {
    const buf = tokenBuf.current;
    if (!buf) return;
    tokenBuf.current = "";
    const target = pendingId.current;
    setMessages((p) => p.map((m) => (m.id === target ? { ...m, content: m.content + buf } : m)));
  });
  const flushTokens = useRef(() => {
    if (rafId.current != null) {
      cancelAnimationFrame(rafId.current);
      rafId.current = null;
    }
    applyTokenBuffer.current();
  });

  const agentName = (id: string | null) => agents.data?.find((a) => a.id === id)?.name ?? "Reviewer";

  // Route change: switching chats / starting a new one (mirrors the General chat).
  useEffect(() => {
    chatRef.current = chatId ?? null;
    setSel(0);
    if (!chatId || chatId !== createdLocally.current) setMessages([]);
  }, [chatId]);

  // Seed history for an existing chat (not the one we just created in this view).
  useEffect(() => {
    if (chatId && history.data && chatId !== createdLocally.current) {
      setMessages(history.data.map((m) => ({ id: m.id, role: m.role as LMsg["role"], content: m.content })));
    }
  }, [chatId, history.data]);

  useEffect(() => {
    bottom.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

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
        case "chat.token": {
          const t = f as { turn_id: string; delta: string };
          turnId.current = t.turn_id;
          // Coalesce: buffer the delta and flush once per frame (R17).
          tokenBuf.current += t.delta;
          if (rafId.current == null) {
            rafId.current = requestAnimationFrame(() => {
              rafId.current = null;
              applyTokenBuffer.current();
            });
          }
          break;
        }
        case "agent.approval": {
          const a = f as { run_id: string; tool: string; summary: string };
          setApproval({ runId: a.run_id, tool: a.tool, summary: a.summary });
          break;
        }
        case "chat.completed": {
          const c = f as { message_id: string; chat_id?: string };
          flushTokens.current(); // apply the final buffered tokens before the id-swap
          // Capture before reassigning the ref — the updater runs later.
          const prevPid = pendingId.current;
          pendingId.current = c.message_id;
          setMessages((p) => p.map((m) => {
            if (m.id === prevPid) return { ...m, id: c.message_id, pending: false };
            return m.role === "assistant" && m.pending ? { ...m, pending: false } : m;
          }));
          turnId.current = null;
          setSending(false);
          setApproval(null);
          // The approved artefact is written server-side just before this frame —
          // refetch so its inline chip appears (the follow-up catches the auto one).
          const cid = c.chat_id ?? chatRef.current ?? undefined;
          if (cid) {
            qc.invalidateQueries({ queryKey: ["artefacts", cid] });
            window.setTimeout(() => qc.invalidateQueries({ queryKey: ["artefacts", cid] }), 1200);
          }
          break;
        }
        case "chat.citations": {
          const c = f as { message_id: string; citations: Citation[] };
          setMessages((p) => p.map((m) => (m.id === c.message_id ? { ...m, citations: c.citations } : m)));
          break;
        }
        case "chat.interrupted":
          flushTokens.current();
          setMessages((p) => p.map((m) => (m.role === "assistant" && m.pending ? { ...m, pending: false } : m)));
          turnId.current = null;
          setSending(false);
          setApproval(null);
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
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // A live-voice user turn was finalised (server already started the turn) — set up
  // the optimistic bubbles like a typed send, minus the chat.send frame.
  function onVoiceFinal(text: string) {
    const content = text.trim();
    if (!content) return;
    const pid = crypto.randomUUID();
    pendingId.current = pid;
    const now = new Date().toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
    setMessages((p) => [
      ...p.map((m) => (m.role === "assistant" && m.pending ? { ...m, pending: false } : m)),
      { id: crypto.randomUUID(), role: "user", content },
      { id: pid, role: "assistant", content: "", pending: true, agent: agentName(agentId), time: now, startedAt: Date.now() },
    ]);
    setSending(true);
  }
  const live = useLiveVoice({
    chatId: chatRef.current,
    agentId,
    projectId: matter?.id ?? null,
    onUserFinal: onVoiceFinal,
    pttDefault: who.data?.voice_live_opts?.ptt_default,
    silenceMs: who.data?.voice_live_opts?.silence_threshold_ms,
  });

  // A typed turn from <Composer>: build the optimistic bubbles + emit chat.send with
  // the per-turn attachments + reasoning effort.
  function onSubmit(content: string, extras: { attachments: ChatAttachmentMeta[]; reasoning: ReasoningSpec | null }) {
    if (!content || !agentId || sending) return;
    const pid = crypto.randomUUID();
    pendingId.current = pid;
    const now = new Date().toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
    setMessages((p) => [
      ...p.map((m) => (m.role === "assistant" && m.pending ? { ...m, pending: false } : m)),
      { id: crypto.randomUUID(), role: "user", content, attachments: extras.attachments.length ? extras.attachments : undefined },
      { id: pid, role: "assistant", content: "", pending: true, agent: agentName(agentId), time: now, startedAt: Date.now() },
    ]);
    setSending(true);
    wsStore.send({
      type: "chat.send",
      content,
      agent_id: agentId,
      chat_id: chatRef.current,
      project_id: chatRef.current ? null : (matter?.id ?? null),
      attachment_ids: extras.attachments.map((a) => a.id),
      reasoning: extras.reasoning,
    });
  }
  function cancel() {
    if (turnId.current) wsStore.send({ type: "chat.cancel", turn_id: turnId.current });
  }

  const allCites = messages.flatMap((m) => m.citations ?? []);
  const cur = allCites[sel] ?? allCites[0] ?? null;
  const empty = messages.length === 0;

  return (
    <div className="legal-grid">
      <div className="legal-main">
        <div className="legal-head">
          <div>
            <div className="eyebrow">Legal assistant</div>
            <h2 className="serif legal-matter">{matter?.name ?? "Legal assistant"}</h2>
          </div>
          <AgentPicker
            agents={visibleAgents}
            value={agentId}
            defaultId={defaultAgentId}
            onChange={setAgentId}
            onSetDefault={pinDefaultAgent}
            onNew={() => nav("/studio/agents")}
            canCreate={["client_admin", "super_admin"].includes(who.data?.role ?? "")}
          />
        </div>

        <div className="legal-thread">
          {empty && (
            <div className="empty" style={{ padding: "16px 0" }}>
              <p className="empty-sub">{matter ? <>Ask about <b>{matter.name}</b> — answers cite the matter's filed documents.</> : "Ask a legal question. Open a matter for document-grounded answers."}</p>
            </div>
          )}
          {messages.map((m) => {
            if (m.role === "user") {
              return (
                <div key={m.id} className="msg user fade-up">
                  <div className="bubble user">{m.content}</div>
                  {m.attachments && m.attachments.length > 0 && (
                    <div className="msg-attachments">
                      {m.attachments.map((a) => (
                        <span key={a.id} className="skill-chip"><Icon.Doc size={13} /> {a.filename}</span>
                      ))}
                    </div>
                  )}
                </div>
              );
            }
            const { reasoning, answer } = splitThink(m.content);
            const live = !!m.pending && !answer && !m.error;
            const copyText = answer || reasoning || m.content;
            return (
              <div key={m.id} className="msg ai fade-up">
                <div className="ai-avatar legal"><Icon.Scale size={14} /></div>
                <div className="ai-body">
                  <div className="ai-name mono">{m.agent ?? agentName(agentId)}{m.time && <span className="ai-time"> · {m.time}</span>}</div>
                  {m.error ? (
                    <div className="ai-text" style={{ color: "var(--red)" }}>{m.content}</div>
                  ) : (
                    <>
                      {reasoning && reasoning.trim() && (
                        <Reasoning reasoning={reasoning} startedAt={m.startedAt} live={live} />
                      )}
                      {answer && (
                        <MessageMarkdown
                          answer={answer}
                          pending={m.pending}
                          className="ai-text [&_p]:my-1 [&_ul]:my-1 [&_ul]:list-disc [&_ul]:pl-5"
                        />
                      )}
                    </>
                  )}
                  {!m.pending && !m.error && (
                    <div className="msg-actions">
                      <button onClick={() => navigator.clipboard?.writeText(copyText)} title="Copy"><Icon.Copy size={15} /></button>
                      <span className="msg-actions-sep" />
                      <button className={"fb" + (fb.feedback[m.id] === "up" ? " on" : "")} onClick={() => fb.rate(m.id, "up")} title="Good response"><Icon.Like size={15} /></button>
                      <button className={"fb" + (fb.feedback[m.id] === "down" ? " on down" : "")} onClick={() => fb.rate(m.id, "down")} title="Needs work"><Icon.Dislike size={15} /></button>
                    </div>
                  )}
                  {m.citations && m.citations.length > 0 && (
                    <div className="cites">
                      {m.citations.map((c, i) => {
                        const idx = allCites.indexOf(c);
                        return (
                          <button key={i} className="cite-chip" onClick={() => setSel(idx < 0 ? 0 : idx)} title={c.quote_text}>
                            {c.risk && <span className="src-dot" style={{ background: c.risk === "amber" ? "var(--warn)" : "var(--ok)" }} />}
                            <Icon.Quote size={12} />
                            <span style={{ maxWidth: "26ch", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{c.clause_section_ref ?? c.quote_text}</span>
                            {c.page_number != null && <span className="ai-time">p.{c.page_number}</span>}
                          </button>
                        );
                      })}
                    </div>
                  )}
                  {artByMsg.get(m.id)?.length ? (
                    <div className="msg-artefacts">
                      {artByMsg.get(m.id)!.map((a) => (
                        <button
                          key={a.id}
                          className="artefact-chip"
                          title={`Download ${a.title}`}
                          onClick={() => downloadArtefact(a.id, a.title, a.kind).catch((e) => toast((e as Error).message))}
                        >
                          <Icon.Doc size={14} />
                          <span className="artefact-name">{a.title}</span>
                          <span className="artefact-kind mono">{a.kind}</span>
                          <Icon.Download size={14} />
                        </button>
                      ))}
                    </div>
                  ) : null}
                </div>
              </div>
            );
          })}
          {approval && (
            <div className="msg ai fade-up">
              <div className="ai-avatar legal"><Icon.Shield size={14} /></div>
              <div className="ai-body">
                <div className="approval-card">
                  <div className="approval-head"><Icon.Shield size={13} /> Approval needed</div>
                  <div className="approval-summary">{approval.summary}</div>
                  <div className="approval-actions">
                    <button className="btn btn-gold sm" onClick={() => decideApproval(true)}><Icon.Check size={14} /> Approve</button>
                    <button className="btn btn-line sm" onClick={() => decideApproval(false)}><Icon.Close size={14} /> Reject</button>
                  </div>
                </div>
              </div>
            </div>
          )}
          <div ref={bottom} />
        </div>

        <Composer
          ref={composerRef}
          setAgentId={setAgentId}
          chatId={chatId ?? null}
          ready={!!agentId}
          sending={sending}
          placeholder={agentId ? (matter ? `Ask about ${matter.name}…` : "Ask a legal question…") : "No agent available"}
          agentName={agentName(agentId)}
          live={live}
          messages={messages}
          onSubmit={onSubmit}
          onCancel={cancel}
        />
      </div>

      {/* Source rail */}
      <div className="legal-side">
        <div className="legal-side-head">
          <span className="side-label mono">Cited sources</span>
        </div>
        <div className="src-list">
          {allCites.length === 0 && (docs.data ?? []).length === 0 && <div className="side-empty">No sources yet.</div>}
          {allCites.length === 0 &&
            (docs.data ?? []).map((d) => (
              <div key={d.id} className="src-card">
                <span className="src-ic"><Icon.Doc size={16} /></span>
                <div className="src-info">
                  <span className="src-name">{d.original_filename}</span>
                  <span className="src-meta mono">filed document</span>
                </div>
              </div>
            ))}
          {allCites.map((c, i) => (
            <button key={i} className={"src-card" + (i === sel ? " on" : "")} onClick={() => setSel(i)}>
              <span className="src-ic"><Icon.Quote size={16} /></span>
              <div className="src-info">
                <span className="src-name">{c.clause_section_ref ?? "Cited passage"}</span>
                <span className="src-meta mono">{c.page_number != null ? `p.${c.page_number}` : "source"}</span>
              </div>
              {c.risk && <span className="src-dot" style={{ background: c.risk === "amber" ? "var(--warn)" : "var(--ok)" }} />}
            </button>
          ))}
        </div>

        {cur && (
          <div className="doc-preview">
            <div className="doc-prev-head mono">
              <span>{cur.clause_section_ref ?? "Clause"}</span>
              {cur.page_number != null && <span>p.{cur.page_number}</span>}
            </div>
            <div className="doc-paper">
              {cur.clause_section_ref && <div className="doc-clause-no">{cur.clause_section_ref}</div>}
              <p className="doc-para hl">{cur.quote_text}</p>
            </div>
            <div className="doc-flags">
              {cur.risk === "amber" ? (
                <div className="flag warn"><span className="flag-dot" /> Flagged clause — review carefully.</div>
              ) : cur.risk === "ok" ? (
                <div className="flag ok"><span className="flag-dot" /> No issues detected.</div>
              ) : null}
            </div>
            <button onClick={() => setCitation(cur)} className="mt-3 text-xs text-gold hover:text-gold-light">Open full source →</button>
          </div>
        )}
      </div>

      {citation && <CitationPanel citation={citation} onClose={() => setCitation(null)} />}
      {fb.modal}
    </div>
  );
}

function LegalTabular({ matter }: { matter: ProjectSummary }) {
  const nav = useNavigate();
  const qc = useQueryClient();
  const reviews = useReviews(matter.id);
  const [creating, setCreating] = useState(false);
  const list = reviews.data ?? [];
  return (
    <div className="main-scroll">
      <div className="proj-panel">
        <div className="proj-panel-head">
          <div>
            <div className="eyebrow">Tabular review</div>
            <h2 className="serif" style={{ fontSize: 26 }}>{matter.name}</h2>
          </div>
          <button className="btn btn-gold sm" title="Reviews run over Workspace documents (Document Viewer tab)" onClick={() => setCreating(true)}>
            <Icon.Plus size={14} /> New review
          </button>
        </div>
        {list.length === 0 ? (
          <div className="side-empty">No reviews yet for this matter.</div>
        ) : (
          <div className="review-grid">
            {list.map((r) => (
              <button key={r.id} className="review-card" onClick={() => nav(`/p/${matter.id}/t/${r.id}`)}>
                <div className="review-card-top">
                  <span className="docs-ic"><Icon.Grid size={17} /></span>
                  <span className={"badge " + (r.status === "complete" || r.status === "done" ? "complete" : r.status === "running" ? "running" : "draft")}>{r.status}</span>
                </div>
                <div className="serif review-name">{r.name}</div>
                <div className="review-meta mono">Open review →</div>
              </button>
            ))}
          </div>
        )}
      </div>
      {creating && (
        <CreateReviewModal
          projectId={matter.id}
          onClose={() => setCreating(false)}
          onCreated={(id) => { setCreating(false); qc.invalidateQueries({ queryKey: ["reviews", matter.id] }); nav(`/p/${matter.id}/t/${id}`); }}
        />
      )}
    </div>
  );
}

function LegalViewer({ matter }: { matter: ProjectSummary }) {
  const nav = useNavigate();
  const qc = useQueryClient();
  const docs = useWorkspaceDocs(matter.id);
  const fileInput = useRef<HTMLInputElement | null>(null);
  const [busy, setBusy] = useState(false);
  const list = docs.data ?? [];

  async function onFiles(files: FileList | null) {
    if (!files?.length) return;
    setBusy(true);
    try {
      for (const f of Array.from(files)) await uploadWorkspaceDoc(matter.id, f);
      await qc.invalidateQueries({ queryKey: ["workspace-docs", matter.id] });
    } catch (e) { toast(`Upload failed: ${(e as Error).message}`); }
    finally { setBusy(false); if (fileInput.current) fileInput.current.value = ""; }
  }

  return (
    <div className="main-scroll">
      <input ref={fileInput} type="file" multiple hidden onChange={(e) => onFiles(e.target.files)} />
      <div className="proj-panel">
        <div className="proj-panel-head">
          <div>
            <div className="eyebrow">Document viewer</div>
            <h2 className="serif" style={{ fontSize: 26 }}>{matter.name}</h2>
          </div>
          <button className="btn btn-gold sm" disabled={busy} onClick={() => fileInput.current?.click()}>
            <Icon.Plus size={14} /> {busy ? "Uploading…" : "Upload"}
          </button>
        </div>
        {list.length === 0 ? (
          <div className="side-empty">No workspace documents for this matter.</div>
        ) : (
          <div className="docs-list flush">
            {list.map((d) => (
              <div key={d.id} className="docs-row" style={{ cursor: "pointer" }} onClick={() => nav(`/p/${matter.id}/d/${d.id}`)}>
                <span className="docs-ic"><Icon.Doc size={17} /></span>
                <div className="docs-main">
                  <span className="docs-name">{d.original_filename}</span>
                  <span className="docs-meta mono">{(d.mime ?? "document").split("/").pop()}</span>
                </div>
                <button className="icon-btn"><Icon.ChevronR size={16} /></button>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function LegalDocs({ matter }: { matter: ProjectSummary }) {
  const qc = useQueryClient();
  const docs = useProjectDocs(matter.id);
  const fileInput = useRef<HTMLInputElement | null>(null);
  const [busy, setBusy] = useState(false);
  const list = docs.data?.documents ?? [];

  async function onFiles(files: FileList | null) {
    if (!files?.length) return;
    setBusy(true);
    try {
      if (!docs.data?.knowledge) await createKnowledge(matter.id);
      for (const f of Array.from(files)) await uploadDocument(matter.id, f);
      await qc.invalidateQueries({ queryKey: ["project-docs", matter.id] });
    } catch (e) { toast(`Add knowledge failed: ${(e as Error).message}`); }
    finally { setBusy(false); if (fileInput.current) fileInput.current.value = ""; }
  }

  return (
    <div className="main-scroll">
      <input ref={fileInput} type="file" multiple hidden onChange={(e) => onFiles(e.target.files)} />
      <div className="docs-view">
        <div className="tab-head">
          <div>
            <div className="eyebrow">Project knowledge</div>
            <h2 className="serif tab-title">Documents &amp; knowledge</h2>
          </div>
          <div className="tab-actions">
            <button className="btn btn-gold" disabled={busy} onClick={() => fileInput.current?.click()}>
              <Icon.Plus size={15} /> {busy ? "Indexing…" : "Add knowledge"}
            </button>
          </div>
        </div>
        <div className="docs-list">
          {list.length === 0 && <div className="side-empty">No indexed documents yet.</div>}
          {list.map((d) => (
            <div key={d.id} className="docs-row">
              <span className="docs-ic"><Icon.Doc size={17} /></span>
              <div className="docs-main">
                <span className="docs-name">{d.filename}</span>
                <span className="docs-meta mono">{(d.mime ?? "document").split("/").pop()}</span>
              </div>
              <span className={"index-badge " + (d.status === "ready" ? "ready" : "indexing")}>
                {d.status === "ready" ? <Icon.Check2 size={13} /> : <span className="cs-spin" />}
                {d.status === "ready" ? "ready" : "indexing"}
              </span>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
