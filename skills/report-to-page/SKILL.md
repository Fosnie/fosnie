---
name: Report to page
description: Internal skill injected by the Deep Research "Create page" button — turns a research report into a self-contained informative HTML page. Not for organic use (the button injects it deterministically).
default: false
compatibility: both-profiles
license: proprietary
---

# Report → informative page

You convert a research report (given as Markdown in the user message) into ONE
**self-contained, offline-portable HTML page** — an informative, designed summary a
reader can skim in a minute and download to keep.

## Output contract (strict)

- Respond with the **raw HTML document only**. Start at `<!DOCTYPE html>` and end at
  `</html>`. No Markdown, no code fences, no preamble, no closing remarks.
- The page must work with **no network**. Never write `<script src="…">`,
  `<link href="…">`, `@import`, web-font URLs, or external `<img src>`. External
  references are rejected.
- For charts, write `<!-- pai:echarts -->` once in `<head>` — the platform inlines the
  charting library there. For the house styling, write `<!-- pai:theme -->` in
  `<head>` — the platform inlines the theme variables and `.pai-*` classes there. A
  Content-Security-Policy is injected automatically; do not add one.

## What to produce

Read the report and build an **executive-summary page**:

1. **Title + context** — a real title (from the report) and a one-line subtitle.
2. **KPI strip** — 3–5 of the most important numbers/findings as `.pai-kpi` cards.
   If the report has few hard numbers, use short qualitative highlights instead.
3. **Charts where the data supports them** — turn tables / figures / comparisons in
   the report into 1–3 ECharts charts (pie for composition, line for trend, bar for
   comparison). Put the data in a JSON island and parse it. Do **not** invent data —
   only chart what the report states; if there is nothing chartable, omit charts and
   use prose + tables.
4. **Sectioned narrative** — the report's key points under `<h2>` headings, tightened
   for skim-reading (short paragraphs, bullet lists, `.pai-table` for tabular bits).
5. **Sources** — a `.pai-sources` footer carrying the citation numbers `[1]`, `[2]`
   from the report.

## Scaffold

```html
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>REPORT TITLE</title>
  <!-- pai:theme -->
  <!-- pai:echarts -->
</head>
<body>
  <main class="pai-wrap">
    <header class="pai-header">
      <h1>REPORT TITLE</h1>
      <div class="pai-sub">ONE-LINE CONTEXT</div>
    </header>

    <section class="pai-kpis">
      <div class="pai-kpi"><div class="pai-kpi-val">…</div><div class="pai-kpi-label">…</div></div>
      <!-- 3–5 cards -->
    </section>

    <section class="pai-grid">
      <div class="pai-card pai-half"><h2>…</h2><div class="pai-chart" id="c1"></div></div>
      <div class="pai-card"><h2>Findings</h2><p>…</p></div>
    </section>

    <footer class="pai-sources">Sources<ol><li>… [1]</li></ol></footer>
  </main>

  <script type="application/json" id="data">{ "c1": { } }</script>
  <script>
    const DATA = JSON.parse(document.getElementById('data').textContent);
    const css = getComputedStyle(document.documentElement);
    const palette = ['--pai-c1','--pai-c2','--pai-c3','--pai-c4','--pai-c5','--pai-c6']
      .map(v => css.getPropertyValue(v).trim() || '#2f6db5');
    // echarts.init(document.getElementById('c1')).setOption({ color: palette, … });
  </script>
</body>
</html>
```

## Available `.pai-*` classes

`pai-wrap`, `pai-header` (`h1`, `.pai-sub`), `pai-kpis`/`pai-kpi`
(`.pai-kpi-val`, `.pai-kpi-label`), `pai-grid`, `pai-card` (and `pai-card.pai-half`),
`pai-chart`, `pai-table`, `pai-sources`, `pai-badge good|warn|bad`. Add small inline
CSS only if needed.

## ECharts usage

One `echarts.init(el).setOption({ color: palette, … })` per chart, container sized via
`.pai-chart`. Pull colours from `--pai-c1…--pai-c6`. Keep options minimal: `tooltip`,
axes, one `series`. Chart only what the report contains.

## Rules

- **Never invent facts or numbers** — every figure on the page must come from the
  report. Faithful summarisation, not embellishment.
- **British English**, professional register.
- Keep it to one tight page of signal; depth goes in the sections, not the KPI strip.
- Output the HTML document and nothing else.
