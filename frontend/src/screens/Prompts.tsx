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

import { toast } from "@/components/dialogs";
import { useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate, useParams } from "react-router-dom";
import {
  createPrompt,
  renderPrompt,
  useAgents,
  usePrompt,
  usePrompts,
  useProjects,
  useWhoami,
  type PromptScope,
  type PromptVariable,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { EditorShell, PanelHead } from "@/components/editor";
import { settle } from "@/components/useBusy";
import { TemplateEditor } from "@/components/TemplateEditor";
import { PromptFillForm } from "@/components/PromptFillForm";

const SCOPE_ORDER: PromptScope[] = ["personal", "project", "global"];
const MANAGER = ["power_user", "client_admin", "super_admin"];

export function Prompts() {
  const { promptId } = useParams();
  const nav = useNavigate();
  const prompts = usePrompts();
  const [creating, setCreating] = useState(false);

  if (creating) return <PromptCreate onBack={() => setCreating(false)} onCreated={(id) => { setCreating(false); nav(`/studio/prompts/${id}`); }} />;
  if (promptId) return <PromptView key={promptId} id={promptId} onBack={() => nav("/studio/prompts")} />;

  const groups = SCOPE_ORDER
    .map((s) => [s, (prompts.data ?? []).filter((p) => p.scope === s)] as const)
    .filter(([, l]) => l.length);

  return (
    <div className="main-scroll">
      <div className="panel anim-on fade-in">
        <PanelHead
          title="Prompts"
          sub="A shared library of vetted prompts your team can reuse and refine."
          action={<button className="btn btn-gold" onClick={() => setCreating(true)}><Icon.Plus size={16} /> New prompt</button>}
        />
        {prompts.isLoading && <p className="text-sm text-slate">Loading…</p>}
        {prompts.data?.length === 0 && <p className="text-sm text-slate/70">No prompts yet.</p>}
        {groups.map(([scope, list]) => (
          <div key={scope} className="prompt-group">
            <div className="prompt-group-head">
              <span className="side-label mono">{scope}</span>
              <span className="ed-hint mono">{list.length}</span>
            </div>
            <div className="card-grid">
              {list.map((p) => (
                <div key={p.id} className="prompt-card" style={{ cursor: "pointer" }} onClick={() => nav(`/studio/prompts/${p.id}`)}>
                  <div className="prompt-tag mono">{p.scope}</div>
                  <h3 className="serif prompt-title">{p.name}</h3>
                  <p className="prompt-body">Open to fill placeholders and render.</p>
                  <div className="prompt-foot mono"><Icon.Copy size={14} /> Use prompt</div>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

// Existing prompts are immutable → this is the fill + render view.
function PromptView({ id, onBack }: { id: string; onBack: () => void }) {
  const detail = usePrompt(id);
  const [values, setValues] = useState<Record<string, string>>({});
  const [rendered, setRendered] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (detail.isError) return <div className="main-scroll"><div className="panel">Couldn't load this prompt. <button className="underline" onClick={() => detail.refetch()}>Retry</button></div></div>;
  if (detail.isLoading || !detail.data) return <div className="main-scroll"><div className="panel">Loading…</div></div>;
  const p = detail.data;

  async function doRender() {
    setBusy(true);
    try { const { content } = await renderPrompt(id, values); setRendered(content); }
    catch (e) { toast(`Render failed: ${(e as Error).message}`); }
    finally { setBusy(false); }
  }

  return (
    <EditorShell eyebrow="Prompt" title={p.name} onBack={onBack}>
      <div className="editor-grid">
        <div className="editor-main">
          <section className="ed-section">
            <h4>Template</h4>
            <div className="ph-preview">{p.content}</div>
            <div className="ed-hint mono">Immutable — create a new prompt to change it. Use it in chat via the composer's attach button.</div>
          </section>
        </div>
        <aside className="editor-side">
          <section className="ed-section">
            <h4>Preview</h4>
            <PromptFillForm placeholders={p.placeholders} variables={p.variables} values={values} onChange={(k, val) => setValues((v) => ({ ...v, [k]: val }))} />
            <button className="btn btn-line sm full-btn" onClick={doRender} disabled={busy}><Icon.Play size={13} /> {busy ? "Rendering…" : "Render preview"}</button>
            {rendered != null && (
              <>
                <div className="proj-panel-head" style={{ margin: "14px 0 8px" }}>
                  <span className="form-label" style={{ margin: 0 }}>Rendered</span>
                  <button className="icon-btn" onClick={() => navigator.clipboard?.writeText(rendered)} title="Copy"><Icon.Copy size={14} /></button>
                </div>
                <div className="ph-preview">{rendered}</div>
              </>
            )}
          </section>
        </aside>
      </div>
    </EditorShell>
  );
}

function PromptCreate({ onBack, onCreated }: { onBack: () => void; onCreated: (id: string) => void }) {
  const qc = useQueryClient();
  const who = useWhoami();
  const canScope = MANAGER.includes(who.data?.role ?? "");
  const projects = useProjects();
  const agents = useAgents();
  const [name, setName] = useState("");
  const [content, setContent] = useState("");
  const [vars, setVars] = useState<PromptVariable[]>([]);
  const [scope, setScope] = useState<PromptScope>("personal");
  const [projectId, setProjectId] = useState("");
  const [agentId, setAgentId] = useState("");
  const [fills, setFills] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);

  const placeholders = useMemo(() => {
    const m = content.match(/\{\{(\w+)\}\}/g) ?? [];
    return [...new Set(m.map((x) => x.replace(/[{}]/g, "")))];
  }, [content]);
  const filled = content.replace(/\{\{(\w+)\}\}/g, (_, k) => fills[k] || `{{${k}}}`);
  const valid = name.trim() && content.trim() && (scope !== "project" || projectId);

  async function submit() {
    if (!valid || busy) return;
    setBusy(true);
    const started = Date.now();
    try {
      const { id } = await createPrompt({ name: name.trim(), content, variables: vars, scope, project_id: scope === "project" ? projectId : undefined, agent_id: agentId || undefined });
      await settle(started);
      qc.invalidateQueries({ queryKey: ["prompts"] });
      toast(`Prompt “${name.trim()}” created.`, { variant: "success" });
      onCreated(id);
    } catch (e) { toast(`Create failed: ${(e as Error).message}`, { variant: "error" }); setBusy(false); }
  }

  return (
    <EditorShell
      eyebrow="New prompt"
      title={name || "New prompt"}
      onBack={onBack}
      actions={
        <>
          <button className="btn btn-ghost sm" onClick={onBack}>Cancel</button>
          <button className="btn btn-gold sm" onClick={submit} disabled={!valid || busy}><Icon.Save size={14} /> {busy ? "Creating…" : "Save prompt"}</button>
        </>
      }
    >
      <div className="editor-grid">
        <div className="editor-main">
          <section className="ed-section">
            <label className="form-label">Prompt name</label>
            <input className="field" value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Clause summariser" />

            <div className="two-col" style={{ marginTop: 18 }}>
              <div>
                <label className="form-label">Scope</label>
                <div className="seg">
                  {(["personal", "project", "global"] as PromptScope[]).map((s) => (
                    <button key={s} className={"seg-opt" + (scope === s ? " on" : "")} disabled={s !== "personal" && !canScope} onClick={() => setScope(s)}>{s}</button>
                  ))}
                </div>
              </div>
              <div>
                <label className="form-label">Default agent <span className="opt">optional</span></label>
                <Dropdown
                  value={agentId}
                  onChange={setAgentId}
                  ariaLabel="Default agent"
                  fullWidth
                  icon={<Icon.Agents size={14} />}
                  options={[
                    { value: "", label: "None" },
                    ...(agents.data ?? []).map((a) => ({ value: a.id, label: a.name })),
                  ]}
                />
              </div>
            </div>

            {scope === "project" && (
              <>
                <label className="form-label">Project</label>
                <Dropdown
                  value={projectId}
                  onChange={setProjectId}
                  ariaLabel="Project"
                  fullWidth
                  icon={<Icon.Folder size={14} />}
                  options={[
                    { value: "", label: "Select…" },
                    ...(projects.data ?? []).map((p) => ({ value: p.id, label: p.name })),
                  ]}
                />
              </>
            )}

            <label className="form-label">Template</label>
            <TemplateEditor onChange={(c, v) => { setContent(c); setVars(v); }} />
            <div className="ed-hint mono">Drop in fields people fill when they use the prompt. Immutable once saved.</div>
          </section>
        </div>

        <aside className="editor-side">
          <section className="ed-section">
            <h4>Preview</h4>
            <PromptFillForm placeholders={placeholders} variables={vars} values={fills} onChange={(k, val) => setFills((f) => ({ ...f, [k]: val }))} />
            <div className="ph-preview" style={{ marginTop: 12 }}>{filled || "Your rendered prompt will appear here."}</div>
          </section>
        </aside>
      </div>
    </EditorShell>
  );
}
