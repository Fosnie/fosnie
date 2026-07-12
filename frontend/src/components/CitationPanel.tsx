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
import { useEffect, useMemo, useRef } from "react";
import { motion } from "motion/react";
import { railRightVariants, scrimVariants, spring } from "@/app/motion";
import { useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  fetchVersionText,
  openVersionPdf,
  useDocument,
  useKnowledgeDocSource,
} from "@/api/client";
import type { Citation } from "@/ws/protocol";

/** Split `text` around the first occurrence of `quote`, tolerant of whitespace
 *  differences and ellipsis-truncated snippets. The ML `_quote` collapses all
 *  whitespace to single spaces, while the raw extracted source keeps the PDF's
 *  line breaks — so a literal indexOf failed ("Quote not found verbatim") on any
 *  doc with ragged extraction. We match on a whitespace-normalised view of `text`
 *  but slice the RAW text via an index map, so the highlight lands on the real
 *  characters. Returns null when not found. */
function locate(text: string, quote: string): { pre: string; match: string; post: string } | null {
  if (!text || !quote) return null;

  // Whitespace-normalised view of `text` + a map from each normalised char index
  // back to its original offset (a collapsed run maps to its first ws char).
  let norm = "";
  const map: number[] = [];
  let prevWs = false;
  for (let i = 0; i < text.length; i++) {
    if (/\s/.test(text[i])) {
      if (!prevWs && norm.length > 0) { norm += " "; map.push(i); }
      prevWs = true;
    } else {
      norm += text[i]; map.push(i); prevWs = false;
    }
  }

  // Normalise the quote the same way; drop leading/trailing ellipsis + space.
  const q = quote.replace(/\s+/g, " ").replace(/^[\s.…]+|[\s.…]+$/g, "").trim();
  if (!q) return null;
  const tries = q.length > 60 ? [q, q.slice(0, 60).trimEnd()] : [q];
  for (const t of tries) {
    if (!t) continue;
    const i = norm.indexOf(t);
    if (i >= 0) {
      const start = map[i];
      const end = map[i + t.length - 1] + 1;
      return { pre: text.slice(0, start), match: text.slice(start, end), post: text.slice(end) };
    }
  }
  return null;
}

export function CitationPanel({
  citation,
  projectId,
  onClose,
}: {
  citation: Citation;
  projectId?: string;
  onClose: () => void;
}) {
  const nav = useNavigate();
  const isWorkspace = !!(citation.document_id && citation.version_id);
  // Web citation: URL-anchored, no platform document to load — the quote and
  // source metadata are shown, with an external link out.
  const isWeb = !!citation.url;

  // Workspace source: extracted text of the cited version + the doc's name.
  const wsText = useQuery({
    queryKey: ["version-text", citation.document_id, citation.version_id],
    queryFn: () => fetchVersionText(citation.document_id!, citation.version_id!),
    enabled: isWorkspace && !isWeb,
  });
  const wsDoc = useDocument(isWorkspace && !isWeb ? citation.document_id ?? undefined : undefined);

  // Knowledge (RAG) source.
  const kSource = useKnowledgeDocSource(isWorkspace || isWeb ? null : citation.doc_id);

  const loading = isWeb ? false : isWorkspace ? wsText.isLoading : kSource.isLoading;
  const error = isWeb ? null : isWorkspace ? wsText.error : kSource.error;
  const text = isWeb ? "" : isWorkspace ? wsText.data ?? "" : kSource.data?.text ?? "";
  const filename = isWeb
    ? citation.title ?? citation.domain ?? "Web source"
    : isWorkspace
      ? wsDoc.data?.original_filename
      : kSource.data?.filename;

  const parts = useMemo(() => locate(text, citation.quote_text), [text, citation.quote_text]);
  const markRef = useRef<HTMLElement | null>(null);
  useEffect(() => {
    if (parts && markRef.current) markRef.current.scrollIntoView({ block: "center" });
  }, [parts]);

  return (
    <motion.div className="fixed inset-0 z-50 flex justify-end bg-navy/60" onClick={onClose}
      variants={scrimVariants} initial="initial" animate="animate">
      {/* Source text is CONTENT — stays solid (no glass), per the refresh rule.
          Only the entrance slides in from the edge. */}
      <motion.div
        className="flex h-full w-full max-w-xl flex-col border-l border-navy-lighter bg-navy-light"
        onClick={(e) => e.stopPropagation()}
        variants={railRightVariants} initial="initial" animate="animate" transition={spring}
      >
        <div className="flex items-start justify-between border-b border-navy-lighter px-5 py-3">
          <div className="min-w-0">
            <p className="text-[0.7rem] uppercase tracking-[0.14em] text-slate">Source</p>
            <h3 className="truncate text-slate-lightest" title={filename}>
              {filename ?? (loading ? "Loading…" : "Cited document")}
            </h3>
            <p className="text-xs text-slate">
              {isWeb ? (
                <>
                  {citation.domain ?? ""}
                  {citation.published_date ? ` · published ${citation.published_date}` : ""}
                  {citation.snippet_only ? " · search snippet only" : ""}
                </>
              ) : (
                <>
                  {citation.page_number != null ? `Page ${citation.page_number}` : ""}
                  {citation.clause_section_ref ? ` · ${citation.clause_section_ref}` : ""}
                </>
              )}
            </p>
          </div>
          <button onClick={onClose} className="ml-3 text-slate hover:text-slate-lightest">✕</button>
        </div>

        <div className="flex-1 overflow-y-auto px-5 py-4 text-sm">
          {isWeb && (
            <div className="border-l-2 border-gold-dark/50 pl-2 text-slate-lightest">
              “{citation.quote_text}”
              <p className="mt-2 text-xs text-slate/60">
                {citation.snippet_only
                  ? "Quoted from the search-result snippet — the page itself was not fetched."
                  : `Quoted from the page as fetched${citation.fetched_at ? ` at ${citation.fetched_at}` : ""}.`}
              </p>
            </div>
          )}
          {!isWeb && loading && <p className="text-slate">Loading source…</p>}
          {!isWeb && !loading && error && (
            <p className="text-urgency-red">Source unavailable: {(error as Error).message}</p>
          )}
          {!isWeb && !loading && !error && (
            <>
              {!parts && (
                <div className="mb-4 border-l-2 border-gold-dark/50 pl-2 text-slate">
                  “{citation.quote_text}”
                  <p className="mt-1 text-xs text-slate/60">Quote not found verbatim in the source (shown below).</p>
                </div>
              )}
              <div className="whitespace-pre-wrap leading-relaxed text-slate-lightest">
                {parts ? (
                  <>
                    {parts.pre}
                    <mark ref={markRef} className="rounded bg-gold/30 px-0.5 text-slate-lightest">
                      {parts.match}
                    </mark>
                    {parts.post}
                  </>
                ) : (
                  text || <span className="text-slate/60">No text extracted.</span>
                )}
              </div>
            </>
          )}
        </div>

        {isWeb && (
          <div className="flex gap-2 border-t border-navy-lighter px-5 py-3">
            <a
              href={citation.url!}
              target="_blank"
              rel="noreferrer"
              className="rounded-lg border border-navy-lighter px-3 py-1.5 text-sm text-slate hover:text-slate-lightest"
            >
              Open source ↗
            </a>
          </div>
        )}
        {isWorkspace && !isWeb && (
          <div className="flex gap-2 border-t border-navy-lighter px-5 py-3">
            <button
              onClick={() => openVersionPdf(citation.document_id!, citation.version_id!).catch((e) => toast((e as Error).message))}
              className="rounded-lg border border-navy-lighter px-3 py-1.5 text-sm text-slate hover:text-slate-lightest"
            >
              Open PDF
            </button>
            {projectId && (
              <button
                onClick={() => {
                  onClose();
                  nav(`/p/${projectId}/d/${citation.document_id}`);
                }}
                className="rounded-lg border border-navy-lighter px-3 py-1.5 text-sm text-slate hover:text-slate-lightest"
              >
                Open in viewer
              </button>
            )}
          </div>
        )}
      </motion.div>
    </motion.div>
  );
}
