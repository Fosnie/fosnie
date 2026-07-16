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

// Annotated-text marker for the Mode-B inline-on-document highlight. Given
// the document's extracted text and a verification run's flagged claims, it wraps
// each claim's located verbatim span (`source_text`) in a <mark>, colour-coded by
// verdict and carrying the verdict + evidence as a tooltip — reusing the same
// groundedness colours as the chat-answer highlight. Plain-string matching on the
// located substring; no offsets, no markdown dependency. Claims that could not be
// located (null source_text) are simply not marked.

import type { ReactNode } from "react";
import type { VerifyClaim } from "@/api/client";

interface Flag {
  text: string;
  cls: string;
  title: string;
}

/** Wrap each flagged claim's verbatim span in the text with a coloured <mark>. */
export function markFlaggedText(text: string, claims: VerifyClaim[]): ReactNode[] {
  // Longest first so a longer span wins over a substring of it.
  const flags: Flag[] = (claims ?? [])
    .filter((c) => c.verdict !== "supported" && !!c.source_text && c.source_text.trim().length >= 3)
    .map((c) => ({
      text: c.source_text!.trim(),
      cls: c.verdict === "contradicted" ? "claim-contradicted" : "claim-unsupported",
      title: `${c.verdict.replace(/_/g, " ")}${c.evidence ? " · " + c.evidence.slice(0, 160) : ""}`,
    }))
    .sort((a, b) => b.text.length - a.text.length);

  if (!flags.length) return [text];

  const out: ReactNode[] = [];
  const lower = text.toLowerCase();
  let i = 0;
  let key = 0;
  while (i < text.length) {
    // Earliest match at or after i; longest wins on a tie.
    let best: { idx: number; len: number; flag: Flag } | null = null;
    for (const f of flags) {
      const idx = lower.indexOf(f.text.toLowerCase(), i);
      if (idx >= 0 && (best === null || idx < best.idx || (idx === best.idx && f.text.length > best.len))) {
        best = { idx, len: f.text.length, flag: f };
      }
    }
    if (!best) {
      out.push(text.slice(i));
      break;
    }
    if (best.idx > i) out.push(text.slice(i, best.idx));
    out.push(
      <mark key={key++} className={best.flag.cls} title={best.flag.title}>
        {text.slice(best.idx, best.idx + best.len)}
      </mark>,
    );
    i = best.idx + best.len;
  }
  return out;
}
