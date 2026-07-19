---
name: Dashboards & informative pages
description: Build a beautiful, self-contained HTML page — a dashboard, infographic, analytical summary, KPI overview, or interactive data page with charts. Use whenever you generate an `html` artefact, or the user asks for a "dashboard", "infographic", "web page", "interactive report", or charts/graphs to explore. Produces ONE offline-portable file (charts via the inlined ECharts marker — never a CDN). Do NOT use for editable Word docs (DOCX documents), printable PDFs (PDF documents), spreadsheets, or slide decks (use Presentations (PPTX)).
default: true
compatibility: both-profiles
license: proprietary
---

# Dashboards & informative pages

You are authoring a **single self-contained HTML page** that the platform stores as
an `html` artefact: it is previewed in a sandboxed frame and downloads as one file
that opens offline anywhere. Call `generate_artefact` with `kind: "html"` and your
complete HTML as `content`.

## The single hard rule: no external resources

The page must work with **no network**. Therefore:

- **Never** point `<script src>` or `<link href>` at an external URL, and never use
  `@import`, web-font URLs, or external `<img>` sources. A page that references a CDN
  is **rejected** by the validator.
- For charts, write the marker `<!-- pai:echarts -->` once — the platform inlines the
  vendored Apache ECharts build there. You never paste library code.
- For the house styling, write `<!-- pai:theme -->` in `<head>` — the platform inlines
  the theme variables there. Use the `.pai-*` classes below.
- Images: omit, or embed as a `data:` URI only.
- A strict Content-Security-Policy is injected automatically; you do not add one.

## Scaffold (copy and adapt)

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Quarterly Risk Review</title>
  <!-- pai:theme -->
  <!-- pai:echarts -->
</head>
<body>
  <main class="pai-wrap">
    <header class="pai-header">
      <h1>Quarterly Risk Review</h1>
      <div class="pai-sub">Q2 2026 · prepared for the Board</div>
    </header>

    <section class="pai-kpis">
      <div class="pai-kpi"><div class="pai-kpi-val">£4.2m</div><div class="pai-kpi-label">Exposure</div></div>
      <div class="pai-kpi"><div class="pai-kpi-val">12</div><div class="pai-kpi-label">Open matters</div></div>
      <div class="pai-kpi"><div class="pai-kpi-val">3</div><div class="pai-kpi-label">High risk</div></div>
    </section>

    <section class="pai-grid">
      <div class="pai-card pai-half">
        <h2>Exposure by category</h2>
        <div class="pai-chart" id="chart-exposure"></div>
      </div>
      <div class="pai-card pai-half">
        <h2>Trend</h2>
        <div class="pai-chart" id="chart-trend"></div>
      </div>
    </section>

    <footer class="pai-sources">
      Sources
      <ol><li>Internal risk register, May 2026 [1]</li></ol>
    </footer>
  </main>

  <script type="application/json" id="data">
  {"exposure":[{"name":"Litigation","value":2.1},{"name":"Regulatory","value":1.3},{"name":"Contract","value":0.8}],
   "trend":{"x":["Q3","Q4","Q1","Q2"],"y":[3.1,3.6,3.9,4.2]}}
  </script>

  <script>
    const DATA = JSON.parse(document.getElementById('data').textContent);
    const css = getComputedStyle(document.documentElement);
    const palette = ['--pai-c1','--pai-c2','--pai-c3','--pai-c4','--pai-c5','--pai-c6']
      .map(v => css.getPropertyValue(v).trim() || '#2f6db5');

    echarts.init(document.getElementById('chart-exposure')).setOption({
      color: palette,
      tooltip: { trigger: 'item' },
      series: [{ type: 'pie', radius: ['45%','70%'], data: DATA.exposure }]
    });
    echarts.init(document.getElementById('chart-trend')).setOption({
      color: palette,
      tooltip: { trigger: 'axis' },
      xAxis: { type: 'category', data: DATA.trend.x },
      yAxis: { type: 'value' },
      series: [{ type: 'line', smooth: true, data: DATA.trend.y, areaStyle: {} }]
    });
  </script>
</body>
</html>
```

## How to build a good page

- **Data lives in a JSON island** (`<script type="application/json">`), parsed once.
  Keep data and presentation separate; the island must be valid JSON (it is checked).
- **Charts**: one `echarts.init(el).setOption({…})` per chart. Pull colours from the
  theme variables (`--pai-c1…--pai-c6`) so the page matches the house palette. See
  `references/echarts-snippets.md` for ready option blocks (bar, line, pie, gauge,
  stacked, treemap).
- **Archetypes**: pick a layout that fits the content — *executive summary*,
  *comparison*, or *timeline infographic*. See `references/archetypes.md`.
- **Carry citations**: keep the source numbers `[1]`, `[2]` from the underlying
  material and list them in the `.pai-sources` footer.
- Use the `.pai-*` classes (`pai-wrap`, `pai-header`, `pai-kpis`/`pai-kpi`,
  `pai-grid`, `pai-card`/`pai-card.pai-half`, `pai-chart`, `pai-table`,
  `pai-sources`, `pai-badge good|warn|bad`). Add small extra CSS inline if needed.

## Why this matters

The page is downloaded, archived, and opened on machines with no internet inside a
regulated perimeter. "Self-contained, no external URL" is not stylistic — it is the
zero-egress guarantee. A single CDN link breaks both the offline promise and the
security posture, which is why the platform inlines libraries for you and rejects
external references outright.
