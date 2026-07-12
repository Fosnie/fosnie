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

// Login welcome message: an admin-set singleton shown once per
// new browser session as a modal. sessionStorage is the gate — it clears when the
// tab session ends, so a fresh login shows it again, but a reload within the same
// session does not. Body is admin-authored markdown, rendered escaped.

import { useEffect, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import { useNotices } from "@/api/client";
import { MessageMarkdown, MD } from "@/components/MessageMarkdown";
import { Icon } from "@/components/icons";
import { popVariants, scrimVariants, spring } from "@/app/motion";

const SEEN_KEY = "pai.welcome-seen";

export function WelcomeGate() {
  const w = useNotices().data?.welcome ?? null;
  const [open, setOpen] = useState(false);

  // Open once per session when a welcome exists and hasn't been seen yet.
  useEffect(() => {
    if (!w) return;
    if (sessionStorage.getItem(SEEN_KEY)) return;
    setOpen(true);
  }, [w]);

  const close = () => {
    try {
      sessionStorage.setItem(SEEN_KEY, "1");
    } catch {
      /* ignore: it just re-shows if storage is unavailable */
    }
    setOpen(false);
  };

  // Esc to dismiss, mirroring the dialog hosts.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        close();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open]);

  return (
    <AnimatePresence>
      {open && w && (
        <motion.div
          className="modal-scrim"
          onClick={close}
          variants={scrimVariants}
          initial="initial"
          animate="animate"
          exit="exit"
        >
          <motion.div
            className="modal dialog-modal glass glass--modal glass-noise"
            onClick={(e) => e.stopPropagation()}
            role="dialog"
            aria-modal="true"
            aria-label={w.title}
            variants={popVariants}
            initial="initial"
            animate="animate"
            exit="exit"
            transition={spring}
          >
            <div className="modal-head">
              <div>
                <div className="eyebrow">Welcome</div>
                <h2 className="serif modal-title">{w.title}</h2>
              </div>
              <button className="icon-btn" onClick={close} aria-label="Close">
                <Icon.Close size={18} />
              </button>
            </div>
            <div className="modal-body">
              <MessageMarkdown answer={w.body} className={"dialog-body " + MD} />
            </div>
            <div className="modal-foot">
              <button className="btn btn-gold" onClick={close} autoFocus>
                Got it
              </button>
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
