---
name: Presentations (PPTX)
description: Produce a downloadable PowerPoint (.pptx) deck - board packs, findings presentations, pitch and briefing decks with native editable text, tables and charts. Use whenever you generate a pptx artefact or the user asks for a "deck", "slides", "presentation", or ".pptx". You emit a JSON slide spec; the platform builds and validates the real file. Do NOT use for prose documents (see DOCX documents), computable data (see Spreadsheets (XLSX)) or on-screen pages (see Dashboards & informative pages).
default: true
compatibility: both-profiles
license: proprietary
---

# Presentations (PPTX)

You are drafting the **content of a slide deck**, not its geometry. Call
`generate_artefact` with `kind: "pptx"` and a **JSON slide spec** as `content`.
The platform renders a 16:9 deck with native, editable text, tables and charts,
then validates the file. You choose what each slide says and which layout carries
it; the engine owns fonts, sizes, colours and positioning.

## The JSON slide spec

```json
{
  "slides": [
    { "layout": "title",   "title": "Q2 Compliance Review", "subtitle": "Findings and recommendations" },
    { "layout": "section", "title": "What we found" },
    { "layout": "bullets", "title": "Three controls failed in the sample",
      "bullets": ["Access reviews missed 2 of 4 quarters", "No dual sign-off on payments over £50k",
                  { "text": "Retention policy not applied", "sub": ["4,200 records past deletion date"] }],
      "notes": "Walk through each failure; the detail sits in the appendix of the written report." },
    { "layout": "stat",  "title": "The exposure in numbers",
      "stats": [ { "value": "£1.2m", "label": "payments without dual sign-off" },
                 { "value": "38%",  "label": "of access reviews overdue" } ] },
    { "layout": "chart", "title": "Overdue reviews doubled since January",
      "chart": { "type": "column", "categories": ["Jan", "Feb", "Mar", "Apr", "May", "Jun"],
                 "series": [ { "name": "Overdue", "values": [9, 11, 14, 15, 17, 19] } ] } },
    { "layout": "closing", "title": "Next steps",
      "bullets": ["Dual sign-off live by 1 September", "Access review backlog cleared in Q3"] }
  ]
}
```

Layouts: `title`, `section`, `bullets`, `two_column`, `stat`, `table`, `chart`,
`quote`, `closing`. Any slide may carry `"notes"` (speaker notes). Full
field-by-field reference and budgets: `references/slide-spec.md`.

## Rules (correctness-critical)

- **One idea per slide.** The slide title states the idea; the body evidences it.
  If a slide needs two titles, it is two slides.
- **Titles are assertions, not topics.** "Overdue reviews doubled since January",
  never "Review status update". A reader skimming only the titles must get the
  whole argument.
- **Respect the budgets** (see `references/slide-spec.md`): at most 5 bullets of
  at most ~90 characters each. Bullets are fragments, not sentences - no full
  stops, no "and then". The engine shrinks or splits overflow, but a shrunk
  slide is a badly written slide.
- **Narration goes in `notes`, not on the slide.** Whatever you would say out
  loud belongs in speaker notes; the slide surface carries only the headline and
  the evidence.
- **Charts are data, never decoration.** Supply real `categories` and `values`
  from the material. Never invent numbers to fill a chart; if you have no data,
  use `bullets` or `stat` instead.

## Choosing the layout

- A **trend, comparison or shape** in the data → `chart` (column/bar for
  comparison, line for trend, pie only for a simple share of a whole).
- **Exact figures** the audience will read or check, up to ~5 columns → `table`.
- **One to four numbers that ARE the message** → `stat` (a big number beats a
  one-bar chart every time).
- **Two options, before/after, us/them** → `two_column`.
- A **verbatim voice** (clause, witness, customer) → `quote`.
- Everything else → `bullets`, and vary the rhythm: three bullet slides in a row
  reads as a wall.

## Shaping the deck

- Slide count follows the material - typically 8 to 15. Do not pad; do not cram.
- A working arc for findings and board decks: `title` → context (`bullets`) →
  `section` per theme → evidence (`chart`/`table`/`stat`) → implications →
  recommendation → `closing` with owners and dates.
- Open sections with a `section` slide when the deck has more than ~8 slides.
- Close with `closing`: concrete next steps, named owners, dates.

## Why this matters

A deck is read in the room, under time pressure, by the most senior audience the
material will ever get - a partner, a board, a regulator. Assertion titles let a
chair absorb the argument in a skim; clean budgets keep the deck legible on a
projector; notes keep the presenter honest. A cluttered or hand-wavy deck reads
as a cluttered or hand-wavy engagement.
