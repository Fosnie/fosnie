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

// Shared building blocks for the secondary panels + their editors, mapped 1:1 to
// the prototype's panels.jsx / editors.jsx classes (.panel-head, .editor-wrap,
// .editor-grid, .toggle). Reused by Agents, Automations, Prompts, Memory, Teams.

import type { ReactNode } from "react";
import { Icon } from "@/components/icons";

export function PanelHead({ title, sub, action }: { title: string; sub?: string; action?: ReactNode }) {
  return (
    <div className="panel-head">
      <div>
        <h1 className="serif panel-title">{title}</h1>
        {sub && <p className="panel-sub">{sub}</p>}
      </div>
      {action}
    </div>
  );
}

// The design's institutional pill switch (distinct from ui.tsx's Toggle).
export function Switch({ on, onClick, disabled }: { on: boolean; onClick: () => void; disabled?: boolean }) {
  return (
    <button
      className={"toggle" + (on ? " on" : "")}
      onClick={onClick}
      disabled={disabled}
      aria-pressed={on}
      style={disabled ? { opacity: 0.5, cursor: "default" } : undefined}
    >
      <span className="toggle-knob" />
    </button>
  );
}

// Full-page editor scaffold: sticky bar (Back + actions), eyebrow + serif title,
// then the body (typically an .editor-grid). Scrolls inside .main-scroll.
export function EditorShell({
  eyebrow, title, onBack, actions, children,
}: {
  eyebrow: string;
  title: string;
  onBack: () => void;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="editor-wrap main-scroll">
      <div className="editor-bar">
        <button className="back-bar flush" onClick={onBack}><Icon.ChevronL size={16} /> Back</button>
        <div className="editor-bar-actions">{actions}</div>
      </div>
      <div className="editor-head">
        <div className="eyebrow">{eyebrow}</div>
        <h1 className="serif editor-title">{title}</h1>
      </div>
      <div className="editor-body">{children}</div>
    </div>
  );
}
