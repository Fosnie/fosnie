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

// Citation chip + ~200ms hover-preview card. The card
// showcases the platform's provenance — the exact quote, the source, and the
// page / clause / pinned-version anchor — which is stronger than any consumer
// product's. Hover preview is desktop-only (hover + fine pointer); touch and a
// plain click still open the full CitationPanel side rail (onOpen).

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Icon } from "@/components/icons";
import type { Citation } from "@/ws/protocol";

const CARD_W = 340;
const canHover = () =>
  typeof window !== "undefined" &&
  window.matchMedia?.("(hover: hover) and (pointer: fine)").matches;

export function CiteChip({ c, onOpen }: { c: Citation; onOpen: () => void }) {
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const ref = useRef<HTMLButtonElement | null>(null);

  const show = () => {
    if (!canHover()) return;
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      const r = ref.current?.getBoundingClientRect();
      if (r) setPos({ x: Math.min(r.left, window.innerWidth - CARD_W - 12), y: r.bottom + 6 });
    }, 200);
  };
  const hide = () => { window.clearTimeout(timer.current); setPos(null); };
  // Clear a pending hover-intent timer if the chip unmounts mid-wait.
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const label = c.url ? c.title ?? c.domain ?? c.url : c.clause_section_ref ?? c.quote_text;
  const sourceName = c.url ? c.title ?? c.domain ?? "Web source" : "Cited source";
  const pin = c.version_id
    ? "pinned version"
    : c.page_number != null
      ? `page ${c.page_number}`
      : c.clause_section_ref ?? c.domain ?? null;

  return (
    <>
      <button
        ref={ref}
        className="cite-chip"
        onClick={onOpen}
        onMouseEnter={show}
        onMouseLeave={hide}
        onFocus={show}
        onBlur={hide}
      >
        <Icon.Quote size={12} />
        <span style={{ maxWidth: "26ch", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {label}
        </span>
        {c.url && c.domain && <span className="ai-time">{c.domain}</span>}
        {!c.url && c.page_number != null && <span className="ai-time">p.{c.page_number}</span>}
      </button>
      {pos && createPortal(
        <div className="cite-hover glass glass--hud" style={{ left: pos.x, top: pos.y, width: CARD_W }} role="tooltip">
          <div className="cite-hover-quote">“{c.quote_text}”</div>
          <div className="cite-hover-meta">
            <Icon.Source size={12} />
            <span className="cite-hover-src">{sourceName}</span>
            {pin && <span className="cite-hover-pin">{pin}</span>}
          </div>
        </div>,
        document.body,
      )}
    </>
  );
}
