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

// Admin announcement banners: a persistent top-right corner stack shown to all
// users in every section. Mirrors the toast card styling but
// never auto-dismisses. A dismissible banner the user closes stays closed in
// that browser (localStorage by id) — matching Open WebUI's behaviour. Content
// is admin-authored markdown, rendered escaped via MessageMarkdown (no raw HTML).

import { useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { useNotices, type Severity } from "@/api/client";
import { MessageMarkdown, MD } from "@/components/MessageMarkdown";
import { Icon } from "@/components/icons";
import { spring, toastVariants } from "@/app/motion";

const DISMISS_KEY = "pai.dismissed-banners";

function readDismissed(): string[] {
  try {
    const raw = localStorage.getItem(DISMISS_KEY);
    return raw ? (JSON.parse(raw) as string[]) : [];
  } catch {
    return [];
  }
}

const glyph = (s: Severity) =>
  s === "error" || s === "warning" ? <Icon.Alert size={16} /> : s === "success" ? <Icon.Check size={16} /> : <Icon.Bell size={16} />;

export function AnnouncementBanners() {
  const notices = useNotices();
  const [dismissed, setDismissed] = useState<string[]>(readDismissed);

  const banners = (notices.data?.banners ?? []).filter((b) => !(b.dismissible && dismissed.includes(b.id)));
  if (banners.length === 0) return null;

  const dismiss = (id: string) => {
    const next = [...new Set([...dismissed, id])];
    setDismissed(next);
    try {
      localStorage.setItem(DISMISS_KEY, JSON.stringify(next));
    } catch {
      /* private-mode / quota: dismissal just won't persist across reloads */
    }
  };

  return (
    <div className="banner-stack">
      <AnimatePresence>
        {banners.map((b) => (
          <motion.div
            key={b.id}
            className={"toast glass glass--hud toast-" + b.severity}
            role="status"
            layout
            variants={toastVariants}
            initial="initial"
            animate="animate"
            exit="exit"
            transition={spring}
          >
            <span className="toast-ic">{glyph(b.severity)}</span>
            <MessageMarkdown answer={b.content} className={"banner-text " + MD} />
            {b.dismissible && (
              <button className="toast-x" onClick={() => dismiss(b.id)} aria-label="Dismiss">
                <Icon.Close size={13} />
              </button>
            )}
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
}
