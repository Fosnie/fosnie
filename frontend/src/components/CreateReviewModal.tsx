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

// Create-tabular-review modal. Shared by the General Project Workspace and the
// Legal shell (both operate on a project = matter). Lives here so neither screen
// has to export a non-screen component (which breaks the lazy-screen typing).

import { toast } from "@/components/dialogs";
import { useMemo, useState } from "react";
import {
  createReview,
  useWorkspaceDocs,
  type CellFormat,
  type CellMechanism,
  type ColumnSpec,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";

// Lawyer-friendly labels for the answer types (the value stays the backend key).
const FORMATS: { value: CellFormat; label: string }[] = [
  { value: "text", label: "Text" },
  { value: "yes_no", label: "Yes / No" },
  { value: "date", label: "Date" },
  { value: "currency", label: "Currency" },
  { value: "monetary_amount", label: "Monetary amount" },
  { value: "number", label: "Number" },
  { value: "percentage", label: "Percentage" },
  { value: "tag", label: "Tag / category" },
  { value: "bulleted_list", label: "Bulleted list" },
];

function slug(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "col";
}

// Tabular review is always per-document RAG — a short prompt → a short, localisable
// answer, scoped to one document (08 §B.16). No whole-document stuffing here.
const MECHANISM: CellMechanism = "per_document_rag";

interface ColumnDraft { name: string; format: CellFormat; prompt: string; }
const BLANK_COLUMN: ColumnDraft = { name: "", format: "text", prompt: "" };

export function CreateReviewModal({ projectId, onClose, onCreated }: { projectId: string; onClose: () => void; onCreated: (id: string) => void }) {
  const wsDocs = useWorkspaceDocs(projectId);
  const [name, setName] = useState("");
  const [docIds, setDocIds] = useState<Set<string>>(new Set());
  const [columns, setColumns] = useState<ColumnDraft[]>([{ ...BLANK_COLUMN }]);
  const [submitting, setSubmitting] = useState(false);

  function toggleDoc(id: string) { setDocIds((prev) => { const next = new Set(prev); if (next.has(id)) next.delete(id); else next.add(id); return next; }); }
  function setCol(i: number, patch: Partial<ColumnDraft>) { setColumns((cs) => cs.map((c, j) => (j === i ? { ...c, ...patch } : c))); }

  const keyedColumns = useMemo<ColumnSpec[]>(() => {
    const seen = new Map<string, number>();
    return columns.map((c) => {
      const base = slug(c.name || "col");
      const n = (seen.get(base) ?? 0) + 1;
      seen.set(base, n);
      return { key: n === 1 ? base : `${base}_${n}`, name: c.name, format: c.format, prompt: c.prompt, mechanism: MECHANISM };
    });
  }, [columns]);

  const valid = name.trim() && docIds.size > 0 && columns.length > 0 && columns.every((c) => c.name.trim() && c.prompt.trim());

  async function submit() {
    if (!valid || submitting) return;
    setSubmitting(true);
    try { const { id } = await createReview({ project_id: projectId, name: name.trim(), document_ids: [...docIds], columns: keyedColumns }); onCreated(id); }
    catch (e) { toast(`Create review failed: ${(e as Error).message}`); setSubmitting(false); }
  }

  return (
    <div className="modal-scrim" onClick={onClose}>
      <div className="modal" style={{ width: 760, maxWidth: "100%" }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <div><div className="eyebrow">Tabular review</div><h2 className="serif modal-title">New tabular review</h2></div>
          <button className="icon-btn" onClick={onClose}><Icon.Close size={18} /></button>
        </div>
        <div className="modal-body">
          <label className="form-label">Name</label>
          <input className="field" value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Lease clause review" />

          <div className="review-cols">
            <div className="review-build">
              <label className="form-label">Documents <span className="opt">rows</span></label>
              {!wsDocs.data?.length ? <p className="ed-hint mono">No documents in this project.</p> : (
                <div className="kb-list scroll">
                  {wsDocs.data.map((d) => (
                    <button key={d.id} className={"kb-opt" + (docIds.has(d.id) ? " on" : "")} onClick={() => toggleDoc(d.id)}>
                      <span className="kb-check">{docIds.has(d.id) && <Icon.Check size={13} />}</span>
                      <Icon.Doc size={15} /><span className="kb-name">{d.original_filename}</span>
                    </button>
                  ))}
                </div>
              )}
            </div>

            <div className="review-build">
              <div className="proj-panel-head" style={{ marginBottom: 8 }}>
                <label className="form-label" style={{ margin: 0 }}>Columns <span className="opt">questions</span></label>
                <button className="btn btn-line sm" onClick={() => setColumns((cs) => [...cs, { ...BLANK_COLUMN }])}><Icon.Plus size={13} /> Add column</button>
              </div>
              <div className="col" style={{ gap: 12 }}>
                {columns.map((c, i) => (
                  <div key={i} className="ed-section" style={{ padding: 12 }}>
                    <div className="row" style={{ gap: 8, marginBottom: 8 }}>
                      <input className="field sm" style={{ flex: 1 }} value={c.name} onChange={(e) => setCol(i, { name: e.target.value })} placeholder="Column name (e.g. Parties)" />
                      <Dropdown value={c.format} onChange={(v) => setCol(i, { format: v as CellFormat })} ariaLabel="Column format" options={FORMATS.map((f) => ({ value: f.value, label: f.label }))} />
                      {columns.length > 1 && <button className="icon-btn" onClick={() => setColumns((cs) => cs.filter((_, j) => j !== i))} title="Remove"><Icon.Close size={14} /></button>}
                    </div>
                    <textarea className="field sm" rows={2} value={c.prompt} onChange={(e) => setCol(i, { prompt: e.target.value })} placeholder="What should the AI extract? e.g. Who are the parties?" style={{ resize: "none" }} />
                  </div>
                ))}
              </div>
            </div>
          </div>
        </div>
        <div className="modal-foot">
          <span className="review-summary mono">{docIds.size} docs × {columns.length} cols = {docIds.size * columns.length} cells</span>
          <button className="btn btn-ghost" onClick={onClose}>Cancel</button>
          <button className="btn btn-gold" onClick={submit} disabled={!valid || submitting}>{submitting ? "Creating…" : "Create review"}</button>
        </div>
      </div>
    </div>
  );
}
