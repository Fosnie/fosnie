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

import { Outlet, useLocation, useNavigate } from "react-router-dom";
import { Icon } from "@/components/icons";

// The Studio building blocks — one page, internal tabs. Each tab is a nested
// route under /studio/<id>; the sub-screens keep their own detail routes
// (/studio/agents/:agentId …) and bring their own .main-scroll > .panel.
const TABS = [
  { id: "agents", label: "Agents", icon: Icon.Agents },
  { id: "libraries", label: "Library", icon: Icon.Book },
  { id: "automations", label: "Automations", icon: Icon.Automations },
  { id: "prompts", label: "Prompts", icon: Icon.Prompts },
  { id: "memory", label: "Memory", icon: Icon.Memory },
] as const;

export function Studio() {
  const nav = useNavigate();
  const { pathname } = useLocation();
  const active = pathname.split("/")[2] ?? "agents";

  return (
    <div className="legal-shell">
      <div className="legal-tabs">
        <div className="legal-tabs-l" style={{ overflowX: "auto" }}>
          {TABS.map(({ id, label, icon: I }) => (
            <button
              key={id}
              className={"legal-tab" + (active === id ? " on" : "")}
              onClick={() => nav(`/studio/${id}`)}
            >
              <I size={15} /> {label}
            </button>
          ))}
        </div>
        <div className="legal-tabs-r mono"><Icon.Blocks size={13} /> Studio</div>
      </div>

      <div className="legal-body">
        <Outlet />
      </div>
    </div>
  );
}
