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

/** Shared UI primitives — "Digital Swiss Bank" design system (sharp geometry,
 *  gold accents). Loading/empty states plus the redesign building blocks. */

import { useId } from "react";

export function Spinner({ label = "Loading…" }: { label?: string }) {
  return (
    <div className="flex h-full items-center justify-center text-sm text-slate">
      <span className="animate-pulse">{label}</span>
    </div>
  );
}

export function EmptyState({ children }: { children: React.ReactNode }) {
  return <div className="flex h-full items-center justify-center text-sm text-slate/70">{children}</div>;
}

/** Uppercase gold label (Barlow 11px / 0.14em). */
export function Eyebrow({ children, className = "" }: { children: React.ReactNode; className?: string }) {
  return <div className={"eyebrow " + className}>{children}</div>;
}

/** Raised panel/card: navy-light surface, hairline border, sharp 2px radius. */
export function Card({ children, className = "" }: { children: React.ReactNode; className?: string }) {
  return (
    <div className={"rounded-sm border border-line bg-navy-light/60 " + className}>{children}</div>
  );
}

/** Small status dot. tone → colour. */
export function StatusDot({ tone = "ok", className = "" }: { tone?: "ok" | "warn" | "risk" | "idle"; className?: string }) {
  const c =
    tone === "ok" ? "bg-ok" : tone === "warn" ? "bg-warn" : tone === "risk" ? "bg-risk" : "bg-slate";
  return <span className={`inline-block h-1.5 w-1.5 rounded-full ${c} ${className}`} />;
}

/** Segmented control (sliding selection). Square chips, gold active. */
export function SegmentedControl<T extends string>({
  options,
  value,
  onChange,
  className = "",
}: {
  options: { value: T; label: React.ReactNode }[];
  value: T;
  onChange: (v: T) => void;
  className?: string;
}) {
  return (
    <div className={"inline-flex rounded-sm border border-line bg-navy p-0.5 " + className}>
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            onClick={() => onChange(o.value)}
            className={
              "px-3.5 py-1.5 text-sm font-medium transition-colors " +
              (active ? "bg-gold text-navy" : "text-slate hover:text-slate-lightest")
            }
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/** Underline tab strip. */
export function Tabs<T extends string>({
  tabs,
  value,
  onChange,
  right,
  className = "",
}: {
  tabs: { value: T; label: React.ReactNode }[];
  value: T;
  onChange: (v: T) => void;
  right?: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={"flex items-center gap-1 border-b border-line " + className}>
      {tabs.map((t) => {
        const active = t.value === value;
        return (
          <button
            key={t.value}
            onClick={() => onChange(t.value)}
            className={
              "-mb-px border-b-2 px-3.5 py-2.5 text-sm font-medium transition-colors " +
              (active
                ? "border-gold text-slate-lightest"
                : "border-transparent text-slate hover:text-slate-lightest")
            }
          >
            {t.label}
          </button>
        );
      })}
      {right && <div className="ml-auto pr-1">{right}</div>}
    </div>
  );
}

/** Pill toggle switch (the one place pills are allowed). */
export function Toggle({
  on,
  onChange,
  label,
  disabled,
}: {
  on: boolean;
  onChange: (v: boolean) => void;
  label?: string;
  disabled?: boolean;
}) {
  // Uses the design's institutional pill switch (`.toggle`/`.toggle-knob`, the same
  // geometry as `editor.tsx`'s Switch) so the knob always stays inside the track —
  // the old Tailwind sizing let the knob overflow the pill on the "on" state.
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!on)}
      className={"toggle" + (on ? " on" : "")}
      style={disabled ? { opacity: 0.5, cursor: "default" } : undefined}
    >
      <span className="toggle-knob" />
    </button>
  );
}

/** Labelled slider: gold track fill + square thumb (native range, restyled). */
export function Slider({
  label,
  value,
  min,
  max,
  step,
  onChange,
  format = (v) => String(v),
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (v: number) => void;
  format?: (v: number) => string;
}) {
  const id = useId();
  const pct = ((value - min) / (max - min)) * 100;
  return (
    <div>
      <div className="mb-1.5 flex items-center justify-between">
        <label htmlFor={id} className="text-sm text-slate-light">{label}</label>
        <span className="mono text-xs text-slate-lightest">{format(value)}</span>
      </div>
      <input
        id={id}
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="ed-slider w-full"
        style={{ background: `linear-gradient(to right, var(--color-gold) ${pct}%, var(--color-navy-lighter) ${pct}%)` }}
      />
    </div>
  );
}

/** Centred modal: scrim (click-out closes) + fade-up sharp card. */
export function Modal({
  title,
  eyebrow,
  onClose,
  children,
  footer,
  width = "max-w-lg",
}: {
  title: React.ReactNode;
  eyebrow?: React.ReactNode;
  onClose: () => void;
  children: React.ReactNode;
  footer?: React.ReactNode;
  width?: string;
}) {
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-navy-deep/70 p-4"
      onClick={onClose}
    >
      <div
        className={`anim-on fade-up flex max-h-[88vh] w-full ${width} flex-col rounded border border-line bg-navy-light shadow-[var(--shadow-pop)]`}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between border-b border-line px-5 py-4">
          <div>
            {eyebrow && <Eyebrow>{eyebrow}</Eyebrow>}
            <h3 className="mt-0.5 text-lg">{title}</h3>
          </div>
          <button onClick={onClose} className="text-slate hover:text-slate-lightest">✕</button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">{children}</div>
        {footer && <div className="flex justify-end gap-2 border-t border-line px-5 py-3">{footer}</div>}
      </div>
    </div>
  );
}
