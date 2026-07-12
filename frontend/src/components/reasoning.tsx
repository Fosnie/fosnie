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

// Shared reasoning-model helpers: split a `<think>…</think>` block from the answer
// and an expandable "Reasoning… Ns" panel (watchable live — so you can see whether
// a model has looped). Used by both the General chat and the Legal assistant.

import { useEffect, useState } from "react";

import { MessageMarkdown, MD } from "@/components/MessageMarkdown";

/**
 * Split a model's output into its `<think>` reasoning and the answer. Robust to
 * messy reasoners: handles MULTIPLE `<think>…</think>` blocks (all folded into
 * the reasoning), an unclosed trailing `<think>` (`thinking` = still reasoning),
 * and strips any orphan `<think>`/`</think>` tags a small model may emit without
 * a partner so they never leak into the answer.
 */
export function splitThink(content: string): { reasoning: string | null; answer: string; thinking: boolean } {
  if (!content.includes("<think>") && !content.includes("</think>")) {
    return { reasoning: null, answer: content, thinking: false };
  }
  let reasoning = "";
  let answer = "";
  let rest = content;
  let thinking = false;
  for (;;) {
    const open = rest.indexOf("<think>");
    if (open === -1) {
      answer += rest;
      break;
    }
    answer += rest.slice(0, open);
    const after = rest.slice(open + 7);
    const close = after.indexOf("</think>");
    if (close === -1) {
      // Open block with no close yet → the model is still reasoning.
      reasoning += after;
      thinking = true;
      break;
    }
    reasoning += (reasoning ? "\n" : "") + after.slice(0, close);
    rest = after.slice(close + 8);
  }
  // Drop orphan tags (e.g. a stray `</think>` with no opener) from the answer.
  answer = answer.replace(/<\/?think>/g, "").trim();
  reasoning = reasoning.trim();
  return { reasoning: reasoning || null, answer, thinking };
}

// Expandable reasoning panel. While `live`, the summary ticks "Reasoning… Ns" with
// the animated dots; open it to watch the chain-of-thought stream. When done it
// collapses to a plain "Reasoning" details holding the final reasoning.
export function Reasoning({ reasoning, startedAt, live, tokens }: { reasoning?: string | null; startedAt?: number; live?: boolean; tokens?: number }) {
  const [secs, setSecs] = useState(startedAt ? Math.max(0, Math.round((Date.now() - startedAt) / 1000)) : 0);
  useEffect(() => {
    if (!live || !startedAt) return;
    const t = setInterval(() => setSecs(Math.max(0, Math.round((Date.now() - startedAt) / 1000))), 1000);
    return () => clearInterval(t);
  }, [live, startedAt]);

  const text = reasoning?.trim();
  return (
    // Auto-open while live so the (summarised) reasoning is visible as it streams;
    // collapses to a plain disclosure when the turn completes (re-opens on click).
    <details className="reasoning mb-2" open={live}>
      <summary className="thinking" style={{ cursor: "pointer", listStyle: "none" }}>
        {live
          ? <span className="think-label"><WaveText text="Reasoning…" /> {secs}s</span>
          : <span className="mono text-xs text-slate">Reasoning{tokens ? ` · ${tokens.toLocaleString()} tokens` : ""}</span>}
      </summary>
      <div className="mt-1 max-h-72 overflow-y-auto border-l-2 border-line pl-2">
        {text
          ? <MessageMarkdown answer={text} className={"text-xs leading-relaxed text-slate/80 " + MD} />
          : <span className="mono text-xs leading-relaxed text-slate/80">…</span>}
      </div>
    </details>
  );
}

/** Pre-token waiting label: the turn is pending but no reasoning/answer text has
 * arrived yet (e.g. extended thinking runs server-side and streams nothing until the
 * summary lands). Ticks an elapsed counter so a long think doesn't look frozen. */
export function Thinking({ startedAt }: { startedAt?: number }) {
  const [secs, setSecs] = useState(startedAt ? Math.max(0, Math.round((Date.now() - startedAt) / 1000)) : 0);
  useEffect(() => {
    if (!startedAt) return;
    const t = setInterval(() => setSecs(Math.max(0, Math.round((Date.now() - startedAt) / 1000))), 1000);
    return () => clearInterval(t);
  }, [startedAt]);
  return (
    <div className="thinking mb-2">
      <span className="think-label"><WaveText text="Reasoning…" />{startedAt ? ` ${secs}s` : ""}</span>
    </div>
  );
}

/** Animate each letter rising in turn, left to right (the live "Reasoning" label). */
function WaveText({ text }: { text: string }) {
  return (
    <span className="think-wave" aria-label={text}>
      {Array.from(text).map((ch, i) => (
        <span key={i} aria-hidden style={{ animationDelay: `${i * 0.07}s` }}>
          {ch}
        </span>
      ))}
    </span>
  );
}
