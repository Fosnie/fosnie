---
name: Spreadsheets (XLSX)
description: Produce a downloadable Excel (.xlsx) spreadsheet — data tables, schedules, financial models, calculation sheets with live formulas. Use whenever you generate an `xlsx` artefact or the user asks for a "spreadsheet", "Excel file", ".xlsx", or a table they can sort/filter/compute on. You emit a JSON workbook spec; the platform builds the real workbook. Do NOT use for tables *inside* a document (use Document tables / DOCX) or for read-only data (use Markdown).
default: true
compatibility: both-profiles
license: proprietary
---

# Spreadsheets (XLSX)

Call `generate_artefact` with `kind: "xlsx"` and a **JSON workbook spec** as
`content`. The platform builds a real `.xlsx` with openpyxl — a styled header row,
column number formats, and **live formulas** that recalculate in Excel / LibreOffice
/ Sheets.

## The JSON workbook spec

```json
{
  "sheets": [
    {
      "name": "Q2 Costs",
      "columns": [
        { "header": "Item" },
        { "header": "Qty" },
        { "header": "Unit £", "format": "#,##0.00" },
        { "header": "Total £", "format": "#,##0.00" }
      ],
      "rows": [
        ["Licences", 12, 250, "=B2*C2"],
        ["Support",   1, 4000, "=B3*C3"],
        ["Total",    "", "",   "=SUM(D2:D3)"]
      ]
    }
  ]
}
```

Rules of the format:

- **`sheets`** — an array; one workbook tab each. (You may also send a single
  `{ "name", "columns", "rows" }` object, or a bare array of rows.)
- **`columns`** — optional; each is `{ "header", "format"? }`. `format` is an Excel
  number format applied to that column's data cells (`"#,##0.00"` money,
  `"0.0%"` percent, `"yyyy-mm-dd"` date, `"#,##0"` integer).
- **`rows`** — an array of arrays; one inner array per row, left to right. Cell
  values are numbers, strings, booleans, or **formulas**.

## Formulas (the important capability)

A cell value that **begins with `=`** is written as a live formula and recalculates:

- Reference cells by their grid address: with a header row, data starts at **row 2**
  (`=B2*C2`), and `A`,`B`,`C`… are the columns left to right.
- Use real functions: `=SUM(D2:D10)`, `=AVERAGE(...)`, `=IF(...)`, `=VLOOKUP(...)`.
- Build totals/subtotals as formulas, **never** as pre-computed constants — the point
  of a spreadsheet is that the maths is live and auditable.

## Discipline (correctness-critical)

- **No `#REF!` / broken references.** Every formula must point at cells that exist;
  count the header row when addressing (data from row 2).
- **Consistent types down a column** — all money, all dates, all integers; set the
  column `format` rather than baking units into strings.
- **One value per cell**; headers in Title Case; a totals row clearly labelled.
- Keep numbers raw (no "£1,234" strings) — apply currency via `format` so they stay
  numeric and computable.

## Why this matters

A spreadsheet is delivered to be *used* — sorted, filtered, extended, and recomputed
by the recipient. A model with hard-coded "totals" or a `#REF!` is worse than useless
in a regulated finance context. Emit clean data + live formulas; the platform builds
and validates the file.
