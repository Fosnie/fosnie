---
name: Document review & proofing
description: Systematic mechanical review of a document — defined terms, cross-references, numbering, internal consistency (names, dates, amounts), leftover placeholders and drafting notes. Use when the user asks to review, check, proof, QA, or sanity-check a contract or formal document, and as a self-check before delivering one you drafted. Produces a findings table with exact quoted text; fixes can then be proposed as tracked changes (Word redlining skill). Do NOT use for substantive legal risk analysis — this is the mechanical layer beneath judgment.
default: true
compatibility: both-profiles
license: proprietary
---

# Document review & proofing

The mechanical pass professionals run (or buy tools to run) before any document
leaves the building. Every finding must quote the document **verbatim** — a
finding the user cannot locate, or that misquotes the text, is worse than no
finding. Read the whole document before reporting; most of these checks are
cross-document by nature.

## The checks

**Defined terms**
- Capitalised term used but never defined.
- Term defined but never used.
- Inconsistent form or capitalisation ("the Agreement" vs "this agreement" vs
  "the Contract" for the same thing).
- Term used before its definition in a document whose convention defines at
  first use.

**References & numbering**
- Cross-reference to a clause/schedule/annex that does not exist.
- Cross-reference that exists but does not say what the citing text implies.
- Numbering gaps, duplicates, or level jumps (3.1 → 3.3; 2 → 2.1.1).
- A schedule referenced in the body but missing, or attached but never
  referenced.

**Internal consistency**
- Party names: exact legal name at first mention, consistent short name after;
  no third variant creeping in.
- Dates: internally coherent (signature vs commencement vs term end; a notice
  period that cannot fit the term).
- Amounts: numbers vs words agree ("thirty (40) days"); currency and VAT
  treatment consistent; totals that should reconcile do.
- Duplicated or contradictory obligations (two clauses governing the same thing
  differently).

**Leftovers**
- Bracketed placeholders (`[date]`, `[●]`, `[insert…]`) in a document meant to
  be final.
- Drafting notes, comments to self, "TBC"/"TODO", highlighted alternatives left
  unresolved.
- Orphan fragments from earlier edits (half-deleted sentences, doubled words,
  double spaces).

## Findings format

Report as a table, ordered by severity then document order:

| # | Severity | Location | Finding | Exact text | Suggested fix |

- **Location**: clause/schedule number if the document has them, else the
  nearest heading.
- **Exact text**: verbatim quote (this is what makes the finding actionable —
  and what a tracked-change fix will need as its `find`).
- Severities: **Blocker** (document cannot be signed/filed as is — missing
  schedule, unresolved placeholder, contradiction), **Error** (objectively
  wrong — broken cross-ref, numbers-vs-words mismatch), **Warning** (likely
  wrong — inconsistent term, suspicious date), **Style** (register, archaisms,
  formatting of the text itself).

Close with a one-line verdict (e.g. "2 blockers, 3 errors — not ready for
signature") and offer to apply the fixes as tracked changes.

## Limits — say what you cannot see

You review the extracted text: fonts, colours, layout, headers/footers, and
whether a change is tracked or accepted are not visible to you. If tracked
changes exist, you see the document as if all were accepted. Say so rather than
guessing; never report on formatting you cannot observe.

## Why this matters

This is the checklist Litera Check and Definely sell as standalone products,
because a dangling cross-reference in a signed contract is a real dispute
waiting to happen. Run mechanically and honestly — verbatim quotes, no
invented findings, limits declared — it is the fastest trust the assistant can
earn with a professional reviewer.
