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
import { renderAsync } from "docx-preview";
import {
  ConflictError,
  acceptAllEdits,
  acceptEdit,
  fetchVersionDocx,
  fetchVersionPdf,
  fetchVersionText,
  openVersionPdf,
  rejectAllEdits,
  rejectEdit,
  startRepair,
  startVerifyDraft,
  useDocEdits,
  useDocument,
  useLatestVerification,
  useVerificationRun,
  useWhoami,
  type DocEdit,
  type EditAuthor,
  type VerifyClaim,
} from "@/api/client";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";
import { markFlaggedText } from "@/components/docHighlight";
import { VerificationReport } from "@/components/verificationReport";
import { wsStore } from "@/ws/store";
import type { ServerFrame } from "@/ws/protocol";

type AuthorFilter = "all" | EditAuthor;
type RenderState = { state: "idle" | "loading" | "done" } | { state: "error"; msg: string };

function isDocxMime(mime: string | null, filename: string): boolean {
  return (mime ?? "").includes("wordprocessingml") || filename.toLowerCase().endsWith(".docx");
}

function isPdfMime(mime: string | null, filename: string): boolean {
  return (mime ?? "").includes("pdf") || filename.toLowerCase().endsWith(".pdf");
}

export function DocumentViewer() {
  const { projectId, documentId } = useParams();
  const nav = useNavigate();
  const qc = useQueryClient();
  const doc = useDocument(documentId);
  const edits = useDocEdits(documentId, "pending");
  const who = useWhoami();
  const groundednessOn = !!who.data?.capabilities.groundedness;
  const latestVerify = useLatestVerification("document", documentId);
  const [verifyRunId, setVerifyRunId] = useState<string | null>(null);
  const effectiveRunId = verifyRunId ?? latestVerify.data?.id ?? null;
  const verifyRun = useVerificationRun(effectiveRunId ?? undefined);
  const [groundView, setGroundView] = useState(false);
  const flaggedClaims = useMemo<VerifyClaim[]>(
    () => (verifyRun.data?.claims ?? []).filter((c) => c.verdict !== "supported"),
    [verifyRun.data],
  );
  const canHighlight = verifyRun.data?.status === "succeeded" && flaggedClaims.length > 0;
  const repairOn = !!who.data?.capabilities.groundedness_repair;
  const [repairing, setRepairing] = useState(false);
  async function verifyDraft() {
    try {
      const { run_id } = await startVerifyDraft("document", documentId!);
      setVerifyRunId(run_id);
    } catch (e) {
      toast(`Verification failed to start: ${(e as Error).message}`);
    }
  }
  async function repairDraft() {
    if (!effectiveRunId) return;
    setRepairing(true);
    setNotice("Repairing flagged claims — proposals will appear as tracked changes…");
    try {
      await startRepair(effectiveRunId);
    } catch (e) {
      setRepairing(false);
      toast(`Repair failed to start: ${(e as Error).message}`);
    }
  }

  const [pinned, setPinned] = useState<string | null>(null); // null = follow current version
  const [docxNonce, setDocxNonce] = useState(0);
  const [filter, setFilter] = useState<AuthorFilter>("all");
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const container = useRef<HTMLDivElement | null>(null);
  const [render, setRender] = useState<RenderState>({ state: "idle" });

  const data = doc.data;
  const selectedVersionId = pinned ?? data?.current_version_id ?? null;
  const docx = data ? isDocxMime(data.mime, data.original_filename) : true;
  const pdf = data ? isPdfMime(data.mime, data.original_filename) : false;

  async function refetchAll() {
    setPinned(null); // jump back to the (new) current version after a resolve
    await Promise.all([
      qc.invalidateQueries({ queryKey: ["document", documentId] }),
      qc.invalidateQueries({ queryKey: ["doc-edits", documentId] }),
    ]);
    setDocxNonce((n) => n + 1);
  }

  async function act(label: string, fn: () => Promise<unknown>) {
    setBusy(label);
    setNotice(null);
    try {
      await fn();
      await refetchAll();
    } catch (e) {
      if (e instanceof ConflictError) {
        setNotice("Document changed elsewhere — reloaded to the latest version.");
        await refetchAll();
      } else {
        toast(`${label} failed: ${(e as Error).message}`);
      }
    } finally {
      setBusy(null);
    }
  }

  // Render the selected version (DOCX → docx-preview with inline redlines; else plain text).
  // Skipped while the groundedness annotated view is shown (it renders separately).
  useEffect(() => {
    const el = container.current;
    if (!documentId || !selectedVersionId || !el || (groundView && canHighlight)) return;
    let cancelled = false;
    let objectUrl: string | null = null;
    setRender({ state: "loading" });
    (async () => {
      try {
        el.innerHTML = "";
        if (docx) {
          const blob = await fetchVersionDocx(documentId, selectedVersionId);
          if (cancelled) return;
          // On a phone the fixed A4 page width overflows the viewport, so reflow
          // the document to the container width (text wraps; only over-wide
          // tables still scroll). Desktop keeps the true page layout.
          const narrow = typeof window !== "undefined" && window.innerWidth <= 640;
          await renderAsync(blob, el, undefined, {
            inWrapper: true,
            renderChanges: true, // draw <w:ins>/<w:del> redlines
            breakPages: true,
            className: "docx",
            ignoreWidth: narrow,
            ignoreHeight: narrow,
          });
        } else if (pdf) {
          // Embed the real PDF (native browser viewer) — far better than extracted text.
          const blob = await fetchVersionPdf(documentId, selectedVersionId);
          if (cancelled) return;
          objectUrl = URL.createObjectURL(blob);
          const frame = document.createElement("iframe");
          frame.src = objectUrl;
          frame.className = "dv-pdf-frame";
          frame.title = "PDF";
          el.appendChild(frame);
        } else {
          const text = await fetchVersionText(documentId, selectedVersionId);
          if (cancelled) return;
          const pre = document.createElement("pre");
          pre.textContent = text;
          pre.className = "whitespace-pre-wrap p-6 text-sm text-slate-lightest";
          el.appendChild(pre);
        }
        if (!cancelled) setRender({ state: "done" });
      } catch (e) {
        if (!cancelled) setRender({ state: "error", msg: (e as Error).message });
      }
    })();
    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [documentId, selectedVersionId, docx, pdf, docxNonce, groundView, canHighlight]);

  // Live: a chat turn proposed new edits on THIS document → reload + re-render.
  useEffect(() => {
    if (!documentId) return;
    return wsStore.onFrame((f: ServerFrame) => {
      if (f.type === "doc.edited" && (f as { document_id?: string }).document_id === documentId) {
        void refetchAll();
      } else if (f.type === "repair.complete" && f.document_id === documentId) {
        setRepairing(false);
        if (f.error) {
          setNotice(`Repair: ${f.error}`);
        } else {
          const rc = f as { regenerated: number; cut: number };
          const proposed = rc.regenerated + rc.cut;
          setNotice(
            proposed > 0
              ? `Repair proposed ${proposed} tracked change(s): ${rc.regenerated} rewritten, ${rc.cut} cut. Review them below.`
              : "Repair found nothing to change — every flagged claim was kept.",
          );
        }
        void refetchAll();
      }
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [documentId]);

  const displayed = useMemo<DocEdit[]>(
    () => (edits.data ?? []).filter((e) => filter === "all" || e.author === filter),
    [edits.data, filter],
  );

  async function acceptAll() {
    if (filter === "all") {
      await act("Accept all", () => acceptAllEdits(documentId!));
    } else {
      await act("Accept all", async () => {
        for (const e of displayed) await acceptEdit(documentId!, e.w_id);
      });
    }
  }
  async function rejectAll() {
    if (filter === "all") {
      await act("Reject all", () => rejectAllEdits(documentId!));
    } else {
      await act("Reject all", async () => {
        for (const e of displayed) await rejectEdit(documentId!, e.w_id);
      });
    }
  }

  if (doc.isLoading) return <div className="main-scroll"><div className="panel">Loading document…</div></div>;
  if (doc.isError || !data) {
    return (
      <div className="main-scroll">
        <div className="panel" style={{ color: "var(--red)" }}>
          Could not load document. {(doc.error as Error | undefined)?.message}
        </div>
      </div>
    );
  }

  return (
    <div className="proj-sub">
      <button className="back-bar" onClick={() => nav(`/p/${projectId}`)}><Icon.ChevronL size={15} /> Back to project</button>
      {notice && (
        <div className="border-b border-gold-dark/40 bg-gold/10 px-5 py-2 text-xs text-gold-light">
          {notice} <button onClick={() => setNotice(null)} className="underline">dismiss</button>
        </div>
      )}

      <div className="dv-wrap">
        {/* Left — rendered document */}
        <div className="dv-main">
          <div className="dv-head">
            <div style={{ minWidth: 0 }}>
              <div className="eyebrow">Document viewer</div>
              <h2 className="serif dv-title" title={data.original_filename} style={{ maxWidth: "42ch", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{data.original_filename}</h2>
            </div>
            <div className="dv-controls">
              <Dropdown
                value={selectedVersionId ?? ""}
                onChange={setPinned}
                ariaLabel="Document version"
                icon={<Icon.Clock size={14} />}
                options={data.versions.map((v) => ({
                  value: v.id,
                  label: `v${v.version_number} · ${v.source}${v.id === data.current_version_id ? " (current)" : ""}`,
                }))}
              />
              <button onClick={() => selectedVersionId && act("Open PDF", () => openVersionPdf(documentId!, selectedVersionId))} disabled={!!busy || !selectedVersionId} className="btn btn-ghost">
                <Icon.External size={14} /> Open PDF
              </button>
              {groundednessOn && (
                <button onClick={verifyDraft} disabled={!!busy} className="btn btn-line" title="Verify the draft against your sources">
                  <Icon.Shield size={14} /> Verify draft
                </button>
              )}
              {groundednessOn && canHighlight && (
                <button
                  onClick={() => setGroundView((v) => !v)}
                  className={"btn btn-line" + (groundView ? " on" : "")}
                  title="Highlight ungrounded claims on the document"
                >
                  <Icon.Flag size={14} /> {groundView ? "Document" : "Groundedness"}
                </button>
              )}
              {repairOn && canHighlight && docx && (
                <button
                  onClick={repairDraft}
                  disabled={!!busy || repairing}
                  className="btn btn-line"
                  title="Regenerate or cut each flagged claim, surfaced as tracked changes"
                >
                  <Icon.Wrench size={14} /> {repairing ? "Repairing…" : "Repair (ground-or-cut)"}
                </button>
              )}
            </div>
          </div>
          <div className="dv-paper-wrap">
            {groundView && canHighlight ? (
              <div className="dv-annot-wrap">
                <div className="dv-ground-legend">
                  <span><i className="gd-dot contradicted" /> contradicted — source disagrees</span>
                  <span><i className="gd-dot unsupported" /> not mentioned — source silent</span>
                </div>
                <AnnotatedText documentId={documentId!} versionId={selectedVersionId!} claims={flaggedClaims} />
              </div>
            ) : (
              <>
                {render.state === "loading" && <div className="absolute left-1/2 top-6 -translate-x-1/2 text-xs text-slate">Rendering…</div>}
                {render.state === "error" && <div className="absolute left-1/2 top-6 -translate-x-1/2 text-xs text-urgency-red">Render failed: {render.msg}</div>}
                <div ref={container} className="docx-host" style={{ width: "100%" }} />
              </>
            )}
          </div>
        </div>

        {/* Right — changes panel */}
        <aside className="dv-side">
          {groundednessOn && effectiveRunId && (
            <div style={{ padding: "12px 14px 4px" }}>
              <span className="side-label mono">Groundedness</span>
              <VerificationReport runId={effectiveRunId} />
            </div>
          )}
          <div className="dv-side-head">
            <span className="side-label mono">Tracked changes</span>
            <span className="dv-count mono">{displayed.length} pending</span>
          </div>
          <div style={{ padding: "0 14px 12px" }}>
            <div className="seg" style={{ width: "100%", justifyContent: "space-between" }}>
              {(["all", "assistant", "human"] as AuthorFilter[]).map((a) => (
                <button key={a} className={"seg-opt" + (filter === a ? " on" : "")} onClick={() => setFilter(a)} style={{ flex: 1 }}>{a}</button>
              ))}
            </div>
          </div>

          <div className="dv-edits">
            {edits.isLoading ? (
              <p className="text-sm text-slate">Loading…</p>
            ) : displayed.length === 0 ? (
              <p className="text-sm text-slate/70">No pending changes.</p>
            ) : (
              displayed.map((e) => (
                <div key={e.id} className="dv-edit">
                  <div className="dv-edit-top">
                    <span className={"dv-kind " + (e.author === "assistant" ? "change" : "ins")}>{e.author}</span>
                  </div>
                  <div className="dv-edit-text">
                    {e.find_text != null && e.find_text !== "" && <span className="rl-del">{e.find_text}</span>}
                    {e.find_text && e.replace_text ? " " : ""}
                    {e.replace_text != null && e.replace_text !== "" && <span className="rl-ins">{e.replace_text}</span>}
                  </div>
                  <div className="dv-edit-foot">
                    <span className="dv-who mono">tracked change</span>
                    <div className="dv-edit-actions">
                      <button className="dv-reject" onClick={() => act("Reject", () => rejectEdit(documentId!, e.w_id))} disabled={!!busy}><Icon.Close size={12} /> Reject</button>
                      <button className="dv-accept" onClick={() => act("Accept", () => acceptEdit(documentId!, e.w_id))} disabled={!!busy}><Icon.Check size={12} /> Accept</button>
                    </div>
                  </div>
                </div>
              ))
            )}
          </div>

          <div className="dv-bulk">
            <button onClick={rejectAll} disabled={!!busy || displayed.length === 0} className="btn btn-ghost">Reject all</button>
            <button onClick={acceptAll} disabled={!!busy || displayed.length === 0} className="btn btn-gold">Accept all</button>
          </div>
        </aside>
      </div>
    </div>
  );
}

/** The Mode-B inline highlight: the version's extracted text with each flagged
 *  claim's verbatim span marked in place (§4.5). Plain text, so the marks land
 *  precisely regardless of how docx-preview splits runs. */
function AnnotatedText({ documentId, versionId, claims }: { documentId: string; versionId: string; claims: VerifyClaim[] }) {
  const [text, setText] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    setText(null);
    setErr(null);
    fetchVersionText(documentId, versionId)
      .then((t) => { if (!cancelled) setText(t); })
      .catch((e) => { if (!cancelled) setErr((e as Error).message); });
    return () => { cancelled = true; };
  }, [documentId, versionId]);

  if (err) return <div className="dv-paper dv-annot" style={{ color: "var(--red)" }}>Could not load text: {err}</div>;
  if (text === null) return <div className="dv-paper dv-annot text-slate">Loading text…</div>;
  return <pre className="dv-paper dv-annot">{markFlaggedText(text, claims)}</pre>;
}
