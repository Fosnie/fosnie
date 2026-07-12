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

import { createContext, use, useMemo, useState, type ReactNode } from "react";
import type { ProjectSummary } from "@/api/client";

interface ProjectState {
  active: ProjectSummary | null;
  setActive: (p: ProjectSummary | null) => void;
}

const Ctx = createContext<ProjectState | null>(null);

export function ProjectProvider({ children }: { children: ReactNode }) {
  const [active, setActive] = useState<ProjectSummary | null>(null);
  // Stable context value: a fresh object per render would re-render every
  // consumer on unrelated provider renders (re-audit §9.3).
  const value = useMemo(() => ({ active, setActive }), [active]);
  return <Ctx value={value}>{children}</Ctx>;
}

export function useActiveProject(): ProjectState {
  const ctx = use(Ctx);
  if (!ctx) throw new Error("useActiveProject outside ProjectProvider");
  return ctx;
}
