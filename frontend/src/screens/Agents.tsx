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

import { confirmDialog, toast } from "@/components/dialogs";
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  useToolCatalog,
  attachSkill,
  createAgent,
  createSkill,
  deleteAgent,
  deleteSkill,
  detachSkill,
  setSkillEnabled,
  testSkill,
  rollbackAgentVersion,
  updateAgent,
  updateSkill,
  useAgent,
  useAgentVersions,
  useAgents,
  useProjectKnowledge,
  useSkill,
  useSkills,
  useWhoami,
  useAdminMcpServers,
  type AgentDetail,
  type AgentParams,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { settle } from "@/components/useBusy";
import { EditorShell, PanelHead, Switch } from "@/components/editor";

interface FormState {
  name: string;
  description: string;
  system_prompt: string;
  temperature: string;
  top_p: string;
  max_tokens: string;
  frequency_penalty: string;
  presence_penalty: string;
  tool_concurrency: string;
  max_steps: string;
  web_depth_max: string;
  web_max_fetches: string;
  modes: Set<string>;
  tools: Set<string>;
}

// The workmodes an agent can be offered in; drives picker filtering.
const WORKMODES: [string, string][] = [
  ["general", "General"],
  ["legal", "Legal"],
  ["research", "Research"],
];

const BLANK: FormState = {
  name: "", description: "", system_prompt: "You are a helpful assistant.",
  temperature: "", top_p: "", max_tokens: "", frequency_penalty: "", presence_penalty: "",
  tool_concurrency: "", max_steps: "", web_depth_max: "", web_max_fetches: "",
  modes: new Set(["general", "legal", "research"]), tools: new Set(),
};

function fromDetail(a: AgentDetail): FormState {
  const p = a.params ?? {};
  const s = (v: number | undefined) => (v == null ? "" : String(v));
  return {
    name: a.name, description: a.description ?? "", system_prompt: a.system_prompt,
    temperature: s(p.temperature), top_p: s(p.top_p), max_tokens: s(p.max_tokens),
    frequency_penalty: s(p.frequency_penalty), presence_penalty: s(p.presence_penalty),
    tool_concurrency: s(p.tool_concurrency), max_steps: s(p.max_steps),
    web_depth_max: p.web_depth_max ?? "", web_max_fetches: s(p.web_max_fetches),
    modes: new Set(a.modes),
    tools: new Set(a.tools),
  };
}

function paramsOf(f: FormState): AgentParams {
  const num = (v: string) => (v.trim() === "" ? undefined : Number(v));
  const out: AgentParams = {};
  const set = (k: keyof AgentParams, v: string) => {
    const n = num(v);
    if (n != null && !Number.isNaN(n)) (out[k] as number) = n;
  };
  set("temperature", f.temperature);
  set("top_p", f.top_p);
  set("max_tokens", f.max_tokens);
  set("frequency_penalty", f.frequency_penalty);
  set("presence_penalty", f.presence_penalty);
  set("tool_concurrency", f.tool_concurrency);
  set("max_steps", f.max_steps);
  set("web_max_fetches", f.web_max_fetches);
  if (f.web_depth_max.trim()) out.web_depth_max = f.web_depth_max.trim();
  return out;
}

export function Agents() {
  const { agentId } = useParams();
  const nav = useNavigate();
  const qc = useQueryClient();
  const agents = useAgents();
  const who = useWhoami();
  // Personal-only: any authenticated user may create their own agent/skill. Edit/delete
  // is gated per item (can_manage) — owner or admin only.
  const canCreate = !!who.data?.user_id;
  const ci = !!who.data?.capabilities.code_interpreter;
  const [tab, setTab] = useState<"agents" | "skills">("agents");
  const [creating, setCreating] = useState(false);

  if (creating) {
    return <AgentEditor codeInterpreter={ci} onBack={() => setCreating(false)} onSaved={(id) => { setCreating(false); nav(`/studio/agents/${id}`); }} />;
  }
  if (agentId) {
    return <AgentEditor key={agentId} agentId={agentId} codeInterpreter={ci} onBack={() => nav("/studio/agents")} onSaved={() => {}} />;
  }

  return (
    <div className="main-scroll">
      <div className="panel anim-on fade-in">
        <PanelHead
          title="Agents & skills"
          sub="Purpose-built assistants and the reusable skills you compose them from."
          action={tab === "agents" && canCreate ? <button className="btn btn-gold" onClick={() => setCreating(true)}><Icon.Plus size={16} /> New agent</button> : undefined}
        />
        <div className="seg" style={{ width: "fit-content", marginBottom: 18 }}>
          <button className={"seg-opt" + (tab === "agents" ? " on" : "")} onClick={() => setTab("agents")}>Agents</button>
          <button className={"seg-opt" + (tab === "skills" ? " on" : "")} onClick={() => setTab("skills")}>Skills</button>
        </div>

        {tab === "skills" ? (
          <SkillsManager canCreate={canCreate} />
        ) : (
          <div className="card-grid">
            {agents.isLoading && <p className="text-sm text-slate">Loading…</p>}
            {agents.data?.length === 0 && <p className="text-sm text-slate/70">No agents yet.</p>}
            {agents.data?.map((a) => (
              <AgentCard
                key={a.id}
                agent={a}
                onOpen={() => nav(`/studio/agents/${a.id}`)}
                onDeleted={() => qc.invalidateQueries({ queryKey: ["agents"] })}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// One agent tile with a real "…" menu (Edit / Delete) instead of a decorative button.
function AgentCard({ agent, onOpen, onDeleted }: {
  agent: { id: string; name: string; description: string | null; tools: string[]; can_manage: boolean };
  onOpen: () => void;
  onDeleted: () => void;
}) {
  const canManage = agent.can_manage;
  const [menu, setMenu] = useState(false);
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    function onDoc(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setMenu(false);
    }
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, []);
  async function del() {
    setMenu(false);
    if (!(await confirmDialog({ title: `Delete agent "${agent.name}"?`, body: "Chats keep their history; the agent is hidden.", danger: true, confirmLabel: "Delete" }))) return;
    try { await deleteAgent(agent.id); onDeleted(); }
    catch (err) { toast(`Delete failed: ${(err as Error).message}`); }
  }
  return (
    <div className="agent-card" style={{ cursor: "pointer" }} onClick={onOpen}>
      <div className="agent-card-top">
        <span className="agent-glyph lg"><Icon.Agents size={16} /></span>
        <div className="menu-wrap" ref={ref}>
          <button className="ghost-dots" title="Actions" onClick={(e) => { e.stopPropagation(); setMenu((m) => !m); }}><Icon.Dots size={16} /></button>
          {menu && (
            <div className="menu fade-up">
              <button className="menu-item" onClick={(e) => { e.stopPropagation(); setMenu(false); onOpen(); }}><Icon.Edit size={15} /> Edit</button>
              {canManage && <button className="menu-item danger" onClick={(e) => { e.stopPropagation(); del(); }}><Icon.Trash size={15} /> Delete</button>}
            </div>
          )}
        </div>
      </div>
      <h3 className="serif agent-card-name">{agent.name}</h3>
      <p className="agent-card-desc">{agent.description || "—"}</p>
      <div className="agent-card-foot mono"><span>{agent.tools.length} tools</span></div>
    </div>
  );
}

// ── Agent editor (new + existing) ──
function AgentEditor({
  agentId, codeInterpreter, onBack, onSaved,
}: {
  agentId?: string;
  codeInterpreter: boolean;
  onBack: () => void;
  onSaved: (id: string) => void;
}) {
  const qc = useQueryClient();
  const detail = useAgent(agentId);
  // Personal-only: a NEW agent is yours to manage; an EXISTING one only if you own it
  // (or you're an admin) — the backend's can_manage is authoritative.
  const canManage = agentId ? (detail.data?.can_manage ?? false) : true;
  const skills = useSkills();
  const pks = useProjectKnowledge();
  // Active MCP servers assignable to this agent (admin-only query; empty/omitted for
  // non-admins, so the section simply hides). A whole server is granted by a `slug__*`
  // entry in the agent's tool list — filtered per-turn by mcp::session_tool_defs.
  const mcpServers = useAdminMcpServers();
  // Native tool catalogue (labels/hints/badges + effective enabled state) — the
  // backend is the single source of truth; a tool an admin switched off shows as
  // a disabled row rather than vanishing (the agent may already reference it).
  const toolCatalog = useToolCatalog();
  const [form, setForm] = useState<FormState>(BLANK);
  const [selSkills, setSelSkills] = useState<Set<string>>(new Set());
  const [selPks, setSelPks] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<string | null>(null);
  const [mcpExpanded, setMcpExpanded] = useState<Set<string>>(new Set());

  useEffect(() => {
    if (agentId && detail.data) {
      setForm(fromDetail(detail.data));
      setSelSkills(new Set(detail.data.skills.map((s) => s.id)));
      setSelPks(new Set(detail.data.project_knowledge_ids));
    }
  }, [agentId, detail.data]);

  const currentSkillIds = useMemo(() => new Set((detail.data?.skills ?? []).map((s) => s.id)), [detail.data]);
  if (agentId && detail.isLoading) return <div className="main-scroll"><div className="panel">Loading…</div></div>;

  const patch = (p: Partial<FormState>) => setForm((f) => ({ ...f, ...p }));
  const toggleTool = (name: string) => setForm((f) => { const tools = new Set(f.tools); if (tools.has(name)) tools.delete(name); else tools.add(name); return { ...f, tools }; });
  // The server-level switch grants the whole (pinned) catalogue as `slug__*`. Turning it
  // on collapses any per-tool grants for that server back to the wildcard; turning it off
  // clears the grant so individual tools can be picked.
  const toggleMcpServer = (slug: string) => setForm((f) => {
    const tools = new Set(f.tools);
    const wild = `${slug}__*`;
    if (tools.has(wild)) {
      tools.delete(wild);
    } else {
      for (const t of [...tools]) if (t.startsWith(`${slug}__`)) tools.delete(t);
      tools.add(wild);
    }
    return { ...f, tools };
  });
  const toggleMcpExpand = (slug: string) => setMcpExpanded((prev) => { const next = new Set(prev); if (next.has(slug)) next.delete(slug); else next.add(slug); return next; });
  const toggleMode = (m: string) => setForm((f) => { const modes = new Set(f.modes); if (modes.has(m)) modes.delete(m); else modes.add(m); return { ...f, modes }; });
  const toggleSkill = (id: string) => setSelSkills((p) => { const n = new Set(p); if (n.has(id)) n.delete(id); else n.add(id); return n; });
  const togglePk = (id: string) => setSelPks((p) => { const n = new Set(p); if (n.has(id)) n.delete(id); else n.add(id); return n; });

  async function save() {
    if (!canManage || !form.name.trim() || form.modes.size === 0) return;
    setBusy("save");
    const started = Date.now();
    try {
      let savedId = agentId;
      if (agentId) {
        await updateAgent(agentId, {
          name: form.name.trim(), description: form.description.trim() || null,
          system_prompt: form.system_prompt, params: paramsOf(form),
          tools: [...form.tools], project_knowledge_ids: [...selPks], modes: [...form.modes],
        });
        for (const id of selSkills) if (!currentSkillIds.has(id)) await attachSkill(agentId, id);
        for (const id of currentSkillIds) if (!selSkills.has(id)) await detachSkill(agentId, id);
        await Promise.all([qc.invalidateQueries({ queryKey: ["agent", agentId] }), qc.invalidateQueries({ queryKey: ["agents"] })]);
      } else {
        savedId = (await createAgent(form.name.trim(), form.system_prompt, [...form.tools], form.description.trim() || undefined, paramsOf(form), [...selPks], null, [...form.modes])).id;
        await qc.invalidateQueries({ queryKey: ["agents"] });
      }
      await settle(started); // keep "Saving…" legible, then confirm + navigate
      toast(agentId ? "Agent saved." : `Agent “${form.name.trim()}” created.`, { variant: "success" });
      onSaved(savedId!);
    } catch (e) {
      toast(`Save failed: ${(e as Error).message}`, { variant: "error" });
    } finally {
      setBusy(null);
    }
  }
  async function remove() {
    if (!agentId || !canManage) return;
    if (!(await confirmDialog({ title: `Delete agent "${form.name}"?`, body: "Chats keep their history; the agent is hidden.", danger: true, confirmLabel: "Delete" }))) return;
    setBusy("del");
    try { await deleteAgent(agentId); await qc.invalidateQueries({ queryKey: ["agents"] }); toast("Agent deleted.", { variant: "success" }); onBack(); }
    catch (e) { toast(`Delete failed: ${(e as Error).message}`, { variant: "error" }); setBusy(null); }
  }

  const isNew = !agentId;
  return (
    <EditorShell
      eyebrow={isNew ? "New agent" : "Edit agent"}
      title={isNew ? "New agent" : form.name || "Agent"}
      onBack={onBack}
      actions={canManage ? (
        <>
          {!isNew && <button className="btn btn-ghost sm" onClick={remove} disabled={!!busy}>{busy === "del" ? "Deleting…" : "Delete"}</button>}
          <button className="btn btn-gold sm" onClick={save} disabled={!!busy || !form.name.trim() || form.modes.size === 0}><Icon.Save size={14} /> {busy === "save" ? "Saving…" : "Save agent"}</button>
        </>
      ) : undefined}
    >
      {!canManage && <div className="ed-section" style={{ marginBottom: 18, color: "var(--ink-3)" }}>Read-only — only the owner or an admin may edit this agent.</div>}
      <div className="editor-grid">
        <div className="editor-main">
          <section className="ed-section">
            <h4>Identity</h4>
            <label className="form-label">Name</label>
            <input className="field" value={form.name} onChange={(e) => patch({ name: e.target.value })} disabled={!canManage} placeholder="e.g. Contract Reviewer" />
            <label className="form-label">Description</label>
            <input className="field" value={form.description} onChange={(e) => patch({ description: e.target.value })} disabled={!canManage} placeholder="What is this agent good at?" />
            <label className="form-label">Workmodes <span className="opt">(where this agent appears)</span></label>
            <div className="row" style={{ gap: 8, flexWrap: "wrap" }}>
              {WORKMODES.map(([value, label]) => (
                <button
                  key={value}
                  type="button"
                  className={"btn btn-line sm" + (form.modes.has(value) ? " on" : "")}
                  aria-pressed={form.modes.has(value)}
                  onClick={() => toggleMode(value)}
                  disabled={!canManage}
                >
                  {label}
                </button>
              ))}
            </div>
            {form.modes.size === 0 && <div className="ed-hint" style={{ color: "var(--danger, #f87171)" }}>Pick at least one workmode, or the agent is hidden everywhere.</div>}
          </section>

          <section className="ed-section">
            <h4>System prompt</h4>
            <textarea className="field code-field" rows={8} value={form.system_prompt} onChange={(e) => patch({ system_prompt: e.target.value })} disabled={!canManage} placeholder="Describe the agent's role, rules and tone…" />
            <div className="ed-hint mono">{form.system_prompt.length} characters · injected on every turn</div>
          </section>

          <section className="ed-section">
            <h4>Parameters</h4>
            <div className="two-col">
              {([
                ["temperature", "Temperature", "0.7"],
                ["top_p", "Top-p", "—"],
                ["max_tokens", "Max tokens", "—"],
                ["frequency_penalty", "Frequency penalty", "—"],
                ["presence_penalty", "Presence penalty", "—"],
                ["tool_concurrency", "Tool concurrency", "4"],
                ["max_steps", "Max steps (agent)", "5"],
              ] as [keyof FormState, string, string][]).map(([key, lbl, ph]) => (
                <div key={key}>
                  <label className="form-label">{lbl}</label>
                  <input type="number" step="any" className="field sm" value={form[key] as string} onChange={(e) => patch({ [key]: e.target.value } as Partial<FormState>)} disabled={!canManage} placeholder={`default ${ph}`} />
                </div>
              ))}
            </div>
            <div className="ed-hint mono">Blank = follow the deployed model's defaults.</div>
            <h4 style={{ marginTop: "1rem" }}>Web search budget</h4>
            <div className="two-col">
              <div>
                <label className="form-label">Max depth</label>
                <Dropdown
                  value={form.web_depth_max ?? ""}
                  onChange={(v) => patch({ web_depth_max: v })}
                  ariaLabel="Max web-search depth"
                  fullWidth
                  disabled={!canManage}
                  options={[
                    { value: "", label: "No cap" },
                    { value: "quick", label: "Quick" },
                    { value: "standard", label: "Standard" },
                    { value: "deep", label: "Deep" },
                  ]}
                />
              </div>
              <div>
                <label className="form-label">Max pages fetched</label>
                <input
                  type="number"
                  step="1"
                  min="1"
                  className="field sm"
                  value={form.web_max_fetches}
                  onChange={(e) => patch({ web_max_fetches: e.target.value })}
                  disabled={!canManage}
                  placeholder="no cap"
                />
              </div>
            </div>
            <div className="ed-hint mono">Caps tighten the web_search tool for this Agent; they never widen it.</div>
          </section>
        </div>

        <aside className="editor-side">
          <section className="ed-section">
            <h4>Tools</h4>
            {(toolCatalog.data?.native ?? []).map((t) => {
              const gated = t.capability === "code_interpreter" && !codeInterpreter;
              // A tool an admin switched off is shown disabled (not hidden) — it
              // may already be selected on this agent; it is simply absent from the
              // per-turn defs, like a dormant connector.
              const disabledTool = !t.enabled;
              // Baseline tools are always on for every agent (backend injects them);
              // show them locked-on so the editor is truthful.
              const off = !canManage || t.dormant || gated || !!t.default || disabledTool;
              return (
                <div key={t.name} className={"tool-row" + (off ? " disabled" : "")}>
                  <span className="tool-ic"><Icon.Wrench size={15} /></span>
                  <div className="tool-info">
                    <span className="tool-name">
                      {t.label}
                      {t.default && <span className="tool-badge default">always on</span>}
                      {t.effect === "run" && <span className="tool-badge approval">agent run</span>}
                      {t.effect === "proposal" && <span className="tool-badge proposal">proposal</span>}
                      {t.egress && <span className="tool-badge egress">egress</span>}
                    </span>
                    <span className="tool-desc">{t.hint}{t.dormant ? " (dormant)" : ""}{gated ? " (not on this host)" : ""}{disabledTool ? " (switched off by admin)" : ""}</span>
                  </div>
                  <Switch on={!!t.default || form.tools.has(t.name)} disabled={off} onClick={() => !off && toggleTool(t.name)} />
                </div>
              );
            })}
            {(toolCatalog.data?.custom ?? []).map((t) => {
              // A custom tool is selectable only once it is enabled + approved
              // (dispatchable); otherwise it shows disabled, like a dormant native tool.
              const live = t.enabled && t.approved;
              const off = !canManage || !live;
              return (
                <div key={t.id} className={"tool-row" + (off ? " disabled" : "")}>
                  <span className="tool-ic"><Icon.Wrench size={15} /></span>
                  <div className="tool-info">
                    <span className="tool-name">
                      {t.display_name || t.name}
                      <span className="tool-badge">{t.kind}</span>
                      {t.side_effecting && <span className="tool-badge approval">needs approval</span>}
                      {t.requires_egress && <span className="tool-badge egress">egress</span>}
                    </span>
                    <span className="tool-desc">{t.description}{!live ? " (not enabled by admin)" : ""}</span>
                  </div>
                  <Switch on={form.tools.has(t.name)} disabled={off} onClick={() => !off && toggleTool(t.name)} />
                </div>
              );
            })}
          </section>

          {(mcpServers.data ?? []).some((s) => s.status === "active") && (
            <section className="ed-section">
              <h4>MCP servers</h4>
              <p className="ed-hint mono">Grant this agent an MCP server's tools — the whole server, or individual tools. Other agents don't see them.</p>
              {(mcpServers.data ?? []).filter((s) => s.status === "active").map((s) => {
                const wild = `${s.slug}__*`;
                const wildOn = form.tools.has(wild);
                const expanded = mcpExpanded.has(s.slug);
                const grantedCount = wildOn ? s.tools.length : s.tools.filter((t) => form.tools.has(`${s.slug}__${t.name}`)).length;
                return (
                  <div key={s.id}>
                    <div className={"tool-row" + (canManage ? "" : " disabled")}>
                      <span className="tool-ic"><Icon.Wrench size={15} /></span>
                      <div className="tool-info">
                        <span className="tool-name">
                          {s.name || s.slug}
                          {s.requires_egress && <span className="tool-badge egress">egress</span>}
                        </span>
                        <span className="tool-desc">
                          {s.slug} · {wildOn ? "all tools, including ones added later" : `${grantedCount} of ${s.tools.length} tool${s.tools.length === 1 ? "" : "s"} granted`}
                          {s.tools.length > 0 && (
                            <button
                              type="button"
                              style={{ marginLeft: 8, background: "none", border: "none", padding: 0, color: "var(--accent-link, var(--color-gold))", cursor: "pointer", font: "inherit", textDecoration: "underline" }}
                              onClick={() => toggleMcpExpand(s.slug)}
                            >
                              {expanded ? "hide tools" : "choose tools"}
                            </button>
                          )}
                        </span>
                      </div>
                      <Switch on={wildOn} disabled={!canManage} onClick={() => canManage && toggleMcpServer(s.slug)} />
                    </div>
                    {expanded && (
                      <div style={{ paddingLeft: 28 }}>
                        {s.tools.map((t) => {
                          const tkey = `${s.slug}__${t.name}`;
                          const on = wildOn || form.tools.has(tkey);
                          const locked = !canManage || wildOn;
                          return (
                            <div key={t.name} className={"tool-row" + (locked ? " disabled" : "")}>
                              <div className="tool-info">
                                <span className="tool-name">{t.name}</span>
                                {t.description && <span className="tool-desc">{t.description}</span>}
                              </div>
                              <Switch on={on} disabled={locked} onClick={() => !locked && toggleTool(tkey)} />
                            </div>
                          );
                        })}
                      </div>
                    )}
                  </div>
                );
              })}
            </section>
          )}

          <section className="ed-section">
            <h4>Skills</h4>
            {skills.data?.length === 0 ? (
              <p className="ed-hint mono">No skills available.</p>
            ) : (
              <div className="chip-wrap">
                {skills.data?.map((s) => (
                  <button key={s.id} className={"skill-chip" + (selSkills.has(s.id) ? " on" : "")} onClick={() => canManage && toggleSkill(s.id)} title={s.description}>
                    {selSkills.has(s.id) ? <Icon.Check size={13} /> : <Icon.Plus size={13} />} {s.name}
                  </button>
                ))}
              </div>
            )}
            {isNew && <div className="ed-hint mono">Skills attach after the agent is created.</div>}
          </section>

          <section className="ed-section">
            <h4>Project knowledge</h4>
            <p className="ed-hint mono" style={{ marginTop: 0, marginBottom: 10 }}>None = the chat's own project. Per-user access is still enforced.</p>
            {!pks.data?.length ? (
              <p className="ed-hint mono">No knowledge bases available.</p>
            ) : (
              <div className="kb-list">
                {pks.data.map((k) => (
                  <button key={k.id} className={"kb-opt" + (selPks.has(k.id) ? " on" : "")} onClick={() => canManage && togglePk(k.id)}>
                    <span className="kb-check">{selPks.has(k.id) && <Icon.Check size={13} />}</span>
                    <Icon.Book size={15} />
                    <span className="kb-name">{k.project_name}</span>
                    <span className="kb-size mono">{k.status}</span>
                  </button>
                ))}
              </div>
            )}
          </section>

          {!isNew && agentId && <VersionHistory agentId={agentId} canManage={canManage} />}
        </aside>
      </div>
    </EditorShell>
  );
}

// Version history for an Agent (Tier-2 #7): snapshots on each save; restore a
// prior one (which itself becomes a new version).
function VersionHistory({ agentId, canManage }: { agentId: string; canManage: boolean }) {
  const qc = useQueryClient();
  const versions = useAgentVersions(agentId);
  const [busy, setBusy] = useState<number | null>(null);

  async function restore(vnum: number) {
    if (!canManage || busy) return;
    if (!(await confirmDialog({ title: `Restore to version ${vnum}?`, body: "A new version is created with that configuration.", confirmLabel: "Restore" }))) return;
    setBusy(vnum);
    try {
      await rollbackAgentVersion(agentId, vnum);
      await Promise.all([
        qc.invalidateQueries({ queryKey: ["agent", agentId] }),
        qc.invalidateQueries({ queryKey: ["agent-versions", agentId] }),
        qc.invalidateQueries({ queryKey: ["agents"] }),
      ]);
    } catch (e) {
      toast(`Restore failed: ${(e as Error).message}`);
    } finally {
      setBusy(null);
    }
  }

  const list = versions.data ?? [];
  return (
    <section className="ed-section">
      <h4>Version history</h4>
      {versions.isLoading ? (
        <p className="ed-hint mono">Loading…</p>
      ) : list.length === 0 ? (
        <p className="ed-hint mono">No versions yet.</p>
      ) : (
        <div className="kb-list">
          {list.map((v, i) => (
            <div key={v.version_number} className="list-row" style={{ gap: 10 }}>
              <span className="mono" style={{ width: 28 }}>v{v.version_number}</span>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div className="mono" style={{ fontSize: 11, textTransform: "uppercase", letterSpacing: ".04em" }}>{v.source}{i === 0 ? " · current" : ""}</div>
                <div className="ed-hint mono" style={{ fontSize: 11 }}>{new Date(v.created_at).toLocaleString()}</div>
              </div>
              {canManage && i !== 0 && (
                <button className="btn btn-ghost sm" disabled={busy !== null} onClick={() => restore(v.version_number)}>
                  {busy === v.version_number ? "…" : "Restore"}
                </button>
              )}
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

// ── Skills manager + editor ──
function SkillsManager({ canCreate }: { canCreate: boolean }) {
  const qc = useQueryClient();
  const skills = useSkills();
  const [sel, setSel] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const refresh = () => qc.invalidateQueries({ queryKey: ["skills"] });

  if (creating) return <SkillEditor onBack={() => setCreating(false)} onSaved={(id) => { setCreating(false); setSel(id); refresh(); }} onDeleted={() => setCreating(false)} />;
  if (sel) return <SkillEditor key={sel} skillId={sel} onBack={() => setSel(null)} onSaved={refresh} onDeleted={() => { setSel(null); refresh(); }} />;

  return (
    <>
      <div className="proj-panel-head" style={{ marginBottom: 16 }}>
        <span className="side-label mono">Reusable skills · attach to any agent</span>
        {canCreate && <button className="btn btn-gold sm" onClick={() => setCreating(true)}><Icon.Plus size={14} /> New skill</button>}
      </div>
      <div className="card-grid">
        {skills.isLoading && <p className="text-sm text-slate">Loading…</p>}
        {skills.data?.length === 0 && <p className="text-sm text-slate/70">No skills yet.</p>}
        {skills.data?.map((s) => (
          <SkillCard key={s.id} skill={s} onOpen={() => setSel(s.id)} onDeleted={refresh} />
        ))}
      </div>
    </>
  );
}

// One skill tile with a real "…" menu (Edit / Delete).
function SkillCard({ skill, onOpen, onDeleted }: {
  skill: { id: string; name: string; description: string | null; scope: string; can_manage: boolean; is_default: boolean; enabled: boolean };
  onOpen: () => void;
  onDeleted: () => void;
}) {
  const canManage = skill.can_manage;
  const [menu, setMenu] = useState(false);
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    function onDoc(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setMenu(false);
    }
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, []);
  async function del() {
    setMenu(false);
    if (!(await confirmDialog({ title: `Delete skill "${skill.name}"?`, body: "Agents using it lose the attachment.", danger: true, confirmLabel: "Delete" }))) return;
    try { await deleteSkill(skill.id); onDeleted(); }
    catch (err) { toast(`Delete failed: ${(err as Error).message}`); }
  }
  async function toggleEnabled() {
    setMenu(false);
    try { await setSkillEnabled(skill.id, !skill.enabled); onDeleted(); }
    catch (err) { toast(`Update failed: ${(err as Error).message}`); }
  }
  return (
    <div className="agent-card" style={{ cursor: "pointer", opacity: skill.enabled ? 1 : 0.6 }} onClick={onOpen}>
      <div className="agent-card-top">
        <span className="agent-glyph lg"><Icon.Skills size={16} /></span>
        <div className="menu-wrap" ref={ref}>
          <button className="ghost-dots" title="Actions" onClick={(e) => { e.stopPropagation(); setMenu((m) => !m); }}><Icon.Dots size={16} /></button>
          {menu && (
            <div className="menu fade-up">
              <button className="menu-item" onClick={(e) => { e.stopPropagation(); setMenu(false); onOpen(); }}><Icon.Edit size={15} /> Edit</button>
              {canManage && <button className="menu-item" onClick={(e) => { e.stopPropagation(); toggleEnabled(); }}><Icon.Blocks size={15} /> {skill.enabled ? "Disable" : "Enable"}</button>}
              {canManage && <button className="menu-item danger" onClick={(e) => { e.stopPropagation(); del(); }}><Icon.Trash size={15} /> Delete</button>}
            </div>
          )}
        </div>
      </div>
      <h3 className="serif agent-card-name">{skill.name}</h3>
      <p className="agent-card-desc">{skill.description || "—"}</p>
      <div className="agent-card-foot mono">
        <span>{skill.is_default ? "default" : skill.scope}</span>
        {!skill.enabled && <span className="skill-badge-off">disabled</span>}
      </div>
    </div>
  );
}

function SkillEditor({
  skillId, onBack, onSaved, onDeleted,
}: {
  skillId?: string;
  onBack: () => void;
  onSaved: (id: string) => void;
  onDeleted: () => void;
}) {
  const detail = useSkill(skillId);
  // New skill is yours; existing only if you own it (or admin) — backend can_manage.
  const canManage = skillId ? (detail.data?.can_manage ?? false) : true;
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [testIn, setTestIn] = useState("");
  const [testOut, setTestOut] = useState<string | null>(null);

  useEffect(() => {
    if (detail.data) { setName(detail.data.name); setDescription(detail.data.description); setBody(detail.data.body); }
  }, [detail.data]);

  if (skillId && detail.isLoading) return <div className="main-scroll"><div className="panel">Loading…</div></div>;

  async function save() {
    if (!canManage || !name.trim()) return;
    setBusy("save");
    const started = Date.now();
    try {
      let savedId = skillId;
      if (skillId) await updateSkill(skillId, { name: name.trim(), description, body });
      else savedId = (await createSkill({ name: name.trim(), description, body })).id;
      await settle(started);
      toast(skillId ? "Skill saved." : `Skill “${name.trim()}” created.`, { variant: "success" });
      onSaved(savedId!);
    } catch (e) { toast(`Save failed: ${(e as Error).message}`, { variant: "error" }); } finally { setBusy(null); }
  }
  async function remove() {
    if (!skillId || !canManage) return;
    if (!(await confirmDialog({ title: `Delete skill "${name}"?`, danger: true, confirmLabel: "Delete" }))) return;
    setBusy("del");
    try { await deleteSkill(skillId); toast("Skill deleted.", { variant: "success" }); onDeleted(); } catch (e) { toast(`Delete failed: ${(e as Error).message}`, { variant: "error" }); setBusy(null); }
  }
  async function dryRun() {
    if (!skillId) return;
    setBusy("test"); setTestOut(null);
    try { const { output } = await testSkill(skillId, testIn); setTestOut(output); }
    catch (e) { setTestOut(`⚠ ${(e as Error).message}`); } finally { setBusy(null); }
  }

  const isNew = !skillId;
  return (
    <EditorShell
      eyebrow={isNew ? "New skill" : "Edit skill"}
      title={isNew ? "New skill" : name || "Skill"}
      onBack={onBack}
      actions={canManage ? (
        <>
          {!isNew && <button className="btn btn-ghost sm" onClick={remove} disabled={!!busy}>{busy === "del" ? "Deleting…" : "Delete"}</button>}
          <button className="btn btn-gold sm" onClick={save} disabled={!!busy || !name.trim()}><Icon.Save size={14} /> {busy === "save" ? "Saving…" : "Save skill"}</button>
        </>
      ) : undefined}
    >
      <div className="editor-grid">
        <div className="editor-main">
          <section className="ed-section">
            <h4>Definition</h4>
            <label className="form-label">Skill name</label>
            <input className="field" value={name} onChange={(e) => setName(e.target.value)} disabled={!canManage} placeholder="e.g. Redline clauses" />
            <label className="form-label">Description</label>
            <input className="field" value={description} onChange={(e) => setDescription(e.target.value)} disabled={!canManage} placeholder="One line — rides the prompt as metadata" />
          </section>
          <section className="ed-section">
            <h4>Instructions</h4>
            <textarea className="field code-field" rows={9} value={body} onChange={(e) => setBody(e.target.value)} disabled={!canManage} placeholder="Markdown instructions the agent loads on demand…" />
            <div className="ed-hint mono">Appended to any agent this skill is attached to.</div>
          </section>
        </div>
        <aside className="editor-side">
          <section className="ed-section">
            <h4>Dry run</h4>
            {isNew ? (
              <div className="ed-hint mono">Save the skill first, then dry-run it here.</div>
            ) : (
              <>
                <textarea className="field sm" rows={3} value={testIn} onChange={(e) => setTestIn(e.target.value)} placeholder="Paste sample input to dry-run this skill…" style={{ resize: "none" }} />
                <button className="btn btn-line sm full-btn" onClick={dryRun} disabled={busy === "test" || !testIn.trim()}><Icon.Play size={13} /> {busy === "test" ? "Running…" : "Dry run"}</button>
                {testOut != null && <pre className="ph-preview" style={{ marginTop: 12 }}>{testOut}</pre>}
              </>
            )}
          </section>
        </aside>
      </div>
    </EditorShell>
  );
}
