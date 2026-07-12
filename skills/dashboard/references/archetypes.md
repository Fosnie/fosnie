# Page archetypes

Three layouts that cover most needs. All use the `.pai-*` classes and the scaffold
in `SKILL.md`. Pick the one that fits the content; combine sections freely.

## 1. Executive summary

Top-down: headline → KPI strip → two or three supporting charts → a short narrative
→ sources. The reader gets the answer in the first screen.

```
header (title + one-line context)
pai-kpis        — 3–5 KPI cards (the numbers that matter)
pai-grid
  pai-card.pai-half  — chart: the main breakdown (pie/bar)
  pai-card.pai-half  — chart: the trend (line)
  pai-card           — short prose: "What this means" (2–3 sentences)
pai-sources
```

## 2. Comparison

Put options/entities side by side. A grouped or stacked bar plus a comparison table.

```
header
pai-grid
  pai-card           — grouped bar: metric across options
  pai-card.pai-half  — table: options × attributes (pai-table)
  pai-card.pai-half  — pie/gauge: a single decisive ratio
pai-sources
```

Use `.pai-badge good|warn|bad` in table cells to flag winners/risks.

## 3. Timeline infographic

A sequence of events/phases with a measure per step. Use a line/area for the measure
and a vertical list of labelled steps.

```
header
pai-card           — line/area across the timeline (x = period)
pai-grid
  pai-card.pai-half ×N — one card per phase: period, headline, 1–2 metrics
pai-sources
```

## Notes

- Keep to **one screen of signal** before the fold; detail below.
- Prefer **3–6 KPIs**; more dilutes. Each KPI is a number + a short label.
- Every chart needs a `<h2>` saying what it shows.
- Numbers and dates: consistent units and formatting across the page.
- Always end with `pai-sources` carrying the citation numbers from the source.
