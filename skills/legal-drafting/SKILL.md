---
name: Contract & legal drafting
description: Drafting discipline for contracts, clauses, amendments, and formal legal documents — defined terms, clause numbering, cross-references, recitals, schedules, execution blocks, amendment formulas. Use whenever producing or revising text with legal effect - agreements, NDAs, variation wording, engagement letters, board minutes, policies. Applies on top of the DOCX/PDF document skills (structure and rendering live there). Do NOT use for general business prose or marketing copy.
default: true
compatibility: both-profiles
license: proprietary
---

# Contract & legal drafting

The document skills say how a DOCX/PDF is rendered; this skill is the drafting
discipline that makes the content stand up as a legal document. The reader is a
professional who will look for exactly these mechanics — and judge the whole
document by them.

## Structure of an agreement

In order: title; parties (full legal names, company numbers, registered
offices); background/recitals (context only — no obligations in recitals);
operative clauses (definitions first, then commercial substance, then
boilerplate: term, termination, liability, confidentiality, data protection,
assignment, notices, entire agreement, variation, governing law and
jurisdiction); schedules; execution blocks. Skeletons for common documents:
`references/skeletons.md`.

## Defined terms (the discipline that gets checked first)

- Define a term once — in the definitions clause or in brackets at first use:
  `…Private AI Ltd (the "Supplier")` — and use it **identically capitalised**
  everywhere after.
- Never use a capitalised term you have not defined; never define a term you
  never use; never use two names for one thing ("the Agreement" vs "this
  Contract").
- Definitions carry no obligations ("Services" means…, not "Services" shall…).

## Numbering and cross-references

- Number clauses **explicitly in the text**: `**3. Term**` as the clause
  heading, `3.1`, `3.2` opening each sub-clause paragraph, `(a) (b) (c)` below
  that. Do NOT use Markdown `1.` list syntax for clauses — auto-numbered lists
  renumber, and a cross-reference to "clause 8.2" must stay pointing at 8.2.
- Every cross-reference ("subject to clause 9.1", "as defined in Schedule 2")
  must point at a clause or schedule that exists and says what you claim.
- Numbering is continuous — no gaps, no duplicates. Renumber consequentials
  when inserting or deleting a clause, and carry the renumbering into every
  cross-reference.

## Register

- British English, plain modern drafting. Short sentences; one obligation per
  sentence; active voice ("the Supplier shall…", not "it is agreed that…").
- **shall** = obligation, **may** = discretion, **must** = condition/state.
  Pick the convention and hold it; never "shall" for mere futurity.
- No archaisms: hereinafter, witnesseth, aforesaid, hereto have no effect and
  date the drafter. "Including" is always "including (without limitation)" only
  if the document's convention requires it — otherwise say including.

## Amendments and variations

Amendment wording operates on the existing text with surgical formulas:

- `Clause 4.1 shall be deleted and replaced with the following: "…"`
- `In clause 9.2, "£950" shall be replaced with "£1,050".`
- `A new clause 7.4 shall be inserted as follows: "…"`

Quote the replaced/inserted text exactly and in full. (Marking amendments in an
existing workspace document is the *Word redlining* skill, not prose formulas.)

## Templates — the one placeholder exception

Delivered documents never contain placeholders. The exception is when the user
explicitly asks for a **template/precedent**: then use square-bracketed
placeholders in one consistent style — `[PARTY NAME]`, `[DATE]`, `[●]` for
values with no obvious label — and nothing else bracketed, so a fill-in pass
can find them all mechanically.

## Why this matters

Legal readers scan for these mechanics before they read for substance: a
mis-capitalised defined term or a dangling cross-reference tells them the
document was not drafted with care, and everything after that is read with
suspicion. Mechanical correctness is what lets the substance be taken
seriously — and it is exactly what this platform's users are paid to check.
