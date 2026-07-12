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

import { useCallback, useRef, useState } from "react";
import type { PromptFieldType, PromptVariable } from "@/api/client";
import { Icon } from "@/components/icons";
import { Dropdown } from "@/components/Dropdown";

const TYPES: { value: PromptFieldType; label: string }[] = [
  { value: "short", label: "Short text" },
  { value: "long", label: "Long text" },
  { value: "date", label: "Date" },
  { value: "select", label: "Dropdown" },
];

function slugify(label: string): string {
  return label.toLowerCase().replace(/[^a-z0-9]+/g, "_").replace(/^_+|_+$/g, "") || "field";
}

type FieldDef = { key: string; label: string; type: PromptFieldType; help: string; options: string[] };
const EMPTY: FieldDef = { key: "", label: "", type: "short", help: "", options: [] };

/** Visual prompt template editor. The author writes prose and drops in named
 *  "fields" as removable pills — never seeing the underlying `{{key}}`. Emits the
 *  serialised `content` ({{key}} tokens) + `variables` (label/type/help/options).
 *  Uncontrolled contenteditable (no JSX children) so React never wipes the chips. */
export function TemplateEditor({ onChange }: { onChange: (content: string, variables: PromptVariable[]) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const savedRange = useRef<Range | null>(null);
  const [popover, setPopover] = useState(false);
  const editingChip = useRef<HTMLElement | null>(null);
  const [draft, setDraft] = useState<FieldDef>(EMPTY);
  const [optionsText, setOptionsText] = useState("");

  const serialize = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    let content = "";
    const variables: PromptVariable[] = [];
    const seen = new Set<string>();
    el.childNodes.forEach((node) => {
      if (node.nodeType === Node.TEXT_NODE) {
        content += (node.textContent ?? "").replace(/ /g, " "); // nbsp → space
      } else if (node instanceof HTMLElement) {
        const key = node.dataset.key;
        if (key) {
          content += `{{${key}}}`;
          if (!seen.has(key)) {
            seen.add(key);
            variables.push({
              key,
              label: node.dataset.label ?? key,
              type: (node.dataset.type as PromptFieldType) ?? "short",
              help: node.dataset.help || undefined,
              options: node.dataset.options ? (JSON.parse(node.dataset.options) as string[]) : undefined,
            });
          }
        } else if (node.tagName === "BR") {
          content += "\n";
        } else {
          content += node.textContent ?? "";
        }
      }
    });
    onChange(content.replace(new RegExp(String.fromCharCode(160),"g")," "), variables);
  }, [onChange]);

  function saveCaret() {
    const sel = window.getSelection();
    if (sel && sel.rangeCount && ref.current?.contains(sel.anchorNode)) {
      savedRange.current = sel.getRangeAt(0).cloneRange();
    }
  }
  function caretRange(): Range {
    const el = ref.current!;
    let range = savedRange.current;
    if (!range || !el.contains(range.commonAncestorContainer)) {
      range = document.createRange();
      range.selectNodeContents(el);
      range.collapse(false);
    }
    return range;
  }

  function existingKeys(exclude?: string): Set<string> {
    const keys = new Set<string>();
    ref.current?.querySelectorAll<HTMLElement>(".field-chip").forEach((c) => {
      if (c.dataset.key && c.dataset.key !== exclude) keys.add(c.dataset.key);
    });
    return keys;
  }
  function uniqueKey(base: string): string {
    const keys = existingKeys();
    if (!keys.has(base)) return base;
    let i = 2;
    while (keys.has(`${base}_${i}`)) i++;
    return `${base}_${i}`;
  }

  function buildChip(f: FieldDef): HTMLElement {
    const span = document.createElement("span");
    span.className = "field-chip";
    span.contentEditable = "false";
    span.dataset.key = f.key;
    span.dataset.label = f.label;
    span.dataset.type = f.type;
    if (f.help) span.dataset.help = f.help;
    if (f.type === "select" && f.options.length) span.dataset.options = JSON.stringify(f.options);
    const text = document.createElement("span");
    text.textContent = f.label;
    span.appendChild(text);
    const x = document.createElement("button");
    x.type = "button";
    x.className = "chip-x";
    x.textContent = "×";
    x.onmousedown = (e) => e.preventDefault();
    x.onclick = (e) => { e.preventDefault(); e.stopPropagation(); span.remove(); serialize(); ref.current?.focus(); };
    span.appendChild(x);
    span.onclick = (e) => { if (e.target === x) return; e.preventDefault(); openEdit(span); };
    return span;
  }

  function openAdd() {
    saveCaret();
    editingChip.current = null;
    setDraft(EMPTY);
    setOptionsText("");
    setPopover(true);
  }
  function openEdit(chip: HTMLElement) {
    editingChip.current = chip;
    const opts = chip.dataset.options ? (JSON.parse(chip.dataset.options) as string[]) : [];
    setDraft({ key: chip.dataset.key!, label: chip.dataset.label ?? "", type: (chip.dataset.type as PromptFieldType) ?? "short", help: chip.dataset.help ?? "", options: opts });
    setOptionsText(opts.join("\n"));
    setPopover(true);
  }

  function confirmField() {
    const label = draft.label.trim();
    if (!label) return;
    const options = draft.type === "select" ? optionsText.split("\n").map((s) => s.trim()).filter(Boolean) : [];
    if (draft.type === "select" && options.length === 0) return;
    const chip = editingChip.current;
    if (chip) {
      chip.replaceWith(buildChip({ ...draft, label, options, key: chip.dataset.key! }));
    } else {
      const key = uniqueKey(slugify(label));
      const fresh = buildChip({ ...draft, label, options, key });
      ref.current?.focus();
      const range = caretRange();
      const sel = window.getSelection()!;
      sel.removeAllRanges();
      sel.addRange(range);
      range.deleteContents();
      const space = document.createTextNode(" ");
      range.insertNode(space);
      range.insertNode(fresh);
      range.setStartAfter(space);
      range.collapse(true);
      sel.removeAllRanges();
      sel.addRange(range);
    }
    setPopover(false);
    editingChip.current = null;
    serialize();
    ref.current?.focus();
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      const sel = window.getSelection();
      if (!sel || !sel.rangeCount) return;
      const range = sel.getRangeAt(0);
      range.deleteContents();
      const nl = document.createTextNode("\n");
      range.insertNode(nl);
      range.setStartAfter(nl);
      range.collapse(true);
      sel.removeAllRanges();
      sel.addRange(range);
      serialize();
    }
  }
  function onPaste(e: React.ClipboardEvent) {
    e.preventDefault();
    const text = e.clipboardData.getData("text/plain");
    const sel = window.getSelection();
    if (!sel || !sel.rangeCount) return;
    const range = sel.getRangeAt(0);
    range.deleteContents();
    const node = document.createTextNode(text);
    range.insertNode(node);
    range.setStartAfter(node);
    range.collapse(true);
    sel.removeAllRanges();
    sel.addRange(range);
    serialize();
  }

  return (
    <div className="tmpl-wrap">
      <div className="tmpl-toolbar">
        <button type="button" className="btn btn-line sm" onClick={openAdd}><Icon.Plus size={13} /> Insert field</button>
        <span className="ed-hint mono">Write your prompt; drop in fields people fill at use-time.</span>
      </div>
      <div
        ref={ref}
        className="tmpl-editor"
        contentEditable
        suppressContentEditableWarning
        data-placeholder="e.g. Summarise the key obligations in [Document] for [Audience] — flag anything unusual."
        onInput={serialize}
        onKeyDown={onKeyDown}
        onPaste={onPaste}
        onBlur={saveCaret}
      />

      {popover && (
        <div className="menu fade-up" style={{ position: "absolute", top: 44, left: 0, width: 360, zIndex: 70, padding: 12 }} onMouseDown={(e) => e.stopPropagation()}>
          <label className="form-label">Field label</label>
          <input
            autoFocus
            className="field sm"
            value={draft.label}
            onChange={(e) => setDraft((d) => ({ ...d, label: e.target.value }))}
            onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); confirmField(); } if (e.key === "Escape") { setPopover(false); editingChip.current = null; } }}
            placeholder="e.g. Client name"
          />
          <label className="form-label" style={{ marginTop: 10 }}>Type</label>
          <Dropdown
            value={draft.type}
            onChange={(v) => setDraft((d) => ({ ...d, type: v as PromptFieldType }))}
            ariaLabel="Field type"
            fullWidth
            options={TYPES.map((t) => ({ value: t.value, label: t.label }))}
          />
          {draft.type === "select" && (
            <>
              <label className="form-label" style={{ marginTop: 10 }}>Options <span className="opt">one per line</span></label>
              <textarea className="field sm" rows={3} value={optionsText} onChange={(e) => setOptionsText(e.target.value)} placeholder={"Indemnity\nLiability\nTermination"} />
            </>
          )}
          <label className="form-label" style={{ marginTop: 10 }}>Help <span className="opt">optional</span></label>
          <input className="field sm" value={draft.help} onChange={(e) => setDraft((d) => ({ ...d, help: e.target.value }))} placeholder="Hint shown under the field" />
          <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
            <button type="button" className="btn btn-gold sm" onClick={confirmField} disabled={!draft.label.trim() || (draft.type === "select" && !optionsText.trim())}>{editingChip.current ? "Update field" : "Add field"}</button>
            <button type="button" className="btn btn-ghost sm" onClick={() => { setPopover(false); editingChip.current = null; }}>Cancel</button>
          </div>
        </div>
      )}
    </div>
  );
}
