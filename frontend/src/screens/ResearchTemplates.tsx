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

// Deep Research report templates, in Studio. A template sets the structure and
// writing style of a report: its section skeleton, an outline mode (fixed
// structure vs a structure that follows the question) and writing instructions
// prepended to the writer. It does NOT change search depth, budgets or
// verification. The four built-ins are read-only starting points; users fork one
// with Duplicate and edit the copy. Personal by default; publishing one
// deployment-wide (global) needs the research.templates.manage permission because
// its writing instructions then run for other people.

import { useEffect, useRef, useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  archiveResearchTemplate,
  createResearchTemplate,
  updateResearchTemplate,
  useResearchTemplate,
  useResearchTemplates,
  useWhoami,
  type ResearchRefineParams,
  type ResearchTemplateSection,
} from "@/api/client";
import { confirmDialog, toast } from "@/components/dialogs";
import { EditorShell } from "@/components/editor";
import { Icon } from "@/components/icons";
import { TemplatePreview } from "@/components/TemplatePreview";

// Mirror of the backend limits (the backend is the source of truth; these drive
// the editor hints and cheap client-side guards).
const MAX_SECTIONS = 12;
const MAX_LABEL = 60;
const MAX_HEADING = 120;
const MAX_BRIEF = 300;
const MAX_WRITING = 4000;
const RESERVED_ANALYSIS_HEADING = "Consensus, contradictions and gaps";

const ADMIN_ROLES = ["client_admin", "super_admin"];

type OutlineMode = "constrained" | "free";
type Scope = "personal" | "global";

export function ResearchTemplates() {
  const location = useLocation();
  const nav = useNavigate();
  const st = location.state as { returnTo?: string; refine?: ResearchRefineParams } | null;
  const returnTo = st?.returnTo;
  const refine = st?.refine;
  // "Back to research" carries the question (and the freshly created template, if
  // any) back to the Deep Research home so a typed question is not lost.
  const backToResearch = returnTo
    ? (templateId?: string) =>
        nav(returnTo, {
          state: {
            refine: templateId && refine ? { ...refine, template: templateId } : refine,
          },
        })
    : undefined;

  return <TemplatesManager backToResearch={backToResearch} />;
}

// ── Manager (card grid) ──
function TemplatesManager({ backToResearch }: { backToResearch?: (id?: string) => void }) {
  const qc = useQueryClient();
  const cat = useResearchTemplates();
  const [sel, setSel] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const refresh = () => qc.invalidateQueries({ queryKey: ["research", "templates"] });

  async function duplicate(id: string) {
    try {
      const { id: newId } = await createResearchTemplate({ duplicate_of: id });
      refresh();
      setSel(newId);
      toast("Copied to an editable template.", { variant: "success" });
    } catch (e) {
      toast(`Duplicate failed: ${(e as Error).message}`, { variant: "error" });
    }
  }

  if (creating)
    return (
      <TemplateEditor
        backToResearch={backToResearch}
        onBack={() => setCreating(false)}
        onSaved={(id) => { setCreating(false); setSel(id); refresh(); }}
        onDeleted={() => setCreating(false)}
      />
    );
  if (sel)
    return (
      <TemplateEditor
        key={sel}
        templateId={sel}
        backToResearch={backToResearch}
        onBack={() => setSel(null)}
        onSaved={refresh}
        onDeleted={() => { setSel(null); refresh(); }}
      />
    );

  return (
    <div className="main-scroll">
      <div className="panel">
        <div className="proj-panel-head" style={{ marginBottom: 16 }}>
          <span className="side-label mono">Report templates · structure and writing style for Deep Research</span>
          <div style={{ display: "flex", gap: 8 }}>
            {backToResearch && (
              <button className="btn btn-ghost sm" onClick={() => backToResearch()}>
                <Icon.ChevronL size={14} /> Back to research
              </button>
            )}
            <button className="btn btn-gold sm" onClick={() => setCreating(true)}>
              <Icon.Plus size={14} /> New template
            </button>
          </div>
        </div>

        {cat.isLoading && <p className="text-sm text-slate">Loading…</p>}

        {cat.data && (
          <>
            <p className="side-label mono" style={{ marginBottom: 8 }}>Built-in</p>
            <div className="card-grid">
              {cat.data.builtin.map((t) => (
                <TemplateCard
                  key={t.id}
                  name={t.label}
                  description={t.description}
                  badge="built-in"
                  onDuplicate={() => duplicate(t.id)}
                />
              ))}
            </div>

            <p className="side-label mono" style={{ margin: "20px 0 8px" }}>Your templates</p>
            {cat.data.custom.length === 0 && (
              <p className="text-sm text-slate/70">No custom templates yet. Duplicate a built-in to start.</p>
            )}
            <div className="card-grid">
              {cat.data.custom.map((t) => (
                <TemplateCard
                  key={t.id}
                  name={t.label}
                  description={t.description}
                  badge={t.scope}
                  canManage={t.can_manage}
                  onOpen={() => setSel(t.id)}
                  onDuplicate={() => duplicate(t.id)}
                  onDelete={async () => {
                    if (!(await confirmDialog({ title: `Delete template "${t.label}"?`, body: "Existing reports that used it keep working; it is removed from the picker.", danger: true, confirmLabel: "Delete" }))) return;
                    try { await archiveResearchTemplate(t.id); refresh(); toast("Template deleted.", { variant: "success" }); }
                    catch (e) { toast(`Delete failed: ${(e as Error).message}`, { variant: "error" }); }
                  }}
                />
              ))}
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function TemplateCard({
  name, description, badge, canManage, onOpen, onDuplicate, onDelete,
}: {
  name: string;
  description: string;
  badge: string;
  canManage?: boolean;
  onOpen?: () => void;
  onDuplicate: () => void;
  onDelete?: () => void;
}) {
  const [menu, setMenu] = useState(false);
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    function onDoc(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) setMenu(false);
    }
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, []);
  const clickable = !!onOpen;
  return (
    <div className="agent-card" style={{ cursor: clickable ? "pointer" : "default" }} onClick={() => onOpen?.()}>
      <div className="agent-card-top">
        <span className="agent-glyph lg"><Icon.Template size={16} /></span>
        <div className="menu-wrap" ref={ref}>
          <button className="ghost-dots" title="Actions" onClick={(e) => { e.stopPropagation(); setMenu((m) => !m); }}><Icon.Dots size={16} /></button>
          {menu && (
            <div className="menu fade-up">
              {onOpen && canManage && <button className="menu-item" onClick={(e) => { e.stopPropagation(); setMenu(false); onOpen(); }}><Icon.Edit size={15} /> Edit</button>}
              <button className="menu-item" onClick={(e) => { e.stopPropagation(); setMenu(false); onDuplicate(); }}><Icon.Copy size={15} /> Duplicate</button>
              {onDelete && canManage && <button className="menu-item danger" onClick={(e) => { e.stopPropagation(); setMenu(false); onDelete(); }}><Icon.Trash size={15} /> Delete</button>}
            </div>
          )}
        </div>
      </div>
      <h3 className="serif agent-card-name">{name}</h3>
      <p className="agent-card-desc">{description || "—"}</p>
      <div className="agent-card-foot mono"><span>{badge}</span></div>
    </div>
  );
}

// ── Editor ──
function TemplateEditor({
  templateId, onBack, onSaved, onDeleted, backToResearch,
}: {
  templateId?: string;
  onBack: () => void;
  onSaved: (id: string) => void;
  onDeleted: () => void;
  backToResearch?: (id?: string) => void;
}) {
  const detail = useResearchTemplate(templateId);
  const who = useWhoami();
  const isAdmin = ADMIN_ROLES.includes(who.data?.role ?? "");
  const perms = who.data?.permissions ?? [];
  const holds = (p: string) => isAdmin || perms.includes(p) || perms.includes(`${p}:scoped`);
  const canManageGlobal = holds("research.templates.manage");
  const canManage = templateId ? (detail.data?.can_manage ?? false) : true;
  const isNew = !templateId;

  const [label, setLabel] = useState("");
  const [description, setDescription] = useState("");
  const [outlineMode, setOutlineMode] = useState<OutlineMode>("constrained");
  const [sections, setSections] = useState<ResearchTemplateSection[]>([]);
  const [writing, setWriting] = useState("");
  const [scope, setScope] = useState<Scope>("personal");
  const [busy, setBusy] = useState<string | null>(null);
  const [showErrors, setShowErrors] = useState(false);
  // The id to hand back to Deep Research once saved (or the existing one).
  const [savedId, setSavedId] = useState<string | undefined>(templateId);

  useEffect(() => {
    if (detail.data) {
      setLabel(detail.data.label);
      setDescription(detail.data.description);
      setOutlineMode(detail.data.outline_mode);
      setSections(detail.data.skeleton);
      setWriting(detail.data.writing_instructions);
      setScope(detail.data.scope);
    }
  }, [detail.data]);

  if (templateId && detail.isLoading) return <div className="main-scroll"><div className="panel">Loading…</div></div>;

  const constrained = outlineMode === "constrained";

  const setRow = (i: number, patch: Partial<ResearchTemplateSection>) =>
    setSections((r) => r.map((row, j) => (j === i ? { ...row, ...patch } : row)));
  const addRow = () =>
    setSections((r) => (r.length >= MAX_SECTIONS ? r : [...r, { heading: "", brief: "", expandable: false, exec_summary: false }]));
  const delRow = (i: number) => setSections((r) => r.filter((_, j) => j !== i));
  const move = (i: number, d: -1 | 1) =>
    setSections((r) => {
      const j = i + d;
      if (j < 0 || j >= r.length) return r;
      const next = [...r];
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });

  // Cheap client-side validation for the hint banner; the backend re-validates.
  function missing(): string[] {
    const m: string[] = [];
    if (!label.trim()) m.push("Name");
    if (constrained && sections.length === 0) m.push("At least one section");
    if (sections.some((s) => !s.heading.trim())) m.push("Every section needs a heading");
    if (constrained && sections.filter((s) => s.exec_summary).length > 1) m.push("Only one executive summary");
    if (
      constrained &&
      sections.some((s) => s.exec_summary && s.heading.trim().toLowerCase() === RESERVED_ANALYSIS_HEADING.toLowerCase())
    )
      m.push("The corpus-analysis heading cannot be the executive summary");
    return m;
  }

  async function save() {
    if (!canManage) return;
    const miss = missing();
    if (miss.length) { setShowErrors(true); return; }
    setShowErrors(false);
    setBusy("save");
    // In free mode the per-section flags are inert; clear them so what is saved
    // matches what runs (the backend normalises the same way).
    const skeleton = sections.map((s) => ({
      heading: s.heading.trim(),
      brief: s.brief,
      expandable: constrained && s.expandable,
      exec_summary: constrained && s.exec_summary,
    }));
    const body = { label: label.trim(), description, skeleton, writing_instructions: writing, outline_mode: outlineMode, scope };
    try {
      let id = templateId;
      if (templateId) await updateResearchTemplate(templateId, body);
      else id = (await createResearchTemplate(body)).id;
      setSavedId(id);
      toast(templateId ? "Template saved." : `Template “${label.trim()}” created.`, { variant: "success" });
      onSaved(id!);
    } catch (e) {
      toast(`Save failed: ${(e as Error).message}`, { variant: "error" });
    } finally {
      setBusy(null);
    }
  }

  async function remove() {
    if (!templateId || !canManage) return;
    if (!(await confirmDialog({ title: `Delete template "${label}"?`, danger: true, confirmLabel: "Delete" }))) return;
    setBusy("del");
    try { await archiveResearchTemplate(templateId); toast("Template deleted.", { variant: "success" }); onDeleted(); }
    catch (e) { toast(`Delete failed: ${(e as Error).message}`, { variant: "error" }); setBusy(null); }
  }

  const miss = showErrors ? missing() : [];
  const previewStructure = sections.map((s) => s.heading).filter((h) => h.trim());

  return (
    <EditorShell
      eyebrow={isNew ? "New template" : "Edit template"}
      title={isNew ? "New template" : label || "Template"}
      onBack={onBack}
      actions={canManage ? (
        <>
          {backToResearch && (
            <button className="btn btn-ghost sm" onClick={() => backToResearch(savedId)}>
              <Icon.ChevronL size={14} /> Back to research
            </button>
          )}
          {!isNew && <button className="btn btn-ghost sm" onClick={remove} disabled={!!busy}>{busy === "del" ? "Deleting…" : "Delete"}</button>}
          <button className="btn btn-gold sm" onClick={save} disabled={!!busy}><Icon.Save size={14} /> {busy === "save" ? "Saving…" : "Save template"}</button>
        </>
      ) : (backToResearch ? (
        <button className="btn btn-ghost sm" onClick={() => backToResearch(savedId)}><Icon.ChevronL size={14} /> Back to research</button>
      ) : undefined)}
    >
      {!canManage && <div className="ed-section">Read-only — this template belongs to someone else.</div>}
      <div className="editor-grid">
        <div className="editor-main">
          <section className="ed-section">
            <h4>Definition</h4>
            <label className="form-label">Name</label>
            <input className={"field" + (showErrors && !label.trim() ? " field-error" : "")} value={label} maxLength={MAX_LABEL} onChange={(e) => setLabel(e.target.value)} disabled={!canManage} placeholder="e.g. Board memo" />
            <label className="form-label">Description</label>
            <input className="field" value={description} onChange={(e) => setDescription(e.target.value)} disabled={!canManage} placeholder="One line shown in the picker" />
          </section>

          <section className="ed-section">
            <h4>Structure mode</h4>
            <label className="flex items-center gap-2 text-sm" style={{ cursor: canManage ? "pointer" : "default" }}>
              <input type="radio" name="outline-mode" checked={constrained} disabled={!canManage} onChange={() => setOutlineMode("constrained")} />
              <span><strong>Fixed structure</strong> — the headings below are kept, in order.</span>
            </label>
            <label className="flex items-center gap-2 text-sm" style={{ cursor: canManage ? "pointer" : "default", marginTop: 6 }}>
              <input type="radio" name="outline-mode" checked={!constrained} disabled={!canManage} onChange={() => setOutlineMode("free")} />
              <span><strong>Structure follows the question</strong> — the headings below are only a starting point; the model may restructure.</span>
            </label>
          </section>

          <section className="ed-section">
            <h4>Structure</h4>
            <div className="ed-hint mono">Up to {MAX_SECTIONS} sections, in report order.</div>
            {sections.map((s, i) => (
              <div key={i} className="ed-row" style={{ display: "flex", flexDirection: "column", gap: 6, borderTop: i ? "1px solid var(--hairline, rgba(255,255,255,0.08))" : undefined, paddingTop: i ? 10 : 0, marginTop: i ? 10 : 0 }}>
                <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
                  <input
                    className={"field" + (showErrors && !s.heading.trim() ? " field-error" : "")}
                    style={{ flex: 1 }}
                    value={s.heading}
                    maxLength={MAX_HEADING}
                    onChange={(e) => setRow(i, { heading: e.target.value })}
                    disabled={!canManage}
                    placeholder="Section heading"
                  />
                  <button className="btn btn-ghost sm" title="Move up" disabled={!canManage || i === 0} onClick={() => move(i, -1)}><Icon.ChevronL size={13} style={{ transform: "rotate(90deg)" }} /></button>
                  <button className="btn btn-ghost sm" title="Move down" disabled={!canManage || i === sections.length - 1} onClick={() => move(i, 1)}><Icon.ChevronL size={13} style={{ transform: "rotate(-90deg)" }} /></button>
                  <button className="btn btn-ghost sm" title="Remove" disabled={!canManage} onClick={() => delRow(i)}><Icon.Close size={13} /></button>
                </div>
                <input
                  className="field sm"
                  value={s.brief}
                  maxLength={MAX_BRIEF}
                  onChange={(e) => setRow(i, { brief: e.target.value })}
                  disabled={!canManage}
                  placeholder="One sentence: what this section covers"
                />
                {constrained && (
                  <div style={{ display: "flex", flexWrap: "wrap", gap: 14 }}>
                    <label className="flex items-center gap-2 text-xs text-slate" style={{ cursor: canManage ? "pointer" : "default" }}>
                      <input type="checkbox" checked={s.expandable} disabled={!canManage} onChange={(e) => setRow(i, { expandable: e.target.checked })} />
                      May expand into several sections
                    </label>
                    <label className="flex items-center gap-2 text-xs text-slate" style={{ cursor: canManage ? "pointer" : "default" }}>
                      <input type="checkbox" checked={s.exec_summary} disabled={!canManage} onChange={(e) => setRow(i, { exec_summary: e.target.checked })} />
                      Executive summary (written last, after every other section)
                    </label>
                  </div>
                )}
              </div>
            ))}
            {canManage && (
              <button className="btn btn-line sm" style={{ marginTop: 10 }} onClick={addRow} disabled={sections.length >= MAX_SECTIONS}>
                <Icon.Plus size={13} /> Add section
              </button>
            )}
          </section>

          <section className="ed-section">
            <h4>Writing style</h4>
            <textarea className="field" rows={6} value={writing} maxLength={MAX_WRITING} onChange={(e) => setWriting(e.target.value)} disabled={!canManage} placeholder="Tone, voice, citation discipline…" />
            <div className="ed-hint mono">Prepended verbatim to the writer's instructions for every section.</div>
          </section>

          <section className="ed-section">
            <h4>Visibility</h4>
            <label className="flex items-center gap-2 text-sm" style={{ cursor: canManage ? "pointer" : "default" }}>
              <input type="radio" name="scope" checked={scope === "personal"} disabled={!canManage} onChange={() => setScope("personal")} />
              Personal — only you
            </label>
            <label className="flex items-center gap-2 text-sm" style={{ cursor: canManage && canManageGlobal ? "pointer" : "default", marginTop: 6, opacity: canManageGlobal ? 1 : 0.6 }}>
              <input type="radio" name="scope" checked={scope === "global"} disabled={!canManage || !canManageGlobal} onChange={() => setScope("global")} />
              Everyone in this deployment
            </label>
            {!canManageGlobal && (
              <div className="ed-hint mono">Publishing deployment-wide needs the “research.templates.manage” permission (its writing style would run for other people).</div>
            )}
          </section>
        </div>

        <aside className="editor-side">
          <section className="ed-section">
            <h4>Preview</h4>
            {/* The exact preview the picker will show for this template. */}
            <TemplatePreview description={description} structure={previewStructure} outlineMode={outlineMode} />
            {miss.length > 0 && (
              <div className="ed-hint mono" style={{ color: "var(--danger, #ff8080)", marginTop: 10 }}>
                {miss.join(" · ")}
              </div>
            )}
          </section>
        </aside>
      </div>
    </EditorShell>
  );
}

export default ResearchTemplates;
