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

import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { motion } from "motion/react";
import { popVariants, spring } from "@/app/motion";

// A small, dependency-free emoji popover (zero-egress: no remote emoji data).
// Rendered in a portal with fixed positioning anchored to the trigger button, so
// it never gets clipped by a scrolling message thread. Click an emoji → onPick;
// outside-click / Escape → onClose.
const GROUPS: { label: string; emojis: string[] }[] = [
  {
    label: "Reactions",
    emojis: ["👍", "👎", "❤️", "🔥", "🎉", "👏", "💯", "✅", "❌", "🙏", "👀", "🚀", "😂", "😍", "🤔", "🙌"],
  },
  {
    label: "Smileys",
    emojis: [
      "😀", "😄", "😁", "😅", "🙂", "😉", "😊", "😘", "😎", "🤨", "😐", "😴",
      "😢", "😭", "😡", "🥳", "🤯", "😱", "🤝", "🤷", "😏", "😇", "🤗", "😬",
    ],
  },
  {
    label: "Gestures",
    emojis: ["👋", "🤙", "✌️", "🤞", "👌", "💪", "👇", "👆", "👉", "👈", "🫶", "🤟", "🖐️", "🙇", "💁", "🤦"],
  },
  {
    label: "Objects",
    emojis: ["⭐", "🌟", "💡", "📌", "📎", "📝", "📊", "📅", "🕐", "✏️", "🔒", "⚠️", "❓", "❗", "💬", "📣"],
  },
];

const W = 300;
const H = 300;

export function EmojiPicker({ anchor, onPick, onClose }: { anchor: DOMRect; onPick: (e: string) => void; onClose: () => void }) {
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    // Defer so the opening click doesn't immediately close it.
    const t = setTimeout(() => document.addEventListener("mousedown", onDoc), 0);
    document.addEventListener("keydown", onKey);
    return () => { clearTimeout(t); document.removeEventListener("mousedown", onDoc); document.removeEventListener("keydown", onKey); };
  }, [onClose]);

  const left = Math.min(Math.max(8, anchor.left), window.innerWidth - W - 8);
  const openUp = anchor.top > H + 16; // enough room above the trigger
  const style: React.CSSProperties = openUp
    ? { left, bottom: window.innerHeight - anchor.top + 8, width: W }
    : { left, top: anchor.bottom + 8, width: W };

  return createPortal(
    <motion.div className="emoji-pop glass glass--menu" ref={ref} style={style}
      variants={popVariants} initial="initial" animate="animate" transition={spring}>
      {GROUPS.map((g) => (
        <div key={g.label} className="emoji-group">
          <div className="menu-label mono">{g.label}</div>
          <div className="emoji-grid">
            {g.emojis.map((e) => (
              <button key={e} className="emoji-btn" onClick={() => onPick(e)} title={e}>{e}</button>
            ))}
          </div>
        </div>
      ))}
    </motion.div>,
    document.body,
  );
}
