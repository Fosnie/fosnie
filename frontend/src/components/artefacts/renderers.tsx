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

// Per-kind renders for the artefact panel. Every renderer reports failure through
// `onError` rather than throwing or toasting, so the panel can fall back to a card
// with a download button: a document that will not render must never take the
// panel down with it.
//
// The kind column does not fully determine the render — `file` covers plots, code,
// CSV and anything the code interpreter wrote — so the dispatch below reads `mime`
// as well.

import { useEffect, useRef, useState } from "react";

import { artefactBlob, artefactBlobUrl, fetchArtefactText, fetchArtefactTextCapped, type Artefact } from "@/api/client";
import { CodeBlock } from "@/components/code";
import { MD, MessageMarkdown } from "@/components/MessageMarkdown";

/** Highlighting a very large file locks the main thread, and rendering a huge one
 *  at all is pointless in a side panel — above the first bound we drop to a plain
 *  <pre>, above the second we refuse and offer the download. */
const HIGHLIGHT_MAX_BYTES = 256 * 1024;
const TEXT_MAX_BYTES = 1024 * 1024;

export type RenderProps = {
  artefact: Artefact;
  onError: (msg: string) => void;
};

function Loading() {
  return <div className="ap-loading mono">Loading…</div>;
}

/** Text-ish payloads a code block can usefully show. */
export function isTextish(mime: string): boolean {
  if (mime.startsWith("text/")) return true;
  return [
    "application/json",
    "application/xml",
    "application/x-ndjson",
    "application/javascript",
    "application/sql",
  ].includes(mime) || mime.endsWith("+json") || mime.endsWith("+xml");
}

function langOf(mime: string, title: string): string | undefined {
  const ext = title.includes(".") ? title.split(".").pop()!.toLowerCase() : "";
  if (ext) return ext;
  if (mime.includes("json")) return "json";
  if (mime.includes("xml")) return "xml";
  if (mime.includes("csv")) return "csv";
  return undefined;
}

/** Markdown reports: the same pipeline as chat answers, in document typography. */
export function MdView({ artefact, onError }: RenderProps) {
  const [text, setText] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    setText(null);
    fetchArtefactText(artefact.id)
      .then((t) => { if (!cancelled) setText(t); })
      .catch((e) => { if (!cancelled) onError((e as Error).message); });
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [artefact.id]);

  if (text === null) return <Loading />;
  return <MessageMarkdown answer={text} className={"ap-doc ai-text " + MD} />;
}

/** Self-contained html artefacts (Deep Research pages, dashboards).
 *
 *  The artefact is a self-contained page with its own injected CSP <meta>; it is
 *  rendered in an `iframe sandbox="allow-scripts"` with NO `allow-same-origin`, so
 *  it has a null origin and no access to app cookies or storage. It is fetched as
 *  text and handed over via `srcDoc` — never as a blob URL, which would inherit
 *  this origin and hand the page same-origin script execution. */
export function HtmlView({ artefact, onError, view }: RenderProps & { view: "preview" | "code" }) {
  const [html, setHtml] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    setHtml(null);
    fetchArtefactText(artefact.id)
      .then((t) => { if (!cancelled) setHtml(t); })
      .catch((e) => { if (!cancelled) onError((e as Error).message); });
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [artefact.id]);

  if (html === null) return <Loading />;
  if (view === "code") return <div className="ap-pad"><CodeBlock code={html} lang="html" /></div>;
  return <iframe className="ap-frame" title={artefact.title} sandbox="allow-scripts" srcDoc={html} />;
}

/** DOCX, rendered by docx-preview into a container element.
 *
 *  The import is dynamic on purpose: docx-preview is a heavy dependency that is
 *  otherwise only pulled in by the lazily-loaded document viewer screen, and a
 *  static import here would drag it into the chat bundle for everyone. */
export function DocxView({ artefact, onError }: RenderProps) {
  const host = useRef<HTMLDivElement | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    let cancelled = false;
    setLoading(true);
    (async () => {
      try {
        const [{ renderAsync }, blob] = await Promise.all([
          import("docx-preview"),
          artefactBlob(artefact.id),
        ]);
        if (cancelled) return;
        el.innerHTML = "";
        // The panel is always narrower than an A4 page, so reflow the document to
        // the container width rather than letting the fixed page width overflow.
        await renderAsync(blob, el, undefined, {
          inWrapper: true,
          renderChanges: true,
          breakPages: true,
          className: "docx",
          ignoreWidth: true,
          ignoreHeight: true,
        });
        if (!cancelled) setLoading(false);
      } catch (e) {
        if (!cancelled) onError((e as Error).message);
      }
    })();
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [artefact.id]);

  return (
    <>
      {loading && <Loading />}
      <div ref={host} className="ap-docx" />
    </>
  );
}

/** PDF in the browser's own viewer.
 *
 *  The download endpoint always answers `Content-Disposition: attachment`, so the
 *  bytes are fetched with the caller's credentials and framed from an object URL.
 *  A blob URL inherits this origin, so the type is forced to application/pdf: the
 *  framed document then cannot be HTML even if the stored mime were wrong. */
export function PdfView({ artefact, onError }: RenderProps) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    let objectUrl: string | null = null;
    setUrl(null);
    artefactBlobUrl(artefact.id, "application/pdf")
      .then((u) => {
        objectUrl = u;
        if (cancelled) { URL.revokeObjectURL(u); return; }
        setUrl(u);
      })
      .catch((e) => { if (!cancelled) onError((e as Error).message); });
    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [artefact.id]);

  if (!url) return <Loading />;
  return <iframe className="ap-frame" title={artefact.title} src={url} />;
}

/** Images (a code-interpreter plot, say). Fetched as a blob so it carries the
 *  caller's credentials — a bare URL only authenticates under cookie sessions. */
export function ImageView({ artefact, onError }: RenderProps) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    let objectUrl: string | null = null;
    setUrl(null);
    artefactBlobUrl(artefact.id)
      .then((u) => {
        objectUrl = u;
        if (cancelled) { URL.revokeObjectURL(u); return; }
        setUrl(u);
      })
      .catch((e) => { if (!cancelled) onError((e as Error).message); });
    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [artefact.id]);

  if (!url) return <Loading />;
  return <img className="ap-img" src={url} alt={artefact.title} />;
}

export function TextView({ artefact, onError }: RenderProps) {
  const [text, setText] = useState<string | null>(null);
  const [tooBig, setTooBig] = useState(false);
  useEffect(() => {
    let cancelled = false;
    setText(null);
    setTooBig(false);
    fetchArtefactTextCapped(artefact.id, TEXT_MAX_BYTES)
      .then((t) => {
        if (cancelled) return;
        if (t === null) setTooBig(true);
        else setText(t);
      })
      .catch((e) => { if (!cancelled) onError((e as Error).message); });
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [artefact.id]);

  if (tooBig) return <UnsupportedNote reason="This file is too large to show here." />;
  if (text === null) return <Loading />;
  if (text.length > HIGHLIGHT_MAX_BYTES) return <pre className="ap-pre">{text}</pre>;
  return <div className="ap-pad"><CodeBlock code={text} lang={langOf(artefact.mime, artefact.title)} /></div>;
}

export function UnsupportedNote({ reason }: { reason: string }) {
  return <div className="ap-card">{reason} Use Download to open it in its own application.</div>;
}
