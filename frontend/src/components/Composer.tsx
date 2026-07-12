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

// The shared chat composer: textarea + the five input affordances (slash-prompt
// palette, per-turn file attach, dictation, live voice, reasoning effort). Used by
// BOTH the general chat (`Chat.tsx`) and the Legal workspace (`LegalShell.tsx`) so
// the two never drift. The parent owns the message list + the `chat.send` WS frame;
// this component gathers the per-turn extras (attachment ids + reasoning spec) and
// hands them back via `onSubmit`. Chat-specific bits (project/library grounding,
// the Deep Research status pill) are injected through the `groundingSlot` /
// `statusSlot` props rather than baked in.

import { forwardRef, useEffect, useImperativeHandle, useMemo, useRef, useState, type ReactNode } from "react";

import { toast } from "@/components/dialogs";
import { Icon } from "@/components/icons";
import { Popover } from "@/components/Popover";
import { PromptPicker } from "@/components/PromptPicker";
import { LiveVoiceOverlay } from "@/components/LiveVoiceOverlay";
import { useDictation } from "@/voice/useVoice";
import type { LiveVoice } from "@/voice/useLiveVoice";
import { ACCEPT_ATTR, MAX_CHAT_ATTACHMENT_LABEL, splitBySize } from "@/lib/files";
import {
  getPrompt,
  renderPrompt,
  setChatLlmProvider,
  uploadChatAttachment,
  useMyLlmProviders,
  usePrompts,
  useWhoami,
  type ChatAttachmentMeta,
  type PromptSummary,
} from "@/api/client";
import type { ReasoningSpec } from "@/ws/protocol";

type ThinkLevel = "off" | "on" | "low" | "medium" | "high" | "xhigh";
const THINK_LEVELS: ThinkLevel[] = ["off", "on", "low", "medium", "high", "xhigh"];

type ComposerMsg = { id: string; role: "user" | "assistant"; content: string; pending?: boolean };

type Attach = { key: string; id?: string; filename: string; size: number; mime: string; status: "processing" | "ready" | "error" };

/** Imperative handle so the parent's drag-drop zone can stage files into the
 *  composer, and so voice/route changes can refocus the box. */
export interface ComposerHandle {
  addFiles: (files: FileList | File[] | null) => void;
  focus: () => void;
}

export interface ComposerProps {
  setAgentId: (id: string) => void;
  /** null on a brand-new (unsaved) chat — reasoning-store key. */
  chatId: string | null;
  ready: boolean;
  sending: boolean;
  placeholder: string;
  agentName: string;
  /** The parent's live-voice session (its `onUserFinal` builds the bubbles); this
   *  component only renders the button + overlay from it. */
  live: LiveVoice;
  /** For the LiveVoiceOverlay conversation strip. */
  messages: ComposerMsg[];
  /** A typed turn — the parent builds the optimistic bubbles + sends the WS frame. */
  onSubmit: (content: string, extras: { attachments: ChatAttachmentMeta[]; reasoning: ReasoningSpec | null; llmProviderId: string | null }) => void;
  /** Stop the in-flight generation. */
  onCancel: () => void;
  /** Extra items inside the "+" popover (Chat: project/library pickers). `close` shuts the menu. */
  groundingSlot?: (close: () => void) => ReactNode;
  /** A pill rendered above the bar (Chat: Deep Research status). */
  statusSlot?: ReactNode;
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

export const Composer = forwardRef<ComposerHandle, ComposerProps>(function Composer({
  setAgentId, chatId, ready, sending, placeholder, agentName,
  live, messages, onSubmit, onCancel, groundingSlot, statusSlot,
}, ref) {
  const who = useWhoami();
  const prompts = usePrompts();

  const [input, setInput] = useState("");
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  function grow(el: HTMLTextAreaElement) {
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 200) + "px";
  }
  useImperativeHandle(ref, () => ({ addFiles: onAttachFiles, focus: () => taRef.current?.focus() }));

  // --- Attachments (per-turn files) ---
  const [attachments, setAttachments] = useState<Attach[]>([]);
  const attachmentsBusy = attachments.some((a) => a.status === "processing");
  const [attachMenu, setAttachMenu] = useState(false);
  const attachFileInput = useRef<HTMLInputElement | null>(null);
  const attachBtnRef = useRef<HTMLButtonElement | null>(null);

  function onAttachFiles(files: FileList | File[] | null) {
    if (!files?.length) return;
    setAttachMenu(false);
    const { ok: withinSize, tooBig } = splitBySize(Array.from(files));
    for (const name of tooBig) toast(`Attach failed: ${name}: file too large (max ${MAX_CHAT_ATTACHMENT_LABEL})`);
    if (!withinSize.length) {
      if (attachFileInput.current) attachFileInput.current.value = "";
      return;
    }
    const picked = withinSize.map((f) => ({ key: crypto.randomUUID(), file: f }));
    setAttachments((p) => [
      ...p,
      ...picked.map(({ key, file }) => ({ key, filename: file.name, size: file.size, mime: file.type, status: "processing" as const })),
    ]);
    for (const { key, file } of picked) {
      uploadChatAttachment(file)
        .then(({ id }) => setAttachments((p) => p.map((a) => (a.key === key ? { ...a, id, status: "ready" } : a))))
        .catch((e) => {
          toast(`Attach failed: ${file.name}: ${(e as Error).message}`);
          setAttachments((p) => p.map((a) => (a.key === key ? { ...a, status: "error" } : a)));
        });
    }
    if (attachFileInput.current) attachFileInput.current.value = "";
  }

  // --- Dictation (speech-to-text into the box) ---
  const voiceOn = !!who.data?.capabilities.voice;
  const inputRef = useRef(input);
  inputRef.current = input;
  const dictation = useDictation({
    streaming: who.data?.capabilities.dictation_streaming,
    onText: (t) => {
      setInput((p) => (p ? p.trimEnd() + " " : "") + t);
      const ta = taRef.current;
      if (ta) grow(ta);
    },
    setComposer: (text) => {
      setInput(text);
      requestAnimationFrame(() => { const ta = taRef.current; if (ta) grow(ta); });
    },
    getComposer: () => inputRef.current,
  });
  function dictate() {
    if (dictation.status === "idle") void dictation.start();
    else void dictation.stop();
  }

  // --- Live voice (real-time call mode) — the session is owned by the parent. ---
  const liveVoiceOn = !!who.data?.capabilities.voice_live;

  // --- LLM provider picker (multi-LLM) ---
  // The visible list = enabled deployment providers + the user's own; each carries
  // its reasoning capability so the Tune control below re-derives per pick (no whoami
  // refetch). Selection is remembered per chat (server pointer) + localStorage.
  const llmList = useMyLlmProviders(chatId);
  const providers = useMemo(() => llmList.data?.providers ?? [], [llmList.data]);
  const [llmSel, setLlmSel] = useState<string | null>(null);
  const [llmMenu, setLlmMenu] = useState(false);
  const llmBtnRef = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    // localStorage pick (when still visible) wins for snappy UI; else the server's
    // active provider for this chat (its stored pointer, or the default).
    const stored = localStorage.getItem(`pai.llm_provider:${chatId ?? "draft"}`);
    const validStored = stored && providers.some((p) => p.id === stored) ? stored : null;
    setLlmSel(validStored ?? llmList.data?.active_id ?? null);
  }, [chatId, llmList.data, providers]);
  const selProvider = providers.find((p) => p.id === llmSel) ?? providers.find((p) => p.is_active) ?? null;
  function pickProvider(id: string) {
    setLlmSel(id);
    localStorage.setItem(`pai.llm_provider:${chatId ?? "draft"}`, id);
    setLlmMenu(false);
    // Persist to an existing chat straight away; a draft chat persists on first send
    // via the chat.send frame's llm_provider_id.
    if (chatId) void setChatLlmProvider(chatId, id).catch(() => {});
  }

  // --- Reasoning effort ---
  // Derive from the SELECTED provider's capability (multi-LLM); fall back to whoami's
  // default-provider descriptor for a draft chat / single-provider deploy.
  const rcap = selProvider?.reasoning ?? who.data?.capabilities.reasoning;
  const reasonMode = rcap?.mode ?? "toggle";
  const reasonChips: ThinkLevel[] =
    reasonMode === "toggle" ? ["off", "on"] : ((rcap?.levels ?? ["low", "medium", "high"]) as ThinkLevel[]);
  const canDisableReason = rcap?.can_disable ?? true;
  const [thinking, setThinking] = useState<ThinkLevel>("off");
  const [showTrace, setShowTrace] = useState(true);
  const [thinkMenu, setThinkMenu] = useState(false);
  const thinkBtnRef = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    const v = (localStorage.getItem(`pai.thinking:${chatId ?? "draft"}`) ??
      localStorage.getItem("pai.thinking:draft")) as ThinkLevel | null;
    setThinking(v && THINK_LEVELS.includes(v) ? v : "off");
  }, [chatId]);
  function pickThinking(v: ThinkLevel) {
    setThinking(v);
    localStorage.setItem(`pai.thinking:${chatId ?? "draft"}`, v);
    localStorage.setItem("pai.thinking:draft", v);
    setThinkMenu(false);
  }
  function reasoningSpec(): ReasoningSpec | null {
    if (reasonMode === "none") return null;
    if (thinking === "off" && canDisableReason) return { enabled: false, level: null, return_trace: showTrace };
    const level = thinking === "on" || thinking === "off" ? "auto" : thinking;
    return { enabled: true, level, return_trace: showTrace };
  }

  // --- Slash prompt palette ---
  const [showPrompts, setShowPrompts] = useState(false);
  const [promptInitialId, setPromptInitialId] = useState<string | undefined>(undefined);
  const [slashIdx, setSlashIdx] = useState(0);
  const slashMatch = /^\/(\w*)$/.exec(input);
  const slashActive = ready && !sending && !!slashMatch;
  const slashQuery = slashMatch?.[1]?.toLowerCase() ?? "";
  const slashItems = useMemo(
    () => (slashActive ? (prompts.data ?? []).filter((p) => p.name.toLowerCase().includes(slashQuery)).slice(0, 8) : []),
    [slashActive, prompts.data, slashQuery],
  );
  useEffect(() => { setSlashIdx(0); }, [slashQuery, slashActive]);
  async function pickSlashPrompt(p: PromptSummary) {
    try {
      const d = await getPrompt(p.id);
      if (d.agent_id) setAgentId(d.agent_id);
      if (d.placeholders.length === 0) {
        const { content } = await renderPrompt(p.id, {});
        setInput(content);
        requestAnimationFrame(() => { const t = taRef.current; if (t) { grow(t); t.focus(); } });
      } else {
        setInput("");
        setPromptInitialId(p.id);
        setShowPrompts(true);
      }
    } catch (e) {
      toast(`Prompt failed: ${(e as Error).message}`);
    }
  }

  // --- Submit ---
  function submit() {
    const content = input.trim();
    if (!content || !ready || sending || attachmentsBusy) return;
    const sent: ChatAttachmentMeta[] = attachments
      .filter((a) => a.status === "ready" && a.id)
      .map((a) => ({ id: a.id!, filename: a.filename, mime: a.mime, byte_size: a.size }));
    onSubmit(content, { attachments: sent, reasoning: reasoningSpec(), llmProviderId: llmSel });
    setInput("");
    if (taRef.current) taRef.current.style.height = "auto";
    setAttachments([]);
  }

  return (
    <>
      <input ref={attachFileInput} type="file" accept={ACCEPT_ATTR} multiple hidden onChange={(e) => onAttachFiles(e.target.files)} />
      <div className="composer-dock">
        {attachments.length > 0 && (
          <div className="composer-wrap" style={{ paddingBottom: 0 }}>
            <div className="composer glass glass--bar" style={{ flexWrap: "wrap", padding: "10px 14px" }}>
              {attachments.map((a) => (
                <span key={a.key} className="skill-chip">
                  {a.status === "processing" ? <span className="think-dots"><span /><span /><span /></span> : <Icon.Doc size={13} />}
                  <span style={{ maxWidth: "14rem", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{a.filename}</span>
                  <span className="mono" style={{ color: a.status === "error" ? "var(--red)" : "var(--ink-3)", fontSize: "0.7rem" }}>
                    {a.status === "error" ? "failed" : a.status === "processing" ? `${fmtBytes(a.size)} · processing…` : fmtBytes(a.size)}
                  </span>
                  <button onClick={() => setAttachments((p) => p.filter((x) => x.key !== a.key))} title="Remove" style={{ background: "none", border: 0, color: "var(--ink-3)", cursor: "pointer", display: "grid", placeItems: "center" }}><Icon.Close size={12} /></button>
                </span>
              ))}
            </div>
          </div>
        )}
        {statusSlot}
        <div className="composer-wrap">
          <div className="composer glass glass--bar glass-noise" style={{ position: "relative" }}>
            {slashActive && slashItems.length > 0 && (
              <div className="menu fade-up glass glass--menu" style={{ position: "absolute", top: "auto", left: 0, right: "auto", bottom: "calc(100% + 8px)", width: "100%", maxHeight: 300, overflowY: "auto", zIndex: 60 }}>
                <div className="menu-label mono">Prompts · ↑↓ Enter · Esc</div>
                {slashItems.map((p, i) => (
                  <button key={p.id} className="menu-item" style={i === slashIdx ? { background: "var(--bg-3)" } : undefined} onMouseEnter={() => setSlashIdx(i)} onClick={() => void pickSlashPrompt(p)}>
                    <Icon.Quote size={14} /> /{p.name}
                    <span className="mono" style={{ marginLeft: "auto", opacity: 0.55, fontSize: 10 }}>{p.scope}</span>
                  </button>
                ))}
              </div>
            )}
            <div className="menu-wrap">
              <button ref={attachBtnRef} className="comp-attach" title="Attach" onClick={() => setAttachMenu((v) => !v)}><Icon.Plus size={18} /></button>
              <Popover anchorRef={attachBtnRef} open={attachMenu} onClose={() => setAttachMenu(false)} placement="top-start" offset={8} className="menu glass glass--menu" role="menu">
                <div style={{ minWidth: 250, maxHeight: 360, overflowY: "auto" }}>
                  <button className="menu-item" onClick={() => attachFileInput.current?.click()}><Icon.Attach size={15} /> Attach file (this turn)</button>
                  {groundingSlot?.(() => setAttachMenu(false))}
                </div>
              </Popover>
            </div>
            <textarea
              ref={taRef}
              className="comp-in"
              rows={1}
              value={input}
              placeholder={placeholder}
              disabled={!ready}
              onChange={(e) => { setInput(e.target.value); grow(e.target); }}
              onKeyDown={(e) => {
                if (slashActive && slashItems.length) {
                  if (e.key === "ArrowDown") { e.preventDefault(); setSlashIdx((i) => (i + 1) % slashItems.length); return; }
                  if (e.key === "ArrowUp") { e.preventDefault(); setSlashIdx((i) => (i - 1 + slashItems.length) % slashItems.length); return; }
                  if (e.key === "Enter") { e.preventDefault(); void pickSlashPrompt(slashItems[slashIdx]); return; }
                  if (e.key === "Escape") { e.preventDefault(); setInput(""); return; }
                }
                if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); submit(); }
              }}
            />
            {providers.length > 1 && (
              <div className="menu-wrap">
                <button ref={llmBtnRef} className="comp-attach comp-model" title="LLM provider" onClick={() => setLlmMenu((v) => !v)} style={{ width: "auto", padding: "0 10px", gap: 5 }}>
                  <span className="mono" style={{ fontSize: 12, maxWidth: 120, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {selProvider?.label ?? selProvider?.model ?? "Model"}
                  </span>
                  <span aria-hidden style={{ fontSize: 10, opacity: 0.6 }}>▾</span>
                </button>
                <Popover anchorRef={llmBtnRef} open={llmMenu} onClose={() => setLlmMenu(false)} placement="top-end" offset={8} className="menu glass glass--menu" role="menu">
                  <div style={{ minWidth: 220, maxHeight: 320, overflowY: "auto" }}>
                    <div className="menu-label mono">Model</div>
                    {providers.map((p) => (
                      <button key={p.id} className="menu-item" onClick={() => pickProvider(p.id)}>
                        {(selProvider?.id ?? null) === p.id ? <Icon.Check size={14} /> : <span style={{ width: 14 }} />}
                        <span style={{ display: "flex", flexDirection: "column", alignItems: "flex-start", minWidth: 0 }}>
                          <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: 180 }}>
                            {p.label ?? p.model ?? "Provider"}{p.source === "user" ? " · yours" : ""}
                          </span>
                          {p.model && <span className="mono" style={{ fontSize: 10, opacity: 0.55, maxWidth: 180, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{p.model}</span>}
                        </span>
                      </button>
                    ))}
                  </div>
                </Popover>
              </div>
            )}
            {reasonMode !== "none" && (
              <div className="menu-wrap">
                <button ref={thinkBtnRef} className={"comp-attach comp-think" + (thinking !== "off" ? " on" : "")} title={thinking === "off" ? "Reasoning" : `Reasoning: ${thinking}`} onClick={() => setThinkMenu((v) => !v)}>
                  <Icon.Tune size={20} />
                </button>
                <Popover anchorRef={thinkBtnRef} open={thinkMenu} onClose={() => setThinkMenu(false)} placement="top-end" offset={8} className="menu glass glass--menu" role="menu">
                  <div style={{ minWidth: 180 }}>
                    <div className="menu-label mono">
                      {reasonMode === "always_on" ? "Reasoning effort (always on)" : reasonMode === "toggle" ? "Reasoning" : "Reasoning effort"}
                    </div>
                    {canDisableReason && reasonMode !== "always_on" && (
                      <button className="menu-item" onClick={() => pickThinking("off")}>
                        {thinking === "off" ? <Icon.Check size={14} /> : <span style={{ width: 14 }} />}
                        Off
                      </button>
                    )}
                    {reasonChips.filter((lv) => lv !== "off").map((lv) => (
                      <button key={lv} className="menu-item" onClick={() => pickThinking(lv)}>
                        {thinking === lv ? <Icon.Check size={14} /> : <span style={{ width: 14 }} />}
                        {lv === "on" ? "On" : lv === "xhigh" ? "X-High" : lv[0].toUpperCase() + lv.slice(1)}
                      </button>
                    ))}
                    {rcap?.supports_trace && (
                      <>
                        <div style={{ height: 1, background: "var(--line)", margin: "4px 0" }} />
                        <button className="menu-item" onClick={() => setShowTrace((v) => !v)}>
                          {showTrace ? <Icon.Check size={14} /> : <span style={{ width: 14 }} />}
                          Show reasoning
                        </button>
                      </>
                    )}
                  </div>
                </Popover>
              </div>
            )}
            {voiceOn && (
              <button className={"comp-attach comp-mic" + (dictation.status !== "idle" ? " recording" : "")} onClick={dictate} title={dictation.status === "listening" ? "Stop dictation" : dictation.status === "transcribing" ? "Transcribing…" : "Dictate"}>
                <Icon.Mic size={18} />
              </button>
            )}
            {/* Primary action, mutually exclusive: Stop while generating; Send once
                there's text; otherwise the live-voice button (empty box) — so the
                user picks either a typed prompt or a live call, like other
                assistants. The dictation mic above stays (it fills the box). */}
            {sending ? (
              <button className="comp-send stop" title="Stop generating" onClick={onCancel}><Icon.Stop size={15} /></button>
            ) : input.trim() ? (
              <button className="comp-send ready" onClick={submit} disabled={!ready || attachmentsBusy}>
                <Icon.Send size={17} />
              </button>
            ) : liveVoiceOn ? (
              <button className={"comp-send comp-send-voice" + (live.active ? " recording" : "")} onClick={() => (live.active ? live.end() : live.start())} title={live.active ? "End live voice" : "Live voice (real-time)"} disabled={!ready}>
                <Icon.LiveVoice size={19} />
              </button>
            ) : (
              <button className="comp-send" onClick={submit} disabled>
                <Icon.Send size={17} />
              </button>
            )}
          </div>
          <div className="composer-hint mono">
            {attachmentsBusy
              ? <span className="dictation-status"><span className="dot busy" /> Processing attachment…</span>
              : dictation.error
              ? <span style={{ color: "var(--red)" }}>{dictation.error} <button onClick={dictation.clearError} className="underline">dismiss</button></span>
              : dictation.status === "listening"
                ? <span className="dictation-status"><span className="dot" /> Listening… <button onClick={dictate} className="underline">stop</button></span>
                : dictation.status === "transcribing"
                  ? <span className="dictation-status"><span className="dot busy" /> Transcribing…</span>
                  : "This is AI and it can make mistakes"}
          </div>
        </div>
      </div>

      {liveVoiceOn && live.active && (
        <LiveVoiceOverlay
          live={live}
          messages={messages}
          agentName={agentName}
          onSwitchToText={() => { live.end(); requestAnimationFrame(() => taRef.current?.focus()); }}
        />
      )}
      {showPrompts && (
        <PromptPicker
          initialId={promptInitialId}
          onInsert={(text) => setInput((p) => (p ? p + "\n\n" : "") + text)}
          onAgent={(id) => setAgentId(id)}
          onClose={() => { setShowPrompts(false); setPromptInitialId(undefined); }}
        />
      )}
    </>
  );
});
