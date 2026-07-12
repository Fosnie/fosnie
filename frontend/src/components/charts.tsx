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

// Hand-rolled SVG/CSS charts (no chart lib) — ports the prototype's analytics.jsx.
// Accent-on-near-black, sharp geometry. Used by Admin → Analytics.

import { useState, type PointerEvent } from "react";

/** Circular progress donut with a centred percentage. */
export function Donut({ pct, label, sub }: { pct: number; label: string; sub?: string }) {
  const r = 30;
  const c = 2 * Math.PI * r;
  return (
    <div className="donut-stat">
      <svg width="84" height="84" viewBox="0 0 84 84">
        <circle cx="42" cy="42" r={r} fill="none" stroke="var(--navy-lighter)" strokeWidth="7" />
        <circle
          cx="42"
          cy="42"
          r={r}
          fill="none"
          stroke="var(--gold)"
          strokeWidth="7"
          strokeLinecap="round"
          strokeDasharray={`${(pct / 100) * c} ${c}`}
          transform="rotate(-90 42 42)"
        />
        <text x="42" y="47" textAnchor="middle" className="donut-pct">{pct}%</text>
      </svg>
      <div className="donut-meta">
        <span className="donut-label">{label}</span>
        {sub && <span className="donut-sub mono">{sub}</span>}
      </div>
    </div>
  );
}

/** "Nice" axis steps: round `max` up to a 1/2/5×10ⁿ step and return the ticks (0..niceMax). */
function niceTicks(max: number, count = 4): { max: number; ticks: number[] } {
  if (!(max > 0)) return { max: 1, ticks: [0, 1] };
  const rawStep = max / count;
  const mag = Math.pow(10, Math.floor(Math.log10(rawStep)));
  const norm = rawStep / mag;
  const step = (norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 5 ? 5 : 10) * mag;
  const niceMax = Math.ceil(max / step) * step;
  const ticks: number[] = [];
  for (let t = 0; t <= niceMax + step * 1e-6; t += step) ticks.push(t);
  return { max: niceMax, ticks };
}

/** Default X formatter: ISO day ("2026-07-09") → short local date ("9 Jul"). */
function shortDate(iso: string): string {
  const d = new Date(iso.length <= 10 ? iso + "T00:00:00" : iso);
  if (isNaN(d.getTime())) return iso;
  return d.toLocaleDateString(undefined, { day: "numeric", month: "short" });
}

/**
 * Filled area line chart over a numeric series (e.g. 30-day daily tokens), with a dynamic
 * Y axis (nice steps), an X axis of dates (when `labels` given) and a hover tooltip
 * (date + value). Responsive via a uniform-scaled viewBox — text never distorts.
 */
export function AreaChart({
  series, labels, formatValue = String, formatX = shortDate,
}: {
  series: number[];
  labels?: string[];
  formatValue?: (n: number) => string;
  formatX?: (label: string) => string;
}) {
  const [hover, setHover] = useState<number | null>(null);
  if (!series.length) return <div className="area-chart-empty" />;

  const VBW = 800, VBH = 300, L = 56, R = 14, T = 14, B = 28;
  const plotW = VBW - L - R, plotH = VBH - T - B;
  const n = series.length;
  const { max: domainMax, ticks } = niceTicks(Math.max(...series), 4);
  const x = (i: number) => L + (i / (n - 1 || 1)) * plotW;
  const y = (v: number) => T + (1 - v / domainMax) * plotH;

  const line = series.map((v, i) => `${i === 0 ? "M" : "L"}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(" ");
  const area = `${line} L${x(n - 1).toFixed(1)},${(T + plotH).toFixed(1)} L${x(0).toFixed(1)},${(T + plotH).toFixed(1)} Z`;
  const last = series[n - 1];

  // ~6 evenly-spaced X-axis label indices (always include the last point).
  const xCount = Math.min(6, n);
  const xIdx = labels
    ? Array.from({ length: xCount }, (_, k) => Math.round((k / (xCount - 1 || 1)) * (n - 1)))
    : [];

  const onMove = (e: PointerEvent<HTMLDivElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    if (!rect.width) return;
    const vbX = ((e.clientX - rect.left) / rect.width) * VBW;
    const frac = (vbX - L) / plotW;
    const i = Math.max(0, Math.min(n - 1, Math.round(frac * (n - 1))));
    setHover(i);
  };

  const tip = hover != null ? { i: hover, lx: (x(hover) / VBW) * 100, ty: (y(series[hover]) / VBH) * 100 } : null;
  const tipTX = tip ? (tip.lx < 15 ? "0%" : tip.lx > 85 ? "-100%" : "-50%") : "-50%";
  // Flip the tooltip below the point when the point is near the top (e.g. the peak), so it
  // doesn't clip above the card.
  const tipTY = tip && tip.ty < 24 ? "calc(0% + 14px)" : "calc(-100% - 12px)";

  return (
    <div className="area-chart-wrap" onPointerMove={onMove} onPointerLeave={() => setHover(null)}>
      <svg className="area-chart" viewBox={`0 0 ${VBW} ${VBH}`} preserveAspectRatio="xMidYMid meet">
        <defs>
          <linearGradient id="areaFill" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor="var(--gold)" stopOpacity="0.28" />
            <stop offset="100%" stopColor="var(--gold)" stopOpacity="0" />
          </linearGradient>
        </defs>
        {/* Y gridlines + labels */}
        {ticks.map((t, k) => (
          <g key={k}>
            <line className="ax-grid" x1={L} y1={y(t)} x2={VBW - R} y2={y(t)} />
            <text className="ax-txt" x={L - 8} y={y(t)} textAnchor="end" dominantBaseline="central">{formatValue(t)}</text>
          </g>
        ))}
        {/* X date labels */}
        {xIdx.map((i, k) => (
          <text key={k} className="ax-txt" x={x(i)} y={T + plotH + 18} textAnchor={k === 0 ? "start" : k === xIdx.length - 1 ? "end" : "middle"}>
            {formatX(labels![i])}
          </text>
        ))}
        <path d={area} fill="url(#areaFill)" />
        <path d={line} fill="none" stroke="var(--gold)" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
        {tip && <line className="ax-guide" x1={x(tip.i)} y1={T} x2={x(tip.i)} y2={T + plotH} />}
        <circle cx={x(n - 1)} cy={y(last)} r="3" fill="var(--gold)" />
        {tip && <circle cx={x(tip.i)} cy={y(series[tip.i])} r="4" fill="var(--gold)" stroke="var(--bg-2)" strokeWidth="1.5" />}
      </svg>
      {tip && (
        <div className="chart-tip" style={{ left: `${tip.lx}%`, top: `${tip.ty}%`, transform: `translate(${tipTX}, ${tipTY})` }}>
          {labels && <div className="chart-tip-x mono">{formatX(labels[tip.i])}</div>}
          <div className="chart-tip-v">{formatValue(series[tip.i])}</div>
        </div>
      )}
    </div>
  );
}

export interface BarDatum {
  name: string;
  /** Raw value — drives the bar width (keep it un-rounded so small values still show). */
  v: number;
  /** Optional pre-formatted display label (e.g. "12.4k"); falls back to `v + unit`. */
  label?: string;
}

/** Horizontal bars: label · track · value. `accentTop` highlights the first bar. */
export function Bars({ data, unit = "", accentTop = false }: { data: BarDatum[]; unit?: string; accentTop?: boolean }) {
  const max = Math.max(1, ...data.map((d) => d.v));
  return (
    <div className="bars">
      {data.map((d, i) => (
        <div key={i} className="bar-row">
          <span className="bar-label" title={d.name}>{d.name}</span>
          <div className="bar-track">
            <div className={"bar-fill" + (accentTop && i === 0 ? " top" : "")} style={{ width: (d.v / max) * 100 + "%" }} />
          </div>
          <span className="bar-val mono">{d.label ?? `${d.v}${unit}`}</span>
        </div>
      ))}
    </div>
  );
}
