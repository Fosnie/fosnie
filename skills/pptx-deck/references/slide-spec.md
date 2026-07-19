# Slide spec reference

Top level: `{ "slides": [ ... ] }`. Each slide is an object with `"layout"`,
layout-specific fields, and an optional `"notes"` string (speaker notes).
Unknown fields are ignored; an unknown `layout` fails the whole request.

## Layouts and budgets

Budgets are soft: the engine steps the font down on overflow, and splits bullet
lists (>6) and table rows (>8) onto continuation slides. Write inside the
budgets; shrinking is a rescue, not a licence.

| layout | fields | budgets |
|---|---|---|
| `title` | `title`, `subtitle?` | title ≤60 chars, subtitle ≤90 |
| `section` | `title` | ≤50; the engine numbers sections automatically |
| `bullets` | `title`, `bullets` | ≤5 bullets, ≤90 chars each; one `sub` level, ≤3 items of ≤70 |
| `two_column` | `title`, `left`, `right` | per column: `heading?` ≤40, ≤4 bullets |
| `stat` | `title`, `stats` | 1-4 items of `{ "value", "label" }`; value ≤12 chars, label ≤45 |
| `table` | `title`, `columns`, `rows`, `caption?` | ≤5 columns; rows split after 8; caption ≤100 |
| `chart` | `title`, `chart`, `caption?` | see below |
| `quote` | `text`, `attribution?` | text ≤220, attribution ≤60 |
| `closing` | `title`, `bullets?` | ≤4 bullets |

`bullets` items are either strings or `{ "text": "...", "sub": ["..."] }`.

## Charts

```json
"chart": {
  "type": "column",
  "categories": ["Jan", "Feb", "Mar"],
  "series": [ { "name": "Overdue", "values": [9, 11, 14] } ]
}
```

- `type`: `bar` | `column` | `line` | `pie` | `doughnut`.
- ≤8 categories, ≤4 series. `pie`/`doughnut` take exactly one series.
- `values` are raw numbers (no "£" or "%" strings); put units in the series
  name or the `caption`.
- Charts are native OOXML parts: the recipient can restyle and edit the data in
  PowerPoint.

## Worked example

A complete six-slide findings deck is shown in the skill body. Pattern to copy:
assertion title on every content slide, one layout change between neighbouring
slides, narration in `notes`, numbers in `stat`/`chart` rather than prose.
