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
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  cancelReview,
  exportReview,
  rerunCell,
  rerunErrors,
  runReview,
  useReview,
  type Cell,
  type CellStatus,
  type ReviewDetail,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { wsStore } from "@/ws/store";
import type { Citation, ServerFrame } from "@/ws/protocol";
import { CitationPanel } from "@/components/CitationPanel";

function cellKey(documentId: string, columnKey: string): string {
  return `${documentId}|${columnKey}`;
}

/** Render an arbitrary cell value (shaped by the column format) to a string. */
function valueText(v: unknown): string {
  if (v == null) return "";
  if (typeof v === "string") return v;
  if (Array.isArray(v)) return v.map(valueText).join(", ");
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

// Tidy a raw cell error for display: drop the httpx/requests boilerplate that
// tacks on an MDN status-page link, and give the common "image sent to the text
// embedder" 400 a plain-English explanation. Keeps the rest of the message intact.
function cleanError(raw: string | null | undefined): string {
  if (!raw || !raw.trim()) return "Unknown error.";
  let s = raw
    .replace(/\s*For more information,? check:?\s*https?:\/\/developer\.mozilla\.org\/\S*/gi, "")
    .replace(/\s+/g, " ")
    .trim();
  if (/\/v1\/embeddings/i.test(raw) && /\b400\b/.test(raw)) {
    s = "This file could not be read as text for review — image or scanned files need a document with extractable text (or OCR enabled).";
  }
  return s || "Unknown error.";
}

export function TabularReview() {
  const { projectId, reviewId } = useParams();
  const nav = useNavigate();
  const qc = useQueryClient();
  const review = useReview(reviewId);
  const [busy, setBusy] = useState<string | null>(null);
  const [selected, setSelected] = useState<{ documentId: string; columnKey: string } | null>(null);

  const data = review.data;
  const running = data?.status === "running";

  const cellMap = useMemo(() => {
    const m = new Map<string, Cell>();
    data?.cells.forEach((c) => m.set(cellKey(c.document_id, c.column_key), c));
    return m;
  }, [data?.cells]);

  // ── Live updates over the WS ──
  // Frames carry status only (per the contract) → coalesce the burst and refetch.
  const debounce = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (!reviewId) return;
    const refetch = () => qc.invalidateQueries({ queryKey: ["review", reviewId] });
    return wsStore.onFrame((f: ServerFrame) => {
      const rid = (f as { review_id?: string }).review_id;
      if (rid !== reviewId) return;
      if (f.type === "tabular.cell") {
        if (debounce.current) clearTimeout(debounce.current);
        debounce.current = setTimeout(refetch, 400);
      } else if (f.type === "tabular.complete") {
        if (debounce.current) clearTimeout(debounce.current);
        refetch();
      }
    });
  }, [reviewId, qc]);

  useEffect(() => () => { if (debounce.current) clearTimeout(debounce.current); }, []);

  async function act(label: string, fn: () => Promise<unknown>) {
    setBusy(label);
    try {
      await fn();
      await qc.invalidateQueries({ queryKey: ["review", reviewId] });
    } catch (e) {
      toast(`${label} failed: ${(e as Error).message}`);
    } finally {
      setBusy(null);
    }
  }

  if (review.isLoading) return <div className="main-scroll"><div className="panel">Loading review…</div></div>;
  if (review.isError || !data) {
    return (
      <div className="main-scroll">
        <div className="panel" style={{ color: "var(--red)" }}>
          Could not load review. {(review.error as Error | undefined)?.message}
        </div>
      </div>
    );
  }

  const errorCount = data.cells.filter((c) => c.status === "error").length;
  const badge = data.status === "complete" || data.status === "done" ? "complete" : data.status === "running" ? "running" : "draft";

  return (
    <div className="tab-wrap">
      <button className="back-bar" onClick={() => nav(`/p/${projectId}`)}><Icon.ChevronL size={15} /> Back to project</button>

      <div className="tab-head">
        <div>
          <div className="eyebrow">Tabular review</div>
          <h2 className="serif tab-title">{data.name}</h2>
        </div>
        <div className="tab-actions">
          <span className={"badge " + badge}>{data.status}</span>
          <button
            onClick={() => act("Rerun errors", () => rerunErrors(reviewId!))}
            disabled={!!busy || errorCount === 0}
            title={errorCount === 0 ? "No errored cells" : `${errorCount} errored`}
            className="btn btn-ghost"
          >
            <Icon.Refresh size={14} /> Rerun errors{errorCount > 0 ? ` (${errorCount})` : ""}
          </button>
          <button onClick={() => act("Export", () => exportReview(reviewId!, data.name))} disabled={!!busy} className="btn btn-ghost">
            <Icon.Download size={14} /> {busy === "Export" ? "Exporting…" : "Export"}
          </button>
          {running ? (
            <button onClick={() => act("Cancel", () => cancelReview(reviewId!))} disabled={busy === "Cancel"} className="btn btn-line">
              <Icon.Stop size={14} /> Stop
            </button>
          ) : (
            <button onClick={() => act("Run", () => runReview(reviewId!))} disabled={!!busy} className="btn btn-gold">
              <Icon.Play size={14} /> {busy === "Run" ? "Starting…" : "Run"}
            </button>
          )}
        </div>
      </div>

      <div className="tab-scroll">
        <table className="tab-grid">
          <thead>
            <tr>
              <th className="th-doc">Document</th>
              {data.columns.map((c) => (
                <th key={c.key} title={c.prompt}>{c.name}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {data.documents.map((d) => (
              <tr key={d.id}>
                <td className="td-doc"><Icon.Doc size={15} /> <span className="max-w-[16rem] truncate">{d.filename}</span></td>
                {data.columns.map((col) => {
                  const cell = cellMap.get(cellKey(d.id, col.key));
                  const status: CellStatus = cell?.status ?? "pending";
                  const clickable = status === "done" || status === "error";
                  const isSel = selected?.documentId === d.id && selected?.columnKey === col.key;
                  return (
                    <td
                      key={col.key}
                      onClick={clickable ? () => setSelected({ documentId: d.id, columnKey: col.key }) : undefined}
                      className={"cell" + (clickable ? " s-done" : "") + (isSel ? " sel" : "")}
                    >
                      <CellBody status={status} cell={cell} />
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
        {data.documents.length === 0 && <p className="px-6 py-4 text-sm text-slate/70">No documents in this review.</p>}
      </div>

      <div className={"cell-drawer" + (selected ? " open" : "")}>
        {selected && (
          <CellDetail
            review={data}
            documentId={selected.documentId}
            columnKey={selected.columnKey}
            cell={cellMap.get(cellKey(selected.documentId, selected.columnKey))}
            projectId={projectId}
            onClose={() => setSelected(null)}
            onRerun={async () => {
              await act("Rerun cell", () => rerunCell(reviewId!, selected.documentId, selected.columnKey));
              setSelected(null);
            }}
          />
        )}
      </div>
    </div>
  );
}

function CellBody({ status, cell }: { status: CellStatus; cell: Cell | undefined }) {
  if (status === "pending") return <span className="cell-state pending">pending</span>;
  if (status === "running") return <span className="cell-state running"><span className="cs-spin" /> working</span>;
  if (status === "error") return <span className="cell-state error">error</span>;
  const text = valueText(cell?.value);
  return <span className="cell-val line-clamp-3 whitespace-pre-wrap">{text || "—"}</span>;
}

function CellDetail({
  review, documentId, columnKey, cell, projectId, onClose, onRerun,
}: {
  review: ReviewDetail;
  documentId: string;
  columnKey: string;
  cell: Cell | undefined;
  projectId?: string;
  onClose: () => void;
  onRerun: () => void;
}) {
  const doc = review.documents.find((d) => d.id === documentId);
  const col = review.columns.find((c) => c.key === columnKey);
  const citations = (cell?.citations ?? []) as Citation[];
  const [selCitation, setSelCitation] = useState<Citation | null>(null);

  return (
    <div>
      <div className="drawer-head">
        <div>
          <div className="eyebrow">{col?.name ?? columnKey}</div>
          <div className="serif drawer-val">{cell?.status === "error" ? "Error" : (valueText(cell?.value) || "—")}</div>
        </div>
        <button className="icon-btn" onClick={onClose}><Icon.Close size={18} /></button>
      </div>
      <div className="drawer-doc mono"><Icon.Doc size={13} /> {doc?.filename}</div>

      {cell?.status === "error" && (
        <>
          <div className="drawer-section-label">Error</div>
          <p className="drawer-reason" style={{ color: "var(--red)" }}>{cleanError(cell.error)}</p>
        </>
      )}
      {cell?.reasoning && (
        <>
          <div className="drawer-section-label">Reasoning</div>
          <p className="drawer-reason">{cell.reasoning}</p>
        </>
      )}
      {citations.length > 0 && (
        <>
          <div className="drawer-section-label">Citations</div>
          {citations.map((c, i) => (
            <button key={i} className="cite-block" onClick={() => setSelCitation(c)}>
              <Icon.Quote size={15} />
              <div>
                <span className="cite-doc">“{c.quote_text.slice(0, 60)}{c.quote_text.length > 60 ? "…" : ""}”</span>
                <span className="cite-loc mono">{c.page_number != null ? `p.${c.page_number}` : "source"}</span>
              </div>
              <Icon.ChevronR size={15} />
            </button>
          ))}
        </>
      )}

      <button onClick={onRerun} className="btn btn-ghost drawer-rerun"><Icon.Refresh size={14} /> Rerun this cell</button>

      {selCitation && <CitationPanel citation={selCitation} projectId={projectId} onClose={() => setSelCitation(null)} />}
    </div>
  );
}
