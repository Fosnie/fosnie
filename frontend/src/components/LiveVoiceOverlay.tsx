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

// Call-mode overlay for live / streaming voice. A
// dedicated, focused voice surface over the Chat screen: a mic orb + conversation
// state, an input-level meter, the muted live partial, a read-only transcript strip
// (the spoken answer stays readable/verifiable), and the controls — push-to-talk
// (the professional default), a hands-free toggle, and hang up. The full, canonical
// transcript lives in the Chat message list behind; hanging up returns to it.

import { useEffect } from "react";
import { Icon } from "@/components/icons";
import { MessageMarkdown, MD } from "@/components/MessageMarkdown";
import { useWsStatus } from "@/ws/store";
import type { LiveVoice, VoiceState } from "@/voice/useLiveVoice";

interface OverlayMsg {
  id: string;
  role: "user" | "assistant";
  content: string;
  pending?: boolean;
}

interface Props {
  live: LiveVoice;
  messages: OverlayMsg[];
  agentName?: string;
  /** Hang up and drop to the text composer (focuses it). Falls back to `live.end`. */
  onSwitchToText?: () => void;
}

const STATE_META: Record<VoiceState, { label: string; color: string }> = {
  idle: { label: "Idle", color: "var(--gold)" },
  connecting: { label: "Connecting…", color: "var(--gold)" },
  listening: { label: "Listening", color: "var(--green)" },
  capturing: { label: "Listening", color: "var(--green)" },
  thinking: { label: "Thinking…", color: "var(--gold)" },
  speaking: { label: "Speaking", color: "var(--gold-bright)" },
  interrupted: { label: "Interrupted", color: "var(--amber)" },
  error: { label: "Voice error", color: "var(--red)" },
};

export function LiveVoiceOverlay({ live, messages, agentName, onSwitchToText }: Props) {
  const wsStatus = useWsStatus();
  const meta = STATE_META[live.state] ?? STATE_META.idle;
  const micOn = live.talking || live.mode === "vad";
  // Orb pulses with the input level while we're listening to the user.
  const listening = live.state === "listening" || live.state === "capturing";
  const scale = listening ? 1 + Math.min(0.6, live.level * 4) : live.state === "speaking" ? 1.08 : 1;

  // Space-bar hold = push-to-talk (no inputs in the overlay, so it's unambiguous).
  useEffect(() => {
    if (live.mode !== "ptt") return;
    const down = (e: KeyboardEvent) => {
      if (e.code === "Space" && !e.repeat) {
        e.preventDefault();
        live.pressTalk();
      }
    };
    const up = (e: KeyboardEvent) => {
      if (e.code === "Space") {
        e.preventDefault();
        live.releaseTalk();
      }
    };
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
    };
  }, [live]);

  const recent = messages.slice(-6);

  return (
    <div
      className="fixed inset-0 z-50 flex flex-col items-center justify-between py-10 px-4 backdrop-blur-md"
      style={{ background: "color-mix(in srgb, var(--navy) 88%, transparent)" }}
    >
      {/* Header: mic-on indicator + close */}
      <div className="flex w-full max-w-2xl items-center justify-between">
        <span
          className="flex items-center gap-2 rounded-full px-3 py-1 text-xs font-medium mono"
          style={{ background: "var(--navy-lighter)", color: micOn ? "var(--green)" : "var(--ink)" }}
        >
          <span
            className="inline-block h-2 w-2 rounded-full"
            style={{ background: micOn ? "var(--green)" : "var(--ink)", boxShadow: micOn ? "0 0 8px var(--green)" : "none" }}
          />
          {micOn ? "Mic on" : "Mic off"}
        </span>
        <button
          className="flex items-center gap-1.5 rounded-full px-3 py-1.5 text-sm"
          style={{ background: "var(--navy-lighter)", color: "var(--ink-cream)" }}
          onClick={live.end}
          title="Hang up"
        >
          <Icon.Logout size={16} /> Hang up
        </button>
      </div>

      {/* Banners */}
      <div className="flex w-full max-w-2xl flex-col gap-2">
        {wsStatus !== "open" && (
          <div className="rounded-md px-3 py-2 text-sm" style={{ background: "var(--navy-lighter)", color: "var(--amber)" }}>
            Reconnecting… your transcript is saved — you can switch to text.
          </div>
        )}
        {live.error && (
          <div className="flex items-center justify-between rounded-md px-3 py-2 text-sm" style={{ background: "var(--navy-lighter)", color: "var(--red)" }}>
            <span className="flex items-center gap-2"><Icon.Alert size={15} /> {live.error}</span>
            <button className="underline" onClick={live.clearError}>dismiss</button>
          </div>
        )}
      </div>

      {/* Orb + state + live partial */}
      <div className="flex flex-col items-center gap-5">
        <div
          className="flex items-center justify-center rounded-full transition-transform duration-100"
          style={{
            width: 160,
            height: 160,
            transform: `scale(${scale})`,
            background: "radial-gradient(circle at 50% 40%, color-mix(in srgb, " + meta.color + " 28%, var(--navy-light)), var(--navy-light))",
            boxShadow: `0 0 48px color-mix(in srgb, ${meta.color} 45%, transparent)`,
            border: `1px solid color-mix(in srgb, ${meta.color} 60%, transparent)`,
          }}
        >
          <Icon.Mic size={48} style={{ color: meta.color }} />
        </div>
        <div className="flex items-center gap-2 text-lg font-medium" style={{ color: meta.color }}>
          {meta.label}
          {/* A search of your Libraries is already running for what you are saying,
              so the answer is ready sooner. Deliberately quiet: it is background
              work, not something to act on. */}
          {live.retrieving && (
            <span className="animate-pulse text-xs font-normal" style={{ color: "var(--ink)" }}>
              Searching your library...
            </span>
          )}
        </div>
        <div className="min-h-[1.5rem] max-w-xl text-center text-sm" style={{ color: "var(--ink)" }}>
          {live.partial || (live.state === "speaking" ? agentName ?? "Assistant" : "")}
        </div>
      </div>

      {/* Transcript strip (read-only; the canonical transcript is in the chat behind) */}
      <div
        className="w-full max-w-2xl flex-1 overflow-y-auto rounded-lg p-3 text-sm"
        style={{ background: "color-mix(in srgb, var(--navy-light) 60%, transparent)", maxHeight: "30vh" }}
      >
        {recent.length === 0 ? (
          <p className="text-center" style={{ color: "var(--ink)" }}>Hold to talk, then ask your question.</p>
        ) : (
          recent.map((m) => (
            <div key={m.id} className="mb-2">
              <span className="mono text-xs uppercase tracking-wide" style={{ color: m.role === "user" ? "var(--gold)" : "var(--green)" }}>
                {m.role === "user" ? "You" : agentName ?? "Assistant"}
              </span>
              {m.role === "assistant" ? (
                // Markdown so bold/lists render like the canonical chat (not raw `**`).
                <MessageMarkdown answer={m.content} pending={m.pending} className={"ai-text " + MD} />
              ) : (
                <p className="whitespace-pre-wrap" style={{ color: "var(--ink-cream)" }}>
                  {m.content || (m.pending ? "…" : "")}
                </p>
              )}
            </div>
          ))
        )}
      </div>

      {/* Controls */}
      <div className="flex w-full max-w-2xl flex-col items-center gap-3">
        {live.mode === "ptt" ? (
          <button
            className="select-none rounded-full px-10 py-4 text-base font-semibold transition-transform active:scale-95"
            style={{
              background: live.talking ? "var(--gold)" : "var(--gold-dark)",
              color: "var(--gold-ink)",
              boxShadow: live.talking ? "0 0 24px color-mix(in srgb, var(--gold) 55%, transparent)" : "none",
            }}
            onPointerDown={(e) => { e.preventDefault(); live.pressTalk(); }}
            onPointerUp={(e) => { e.preventDefault(); live.releaseTalk(); }}
            onPointerLeave={() => { if (live.talking) live.releaseTalk(); }}
            onPointerCancel={() => { if (live.talking) live.releaseTalk(); }}
          >
            <span className="flex items-center gap-2"><Icon.Mic size={18} /> {live.talking ? "Listening…" : "Hold to talk"}</span>
          </button>
        ) : (
          <button
            className="select-none rounded-full px-10 py-4 text-base font-semibold"
            style={{ background: "var(--gold-dark)", color: "var(--gold-ink)" }}
            onClick={live.state === "speaking" ? live.bargeIn : undefined}
          >
            <span className="flex items-center gap-2">
              <Icon.Mic size={18} /> {live.state === "speaking" ? "Tap to interrupt" : "Hands-free — speak freely"}
            </span>
          </button>
        )}

        <div className="flex items-center gap-4 text-sm" style={{ color: "var(--ink)" }}>
          <button
            className="underline"
            onClick={() => live.setMode(live.mode === "ptt" ? "vad" : "ptt")}
            title="Toggle push-to-talk / hands-free"
          >
            {live.mode === "ptt" ? "Switch to hands-free" : "Switch to push-to-talk"}
          </button>
          <span aria-hidden>·</span>
          <button className="underline" onClick={onSwitchToText ?? live.end}>Switch to text</button>
        </div>
      </div>
    </div>
  );
}
