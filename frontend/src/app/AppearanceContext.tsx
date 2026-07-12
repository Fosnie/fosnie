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

// Appearance = the per-user skin controls introduced by the glass-era UI refresh.
// Three render tiers (translucency), a density
// toggle and a motion toggle. Persisted across reloads; applied to <html> as
// data-* attributes by the Shell so the CSS token bundles switch with one flip.

import { createContext, use, useEffect, useState, type ReactNode } from "react";

export type GlassTier = "tinted" | "reduced" | "contrast";
export type Density = "comfortable" | "compact";
export type Motion = "full" | "reduced";
// Palette skin: the fosnie.dev near-black/purple default vs the classic navy/gold
// look (preserved as an A/B toggle + candidate Enterprise skin). Reflected as
// [data-theme] on <html>; the token bundle in design.css switches on the attribute.
export type Theme = "fosnie" | "gold" | "classic";

export interface Appearance {
  glass: GlassTier;
  density: Density;
  motion: Motion;
  theme: Theme;
}

interface AppearanceState extends Appearance {
  set: (patch: Partial<Appearance>) => void;
}

const DEFAULTS: Appearance = { glass: "tinted", density: "comfortable", motion: "full", theme: "fosnie" };
const Ctx = createContext<AppearanceState | null>(null);
const KEY = "pai.appearance";

function load(): Appearance {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return DEFAULTS;
    return { ...DEFAULTS, ...(JSON.parse(raw) as Partial<Appearance>) };
  } catch {
    return DEFAULTS;
  }
}

export function AppearanceProvider({ children }: { children: ReactNode }) {
  const [appearance, setAppearance] = useState<Appearance>(load);

  // Persist + reflect onto <html> as data-* so the CSS token bundles (glass tier,
  // density) and the motion gate switch with a single attribute flip. Sits
  // alongside the branding overrides the Shell writes to documentElement.style.
  useEffect(() => {
    localStorage.setItem(KEY, JSON.stringify(appearance));
    const root = document.documentElement;
    root.dataset.glass = appearance.glass;
    root.dataset.density = appearance.density;
    root.dataset.motion = appearance.motion;
    root.dataset.theme = appearance.theme;
  }, [appearance]);

  const set = (patch: Partial<Appearance>) => setAppearance((a) => ({ ...a, ...patch }));
  return <Ctx value={{ ...appearance, set }}>{children}</Ctx>;
}

export function useAppearance(): AppearanceState {
  const ctx = use(Ctx);
  if (!ctx) throw new Error("useAppearance outside AppearanceProvider");
  return ctx;
}
