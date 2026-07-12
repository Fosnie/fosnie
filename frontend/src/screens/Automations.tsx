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

import { confirmDialog } from "@/components/dialogs";
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  createAutomation,
  deleteAutomation,
  runAutomation,
  updateAutomation,
  useAgents,
  useAutomation,
  useAutomationRuns,
  useAutomations,
  useCalendar,
  useGroupChats,
  useLibraries,
  useProjects,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { EditorShell, PanelHead, Switch } from "@/components/editor";
import { useBusy } from "@/components/useBusy";

const PRESETS: { label: string; cron: string }[] = [
  { label: "Daily 09:00", cron: "0 0 9 * * *" },
  { label: "Weekdays 09:00", cron: "0 0 9 * * Mon-Fri" },
  { label: "Hourly", cron: "0 0 * * * *" },
  { label: "Mondays 09:00", cron: "0 0 9 * * Mon" },
];

function fmt(s: string | null | undefined): string {
  if (!s) return "—";
  const d = new Date(s);
  return isNaN(d.getTime()) ? s : d.toLocaleString();
}

export function Automations() {
  const { automationId } = useParams();
  const nav = useNavigate();
  const qc = useQueryClient();
  const automations = useAutomations();
  const agents = useAgents();
  const { run } = useBusy();
  const [tab, setTab] = useState<"list" | "calendar">("list");
  const [creating, setCreating] = useState(false);

  const agentName = (id: string | null | undefined) => agents.data?.find((a) => a.id === id)?.name ?? "Default agent";

  if (creating) return <AutomationEditor onBack={() => setCreating(false)} onSaved={(id) => { setCreating(false); nav(`/studio/automations/${id}`); }} />;
  if (automationId) return <AutomationEditor key={automationId} id={automationId} onBack={() => nav("/studio/automations")} onSaved={() => {}} />;

  async function toggle(id: string, status: string) {
    const next = status === "active" ? "paused" : "active";
    await run("Toggle", () => updateAutomation(id, { status: next }).then(() => qc.invalidateQueries({ queryKey: ["automations"] })), next === "active" ? "Automation activated." : "Automation paused.");
  }

  return (
    <div className="main-scroll">
      <div className="panel anim-on fade-in">
        <PanelHead
          title="Automations"
          sub="Recurring jobs that run on a schedule — fully inside your environment."
          action={<button className="btn btn-gold" onClick={() => setCreating(true)}><Icon.Plus size={16} /> New automation</button>}
        />
        <div className="seg" style={{ width: "fit-content", marginBottom: 18 }}>
          <button className={"seg-opt" + (tab === "list" ? " on" : "")} onClick={() => setTab("list")}>List</button>
          <button className={"seg-opt" + (tab === "calendar" ? " on" : "")} onClick={() => setTab("calendar")}>Calendar</button>
        </div>

        {tab === "calendar" ? (
          <CalendarView onPick={(id) => nav(`/studio/automations/${id}`)} />
        ) : (
          <div className="rows">
            {automations.isLoading && <p className="text-sm text-slate">Loading…</p>}
            {automations.data?.length === 0 && <p className="text-sm text-slate/70">No automations yet.</p>}
            {automations.data?.map((a) => (
              <div key={a.id} className="list-row clickable" onClick={() => nav(`/studio/automations/${a.id}`)}>
                <span className="row-ic"><Icon.Automations size={18} /></span>
                <div className="row-main">
                  <span className="row-title">{a.name}</span>
                  <span className="row-sub mono">{a.schedule} · {agentName(a.agent_id)}</span>
                </div>
                <span className={"row-tag mono " + (a.status === "active" ? "live" : "")}>{a.status}</span>
                <div onClick={(e) => e.stopPropagation()}>
                  <Switch on={a.status === "active"} onClick={() => toggle(a.id, a.status)} />
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// ── Editor (new + existing) ──
function AutomationEditor({ id, onBack, onSaved }: { id?: string; onBack: () => void; onSaved: (id: string) => void }) {
  const qc = useQueryClient();
  const nav = useNavigate();
  const detail = useAutomation(id);
  const runs = useAutomationRuns(id);
  const agents = useAgents();
  const projects = useProjects();
  const libraries = useLibraries();
  const groupChats = useGroupChats();
  const { busy, run } = useBusy();
  const [name, setName] = useState("");
  const [schedule, setSchedule] = useState("0 0 9 * * Mon-Fri");
  const [prompt, setPrompt] = useState("");
  const [agentId, setAgentId] = useState("");
  const [projectId, setProjectId] = useState("");
  const [kbIds, setKbIds] = useState<string[]>([]);
  const [deliverChatId, setDeliverChatId] = useState("");
  const [justRan, setJustRan] = useState(false);

  // "Run now" feedback: the scheduler creates the `running` row a few seconds
  // after the click, so show a note straight away and keep it while a run is in
  // flight; clear it once the run finishes.
  const anyRunning = !!runs.data?.some((r) => r.status === "running");
  const wasRunning = useRef(false);
  useEffect(() => {
    if (wasRunning.current && !anyRunning) setJustRan(false);
    wasRunning.current = anyRunning;
  }, [anyRunning]);

  useEffect(() => {
    const a = detail.data;
    if (id && a) {
      setName(a.name); setSchedule(a.schedule); setPrompt(a.prompt); setAgentId(a.agent_id ?? "");
      setProjectId(a.project_id ?? ""); setKbIds(a.kb_ids ?? []); setDeliverChatId(a.deliver_group_chat_id ?? "");
    }
  }, [id, detail.data]);

  if (id && (detail.isLoading || !detail.data)) return <div className="main-scroll"><div className="panel">Loading…</div></div>;
  const a = detail.data;
  const isNew = !id;
  const active = a?.status === "active";
  const refresh = () => { qc.invalidateQueries({ queryKey: ["automation", id] }); qc.invalidateQueries({ queryKey: ["automations"] }); };
  const valid = name.trim() && schedule.trim() && prompt.trim();

  // Tell the user exactly where each run's output will surface (the chat list is
  // workmode-scoped, so a personal run only shows in General).
  const selProject = projects.data?.find((p) => p.id === projectId);
  const selGroup = groupChats.data?.find((c) => c.id === deliverChatId);
  const outputHint =
    (selProject
      ? `Output appears in project “${selProject.name}” (${selProject.sector})`
      : "Output appears in General · your chats") +
    (selGroup ? ` · also posted to ${selGroup.name ?? selGroup.kind} in Teams` : "");

  async function save() {
    if (!valid) return;
    // Targets are nullable: send an explicit null to clear, an id to set.
    const targets = {
      project_id: projectId || null,
      kb_ids: kbIds,
      deliver_group_chat_id: deliverChatId || null,
    };
    if (isNew) {
      await run("Create", () => createAutomation({ name: name.trim(), schedule: schedule.trim(), prompt: prompt.trim(), agent_id: agentId || undefined, ...targets }).then((r) => { qc.invalidateQueries({ queryKey: ["automations"] }); onSaved((r as { id: string }).id); }), `Automation “${name.trim()}” created.`);
    } else {
      await run("Save", () => updateAutomation(id!, { name: name.trim(), schedule, prompt, ...targets }).then(refresh), "Automation saved.");
    }
  }

  return (
    <EditorShell
      eyebrow={isNew ? "New automation" : "Edit automation"}
      title={isNew ? "New automation" : name || "Automation"}
      onBack={onBack}
      actions={
        <>
          {!isNew && (
            <button className="btn btn-ghost sm" disabled={!!busy} onClick={async () => { if (await confirmDialog({ title: `Delete "${name}"?`, danger: true, confirmLabel: "Delete" })) run("Delete", () => deleteAutomation(id!).then(() => { qc.invalidateQueries({ queryKey: ["automations"] }); onBack(); }), "Automation deleted."); }}>Delete</button>
          )}
          <button className="btn btn-gold sm" disabled={!!busy || !valid} onClick={save}><Icon.Save size={14} /> {busy === "Save" || busy === "Create" ? "Saving…" : "Save"}</button>
        </>
      }
    >
      <div className="editor-grid">
        <div className="editor-main">
          {!isNew && a && (
            <section className="ed-section">
              <div className="auto-enable">
                <div>
                  <h4 style={{ margin: 0 }}>{active ? "Active" : "Paused"}</h4>
                  <span className="ed-hint mono" style={{ marginTop: 2 }}>{active ? "Runs on schedule" : "Will not run"}</span>
                </div>
                <Switch on={active} onClick={() => run("Toggle", () => updateAutomation(id!, { status: active ? "paused" : "active" }).then(refresh))} />
              </div>
            </section>
          )}

          <section className="ed-section">
            <label className="form-label">Name</label>
            <input className="field" value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Daily inbox triage" />

            <label className="form-label">Schedule</label>
            <div className="chip-wrap">
              {PRESETS.map((p) => (
                <button key={p.cron} className={"skill-chip" + (schedule === p.cron ? " on" : "")} onClick={() => setSchedule(p.cron)}><Icon.Clock size={13} /> {p.label}</button>
              ))}
            </div>
            <input className="field mono" style={{ marginTop: 10 }} value={schedule} onChange={(e) => setSchedule(e.target.value)} placeholder="0 0 9 * * Mon-Fri" />
            <div className="ed-hint mono">6-field cron, seconds first: sec min hour dom mon dow. Min interval 5 min.</div>

            <label className="form-label">Agent</label>
            <Dropdown
              value={agentId}
              onChange={setAgentId}
              ariaLabel="Agent"
              fullWidth
              icon={<Icon.Agents size={14} />}
              options={[
                { value: "", label: "Default agent" },
                ...(agents.data ?? []).map((a2) => ({ value: a2.id, label: a2.name })),
              ]}
            />

            <label className="form-label">Project</label>
            <Dropdown
              value={projectId}
              onChange={setProjectId}
              ariaLabel="Project"
              fullWidth
              icon={<Icon.Folder size={14} />}
              options={[
                { value: "", label: "None — General · your chats" },
                ...(projects.data ?? []).filter((p) => p.sector === "general").map((p) => ({ value: p.id, label: p.name, group: "General" })),
                ...(projects.data ?? []).filter((p) => p.sector === "legal").map((p) => ({ value: p.id, label: p.name, group: "Legal" })),
              ]}
            />

            <label className="form-label">Library</label>
            {(libraries.data ?? []).length === 0 ? (
              <div className="ed-hint mono">No libraries available.</div>
            ) : (
              <div className="chip-wrap">
                {(libraries.data ?? []).map((lib) => {
                  const on = kbIds.includes(lib.id);
                  return (
                    <button
                      key={lib.id}
                      type="button"
                      className={"skill-chip" + (on ? " on" : "")}
                      onClick={() => setKbIds((cur) => (on ? cur.filter((x) => x !== lib.id) : [...cur, lib.id]))}
                    >
                      <Icon.Book size={13} /> {lib.name}
                    </button>
                  );
                })}
              </div>
            )}

            <label className="form-label">Deliver to (Teams)</label>
            <Dropdown
              value={deliverChatId}
              onChange={setDeliverChatId}
              ariaLabel="Deliver to team chat"
              fullWidth
              icon={<Icon.Team size={14} />}
              options={[
                { value: "", label: "None — output chat only" },
                ...(groupChats.data ?? []).map((c) => ({ value: c.id, label: c.name ?? c.kind })),
              ]}
            />
            <div className="ed-hint mono">{outputHint}</div>

            <label className="form-label">Prompt to run</label>
            <textarea className="field" rows={4} value={prompt} onChange={(e) => setPrompt(e.target.value)} placeholder="What should the agent do on each run?" style={{ resize: "vertical" }} />
          </section>
        </div>

        <aside className="editor-side">
          {!isNew && a && (
            <section className="ed-section">
              <div className="proj-panel-head" style={{ marginBottom: 12 }}>
                <h4 style={{ margin: 0 }}>Run history</h4>
                <button className="btn btn-line sm" disabled={!!busy} onClick={() => { setJustRan(true); run("Run now", () => runAutomation(id!).then(() => qc.invalidateQueries({ queryKey: ["automation-runs", id] }))); }}><Icon.Play size={13} /> Run now</button>
              </div>
              {(justRan || anyRunning) && (
                <div className="ed-hint mono" style={{ marginBottom: 12 }}>Queued · the first token can take a few minutes on this host (slow local model).</div>
              )}
              <div className="two-col" style={{ marginBottom: 12 }}>
                <div className="ed-hint mono">Next · {fmt(a.next_run_at)}</div>
                <div className="ed-hint mono">Last · {fmt(a.last_run_at)}</div>
              </div>
              <div className="run-list">
                {runs.isLoading && <p className="text-sm text-slate">Loading…</p>}
                {runs.data?.length === 0 && <p className="text-sm text-slate/70">No runs yet.</p>}
                {runs.data?.map((r) => (
                  <div key={r.id} className="run-row">
                    <span className={"run-dot " + (r.status === "failed" ? "error" : "done")} />
                    <div className="run-info">
                      <span className="run-out">{r.status === "failed" ? (r.error ?? "error") : r.status}</span>
                      <span className="run-when mono">{fmt(r.started_at)}</span>
                    </div>
                    {r.output_chat_id && <button className="icon-btn" title="Open output chat" onClick={() => nav(`/c/${r.output_chat_id}`)}><Icon.ChevronR size={15} /></button>}
                  </div>
                ))}
              </div>
            </section>
          )}
          {isNew && <section className="ed-section"><div className="ed-hint mono">Save the automation to see its run history.</div></section>}
        </aside>
      </div>
    </EditorShell>
  );
}

// ── Calendar (month grid + upcoming) ──
type CalEntry = { at: string; automation_id: string; name: string };
const WEEKDAYS = ["M", "T", "W", "T", "F", "S", "S"];

function CalendarView({ onPick }: { onPick: (automationId: string) => void }) {
  const [month, setMonth] = useState(() => { const d = new Date(); return new Date(d.getFullYear(), d.getMonth(), 1); });
  const [selectedDay, setSelectedDay] = useState<number | null>(null);
  const monthEnd = new Date(month.getFullYear(), month.getMonth() + 1, 0);
  const cal = useCalendar(
    month.toISOString(),
    new Date(monthEnd.getFullYear(), monthEnd.getMonth(), monthEnd.getDate(), 23, 59, 59).toISOString(),
  );

  const byDate = useMemo(() => {
    const m = new Map<number, CalEntry[]>();
    (cal.data ?? []).forEach((e) => {
      const d = new Date(e.at);
      if (d.getFullYear() === month.getFullYear() && d.getMonth() === month.getMonth()) {
        const arr = m.get(d.getDate()) ?? [];
        arr.push(e);
        m.set(d.getDate(), arr);
      }
    });
    return m;
  }, [cal.data, month]);

  const upcoming = useMemo(
    () => [...(cal.data ?? [])].filter((e) => new Date(e.at) >= new Date()).sort((a, b) => a.at.localeCompare(b.at)).slice(0, 8),
    [cal.data],
  );

  const daysInMonth = monthEnd.getDate();
  const firstOffset = (new Date(month.getFullYear(), month.getMonth(), 1).getDay() + 6) % 7;
  const today = new Date();
  const isToday = (day: number) => today.getFullYear() === month.getFullYear() && today.getMonth() === month.getMonth() && today.getDate() === day;
  const cells: (number | null)[] = [...Array(firstOffset).fill(null), ...Array.from({ length: daysInMonth }, (_, i) => i + 1)];
  const shift = (n: number) => { setSelectedDay(null); setMonth((m) => new Date(m.getFullYear(), m.getMonth() + n, 1)); };

  // The selected day's tasks (sorted by time) — drives the right-hand detail list.
  const dayTasks = useMemo(
    () => (selectedDay == null ? [] : [...(byDate.get(selectedDay) ?? [])].sort((a, b) => a.at.localeCompare(b.at))),
    [byDate, selectedDay],
  );
  const selectedDate = selectedDay == null ? null : new Date(month.getFullYear(), month.getMonth(), selectedDay);

  return (
    <div className="cal-layout">
      <div className="cal">
        <div className="cal-head">
          <span className="serif cal-month">{month.toLocaleDateString(undefined, { month: "long", year: "numeric" })}</span>
          <div className="row" style={{ gap: 4 }}>
            <button className="icon-btn" onClick={() => shift(-1)}><Icon.ChevronL size={16} /></button>
            <button className="icon-btn" onClick={() => { setSelectedDay(null); setMonth(new Date(today.getFullYear(), today.getMonth(), 1)); }} title="Today"><Icon.Dot size={16} /></button>
            <button className="icon-btn" onClick={() => shift(1)}><Icon.ChevronR size={16} /></button>
          </div>
        </div>
        <div className="cal-grid">
          {WEEKDAYS.map((w, i) => <div key={"h" + i} className="cal-dow">{w}</div>)}
          {cells.map((day, i) => {
            if (day == null) return <div key={i} className="cal-cell empty" />;
            const ev = byDate.get(day) ?? [];
            return (
              <button
                key={i}
                onClick={() => ev.length && setSelectedDay(day)}
                disabled={ev.length === 0}
                title={ev.map((e) => e.name).join(", ")}
                className={"cal-cell" + (isToday(day) ? " today" : "") + (selectedDay === day ? " selected" : "")}
                style={ev.length ? { cursor: "pointer" } : undefined}
              >
                <span className="cal-num">{day}</span>
                {ev.length > 0 && <span className="cal-dot" />}
              </button>
            );
          })}
        </div>
      </div>

      <div className="cal-upcoming">
        {selectedDate ? (
          <>
            <div className="up-head">
              <span className="side-label mono">Tasks on {selectedDate.toLocaleDateString(undefined, { day: "numeric", month: "long" })}</span>
              <button className="btn btn-line sm" onClick={() => setSelectedDay(null)}><Icon.ChevronL size={13} /> Upcoming</button>
            </div>
            {dayTasks.length === 0 && <p className="text-sm text-slate/70">No tasks this day.</p>}
            {dayTasks.map((e, i) => (
              <div key={i} className="up-row" onClick={() => onPick(e.automation_id)}>
                <span className="up-dot" />
                <div className="row-main">
                  <span className="row-title">{e.name}</span>
                  <span className="row-sub mono">{new Date(e.at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}</span>
                </div>
                <Icon.ChevronR size={15} />
              </div>
            ))}
          </>
        ) : (
          <>
            <span className="side-label mono">Upcoming</span>
            {cal.isLoading && <p className="text-sm text-slate">Loading…</p>}
            {!cal.isLoading && upcoming.length === 0 && <p className="text-sm text-slate/70">No upcoming runs this month.</p>}
            {upcoming.map((e, i) => (
              <div key={i} className="up-row" onClick={() => onPick(e.automation_id)}>
                <span className="up-dot" />
                <div className="row-main">
                  <span className="row-title">{e.name}</span>
                  <span className="row-sub mono">{new Date(e.at).toLocaleDateString(undefined, { day: "numeric", month: "short" })} {new Date(e.at).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}</span>
                </div>
                <Icon.ChevronR size={15} />
              </div>
            ))}
          </>
        )}
      </div>
    </div>
  );
}
