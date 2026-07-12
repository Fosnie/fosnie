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

import type { PromptVariable } from "@/api/client";
import { Dropdown } from "@/components/Dropdown";

/** Title-case a bare `{{key}}` for legacy prompts with no field metadata. */
function deslug(key: string): string {
  const s = key.replace(/_+/g, " ").trim();
  return s ? s.charAt(0).toUpperCase() + s.slice(1) : key;
}

/** Ordered fields for a prompt: metadata-described first (in author order), then
 *  any template keys lacking metadata (derived as short-text). */
export function fieldsFor(placeholders: string[], variables: PromptVariable[]): PromptVariable[] {
  const present = new Set(placeholders);
  const seen = new Set<string>();
  const out: PromptVariable[] = [];
  for (const v of variables) {
    if (present.has(v.key) && !seen.has(v.key)) { out.push(v); seen.add(v.key); }
  }
  for (const k of placeholders) {
    if (!seen.has(k)) { out.push({ key: k, label: deslug(k), type: "short" }); seen.add(k); }
  }
  return out;
}

/** Typed fill inputs for a prompt's fields (short / long / date / dropdown), with
 *  friendly labels + help. Controlled by the parent's `values` map. Shared by the
 *  Prompts screen, the "+" picker, and the composer "/" fill. */
export function PromptFillForm({
  placeholders,
  variables,
  values,
  onChange,
}: {
  placeholders: string[];
  variables: PromptVariable[];
  values: Record<string, string>;
  onChange: (key: string, value: string) => void;
}) {
  const fields = fieldsFor(placeholders, variables);
  if (fields.length === 0) return <div className="ed-hint mono">This prompt has no fields — insert it as-is.</div>;
  return (
    <div className="ph-fields">
      {fields.map((f) => (
        <div key={f.key} className="fill-field">
          <label className="form-label">{f.label}</label>
          {f.help && <div className="field-help">{f.help}</div>}
          {f.type === "long" ? (
            <textarea className="field" rows={3} value={values[f.key] ?? ""} onChange={(e) => onChange(f.key, e.target.value)} placeholder={"Enter " + f.label.toLowerCase()} />
          ) : f.type === "date" ? (
            <input type="date" className="field sm" value={values[f.key] ?? ""} onChange={(e) => onChange(f.key, e.target.value)} />
          ) : f.type === "select" ? (
            <Dropdown
              value={values[f.key] ?? ""}
              onChange={(v) => onChange(f.key, v)}
              ariaLabel={f.label}
              fullWidth
              options={[
                { value: "", label: "Select…" },
                ...(f.options ?? []).map((o) => ({ value: o, label: o })),
              ]}
            />
          ) : (
            <input className="field sm" value={values[f.key] ?? ""} onChange={(e) => onChange(f.key, e.target.value)} placeholder={"Enter " + f.label.toLowerCase()} />
          )}
        </div>
      ))}
    </div>
  );
}
