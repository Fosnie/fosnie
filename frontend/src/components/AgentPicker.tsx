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

// Shared agent picker + selection hook — used by the General chat and the Legal
// assistant. A pinned default (localStorage) survives reloads; unpinning resets
// to the system "General Assistant" agent.

import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion } from "motion/react";
import type { Workmode } from "@/app/WorkmodeContext";
import { popVariants, spring } from "@/app/motion";
import { Icon } from "@/components/icons";

type AgentLite = { id: string; name: string; description: string | null; sector?: string | null; modes?: string[] };

const DEFAULT_KEY = "pai.defaultAgentId";

/** Agents available in a given workmode: strict membership — an agent shows iff
 *  its `modes` includes the active workmode. Call this at each chat surface and
 *  pass the result to BOTH useAgentSelection and <AgentPicker>. */
export function agentsForMode(agents: AgentLite[], mode: Workmode): AgentLite[] {
  return agents.filter((a) => a.modes?.includes(mode));
}

/** Agent selection + pinned-default behaviour, shared across chat surfaces. Pass
 *  the already-mode-filtered list. A pinned default (localStorage) wins when it is
 *  visible in the current mode; otherwise the system default is the first agent in
 *  the (mode-filtered) list. Switching mode re-selects when the current pick drops
 *  out of the new mode's list. */
export function useAgentSelection(agents: AgentLite[] | undefined) {
  const [agentId, setAgentId] = useState<string | null>(null);
  const [defaultAgentId, setDefaultAgentId] = useState<string | null>(() => localStorage.getItem(DEFAULT_KEY));
  const systemDefaultId = useMemo(() => {
    if (!agents || agents.length === 0) return null;
    return agents[0].id;
  }, [agents]);
  function pinDefaultAgent(id: string) {
    if (id === defaultAgentId) {
      // Toggle off → unpin and reset to the system default agent.
      localStorage.removeItem(DEFAULT_KEY);
      setDefaultAgentId(null);
      if (systemDefaultId) setAgentId(systemDefaultId);
    } else {
      localStorage.setItem(DEFAULT_KEY, id);
      setDefaultAgentId(id);
    }
  }
  useEffect(() => {
    if (!agents || agents.length === 0) return;
    // Keep the current pick only while it is still visible in this mode.
    if (agentId && agents.some((a) => a.id === agentId)) return;
    const saved = defaultAgentId && agents.find((a) => a.id === defaultAgentId)?.id;
    setAgentId(saved || systemDefaultId || agents[0].id);
  }, [agents, agentId, defaultAgentId, systemDefaultId]);

  return { agentId, setAgentId, defaultAgentId, pinDefaultAgent };
}

export function AgentPicker({
  agents, value, defaultId, onChange, onSetDefault, onNew, canCreate = false,
}: {
  agents: AgentLite[];
  value: string | null;
  defaultId: string | null;
  onChange: (id: string) => void;
  onSetDefault: (id: string) => void;
  onNew: () => void;
  /** Show the inline "Create new agent" shortcut — admins only; others create in Studio. */
  canCreate?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [rect, setRect] = useState<DOMRect | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const sel = agents.find((a) => a.id === value);

  // Portalled to <body> so the menu escapes the topbar's backdrop-filter root and
  // can actually frost the thread behind it. Position from the
  // trigger rect; outside-click must check BOTH the trigger and the portalled menu.
  function toggle() {
    setOpen((o) => {
      const next = !o;
      if (next && triggerRef.current) setRect(triggerRef.current.getBoundingClientRect());
      return next;
    });
  }
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      const t = e.target as Node;
      if (triggerRef.current?.contains(t) || menuRef.current?.contains(t)) return;
      setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") setOpen(false); };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => { document.removeEventListener("mousedown", onDoc); document.removeEventListener("keydown", onKey); };
  }, [open]);

  return (
    <div className="agent-pick">
      <button ref={triggerRef} className={"agent-trigger" + (open ? " open" : "")} onClick={toggle}>
        {sel ? (
          <>
            <span className="agent-glyph"><Icon.Agents size={13} /></span>
            <span className="agent-trig-name">{sel.name}</span>
          </>
        ) : (
          <span className="agent-trig-name dim">Select an agent</span>
        )}
        <Icon.Chevron size={15} />
      </button>
      {createPortal(
        <AnimatePresence>
        {open && rect && (
          <motion.div
            ref={menuRef}
            className="agent-menu glass glass--menu"
            style={{ position: "fixed", top: rect.bottom + 8, left: rect.left }}
            variants={popVariants} initial="initial" animate="animate" exit="exit" transition={spring}
          >
          <div className="agent-menu-head mono">Agents</div>
          <div className="agent-menu-scroll thin-scroll">
          {agents.map((a) => (
            <div
              key={a.id}
              role="button"
              tabIndex={0}
              className={"agent-row" + (a.id === value ? " on" : "")}
              onClick={() => { onChange(a.id); setOpen(false); }}
              onKeyDown={(e) => { if (e.key === "Enter") { onChange(a.id); setOpen(false); } }}
            >
              <span className="agent-glyph"><Icon.Agents size={13} /></span>
              <span className="agent-info">
                <span className="agent-name">{a.name}</span>
                {a.description && <span className="agent-desc">{a.description}</span>}
              </span>
              <button
                type="button"
                className={"agent-default-btn" + (a.id === defaultId ? " on" : "")}
                title={a.id === defaultId ? "Unpin default (reset to this mode's default agent)" : "Pin as default agent"}
                onClick={(e) => { e.stopPropagation(); onSetDefault(a.id); }}
              >
                <Icon.Pin size={14} />
              </button>
              {a.id === value && <Icon.Check size={16} />}
            </div>
          ))}
          {canCreate && <>
            <div className="divider" style={{ margin: "6px 0" }} />
            <button className="agent-new" onClick={() => { setOpen(false); onNew(); }}><Icon.Plus size={15} /> Create new agent</button>
          </>}
          </div>
        </motion.div>
        )}
        </AnimatePresence>,
        document.body,
      )}
    </div>
  );
}
