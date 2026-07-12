---
name: DOCX documents
description: Produce a beautiful, downloadable Word (.docx) document — report, memo, letter, briefing note, policy, contract draft. Use whenever you are generating a DOCX artefact or the user asks for a "Word document", ".docx", or something to "download" / "send" / "print". Write clean Markdown; it is rendered with real Word styles. Do NOT use for spreadsheets (.xlsx), slide decks (.pptx), or when only an on-screen answer is wanted.
default: true
compatibility: both-profiles
license: proprietary
---

# DOCX documents

You are drafting the **content of a Word document**. What you write here is
rendered into a styled `.docx` by the platform: your Markdown headings become real
Word heading styles, your tables become real Word tables, your lists become real
Word lists. Write the **finished document and nothing else** — no preamble, no
"here is your document", no notes about formatting.

## Generate vs edit — route first

This skill produces a **new** document. If the user wants changes to a document
that already exists in the workspace, do not regenerate it — propose tracked
changes instead (see *Word redlining*). Regenerating loses the user's
formatting and the reviewable diff. For contracts, clauses, and anything with
legal effect, apply *Contract & legal drafting* on top of this skill.

## Write clean Markdown

The renderer (pandoc against a styled reference document) turns Markdown into proper
Word styling. So **use Markdown deliberately**:

- `#` for the document title (exactly one, the first line).
- `##` and `###` for section and sub-section headings.
- `-` for bullet lists, `1.` for numbered lists.
- `**bold**` for emphasis that must stand out; `*italic*` sparingly.
- Pipe tables for tabular data (see the `doc-tables` skill for the discipline).
- `>` for a pulled-out quotation or a standout note.

Do **not** paste raw HTML, and do not hand-draw boxes or rules with `===` or ASCII —
the styles handle that.

## Structure

1. **Title** — a single `#` line in Title Case naming the document, e.g.
   `# Confidentiality Memo — NDA Clauses`. No "Subject:" or "Title:" prefix.
2. **Body** — sections under `##` headings, in a logical order. Open with a short
   framing paragraph where the document type expects one (a report has a summary; a
   memo has a purpose line).
3. **Close** — only if the document type needs it (a letter has a salutation and a
   sign-off; a report usually does not).

## Rules (correctness-critical)

- **No bracketed placeholders.** Never write `[Insert Date]`, `[Client Name]`,
  `[Insert Document ID]`. Use concrete values where you have them; otherwise write
  the unknown in prose ("the date of signing") or omit the line. A delivered
  document with `[…]` in it is a defect. (Sole exception: the user explicitly
  asked for a **template** — then use one consistent placeholder style; see
  *Contract & legal drafting*.)
- **Clause numbering is literal.** In contracts and formal instruments, write
  clause numbers as text (`**3. Term**`, `3.1 …`) — never Markdown `1.` lists,
  which renumber and break cross-references.
- **Do not refer to the document or to producing it.** No "Generated Artefact", no
  "please find attached", no "the PDF below", no confirmation chatter.
- **One title only.** Additional top-level `#` headings confuse the outline; use
  `##` for everything below the title.
- **British English** throughout, in a clear, professional register.
- **Tables**: every column needs a header row; keep cell text short. See
  `references/markdown-style.md` for the gotchas (escaping pipes, alignment).
- **Self-check before delivering** a contract or long formal document: run the
  mechanical pass from *Document review & proofing* (defined terms,
  cross-references, numbering, leftovers) on your own draft.

## Why this matters

The document is the deliverable a regulated client downloads, prints, and files.
It must read as though a careful professional wrote it directly in Word — correct
structure, no machine artefacts, no placeholders, house style throughout. The
styling is automatic; your job is clean, complete, correctly-structured content.
