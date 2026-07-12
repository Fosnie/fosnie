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

// Reusable popover: portals its content to <body> (so it escapes any ancestor
// backdrop-filter / transform / opacity "backdrop root" and can actually frost the
// content behind it), positions from an anchor element's rect,
// repositions on scroll/resize, and closes on outside-click or Escape. Animates via
// the shared popVariants (reduced-motion handled by the global <MotionConfig>).

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion } from "motion/react";
import { popVariants, spring } from "@/app/motion";

type Placement = "bottom-start" | "bottom-end" | "top-start" | "top-end";

export function Popover({
  anchorRef,
  open,
  onClose,
  placement = "bottom-start",
  matchWidth = false,
  offset = 6,
  className = "menu glass glass--menu",
  role = "menu",
  children,
}: {
  anchorRef: React.RefObject<HTMLElement | null>;
  open: boolean;
  onClose: () => void;
  placement?: Placement;
  matchWidth?: boolean;
  offset?: number;
  className?: string;
  role?: string;
  children: React.ReactNode;
}) {
  const menuRef = useRef<HTMLDivElement | null>(null);
  const [style, setStyle] = useState<React.CSSProperties | null>(null);

  function place() {
    const a = anchorRef.current?.getBoundingClientRect();
    if (!a) return;
    // Reset ALL four offsets — the surface's own CSS (.menu sets top/right) would
    // otherwise fight the ones we set and stretch the panel off-screen.
    const s: React.CSSProperties = { position: "fixed", top: "auto", bottom: "auto", left: "auto", right: "auto" };
    const openUp = placement === "top-start" || placement === "top-end";
    // Clamp the surface to the space available on the side it opens, and let it
    // scroll — otherwise a tall menu (many items + a footer) from a low/high anchor
    // spills past the viewport edge and its footer becomes unclickable.
    const margin = 8;
    if (openUp) {
      s.bottom = window.innerHeight - a.top + offset;
      s.maxHeight = a.top - offset - margin;
    } else {
      s.top = a.bottom + offset;
      s.maxHeight = window.innerHeight - a.bottom - offset - margin;
    }
    s.overflowY = "auto";
    if (placement === "bottom-end" || placement === "top-end") s.right = window.innerWidth - a.right;
    else s.left = a.left;
    if (matchWidth) s.minWidth = a.width;
    setStyle(s);
  }

  // Position before paint so the menu never flashes at the wrong spot.
  useLayoutEffect(() => { if (open) place(); /* eslint-disable-next-line */ }, [open]);

  useEffect(() => {
    if (!open) return;
    const reposition = () => place();
    const onDoc = (e: MouseEvent) => {
      const t = e.target as Node;
      if (anchorRef.current?.contains(t) || menuRef.current?.contains(t)) return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("scroll", reposition, true);
    window.addEventListener("resize", reposition);
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("scroll", reposition, true);
      window.removeEventListener("resize", reposition);
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  return createPortal(
    <AnimatePresence>
      {open && style && (
        <motion.div
          ref={menuRef}
          className={className}
          role={role}
          style={style}
          variants={popVariants}
          initial="initial"
          animate="animate"
          exit="exit"
          transition={spring}
        >
          {children}
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
