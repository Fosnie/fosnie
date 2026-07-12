// Sandboxed preview of an `html` artefact.
// The artefact is a self-contained page with its own injected CSP <meta>; we render
// it in an `iframe sandbox="allow-scripts"` (NO allow-same-origin → null origin, no
// access to app cookies/storage; postMessage only). Download stays the primary
// affordance on the chip; this is an inline Code/Preview toggle.

import { useState } from "react";

import { fetchArtefactText, type Artefact } from "@/api/client";
import { CodeBlock } from "@/components/code";
import { toast } from "@/components/dialogs";

export function HtmlArtefactPreview({ artefact }: { artefact: Artefact }) {
  const [open, setOpen] = useState(false);
  const [view, setView] = useState<"preview" | "code">("preview");
  const [html, setHtml] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  async function ensureLoaded() {
    if (html !== null || loading) return;
    setLoading(true);
    try {
      setHtml(await fetchArtefactText(artefact.id));
    } catch (e) {
      toast((e as Error).message, { variant: "error" });
      setOpen(false);
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="artefact-preview-wrap">
      <div className="artefact-preview-bar">
        <button
          className="btn btn-line sm"
          onClick={() => {
            const next = !open;
            setOpen(next);
            if (next) void ensureLoaded();
          }}
        >
          {open ? "Hide" : "Preview"}
        </button>
        {open && (
          <>
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
          </>
        )}
      </div>

      {open && (
        <div className="artefact-preview-body">
          {loading || html === null ? (
            <div className="artefact-preview-loading mono">Loading…</div>
          ) : view === "preview" ? (
            <iframe
              className="artefact-preview"
              title={artefact.title}
              sandbox="allow-scripts"
              srcDoc={html}
            />
          ) : (
            <CodeBlock code={html} lang="html" />
          )}
        </div>
      )}
    </div>
  );
}
