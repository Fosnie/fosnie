---
name: Document tables
description: Build clear, correct tables inside a document — comparison tables, schedules, obligation matrices, data summaries, term sheets. Use whenever a DOCX or PDF you are producing needs tabular data, or the user asks for information "in a table" / "as a grid" / "side by side". Covers Markdown pipe-table syntax, column discipline, and when a table is the wrong choice. Do NOT use for spreadsheet files (.xlsx) — this is tables *within* a document.
default: true
compatibility: both-profiles
license: proprietary
---

# Document tables

Tables are rendered as real Word tables (DOCX) or styled HTML tables (PDF). A good
table is scannable; a bad one is a wall of text in a grid. This skill is the
discipline for getting them right.

## Syntax (Markdown pipe tables)

```
| Clause          | Party bound | Trigger            | Remedy             |
| --------------- | ----------- | ------------------ | ------------------ |
| Confidentiality | Both        | On disclosure      | Injunction         |
| Non-compete     | Recipient   | On termination     | Liquidated damages |
| Audit rights    | Discloser   | On 5 days' notice  | Inspection         |
```

- A **header row** is mandatory, followed by the `---` separator row.
- Alignment lives in the separator: `:---` left, `:--:` centre, `---:` right
  (right-align numeric columns).
- Escape a literal pipe in a cell as `\|`.

## When a table is the right tool

Use a table when every row shares the same **small set of attributes** — a
comparison, a schedule, a matrix. Reach for it when the alternative is a paragraph
that repeats "X is …, Y is …, Z is …".

Do **not** use a table for:

- A single list of items → use a bullet list.
- Long prose per cell → that is a section with sub-headings, not a table.
- Nested structure → cells cannot contain lists or headings; restructure instead.

## Column discipline (the difference between good and bad)

- **3–6 columns.** More than ~6 will not fit a portrait page; split the table or
  rotate it (attributes as rows).
- **Short cells.** A few words each. If a cell needs a sentence, the table is doing
  the wrong job.
- **Consistent units and format** down a column — all dates the same way, all money
  with the same currency and precision, right-aligned.
- **A header for every column**, in Title Case, naming the attribute not an example.
- **No empty cells** — write "—" or "None" so a blank is never ambiguous.
- **One idea per column.** Do not pack "Party / Date" into one cell.

## Gotchas

- Blank line **before** the table or it merges with the preceding paragraph.
- Keep the total width sane: in a PDF the page width is fixed; an over-wide table is
  clipped or shrunk to unreadable. Prefer fewer, well-chosen columns.
- The *DOCX documents* skill bundles `references/markdown-style.md` with the
  shared Markdown gotchas — read it via that skill, not a relative path.

## Why this matters

In legal and financial documents a table is often the load-bearing content — an
obligations matrix, a payment schedule, a comparison a decision rests on. A
mis-aligned column or an ambiguous blank cell is a real error, not a cosmetic one.
Discipline here is correctness, not polish.
