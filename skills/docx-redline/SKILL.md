---
name: Word redlining (tracked changes)
description: Amend an existing Word workspace document with native tracked changes the user accepts or rejects — clause revisions, term changes, corrections, deletions, insertions. Use whenever the user asks to change, amend, fix, redline, or mark up a document that already exists in the workspace (requires the edit_document tool). Do NOT use to produce a new file (use DOCX documents) or for read-only checking (use Document review & proofing).
default: true
compatibility: both-profiles
license: proprietary
---

# Word redlining (tracked changes)

`edit_document` applies your edits to a workspace DOCX as **native Word tracked
changes** (real `w:ins`/`w:del` revisions, author "Assistant"). A new document
version is created and the user reviews each change in the document viewer,
accepting or rejecting it — exactly as with a human reviewer's markup. Nothing
is final until the user accepts it; never describe a proposed change as made.

## Workflow

1. `list_workspace_documents` → confirm which document (id) the user means.
2. Get the document's **exact current text** for every passage you will touch —
   from the retrieved passages or `read_document`. Never quote from memory.
3. Make **one** `edit_document` call carrying the whole related batch of edits.
4. Report what was proposed (and anything that failed) — see *Verify* below.

## Matching rules (correctness-critical)

- `find` must match the document **character for character**: case, spacing,
  punctuation, curly vs straight quotes (’ vs '), en vs em dash. The single most
  common failure is retyping text instead of copying it verbatim.
- A match must lie **within one paragraph**. You cannot find or replace text
  across a paragraph break — split such an edit into per-paragraph edits.
- **The first match in the document wins.** If the text occurs more than once,
  disambiguate with `context_before` / `context_after` (the text immediately
  adjacent, in the same paragraph).
- To change **every occurrence** of a term, submit one edit per occurrence, each
  with its own distinguishing context. Identical duplicate edits all land on the
  same first match and are rejected as overlapping.
- Two edits must not overlap within a paragraph.

## Edit shapes

- **Replace**: `find` + `replace`.
- **Delete**: `find` + empty `replace`. Deleting all the text of a paragraph
  leaves the empty paragraph mark behind — tell the user if that matters.
- **Insert**: empty `find` + `context_before`; the new text lands immediately
  after the anchor, inheriting its formatting. Insertions are **inline only**:
  you cannot create a new paragraph, heading, list item, or table row this way.
  For a whole new clause or section, say so and offer the drafted text for the
  user to place (or a regenerated document) instead of faking it inline.
- Replacement text takes the formatting of the first character it replaces.

## Redline discipline (what makes a professional redline)

- **Minimal diffs.** Change only the words that change. Never delete and retype
  a whole clause to alter three words — the reviewer must see exactly what
  moved, and every extra touched word is something they must re-read.
- **One logical change per edit item**; a batch is a set of related changes.
- **Defined terms travel together.** Renaming or redefining a term is a batch
  covering every occurrence — a half-renamed term is worse than none.
- Keep substantive changes and cosmetic tidy-ups in separate batches (or flag
  them), so the user can accept one and reject the other.

## Verify and report

The tool result lists the applied change ids and a per-edit error for anything
that did not land. Report both truthfully:

- `find text not located in context` → your text differs from the document.
  Re-read the passage (quotes, dashes, spacing are the usual culprits) and
  resubmit the corrected edit — do not silently drop it.
- Partial success is normal: say which changes were proposed and which failed.
- The changes are **proposals** pending the user's accept/reject — phrase your
  summary accordingly ("I've proposed 4 tracked changes…").

Worked examples of all of the above: `references/patterns.md`.

## Why this matters

The ICP for this platform reviews other people's paper for a living. A redline
is trusted when it is surgical, attributable, and reviewable change by change;
one sloppy whole-clause rewrite (or one claimed-but-failed edit) and the user
stops trusting every mark in the document. Precision here is the product.
