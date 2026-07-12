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
import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  createWorkflow,
  deleteWorkflow,
  updateWorkflow,
  useAgents,
  useGroupChats,
  useLibraries,
  useProjects,
  useWorkflow,
  useWorkflowRuns,
  useWorkflows,
  useWorkflowTriggers,
  type CreateWorkflowBody,
  type Workflow,
  type WorkflowActionType,
  type WorkflowTrigger,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { EditorShell, PanelHead, Switch } from "@/components/editor";
import { useBusy } from "@/components/useBusy";

// The trigger catalogue is fetched from `GET /api/workflows/triggers` — the single
// backend source (event constants + `ALLOWED_TRIGGERS`), so the dropdown never
// drifts from what the engine actually emits (§4/D4).
const triggerLabel = (id: string, list?: WorkflowTrigger[]) =>
  list?.find((t) => t.name === id)?.description ?? id;

const OPS: { id: string; label: string }[] = [
  { id: "eq", label: "is" },
  { id: "ne", label: "is not" },
  { id: "gt", label: ">" },
  { id: "lt", label: "<" },
  { id: "in", label: "in (comma list)" },
  { id: "contains", label: "contains" },
];
const FIELD_SUGGESTIONS = ["payload.mime", "payload.filename", "payload.pages", "payload.kb_id", "actor_type", "event_type"];
const COALESCE_PRESETS: { label: string; v: number }[] = [
  { label: "Off", v: 0 },
  { label: "10s", v: 10 },
  { label: "1m", v: 60 },
  { label: "5m", v: 300 },
];

function fmt(s: string | null | undefined): string {
  if (!s) return "—";
  const d = new Date(s);
  return isNaN(d.getTime()) ? s : d.toLocaleString();
}

// ── Condition (IF) — AND/OR rows ↔ {all|any:[{field,op,value}]} ──
type CondRow = { field: string; op: string; value: string };
type CondMode = "all" | "any";

function buildCondition(mode: CondMode, rows: CondRow[]): Record<string, unknown> | null {
  const clauses = rows
    .filter((r) => r.field.trim() && r.op)
    .map((r) => ({ field: r.field.trim(), op: r.op, value: coerce(r.op, r.value) }));
  if (clauses.length === 0) return null;
  return { [mode]: clauses };
}
function coerce(op: string, raw: string): unknown {
  if (op === "gt" || op === "lt") { const n = Number(raw); return raw.trim() === "" || isNaN(n) ? raw : n; }
  if (op === "in") return raw.split(",").map((s) => s.trim()).filter(Boolean);
  return raw;
}
function parseCondition(cond: Record<string, unknown> | null): { mode: CondMode; rows: CondRow[]; unsupported: boolean } {
  if (!cond) return { mode: "all", rows: [], unsupported: false };
  const mode: CondMode | null = Array.isArray((cond as Record<string, unknown>).all)
    ? "all"
    : Array.isArray((cond as Record<string, unknown>).any)
      ? "any"
      : null;
  if (!mode) return { mode: "all", rows: [], unsupported: true };
  const arr = (cond[mode] as unknown[]) ?? [];
  const ok = arr.every((c) => c && typeof c === "object" && "field" in (c as object) && "op" in (c as object));
  const rows = arr.map((c) => {
    const o = c as { field?: string; op?: string; value?: unknown };
    return { field: o.field ?? "", op: o.op ?? "eq", value: Array.isArray(o.value) ? o.value.join(", ") : String(o.value ?? "") };
  });
  return { mode, rows, unsupported: !ok };
}

export function Workflows({ showOwner = false }: { showOwner?: boolean }) {
  const qc = useQueryClient();
  const workflows = useWorkflows();
  const triggers = useWorkflowTriggers();
  const { run } = useBusy();
  const [creating, setCreating] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);

  if (creating) return <WorkflowEditor onBack={() => setCreating(false)} onSaved={(id) => { setCreating(false); setSelected(id); }} />;
  if (selected) return <WorkflowEditor key={selected} id={selected} onBack={() => setSelected(null)} onSaved={() => {}} />;

  async function toggle(w: Workflow) {
    await run("Toggle", () => updateWorkflow(w.id, { enabled: !w.enabled }).then(() => qc.invalidateQueries({ queryKey: ["workflows"] })), w.enabled ? "Workflow disabled." : "Workflow enabled.");
  }

  return (
    <div className="main-scroll">
      <div className="panel anim-on fade-in">
        <PanelHead
          title="Workflows"
          sub={showOwner ? "Every workflow across the platform — observe, build, enable or disable." : "When something happens, run an action — event-driven, inside your environment."}
          action={<button className="btn btn-gold" onClick={() => setCreating(true)}><Icon.Plus size={16} /> New workflow</button>}
        />
        <div className="rows">
          {workflows.isLoading && <p className="text-sm text-slate">Loading…</p>}
          {workflows.data?.length === 0 && <p className="text-sm text-slate/70">No workflows yet.</p>}
          {workflows.data?.map((w) => (
            <div key={w.id} className="list-row clickable" onClick={() => setSelected(w.id)}>
              <span className="row-ic"><Icon.Workflows size={18} /></span>
              <div className="row-main">
                <span className="row-title">{w.name}</span>
                <span className="row-sub mono">{triggerLabel(w.trigger_event_type, triggers.data)} → {w.action_type === "agent_run" ? "agent run" : actionKind(w)}{showOwner && w.owner_name ? ` · ${w.owner_name}` : ""}</span>
              </div>
              <span className="row-tag mono">additive</span>
              <span className={"row-tag mono " + (w.enabled ? "live" : "")}>{w.enabled ? "enabled" : "disabled"}</span>
              <div onClick={(e) => e.stopPropagation()}>
                <Switch on={w.enabled} onClick={() => toggle(w)} />
              </div>
            </div>
          ))}
        </div>
        <p className="ed-hint mono" style={{ marginTop: 14 }}>The trigger catalogue is expanding; more events become available over time.</p>
      </div>
    </div>
  );
}

function actionKind(w: Workflow): string {
  const k = (w.action_config as { kind?: string })?.kind;
  return k === "post_message" ? "post message" : k === "notify_owner" ? "notify owner" : (k ?? "action");
}

// ── Editor (new + existing) ──
function WorkflowEditor({ id, onBack, onSaved }: { id?: string; onBack: () => void; onSaved: (id: string) => void }) {
  const qc = useQueryClient();
  const nav = useNavigate();
  const detail = useWorkflow(id);
  const runs = useWorkflowRuns(id);
  const triggers = useWorkflowTriggers();
  const agents = useAgents();
  const projects = useProjects();
  const libraries = useLibraries();
  const groupChats = useGroupChats();
  const { busy, run } = useBusy();
  const isNew = !id;

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [trigger, setTrigger] = useState("");
  const [projectId, setProjectId] = useState("");
  const [onSystem, setOnSystem] = useState(false);
  const [condMode, setCondMode] = useState<CondMode>("all");
  const [condRows, setCondRows] = useState<CondRow[]>([]);
  const [condUnsupported, setCondUnsupported] = useState(false);
  const [actionType, setActionType] = useState<WorkflowActionType>("system_action");
  const [kind, setKind] = useState("post_message");
  const [groupChatId, setGroupChatId] = useState("");
  const [template, setTemplate] = useState("");
  const [agentId, setAgentId] = useState("");
  const [prompt, setPrompt] = useState("");
  const [kbIds, setKbIds] = useState<string[]>([]);
  const [coalesce, setCoalesce] = useState(0);
  const [maxRuns, setMaxRuns] = useState(60);
  const [showErrors, setShowErrors] = useState(false);

  useEffect(() => {
    const w = detail.data;
    if (!id || !w) return;
    setName(w.name); setDescription(w.description ?? "");
    setTrigger(w.trigger_event_type); setProjectId(w.project_id ?? ""); setOnSystem(w.trigger_on_system_events);
    const c = parseCondition(w.condition); setCondMode(c.mode); setCondRows(c.rows); setCondUnsupported(c.unsupported);
    setActionType(w.action_type); setCoalesce(w.coalesce_window_secs); setMaxRuns(w.max_runs_per_window);
    const cfg = (w.action_config ?? {}) as Record<string, unknown>;
    setKind((cfg.kind as string) ?? "post_message");
    setGroupChatId((cfg.group_chat_id as string) ?? (cfg.deliver_group_chat_id as string) ?? "");
    setTemplate((cfg.template as string) ?? "");
    setAgentId(w.agent_id ?? "");
    setPrompt((cfg.prompt as string) ?? "");
    setKbIds(Array.isArray(cfg.kb_ids) ? (cfg.kb_ids as string[]) : []);
  }, [id, detail.data]);

  // For a new workflow, default the trigger to the first emitted catalogue entry
  // once the catalogue loads (the list starts empty until fetched).
  const emittedTriggers = (triggers.data ?? []).filter((t) => t.emitted);
  useEffect(() => {
    if (isNew && !trigger && emittedTriggers.length) setTrigger(emittedTriggers[0].name);
  }, [isNew, trigger, emittedTriggers]);

  if (id && (detail.isLoading || !detail.data)) return <div className="main-scroll"><div className="panel">Loading…</div></div>;
  const w = detail.data;
  const enabled = !!w?.enabled;
  const refresh = () => { qc.invalidateQueries({ queryKey: ["workflow", id] }); qc.invalidateQueries({ queryKey: ["workflows"] }); };

  // The required fields that are still empty — drives the inline markers + the
  // "what's missing" toast. A name is always required; the action decides the rest.
  function missing(): string[] {
    const m: string[] = [];
    if (!name.trim()) m.push("Name");
    if (actionType === "agent_run") {
      if (!prompt.trim()) m.push("Prompt");
    } else if (kind === "post_message" && !groupChatId && !projectId) {
      m.push("Target chat (or set a project scope)");
    }
    return m;
  }
  // Highlight offending fields only after a failed Save attempt (then live).
  const badName = showErrors && !name.trim();
  const badPrompt = showErrors && actionType === "agent_run" && !prompt.trim();
  const badTarget = showErrors && actionType === "system_action" && kind === "post_message" && !groupChatId && !projectId;

  function actionConfig(): Record<string, unknown> {
    if (actionType === "agent_run") {
      const cfg: Record<string, unknown> = { prompt: prompt.trim() };
      if (kbIds.length) cfg.kb_ids = kbIds;
      if (groupChatId) cfg.deliver_group_chat_id = groupChatId;
      return cfg;
    }
    if (kind === "post_message") {
      const cfg: Record<string, unknown> = { kind, template: template.trim() || "A workflow was triggered." };
      if (groupChatId) cfg.group_chat_id = groupChatId;
      return cfg;
    }
    return { kind: "notify_owner" };
  }

  async function save() {
    const miss = missing();
    if (miss.length) {
      setShowErrors(true);
      toast(`Please fill in: ${miss.join(", ")}.`, { variant: "error" });
      return;
    }
    setShowErrors(false);
    const condition = buildCondition(condMode, condRows);
    if (isNew) {
      const body: CreateWorkflowBody = {
        name: name.trim(),
        description: description.trim() || undefined,
        project_id: projectId || null,
        trigger_event_type: trigger,
        trigger_on_system_events: onSystem,
        condition,
        coalesce_window_secs: coalesce,
        action_type: actionType,
        agent_id: actionType === "agent_run" ? (agentId || null) : null,
        action_config: actionConfig(),
        max_runs_per_window: maxRuns,
      };
      await run("Create", () => createWorkflow(body).then((r) => { qc.invalidateQueries({ queryKey: ["workflows"] }); onSaved((r as { id: string }).id); }), `Workflow “${name.trim()}” created — disabled until you enable it.`);
    } else {
      await run("Save", () => updateWorkflow(id!, {
        name: name.trim(),
        description: description.trim() || null,
        trigger_on_system_events: onSystem,
        condition,
        action_config: actionConfig(),
        coalesce_window_secs: coalesce,
        max_runs_per_window: maxRuns,
      }).then(refresh), "Workflow saved.");
    }
  }

  const addRow = () => setCondRows((r) => [...r, { field: "", op: "eq", value: "" }]);
  const setRow = (i: number, patch: Partial<CondRow>) => setCondRows((r) => r.map((row, j) => (j === i ? { ...row, ...patch } : row)));
  const delRow = (i: number) => setCondRows((r) => r.filter((_, j) => j !== i));

  return (
    <EditorShell
      eyebrow={isNew ? "New workflow" : "Edit workflow"}
      title={isNew ? "New workflow" : name || "Workflow"}
      onBack={onBack}
      actions={
        <>
          {!isNew && (
            <button className="btn btn-ghost sm" disabled={!!busy} onClick={async () => { if (await confirmDialog({ title: `Delete "${name}"?`, danger: true, confirmLabel: "Delete" })) run("Delete", () => deleteWorkflow(id!).then(() => { qc.invalidateQueries({ queryKey: ["workflows"] }); onBack(); }), "Workflow deleted."); }}>Delete</button>
          )}
          <button className="btn btn-gold sm" disabled={!!busy} onClick={save}><Icon.Save size={14} /> {busy === "Save" || busy === "Create" ? "Saving…" : "Save"}</button>
        </>
      }
    >
      <div className="editor-grid">
        <div className="editor-main">
          {showErrors && missing().length > 0 && (
            <div className="ed-error-banner"><Icon.Alert size={14} /> Fill in the highlighted field{missing().length > 1 ? "s" : ""}: {missing().join(", ")}.</div>
          )}
          {!isNew && w && (
            <section className="ed-section">
              <div className="auto-enable">
                <div>
                  <h4 style={{ margin: 0 }}>{enabled ? "Enabled" : "Disabled"}</h4>
                  <span className="ed-hint mono" style={{ marginTop: 2 }}>{enabled ? "Fires on its trigger" : "Will not fire"}</span>
                </div>
                <Switch on={enabled} onClick={() => run("Toggle", () => updateWorkflow(id!, { enabled: !enabled }).then(refresh), enabled ? "Workflow disabled." : "Workflow enabled.")} />
              </div>
            </section>
          )}

          <section className="ed-section">
            <label className="form-label">Name <span className="req">*</span></label>
            <input className={"field" + (badName ? " field-error" : "")} value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Summarise new matter documents" />

            <label className="form-label">Description</label>
            <textarea className="field" rows={2} value={description} onChange={(e) => setDescription(e.target.value)} placeholder="Optional — what this workflow does" style={{ resize: "vertical" }} />
          </section>

          {/* WHEN — trigger */}
          <section className="ed-section">
            <label className="form-label">When (trigger)</label>
            <Dropdown
              value={trigger}
              onChange={setTrigger}
              ariaLabel="Trigger"
              fullWidth
              disabled={!isNew}
              icon={<Icon.Spark size={14} />}
              options={isNew
                ? emittedTriggers.map((t) => ({ value: t.name, label: t.description }))
                : [{ value: trigger, label: triggerLabel(trigger, triggers.data) }]}
            />

            <label className="form-label">Scope — project</label>
            <Dropdown
              value={projectId}
              onChange={setProjectId}
              ariaLabel="Scope project"
              fullWidth
              disabled={!isNew}
              icon={<Icon.Folder size={14} />}
              options={[
                { value: "", label: "Any project (owner-global)" },
                ...(projects.data ?? []).map((p) => ({ value: p.id, label: p.name })),
              ]}
            />
            {!isNew && <div className="ed-hint mono">Trigger, scope and (for agent runs) the agent are fixed after creation.</div>}

            <div className="auto-enable" style={{ marginTop: 12 }}>
              <div>
                <h4 style={{ margin: 0, fontSize: 14 }}>React to system events</h4>
                <span className="ed-hint mono" style={{ marginTop: 2 }}>Advanced — off keeps the loop guard (human-originated only).</span>
              </div>
              <Switch on={onSystem} onClick={() => setOnSystem((v) => !v)} />
            </div>
          </section>

          {/* IF — condition */}
          <section className="ed-section">
            <label className="form-label">If (condition, optional)</label>
            {condUnsupported ? (
              <div className="ed-hint mono">Advanced condition set via API — edit it there to keep it intact.</div>
            ) : (
              <>
                <div className="row" style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
                  <span className="ed-hint mono">match</span>
                  <Dropdown
                    value={condMode}
                    onChange={(v) => setCondMode(v as CondMode)}
                    ariaLabel="Match mode"
                    options={[
                      { value: "all", label: "all" },
                      { value: "any", label: "any" },
                    ]}
                  />
                  <span className="ed-hint mono">of:</span>
                </div>
                <datalist id="wf-fields">{FIELD_SUGGESTIONS.map((f) => <option key={f} value={f} />)}</datalist>
                {condRows.map((r, i) => (
                  <div key={i} className="row" style={{ display: "flex", gap: 6, marginBottom: 6 }}>
                    <input className="field mono" style={{ flex: 2 }} list="wf-fields" placeholder="field" value={r.field} onChange={(e) => setRow(i, { field: e.target.value })} />
                    <Dropdown
                      value={r.op}
                      onChange={(v) => setRow(i, { op: v })}
                      ariaLabel="Operator"
                      options={OPS.map((o) => ({ value: o.id, label: o.label }))}
                    />
                    <input className="field mono" style={{ flex: 2 }} placeholder="value" value={r.value} onChange={(e) => setRow(i, { value: e.target.value })} />
                    <button className="icon-btn" title="Remove" onClick={() => delRow(i)}><Icon.Close size={14} /></button>
                  </div>
                ))}
                <button className="btn btn-line sm" onClick={addRow} style={{ marginTop: 4 }}><Icon.Plus size={13} /> Add condition</button>
              </>
            )}
          </section>

          {/* THEN — action */}
          <section className="ed-section">
            <label className="form-label">Then (action)</label>
            <div className="seg" style={{ width: "fit-content", marginBottom: 12 }}>
              <button className={"seg-opt" + (actionType === "system_action" ? " on" : "")} onClick={() => setActionType("system_action")}>System action</button>
              <button className={"seg-opt" + (actionType === "agent_run" ? " on" : "")} onClick={() => setActionType("agent_run")}>Agent run</button>
            </div>

            {actionType === "system_action" ? (
              <>
                <label className="form-label">Kind</label>
                <Dropdown
                  value={kind}
                  onChange={setKind}
                  ariaLabel="Action kind"
                  fullWidth
                  icon={<Icon.Wrench size={14} />}
                  options={[
                    { value: "post_message", label: "Post a message to a chat" },
                    { value: "notify_owner", label: "Notify me" },
                  ]}
                />
                {kind === "post_message" && (
                  <>
                    <label className="form-label">Target chat (Teams) <span className="req">*</span></label>
                    <Dropdown
                      value={groupChatId}
                      onChange={setGroupChatId}
                      ariaLabel="Target chat"
                      fullWidth
                      icon={<Icon.Team size={14} />}
                      options={[
                        { value: "", label: "Project chat (if scoped to a project)" },
                        ...(groupChats.data ?? []).map((c) => ({ value: c.id, label: c.name ?? c.kind })),
                      ]}
                    />
                    {badTarget && <div className="ed-hint mono" style={{ color: "var(--danger, #c0392b)" }}>Pick a target chat, or set a project scope above to post to its chat.</div>}
                    <label className="form-label">Message template</label>
                    <input className="field" value={template} onChange={(e) => setTemplate(e.target.value)} placeholder="e.g. Ingested {{filename}}" />
                    <div className="ed-hint mono">Use {"{{filename}}"} / {"{{mime}}"} tokens from the event.</div>
                  </>
                )}
              </>
            ) : (
              <>
                <label className="form-label">Agent</label>
                <Dropdown
                  value={agentId}
                  onChange={setAgentId}
                  ariaLabel="Agent"
                  fullWidth
                  disabled={!isNew}
                  icon={<Icon.Agents size={14} />}
                  options={[
                    { value: "", label: "Default agent" },
                    ...(agents.data ?? []).map((a) => ({ value: a.id, label: a.name })),
                  ]}
                />

                <label className="form-label">Prompt <span className="req">*</span></label>
                <textarea className={"field" + (badPrompt ? " field-error" : "")} rows={4} value={prompt} onChange={(e) => setPrompt(e.target.value)} placeholder="What should the agent do? Use {{filename}} for the event's file." style={{ resize: "vertical" }} />

                <label className="form-label">Libraries</label>
                {(libraries.data ?? []).length === 0 ? (
                  <div className="ed-hint mono">No libraries available.</div>
                ) : (
                  <div className="chip-wrap">
                    {(libraries.data ?? []).map((lib) => {
                      const on = kbIds.includes(lib.id);
                      return (
                        <button key={lib.id} type="button" className={"skill-chip" + (on ? " on" : "")} onClick={() => setKbIds((cur) => (on ? cur.filter((x) => x !== lib.id) : [...cur, lib.id]))}>
                          <Icon.Book size={13} /> {lib.name}
                        </button>
                      );
                    })}
                  </div>
                )}

                <label className="form-label">Deliver to (Teams, optional)</label>
                <Dropdown
                  value={groupChatId}
                  onChange={setGroupChatId}
                  ariaLabel="Deliver to team chat"
                  fullWidth
                  icon={<Icon.Team size={14} />}
                  options={[
                    { value: "", label: "None — output chat only" },
                    ...(groupChats.data ?? []).map((c) => ({ value: c.id, label: c.name ?? c.kind })),
                  ]}
                />
              </>
            )}
            <div className="ed-hint mono" style={{ marginTop: 8 }}><Icon.Check size={12} /> Additive action — output lands somewhere reviewable, never a silent destructive change.</div>
          </section>

          {/* Throughput */}
          <section className="ed-section">
            <label className="form-label">Coalescing window</label>
            <div className="chip-wrap">
              {COALESCE_PRESETS.map((p) => (
                <button key={p.v} className={"skill-chip" + (coalesce === p.v ? " on" : "")} onClick={() => setCoalesce(p.v)}><Icon.Clock size={13} /> {p.label}</button>
              ))}
            </div>
            <input className="field mono" style={{ marginTop: 8, width: 160 }} type="number" min={0} value={coalesce} onChange={(e) => setCoalesce(Math.max(0, Number(e.target.value) || 0))} />
            <div className="ed-hint mono">Batch N events in the window into one run (e.g. 50 uploads → 1 run).</div>

            <label className="form-label">Max runs / minute</label>
            <input className="field mono" style={{ width: 160 }} type="number" min={0} value={maxRuns} onChange={(e) => setMaxRuns(Math.max(0, Number(e.target.value) || 0))} />
            <div className="ed-hint mono">Rate cap; repeated trips auto-disable the workflow.</div>
          </section>
        </div>

        <aside className="editor-side">
          <section className="ed-section">
            <div className="proj-panel-head" style={{ marginBottom: 12 }}>
              <h4 style={{ margin: 0 }}>Run history</h4>
            </div>
            {isNew ? (
              <div className="ed-hint mono">Save the workflow to see its run history.</div>
            ) : (
              <div className="run-list">
                {runs.isLoading && <p className="text-sm text-slate">Loading…</p>}
                {runs.data?.length === 0 && <p className="text-sm text-slate/70">No runs yet.</p>}
                {runs.data?.map((r) => {
                  const chat = (r.outcome as { output_chat_id?: string; chat_id?: string } | null);
                  const chatId = chat?.output_chat_id ?? chat?.chat_id;
                  return (
                    <div key={r.id} className="run-row">
                      <span className={"run-dot " + (r.status === "failed" ? "error" : r.status === "skipped" ? "" : "done")} />
                      <div className="run-info">
                        <span className="run-out">{r.status === "failed" ? (r.error ?? "error") : r.status}{r.event_count > 1 ? ` · ${r.event_count} events` : ""}</span>
                        <span className="run-when mono">{fmt(r.created_at)}</span>
                      </div>
                      {chatId && <button className="icon-btn" title="Open output chat" onClick={() => nav(`/c/${chatId}`)}><Icon.ChevronR size={15} /></button>}
                    </div>
                  );
                })}
              </div>
            )}
          </section>
        </aside>
      </div>
    </EditorShell>
  );
}
