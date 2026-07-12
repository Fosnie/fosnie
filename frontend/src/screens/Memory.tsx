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
import { useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import {
  createFact,
  deleteFact,
  updateFact,
  useMemoryFacts,
  useProjects,
  useWhoami,
  type MemoryFact,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { PanelHead } from "@/components/editor";
import { settle } from "@/components/useBusy";

const MANAGER = ["power_user", "client_admin", "super_admin"];

function useBusy() {
  const [busy, setBusy] = useState(false);
  const run = async (fn: () => Promise<unknown>, success?: string) => {
    setBusy(true);
    const started = Date.now();
    try { await fn(); await settle(started); if (success) toast(success, { variant: "success" }); }
    catch (e) { toast((e as Error).message, { variant: "error" }); } finally { setBusy(false); }
  };
  return { busy, run };
}

export function Memory() {
  const qc = useQueryClient();
  const who = useWhoami();
  const projects = useProjects();
  const canModerateProject = MANAGER.includes(who.data?.role ?? "");
  const [projectId, setProjectId] = useState<string>("");
  const facts = useMemoryFacts(projectId || null);
  const { busy, run } = useBusy();

  const [content, setContent] = useState("");
  const [scope, setScope] = useState<"user" | "project">("user");

  const projectName = projects.data?.find((p) => p.id === projectId)?.name;
  const userFacts = useMemo(() => (facts.data ?? []).filter((f) => f.scope === "user"), [facts.data]);
  const projectFacts = useMemo(() => (facts.data ?? []).filter((f) => f.scope === "project"), [facts.data]);
  const refresh = () => qc.invalidateQueries({ queryKey: ["memory", projectId || null] });

  function add() {
    if (!content.trim()) return;
    run(() => createFact({ content: content.trim(), scope, project_id: scope === "project" ? projectId : undefined }).then(() => { setContent(""); refresh(); }), "Memory added.");
  }

  return (
    <div className="main-scroll">
      <div className="panel anim-on fade-in">
        <PanelHead
          title="Memory"
          sub="Durable facts the platform remembers — explicit only, editable, never shared."
          action={
            <Dropdown
              value={projectId}
              onChange={(v) => { setProjectId(v); if (!v) setScope("user"); }}
              ariaLabel="Memory scope project"
              icon={<Icon.Folder size={14} />}
              options={[
                { value: "", label: "Your memory only" },
                ...(projects.data ?? []).map((p) => ({ value: p.id, label: p.name })),
              ]}
            />
          }
        />

        {/* Add a fact */}
        <div className="ed-section" style={{ marginBottom: 22 }}>
          <label className="form-label">New fact</label>
          <div className="col-add">
            <input className="field" value={content} onChange={(e) => setContent(e.target.value)} onKeyDown={(e) => e.key === "Enter" && add()} placeholder="e.g. Prefers UK English" />
            <Dropdown
              value={scope}
              onChange={(v) => setScope(v)}
              ariaLabel="Fact scope"
              options={[
                { value: "user", label: "user" },
                { value: "project", label: "project", disabled: !projectId },
              ]}
            />
            <button className="btn btn-gold" onClick={add} disabled={busy || !content.trim()}><Icon.Plus size={15} /> Add</button>
          </div>
          <div className="ed-hint mono">Nothing is stored unless you add it here or ask in chat.</div>
        </div>

        {facts.isLoading ? (
          <p className="text-sm text-slate">Loading…</p>
        ) : (
          projectId ? (
            <Section
              title={`${projectName ?? "Project"} facts`}
              facts={projectFacts}
              canEdit={canModerateProject}
              busy={busy}
              run={run}
              refresh={refresh}
              empty="No project facts yet."
              note={canModerateProject ? undefined : "Read-only — only a power user or admin may moderate project memory."}
            />
          ) : (
            <Section title="Your facts" facts={userFacts} canEdit busy={busy} run={run} refresh={refresh} empty="No personal facts yet." />
          )
        )}
      </div>
    </div>
  );
}

function Section({
  title, facts, canEdit, busy, run, refresh, empty, note,
}: {
  title: string;
  facts: MemoryFact[];
  canEdit: boolean;
  busy: boolean;
  run: (fn: () => Promise<unknown>, success?: string) => void;
  refresh: () => void;
  empty: string;
  note?: string;
}) {
  return (
    <div className="prompt-group">
      <div className="prompt-group-head"><span className="side-label mono">{title}</span><span className="ed-hint mono">{facts.length}</span></div>
      {note && <p className="ed-hint mono" style={{ marginTop: 0, marginBottom: 10 }}>{note}</p>}
      {facts.length === 0 ? (
        <p className="text-sm text-slate/70">{empty}</p>
      ) : (
        <div className="rows">
          {facts.map((f) => <FactRow key={f.id} fact={f} canEdit={canEdit} busy={busy} run={run} refresh={refresh} />)}
        </div>
      )}
    </div>
  );
}

function FactRow({
  fact, canEdit, busy, run, refresh,
}: {
  fact: MemoryFact;
  canEdit: boolean;
  busy: boolean;
  run: (fn: () => Promise<unknown>, success?: string) => void;
  refresh: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(fact.content);

  return (
    <div className="mem-row">
      <span className="mem-cat mono">{fact.scope}{fact.pinned ? " · pinned" : ""}</span>
      {editing ? (
        <textarea className="field sm" rows={2} value={draft} onChange={(e) => setDraft(e.target.value)} style={{ resize: "vertical" }} />
      ) : (
        <p className="mem-text">{fact.content}{fact.user_edited && <span className="ml-2 text-[0.65rem] uppercase tracking-wide text-slate/60">edited</span>}</p>
      )}
      <div className="row" style={{ gap: 4 }}>
        {canEdit && (
          <button className="icon-btn" title={fact.pinned ? "Unpin" : "Pin"} disabled={busy} onClick={() => run(() => updateFact(fact.id, { pinned: !fact.pinned }).then(refresh))}><Icon.Pin size={14} /></button>
        )}
        {!canEdit && fact.pinned && <Icon.Pin size={14} className="text-gold" />}
        {canEdit && (editing ? (
          <>
            <button className="icon-btn" title="Save" disabled={busy || !draft.trim()} onClick={() => run(() => updateFact(fact.id, { content: draft.trim() }).then(() => { setEditing(false); refresh(); }), "Memory updated.")}><Icon.Check size={14} /></button>
            <button className="icon-btn" title="Cancel" onClick={() => { setEditing(false); setDraft(fact.content); }}><Icon.Close size={14} /></button>
          </>
        ) : (
          <>
            <button className="icon-btn" title="Edit" onClick={() => setEditing(true)}><Icon.Edit size={14} /></button>
            <button className="icon-btn" title="Delete" disabled={busy} onClick={async () => { if (await confirmDialog({ title: "Delete this fact?", danger: true, confirmLabel: "Delete" })) run(() => deleteFact(fact.id).then(refresh), "Memory deleted."); }}><Icon.Trash size={14} /></button>
          </>
        ))}
      </div>
    </div>
  );
}
