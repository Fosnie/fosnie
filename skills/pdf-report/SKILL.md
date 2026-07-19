---
name: PDF documents
description: Produce a beautiful, downloadable PDF — report, briefing, research write-up, formatted letter, anything that must look designed and paginate cleanly with page numbers and running headers. Use whenever you are generating a PDF artefact or the user asks for a "PDF" to download, print, or circulate. Write clean Markdown with clear sections; it is rendered through a print stylesheet. Do NOT use for editable Word files (use DOCX documents), spreadsheets, or slide decks (use Presentations (PPTX)).
default: true
compatibility: both-profiles
license: proprietary
---

# PDF documents

You are drafting the **content of a PDF**. It is rendered through a print
stylesheet (CSS Paged Media): your Markdown becomes typeset pages with a running
header, page numbers in the footer, and proper heading/table styling. Write the
**finished document and nothing else**.

## Write clean, well-sectioned Markdown

The stylesheet does the design; you provide correct structure:

- `#` — the title (exactly one, first line). It sets the running header.
- `##` / `###` — sections and sub-sections. These drive pagination and the
  document outline; use them generously so long documents break sensibly.
- `-` / `1.` — bullet and numbered lists.
- Pipe tables for data (see `doc-tables`); they get zebra striping and borders from
  the stylesheet.
- `>` — a callout / standout note (rendered as a tinted block).

## Structure for print

1. **Title** — `# Quarterly Risk Review — Q2 2026`.
2. **Lead paragraph / executive summary** — the first thing a reader skims.
3. **Sections** under `##`, each self-contained enough to start on a new page if the
   engine breaks there.
4. **Sources / References** as a final `##` section when the document makes claims
   that need attribution (carry citation numbers from the source material).

## Rules

- **No bracketed placeholders** (`[Insert …]`) — a printed PDF cannot be filled in
  later. Concrete values or prose, never brackets.
- **Do not reference the artefact** or the act of producing it.
- **One `#` title only**; everything else `##`/`###`.
- **British English**, professional register.
- Avoid manual page breaks and ASCII rules — the page engine paginates; trust it.
- Keep tables narrow enough to fit the page width; very wide tables belong in a DOCX
  or should be split.

## Why this matters

A PDF is the fixed, circulated, archived form of a document — it cannot be edited
after the fact, so it must be complete and correct on the first render. Clean
sectioning is what lets the page engine produce sensible page breaks, a usable
running header, and correct page numbers. Give it structure and it looks designed;
give it a wall of text and it looks like a dump.
