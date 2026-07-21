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

// The artefact panel: a generated document read beside the chat rather than
// downloaded and opened elsewhere. On a wide viewport it is a third column that
// narrows the thread; below that it slides in over the chat as a drawer. The two
// differ in more than layout — the drawer is modal (scrim, focus trap, dialog
// role), the docked column is a peer region — so the mode is chosen in JS and the
// grid rule keys off the class it sets.

import { AnimatePresence, motion } from "motion/react";
import { useCallback, useEffect, useRef, useState } from "react";

import type { Artefact } from "@/api/client";
import { railRightVariants, scrimVariants, spring } from "@/app/motion";
import type { ArtefactActions } from "@/components/artefacts/useArtefactActions";
import { DocxView, HtmlView, ImageView, isTextish, MdView, PdfView, TextView, UnsupportedNote } from "@/components/artefacts/renderers";
import { Icon } from "@/components/icons";
import { VerificationReport } from "@/components/verificationReport";

export type PanelMode = "docked" | "overlay";

export function ArtefactPanel({
  artefact,
  mode,
  loading,
  missing,
  actions,
  groundednessOn,
  onClose,
  onInteract,
}: {
  artefact: Artefact | null;
  mode: PanelMode;
  /** The chat's artefact list is still loading, so we cannot resolve the id yet. */
  loading?: boolean;
  /** Resolved and not in this chat (deleted, or a link from somewhere else). */
  missing?: boolean;
  actions: ArtefactActions;
  groundednessOn: boolean;
  onClose: () => void;
  onInteract: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<"preview" | "code">("preview");
  const panelRef = useRef<HTMLDivElement | null>(null);
  const headRef = useRef<HTMLHeadingElement | null>(null);
  const returnFocusTo = useRef<Element | null>(null);

  const id = artefact?.id;
  useEffect(() => {
    setError(null);
    setView("preview");
  }, [id]);

  // Focus moves into the panel on open and back to whatever opened it on close,
  // so the chip → panel → close round trip works from the keyboard.
  useEffect(() => {
    returnFocusTo.current = document.activeElement;
    headRef.current?.focus();
    return () => {
      const back = returnFocusTo.current;
      if (back instanceof HTMLElement && document.contains(back)) back.focus();
    };
  }, []);

  // Escape closes. In the docked column it must only do so while focus is inside
  // the panel — the composer owns Escape everywhere else on this screen.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      if (mode === "docked" && !panelRef.current?.contains(document.activeElement)) return;
      e.preventDefault();
      onClose();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mode, onClose]);

  const trapTab = useCallback(
    (e: React.KeyboardEvent) => {
      if (mode !== "overlay" || e.key !== "Tab") return;
      const focusable = panelRef.current?.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input, select, textarea, iframe, [tabindex]:not([tabindex="-1"])',
      );
      if (!focusable?.length) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    },
    [mode],
  );

  const verifyRun = artefact ? actions.verifyRuns[artefact.id] : undefined;

  const body = (
    <div
      ref={panelRef}
      className="artefact-panel"
      role={mode === "overlay" ? "dialog" : "complementary"}
      aria-modal={mode === "overlay" ? true : undefined}
      aria-label={artefact ? `Artefact: ${artefact.title}` : "Artefact"}
      onKeyDown={trapTab}
      onPointerDownCapture={onInteract}
      onFocusCapture={onInteract}
    >
      <div className="ap-head">
        <div className="ap-title">
          <h2 ref={headRef} tabIndex={-1} className="ap-name">
            {artefact?.title ?? (missing ? "Artefact" : "Loading…")}
          </h2>
          {artefact && <span className="artefact-kind mono">{artefact.kind}</span>}
        </div>
        <button className="ap-close" title="Close" aria-label="Close artefact" onClick={onClose}>
          <Icon.Close size={16} />
        </button>
      </div>

      {artefact && (
        <div className="ap-actions">
          <button className="btn btn-line sm" title={`Download ${artefact.title}`} onClick={() => actions.download(artefact)}>
            <Icon.Download size={13} /> Download
          </button>
          {artefact.kind === "md" && (
            <>
              <button className="btn btn-line sm" title="Save as DOCX" onClick={() => actions.convert(artefact, "docx")}>
                DOCX
              </button>
              <button className="btn btn-line sm" title="Save as PDF" onClick={() => actions.convert(artefact, "pdf")}>
                PDF
              </button>
              {artefact.chat_mode === "research" && (
                <button
                  className="btn btn-line sm"
                  title="Create a self-contained HTML page from this report"
                  onClick={() => actions.toPage(artefact)}
                >
                  Create page
                </button>
              )}
            </>
          )}
          {groundednessOn && artefact.kind === "md" && (
            <button
              className="btn btn-line sm"
              title="Verify the draft against your sources"
              onClick={() => actions.verify(artefact)}
              disabled={verifyRun === "starting"}
            >
              <Icon.Shield size={13} /> {verifyRun ? "Re-verify" : "Verify"}
            </button>
          )}
          {artefact.kind === "html" && !error && (
            <span className="ap-seg">
              <button
                className={"btn btn-line sm" + (view === "preview" ? " active" : "")}
                onClick={() => setView("preview")}
              >
                Preview
              </button>
              <button
                className={"btn btn-line sm" + (view === "code" ? " active" : "")}
                onClick={() => setView("code")}
              >
                Code
              </button>
            </span>
          )}
        </div>
      )}

      <div className="ap-body">
        <ArtefactBody
          artefact={artefact}
          loading={loading}
          missing={missing}
          error={error}
          view={view}
          onError={setError}
          onDownload={() => artefact && actions.download(artefact)}
        />
        {verifyRun && verifyRun !== "starting" && (
          <div className="ap-pad">
            <VerificationReport runId={verifyRun} />
          </div>
        )}
      </div>
    </div>
  );

  if (mode === "docked") return body;
  return (
    <motion.div
      className="ap-scrim"
      onClick={onClose}
      variants={scrimVariants}
      initial="initial"
      animate="animate"
      exit="exit"
    >
      <motion.div
        className="ap-drawer"
        onClick={(e) => e.stopPropagation()}
        variants={railRightVariants}
        initial="initial"
        animate="animate"
        exit="exit"
        transition={spring}
      >
        {body}
      </motion.div>
    </motion.div>
  );
}

/** Which render an artefact gets. `kind` decides first — a `pdf` artefact and a
 *  `file` artefact holding a PDF must look the same — then `mime`, because `kind`
 *  "file" covers plots, code and CSV alike. */
function ArtefactBody({
  artefact,
  loading,
  missing,
  error,
  view,
  onError,
  onDownload,
}: {
  artefact: Artefact | null;
  loading?: boolean;
  missing?: boolean;
  error: string | null;
  view: "preview" | "code";
  onError: (msg: string) => void;
  onDownload: () => void;
}) {
  if (missing) return <div className="ap-card">That artefact is not part of this chat. It may have been deleted.</div>;
  if (loading || !artefact) return <div className="ap-loading mono">Loading…</div>;
  if (error) {
    return (
      <div className="ap-card">
        <div>Could not display this artefact.</div>
        <div className="ap-card-detail mono">{error}</div>
        <button className="btn btn-line sm" onClick={onDownload}>
          <Icon.Download size={13} /> Download
        </button>
      </div>
    );
  }

  const mime = artefact.mime ?? "";
  const props = { artefact, onError };
  if (artefact.kind === "md") return <MdView {...props} />;
  if (artefact.kind === "html") return <HtmlView {...props} view={view} />;
  if (artefact.kind === "docx") return <DocxView {...props} />;
  if (artefact.kind === "pdf" || mime === "application/pdf") return <PdfView {...props} />;
  if (mime.startsWith("image/")) return <ImageView {...props} />;
  if (isTextish(mime)) return <TextView {...props} />;
  return <UnsupportedNote reason="This file type cannot be shown in the browser." />;
}

/** Mount point: keeps the panel out of the tree entirely when nothing is
 *  selected, and keys it on the mode so crossing the breakpoint remounts with the
 *  right role, scrim and focus behaviour. */
export function ArtefactPanelHost(props: Parameters<typeof ArtefactPanel>[0] & { open: boolean }) {
  const { open, ...rest } = props;
  return <AnimatePresence>{open && <ArtefactPanel key={rest.mode} {...rest} />}</AnimatePresence>;
}
