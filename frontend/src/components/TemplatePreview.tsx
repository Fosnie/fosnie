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

// The report-template preview, shared by the Deep Research picker and the Studio
// template editor so the two can never drift. Keyed on the outline mode, not the
// section count: `free` with a non-empty skeleton is valid (the headings are only
// a starting point) and must not read as a fixed structure.

interface Props {
  description: string;
  structure: string[];
  outlineMode: "constrained" | "free";
  /** Show an "(archived)" marker beside the description (picker only). */
  archived?: boolean;
}

export function TemplatePreview({ description, structure, outlineMode, archived }: Props) {
  return (
    <div className="tpl-preview">
      <p className="tpl-desc">
        {description || "No description yet."}
        {archived && (
          <span className="mono" style={{ marginLeft: 8, fontSize: "0.65rem", opacity: 0.7 }}>
            (archived)
          </span>
        )}
      </p>
      {outlineMode === "constrained" ? (
        structure.length > 0 ? (
          <p className="tpl-structure mono">{structure.join(" · ")}</p>
        ) : (
          <p className="tpl-structure mono">No sections yet.</p>
        )
      ) : structure.length > 0 ? (
        <p className="tpl-structure mono">
          {structure.join(" · ")} · starting point; the model may restructure
        </p>
      ) : (
        <p className="tpl-structure mono">Structure follows your question.</p>
      )}
    </div>
  );
}
