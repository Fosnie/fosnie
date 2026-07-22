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

import { useMemo, useState } from "react";
import { flushSync } from "react-dom";
import { useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import { approveAgentRun, createProject, decideMemberRequest, deleteChat, rejectAgentRun, renameChat, useChats, useGroupChats, useBranding, usePendingApprovals, usePendingMemberRequests, useProjects, useResearchChats, useWhoami, type ChatSummary } from "@/api/client";
import { confirmDialog, promptDialog, toast } from "@/components/dialogs";
import { useActiveProject } from "@/app/ProjectContext";
import { useWorkmode } from "@/app/WorkmodeContext";
import { useAppearance } from "@/app/AppearanceContext";
import { Avatar } from "@/components/Avatar";
import { Icon } from "@/components/icons";
import { getNavItems } from "@/ext/registry";

function relTime(iso: string): string {
  const d = new Date(iso);
  const s = (Date.now() - d.getTime()) / 1000;
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  if (s < 604800) return `${Math.floor(s / 86400)}d`;
  return d.toLocaleDateString(undefined, { day: "numeric", month: "short" });
}

// Primary tabs — always visible. Studio opens the building-blocks page (Agents,
// Library, Automations, Prompts, Memory live as tabs inside /studio). Admin
// (role-gated) is rendered after these.
const PRIMARY_NAV = [
  { to: "/studio", label: "Studio", icon: Icon.Blocks },
  { to: "/teams", label: "Teams", icon: Icon.Team },
  { to: "/dm", label: "Direct messages", icon: Icon.Chat },
] as const;

// Human-friendly role labels (the raw Keycloak role names aren't for users).
const ROLE_LABEL: Record<string, string> = {
  super_admin: "Super admin",
  client_admin: "Admin",
  power_user: "Power user",
  user: "Member",
};
const roleLabel = (r?: string | null) => (r ? (ROLE_LABEL[r] ?? r) : "");

export function Sidebar() {
  const nav = useNavigate();
  const { chatId } = useParams();
  const qc = useQueryClient();
  const chats = useChats();
  const groupChats = useGroupChats();
  const projects = useProjects();
  const who = useWhoami();
  // Unread totals for the Teams / Direct-messages nav badges (#12).
  const teamsUnread = (groupChats.data ?? []).filter((c) => c.kind !== "dm").reduce((s, c) => s + (c.unread_count || 0), 0);
  const dmUnread = (groupChats.data ?? []).filter((c) => c.kind === "dm").reduce((s, c) => s + (c.unread_count || 0), 0);
  const canAdmin = ["client_admin", "super_admin"].includes(who.data?.role ?? "");
  // Teams + DMs are gated by the `messaging` presence capability (default on).
  const messagingOn = who.data?.capabilities.messaging !== false;
  // Durable approval inbox: the caller's agent runs awaiting approval (surfaced on
  // login, so an offline owner of an unattended run sees it).
  const branding = useBranding();
  const logoSrc = branding.data?.some((x) => x.kind === "logo")
    ? `/api/branding/logo?v=${branding.dataUpdatedAt}`
    : "/logo.svg";
  const pending = usePendingApprovals();
  const pendingCount = pending.data?.length ?? 0;
  const [inboxOpen, setInboxOpen] = useState(false);
  async function decideApproval(runId: string, ok: boolean) {
    try {
      if (ok) await approveAgentRun(runId);
      else await rejectAgentRun(runId);
    } catch (e) {
      toast((e as Error).message);
    } finally {
      qc.invalidateQueries({ queryKey: ["pending-approvals"] });
    }
  }
  // Matter-owner approval inbox: group-membership adds awaiting the caller's consent
  // (they own a matter the membership would expose).
  const memberReqs = usePendingMemberRequests(!!who.data?.capabilities.data_owner_approval);
  const memberReqCount = memberReqs.data?.length ?? 0;
  const [accessOpen, setAccessOpen] = useState(false);
  async function decideAccess(id: string, ok: boolean) {
    try {
      await decideMemberRequest(id, ok);
    } catch (e) {
      toast((e as Error).message);
    } finally {
      qc.invalidateQueries({ queryKey: ["pending-member-requests"] });
    }
  }
  const { active, setActive } = useActiveProject();
  const { mode, setMode } = useWorkmode();
  const look = useAppearance();
  const [q, setQ] = useState("");
  const ql = q.trim().toLowerCase();
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [showAllProjects, setShowAllProjects] = useState(false);
  const [showAllChats, setShowAllChats] = useState(false);
  const PROJ_CAP = 6;
  const CHAT_CAP = 8;

  async function saveRename(id: string) {
    const title = draft.trim();
    setEditingId(null);
    if (!title) return;
    try {
      await renameChat(id, title);
      await qc.invalidateQueries({ queryKey: ["chats"] });
    } catch (e) {
      toast(`Rename failed: ${(e as Error).message}`);
    }
  }
  async function removeChat(id: string) {
    if (!(await confirmDialog({ title: "Delete this chat?", body: "History is retained; the chat is hidden from the list.", danger: true, confirmLabel: "Delete" }))) return;
    try {
      await deleteChat(id);
      await qc.invalidateQueries({ queryKey: ["chats"] });
      if (chatId === id) nav("/");
    } catch (e) {
      toast(`Delete failed: ${(e as Error).message}`);
    }
  }


  // Research runs are a separate, mode-flagged list — never mixed into the
  // General/Legal chats (the default fetch excludes them).
  const researchChats = useResearchChats(mode === "research");

  // Flat chat list, newest first. Scoped to the workmode by each chat's own
  // authoritative `mode` (set at creation), NOT derived from its agent's sector —
  // agents are now multi-mode, so agent sector no longer determines a chat's bucket.
  const chatList = useMemo<ChatSummary[]>(() => {
    const base =
      mode === "research"
        ? (researchChats.data ?? [])
        : (chats.data ?? []).filter((c) => (c.mode ?? "general") === mode);
    return base
      .filter((c) => !ql || (c.title ?? "").toLowerCase().includes(ql))
      .slice()
      .sort((a, b) => b.created_at.localeCompare(a.created_at));
  }, [chats.data, researchChats.data, ql, mode]);

  const filteredProjects = useMemo(
    () => (projects.data ?? [])
      .filter((p) => (p.sector ?? "general") === mode)
      .filter((p) => !ql || p.name.toLowerCase().includes(ql)),
    [projects.data, ql, mode],
  );

  function switchMode(m: "general" | "legal" | "research") {
    if (m === mode) return;
    // Subtle cross-fade via the View Transitions API. flushSync makes the mode
    // state apply synchronously inside the transition callback so the API captures
    // the before/after frames. Falls back to an instant switch when unsupported or
    // when the user has chosen reduced motion.
    const apply = () => flushSync(() => { setMode(m); setActive(null); });
    const vt = (document as Document & { startViewTransition?: (cb: () => void) => void }).startViewTransition;
    if (look.motion !== "reduced" && typeof vt === "function") {
      vt.call(document, apply);
    } else {
      apply();
    }
    nav("/");
  }

  async function newProject() {
    const name = await promptDialog({ title: `New ${mode} project`, label: "Project name", placeholder: "e.g. Acme acquisition" });
    if (!name) return;
    const { id } = await createProject(name, mode);
    await qc.invalidateQueries({ queryKey: ["projects"] });
    setActive({ id, name, sector: mode, description: null });
  }

  return (
    <aside className="sidebar">
      <div className="side-brand">
        <img src={logoSrc} alt="Private AI" className="brand-logo" onError={(e) => (e.currentTarget.style.display = "none")} />
        <span className="brand-name serif">Fosnie</span>
      </div>

      <div className="side-pad">
        {/* Sliding-thumb workmode switch — icon-only at three modes */}
        <div className="mode-switch three glass glass--pill" role="tablist" aria-label="Workmode">
          <div className={"mode-thumb " + mode} />
          <button role="tab" title="General" aria-label="General" onClick={() => switchMode("general")} className={"mode-opt" + (mode === "general" ? " on" : "")}>
            <Icon.General size={15} />
          </button>
          <button role="tab" title="Legal" aria-label="Legal" onClick={() => switchMode("legal")} className={"mode-opt" + (mode === "legal" ? " on" : "")}>
            <Icon.Legal size={15} />
          </button>
          <button role="tab" title="Deep Research" aria-label="Deep Research" onClick={() => switchMode("research")} className={"mode-opt" + (mode === "research" ? " on" : "")}>
            <Icon.Research size={15} />
          </button>
        </div>
        <button onClick={() => { setActive(null); nav("/", { state: { newChat: Date.now() } }); }} className="btn btn-gold newchat">
          <Icon.Plus size={13} /> {mode === "research" ? "New research" : "New chat"}
        </button>
      </div>

      <nav className="side-nav">
        {PRIMARY_NAV.filter((n) => messagingOn || (n.to !== "/teams" && n.to !== "/dm")).map(({ to, label, icon: I }) => {
          const unread = to === "/teams" ? teamsUnread : to === "/dm" ? dmUnread : 0;
          return (
            <button key={to} onClick={() => nav(to)} className="nav-item">
              <span className="nav-ic"><I size={15} /></span>
              <span>{label}</span>
              {unread > 0 && <span className="nav-badge unread mono" style={{ marginLeft: "auto" }}>{unread}</span>}
            </button>
          );
        })}
        {canAdmin && (
          <button onClick={() => nav("/admin")} className="nav-item">
            <span className="nav-ic"><Icon.Admin size={15} /></span>
            <span>Admin</span>
          </button>
        )}
        {who.data?.role === "power_user" && (
          <button onClick={() => nav("/power")} className="nav-item">
            <span className="nav-ic"><Icon.Lightning size={15} /></span>
            <span>Power</span>
          </button>
        )}
        {/* Extension nav items — Enterprise-bound (e.g. Moderation) registered via
            @/ext/registrations; each gated by its own predicate. */}
        {getNavItems()
          .filter((n) => n.predicate(who.data))
          .map(({ to, label, icon: I }) => (
            <button key={to} onClick={() => nav(to)} className="nav-item">
              <span className="nav-ic"><I size={15} /></span>
              <span>{label}</span>
            </button>
          ))}
        {pendingCount > 0 && (
          <div className="approvals-wrap">
            <button onClick={() => setInboxOpen((v) => !v)} className={"nav-item" + (inboxOpen ? " on" : "")}>
              <span className="nav-ic"><Icon.Shield size={15} /></span>
              <span>Approvals</span>
              <span className="nav-badge mono" style={{ marginLeft: "auto" }}>{pendingCount}</span>
            </button>
            {inboxOpen && (
              <div className="approvals-pop glass glass--menu">
                <div className="approvals-head mono">Awaiting your approval</div>
                {(pending.data ?? []).map((p) => (
                  <div key={p.run_id} className="approvals-item">
                    <div className="approvals-info">
                      <span className="approvals-summary">{p.summary}</span>
                      <span className="approvals-ctx mono">{p.context}{p.tool ? ` · ${p.tool}` : ""}</span>
                    </div>
                    <div className="approvals-actions">
                      <button className="btn btn-gold xs" title="Approve" onClick={() => decideApproval(p.run_id, true)}><Icon.Check size={13} /></button>
                      <button className="btn btn-line xs" title="Reject" onClick={() => decideApproval(p.run_id, false)}><Icon.Close size={13} /></button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
        {who.data?.capabilities.data_owner_approval && memberReqCount > 0 && (
          <div className="approvals-wrap">
            <button onClick={() => setAccessOpen((v) => !v)} className={"nav-item" + (accessOpen ? " on" : "")}>
              <span className="nav-ic"><Icon.Key size={15} /></span>
              <span>Access requests</span>
              <span className="nav-badge unread mono" style={{ marginLeft: "auto" }}>{memberReqCount}</span>
            </button>
            {accessOpen && (
              <div className="approvals-pop glass glass--menu">
                <div className="approvals-head mono">Adds awaiting your approval</div>
                {(memberReqs.data ?? []).map((r) => (
                  <div key={r.id} className="approvals-item">
                    <div className="approvals-info">
                      <span className="approvals-summary">{r.target_name} → {r.group_name}</span>
                      <span className="approvals-ctx mono">{r.requester_name} · {r.projects.map((p) => p.name).join(", ")}</span>
                    </div>
                    <div className="approvals-actions">
                      <button className="btn btn-gold xs" title="Approve" onClick={() => decideAccess(r.id, true)}><Icon.Check size={13} /></button>
                      <button className="btn btn-line xs" title="Reject" onClick={() => decideAccess(r.id, false)}><Icon.Close size={13} /></button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </nav>

      <div className="side-pad">
        <div className="search-box">
          <Icon.Search size={13} />
          <input className="search-in" placeholder="Search chats & projects" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
      </div>

      <div className="side-scroll">
        {/* Projects (General) / Matters (Legal) — not a research-mode concept
            (runs are project-less). The backend already scopes by sector; this is
            just the label the Legal workspace uses ("matter"). */}
        {mode !== "research" && (<>
        <div className="side-section">
          <span className="side-label mono">{mode === "legal" ? "Matters" : "Projects"}</span>
          <button className="side-add" title={mode === "legal" ? "New matter" : "New project"} onClick={newProject}><Icon.Plus size={14} /></button>
        </div>
        {ql && filteredProjects.length === 0 && <div className="side-empty">No matching {mode === "legal" ? "matters" : "projects"}.</div>}
        <div className="min-w-0">
          {(showAllProjects ? filteredProjects : filteredProjects.slice(0, PROJ_CAP)).map((p) => {
            const count = (chats.data ?? []).filter((c) => c.project_id === p.id).length;
            return (
              <button
                key={p.id}
                onClick={() => { setActive(p); nav(`/p/${p.id}`); }}
                className={"proj-item" + (active?.id === p.id ? " on" : "")}
                title={p.name}
              >
                <Icon.Folder size={14} />
                <span className="proj-name">{p.name}</span>
                {count > 0 && <span className="proj-count mono">{count}</span>}
              </button>
            );
          })}
        </div>
        {filteredProjects.length > PROJ_CAP && (
          <button onClick={() => setShowAllProjects((v) => !v)} className="mt-1 px-2 py-1 text-left text-[11px] text-gold hover:text-gold-light">
            {showAllProjects ? "Show less" : `Show all (${filteredProjects.length})`}
          </button>
        )}
        </>)}

        {/* Chats / research runs */}
        <div className="side-section" style={{ marginTop: mode === "research" ? 0 : 18 }}>
          <span className="side-label mono">{mode === "research" ? "Research runs" : "Chats"}</span>
        </div>
        {(mode === "research" ? researchChats.isLoading : chats.isLoading) && <div className="side-empty">Loading…</div>}
        {!(mode === "research" ? researchChats.isLoading : chats.isLoading) && chatList.length === 0 && (
          <div className="side-empty">
            {ql ? "No matching chats." : mode === "research" ? "No research runs yet." : active ? "No chats in this project yet." : "No chats yet."}
          </div>
        )}
        <div className="min-w-0">
          {(showAllChats ? chatList : chatList.slice(0, CHAT_CAP)).map((c) =>
            editingId === c.id ? (
              <div key={c.id} className="chat-item editing">
                <input
                  autoFocus
                  className="chat-edit-in"
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") saveRename(c.id);
                    if (e.key === "Escape") setEditingId(null);
                  }}
                  onBlur={() => saveRename(c.id)}
                />
              </div>
            ) : (
              <div
                key={c.id}
                className={"chat-item" + (chatId === c.id ? " on" : "")}
                onClick={() => nav(`/c/${c.id}`)}
              >
                <div className="chat-item-main">
                  <span className="chat-title-row">
                    {c.origin === "desktop" && (
                      <Icon.Desktop size={12} className="chat-origin-icon" aria-label="Created on desktop" />
                    )}
                    <span className="chat-title" title={c.title}>{c.title}</span>
                  </span>
                  <span className="chat-meta mono">{relTime(c.created_at)}</span>
                </div>
                <div className="chat-item-actions">
                  <button title="Rename" onClick={(e) => { e.stopPropagation(); setEditingId(c.id); setDraft(c.title); }}><Icon.Edit size={13} /></button>
                  <button title="Delete" onClick={(e) => { e.stopPropagation(); removeChat(c.id); }}><Icon.Close size={13} /></button>
                </div>
              </div>
            ),
          )}
        </div>
        {chatList.length > CHAT_CAP && (
          <button onClick={() => setShowAllChats((v) => !v)} className="mt-1 px-2 py-1 text-left text-[11px] text-gold hover:text-gold-light">
            {showAllChats ? "Show less" : `Show all (${chatList.length})`}
          </button>
        )}
      </div>

      {/* Footer user chip — click the identity to open your profile */}
      <div className="side-foot">
        <div className="foot-id-click" onClick={() => nav("/profile")} title="Your profile" role="button" tabIndex={0}
          onKeyDown={(e) => { if (e.key === "Enter") nav("/profile"); }}>
          <Avatar id={who.data?.user_id} name={who.data?.display_name} email={who.data?.email} avatarUpdatedAt={who.data?.avatar_updated_at} />
          <div className="foot-id">
            <span className="foot-name">{who.data?.display_name ?? who.data?.email ?? "User"}</span>
            <span className="foot-org mono">{roleLabel(who.data?.role)}</span>
          </div>
        </div>
      </div>
    </aside>
  );
}
