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

// ⌘K command palette. A Raycast-style glass overlay over the
// existing actions: new chat / research, mode switch, go-to screens, plus live
// fuzzy search of the user's chats, research runs, projects and agents. Mounted
// once in the Shell; opens on Cmd/Ctrl+K. Fully keyboard-driven. Role-gated like
// the sidebar. Honours reduced motion via the global <MotionConfig>.

import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion } from "motion/react";
import { useNavigate } from "react-router-dom";
import { useAgents, useChats, useProjects, useResearchChats, useWhoami } from "@/api/client";
import { useActiveProject } from "@/app/ProjectContext";
import { useWorkmode } from "@/app/WorkmodeContext";
import { popVariants, scrimVariants, spring } from "@/app/motion";
import { Icon } from "@/components/icons";

type IconKey = keyof typeof Icon;
interface Cmd {
  id: string;
  label: string;
  hint?: string;
  group: string;
  icon: IconKey;
  run: () => void;
}

/** Substring match across label + hint + group, ranked by match position. */
function fuzzy(cmds: Cmd[], q: string): Cmd[] {
  const s = q.trim().toLowerCase();
  if (!s) return cmds;
  return cmds
    .map((c) => ({ c, i: `${c.label} ${c.hint ?? ""} ${c.group}`.toLowerCase().indexOf(s) }))
    .filter((x) => x.i >= 0)
    .sort((a, b) => a.i - b.i)
    .map((x) => x.c);
}

export function CommandPalette() {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [active, setActive] = useState(0);
  const nav = useNavigate();
  const { setMode } = useWorkmode();
  const { setActive: setActiveProject } = useActiveProject();
  const who = useWhoami();
  const chats = useChats();
  const research = useResearchChats();
  const projects = useProjects();
  const agents = useAgents();

  // Global ⌘K / Ctrl-K toggles; Esc closes.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === "k" || e.key === "K")) {
        e.preventDefault();
        setOpen((o) => !o);
      } else if (e.key === "Escape" && open) {
        setOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open]);

  useEffect(() => { if (!open) { setQ(""); setActive(0); } }, [open]);
  useEffect(() => { setActive(0); }, [q]);

  const role = who.data?.role ?? "";
  const isAdmin = role === "client_admin" || role === "super_admin";
  const isPower = isAdmin || role === "power_user";
  const isMod = !!who.data?.is_moderator;

  const close = () => setOpen(false);
  const go = (to: string, opts?: { state?: unknown }) => { close(); nav(to, opts as never); };
  const switchTo = (m: "general" | "legal" | "research") => { close(); setActiveProject(null); setMode(m); nav("/"); };

  const commands: Cmd[] = useMemo(() => {
    const c: Cmd[] = [
      { id: "new-chat", group: "Actions", label: "New chat", icon: "Plus", run: () => { setActiveProject(null); go("/", { state: { newChat: Date.now() } }); } },
      { id: "new-research", group: "Actions", label: "New research", icon: "Research", run: () => switchTo("research") },
      { id: "mode-general", group: "Switch mode", label: "General workspace", icon: "General", run: () => switchTo("general") },
      { id: "mode-legal", group: "Switch mode", label: "Legal workspace", icon: "Legal", run: () => switchTo("legal") },
      { id: "mode-research", group: "Switch mode", label: "Deep Research", icon: "Research", run: () => switchTo("research") },
      { id: "go-agents", group: "Go to", label: "Studio · Agents", icon: "Agents", run: () => go("/studio/agents") },
      { id: "go-libraries", group: "Go to", label: "Studio · Libraries", icon: "Book", run: () => go("/studio/libraries") },
      { id: "go-automations", group: "Go to", label: "Studio · Automations", icon: "Automations", run: () => go("/studio/automations") },
      { id: "go-prompts", group: "Go to", label: "Studio · Prompts", icon: "Prompts", run: () => go("/studio/prompts") },
      { id: "go-memory", group: "Go to", label: "Studio · Memory", icon: "Memory", run: () => go("/studio/memory") },
      { id: "go-teams", group: "Go to", label: "Teams", icon: "Team", run: () => go("/teams") },
      { id: "go-dm", group: "Go to", label: "Direct messages", icon: "Chat", run: () => go("/dm") },
      { id: "go-profile", group: "Go to", label: "Profile & appearance", icon: "User", run: () => go("/profile") },
    ];
    if (isAdmin) c.push({ id: "go-admin", group: "Go to", label: "Admin", icon: "Admin", run: () => go("/admin") });
    if (isPower) c.push({ id: "go-power", group: "Go to", label: "Power tools", icon: "Sliders", run: () => go("/power") });
    if (isMod) c.push({ id: "go-moderation", group: "Go to", label: "Moderation", icon: "Flag", run: () => go("/moderation") });

    for (const ch of chats.data ?? []) c.push({ id: `chat-${ch.id}`, group: "Chats", label: ch.title || "Untitled chat", icon: "Chat", run: () => go(`/c/${ch.id}`) });
    for (const r of research.data ?? []) c.push({ id: `run-${r.id}`, group: "Research runs", label: r.title || "Research run", icon: "Research", run: () => go(`/c/${r.id}`) });
    for (const p of projects.data ?? []) c.push({ id: `proj-${p.id}`, group: "Projects", label: p.name, hint: p.sector, icon: "Folder", run: () => { setActiveProject({ id: p.id, name: p.name, sector: p.sector, description: p.description }); go(`/p/${p.id}`); } });
    for (const a of agents.data ?? []) c.push({ id: `agent-${a.id}`, group: "Agents", label: a.name, hint: a.description ?? undefined, icon: "Spark", run: () => go(`/studio/agents/${a.id}`) });
    return c;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chats.data, research.data, projects.data, agents.data, isAdmin, isPower, isMod]);

  const filtered = useMemo(() => fuzzy(commands, q), [commands, q]);
  const activeClamped = Math.min(active, Math.max(0, filtered.length - 1));

  // Keep the active row in view as the user arrows through.
  const listRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!open) return;
    listRef.current?.querySelector(`#cmdk-item-${activeClamped}`)?.scrollIntoView({ block: "nearest" });
  }, [activeClamped, open]);

  function onKey(e: React.KeyboardEvent) {
    if (e.key === "ArrowDown") { e.preventDefault(); setActive((a) => Math.min(a + 1, filtered.length - 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setActive((a) => Math.max(a - 1, 0)); }
    else if (e.key === "Enter") { e.preventDefault(); filtered[activeClamped]?.run(); }
    else if (e.key === "Escape") { e.preventDefault(); close(); }
  }

  return createPortal(
    <AnimatePresence>
      {open && (
        <motion.div
          className="modal-scrim cmdk-scrim"
          onClick={close}
          variants={scrimVariants}
          initial="initial"
          animate="animate"
          exit="exit"
        >
          <motion.div
            className="cmdk glass glass--modal glass-noise"
            onClick={(e) => e.stopPropagation()}
            variants={popVariants}
            initial="initial"
            animate="animate"
            exit="exit"
            transition={spring}
            role="dialog"
            aria-modal="true"
            aria-label="Command palette"
          >
            <div className="cmdk-input-row">
              <Icon.Filter size={15} />
              <input
                autoFocus
                className="cmdk-input"
                placeholder="Search commands, chats, projects, agents…"
                value={q}
                onChange={(e) => setQ(e.target.value)}
                onKeyDown={onKey}
                role="combobox"
                aria-expanded
                aria-controls="cmdk-list"
                aria-activedescendant={filtered.length ? `cmdk-item-${activeClamped}` : undefined}
              />
              <kbd className="cmdk-kbd">esc</kbd>
            </div>
            <div className="cmdk-list" id="cmdk-list" role="listbox" ref={listRef}>
              {filtered.length === 0 ? (
                <div className="cmdk-empty">No matches</div>
              ) : (
                filtered.map((c, i) => {
                  const I = Icon[c.icon];
                  const newGroup = i === 0 || filtered[i - 1].group !== c.group;
                  return (
                    <div key={c.id}>
                      {newGroup && <div className="cmdk-group mono">{c.group}</div>}
                      <button
                        id={`cmdk-item-${i}`}
                        role="option"
                        aria-selected={i === activeClamped}
                        className={"cmdk-item" + (i === activeClamped ? " on" : "")}
                        onMouseEnter={() => setActive(i)}
                        onClick={() => c.run()}
                      >
                        <I size={15} />
                        <span className="cmdk-label">{c.label}</span>
                        {c.hint && <span className="cmdk-hint">{c.hint}</span>}
                      </button>
                    </div>
                  );
                })
              )}
            </div>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
}
