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

// Workmode = General vs Legal vs Research. Gates the agent set, project/chat
// scoping (by project `sector`), the Legal workspace surface, and the Deep
// Research mode (research runs live only there). Persisted across reloads.

import { createContext, use, useEffect, useState, type ReactNode } from "react";

export type Workmode = "general" | "legal" | "research";

interface WorkmodeState {
  mode: Workmode;
  setMode: (m: Workmode) => void;
}

const Ctx = createContext<WorkmodeState | null>(null);
const KEY = "pai.workmode";

export function WorkmodeProvider({ children }: { children: ReactNode }) {
  const [mode, setMode] = useState<Workmode>(
    () => (localStorage.getItem(KEY) as Workmode) || "general",
  );
  useEffect(() => {
    localStorage.setItem(KEY, mode);
  }, [mode]);
  return <Ctx value={{ mode, setMode }}>{children}</Ctx>;
}

export function useWorkmode(): WorkmodeState {
  const ctx = use(Ctx);
  if (!ctx) throw new Error("useWorkmode outside WorkmodeProvider");
  return ctx;
}
