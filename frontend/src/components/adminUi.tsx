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

// Shared admin-console UI primitives (Core). Used by the Core admin sections and,
// via the edition overlay, by the private Enterprise edition's admin sections.
// Pure presentation — no data, no edition gating.

import type { ReactNode } from "react";

export const INPUT =
  "rounded-lg border border-navy-lighter bg-navy px-3 py-2 text-sm text-slate-lightest outline-none focus:border-gold disabled:opacity-60";
export const LABEL = "mb-1 block text-xs uppercase tracking-[0.14em] text-slate";
export const BTN = "rounded-lg bg-gold px-4 py-2 text-sm font-medium text-navy hover:bg-gold-light disabled:opacity-40";
export const BTN2 = "rounded-lg border border-navy-lighter px-3 py-1.5 text-sm text-slate hover:text-slate-lightest disabled:opacity-50";
export const BTN_DANGER = "rounded-lg border border-urgency-red/60 px-3 py-1.5 text-sm text-urgency-red hover:bg-urgency-red/10 disabled:opacity-50";
export const TH = "border border-navy-lighter bg-navy-light px-3 py-2 text-left font-medium text-slate";
export const TD = "border border-navy-lighter px-3 py-2 text-slate-lightest align-top";

export function Badge({ children, tone = "slate", className = "" }: { children: ReactNode; tone?: "slate" | "gold" | "red" | "green"; className?: string }) {
  const cls = {
    slate: "bg-navy-lighter text-slate",
    gold: "bg-gold/15 text-gold-light",
    red: "bg-urgency-red/15 text-urgency-red",
    green: "bg-gold/10 text-gold",
  }[tone];
  return <span className={"rounded-full px-2 py-0.5 text-[0.65rem] uppercase tracking-wide " + cls + (className ? " " + className : "")}>{children}</span>;
}

export function H1({ children }: { children: ReactNode }) {
  return <h1 className="mb-4 text-xl text-slate-lightest">{children}</h1>;
}
