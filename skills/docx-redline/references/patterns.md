# Redline patterns — worked examples

Concrete `edit_document` payload shapes for the common cases. Document text in
the examples is what `read_document` returned (the accepted view).

## 1. Targeted replace (one occurrence)

Document: `The Supplier shall invoice monthly at a rate of £950 per day.`

```json
{ "edits": [ { "find": "£950", "replace": "£1,050" } ] }
```

Minimal diff: only the money changes. Do NOT resubmit the whole sentence.

## 2. Disambiguating a repeated phrase

`£950` appears in clause 4.1 (fees) and clause 9.2 (liability cap); only the
fee changes:

```json
{ "edits": [ {
  "find": "£950",
  "replace": "£1,050",
  "context_before": "at a rate of ",
  "context_after": " per day"
} ] }
```

`context_before`/`context_after` must be the text **immediately** adjacent to
the match, in the same paragraph.

## 3. Renaming a term everywhere (one edit per occurrence)

"the Consultant" → "the Supplier", three occurrences. Identical edits collapse
onto the same first match — give each its own anchor:

```json
{ "edits": [
  { "find": "the Consultant", "replace": "the Supplier",
    "context_before": "agreement between the Client and " },
  { "find": "the Consultant", "replace": "the Supplier",
    "context_after": " shall provide the Services" },
  { "find": "the Consultant", "replace": "the Supplier",
    "context_before": "All intellectual property created by " }
] }
```

Check the result: three changes proposed = the rename is complete; fewer = say
which occurrence still needs doing.

## 4. Deleting a sentence

```json
{ "edits": [ {
  "find": " The Supplier may subcontract any of its obligations without consent.",
  "replace": ""
} ] }
```

Include the leading space so the remaining text does not end up with a double
space. Deleting a whole paragraph's text leaves an empty paragraph behind.

## 5. Inserting a sentence (inline)

After "…terminate on 30 days' written notice." add a carve-out:

```json
{ "edits": [ {
  "find": "",
  "replace": " Termination under this clause does not affect accrued rights.",
  "context_before": "terminate on 30 days' written notice."
} ] }
```

Leading space, because the insertion lands immediately after the anchor. This
cannot create a new paragraph — a whole new clause is not an insert (offer the
drafted clause to the user instead).

## 6. Recovering from `find text not located in context`

You sent `"find": "the Supplier's staff"` but the document has a curly
apostrophe: `the Supplier’s staff`. Re-read the exact passage, copy it
verbatim, resubmit just the failed edit. Usual culprits: ’ vs ', – vs —,
double spaces, "£ 950" vs "£950", and text that spans a paragraph break.

## 7. What not to do

- ✗ Replacing an entire clause to change three words (destroys reviewability).
- ✗ Re-sending an edit that succeeded (it will double-apply as a new revision).
- ✗ Reporting "I have updated the document" — you have **proposed** changes.
- ✗ Mixing a substantive liability change and comma fixes in one batch silently.
