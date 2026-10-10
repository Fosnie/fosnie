# Reviewer schema cardinality

Use `semantic_catalog_judge_v3.py` for new catalogue review allocations. V37 and
V38 remain frozen historical protocols; do not edit or rerun their consumed cases.
Version 3 reuses the exact same rubric, evidence catalogue, outcome rules and
strict local assessment validator as version 2.

V38 recorded eight contract refusals caused by more than four evidence IDs in a
rating. Its local validator imposed a four-ID limit while its native schema and
system instruction did not state that limit. Those records remain unknown. Do not
trim their anchors, change the validator limit or rescore their old outcomes to
make the comparison appear complete. The record has no independent quality truth.

Version 3 sets `maxItems=4` on `evidence_ids`, and sets both `minItems` and
`maxItems` on `ratings` to the actual number of criteria. The system instruction
also requires each criterion exactly once and at most four distinct evidence IDs.
The local validator still enforces identity, uniqueness, complete coverage,
rationale, valid source anchors and nonempty evidence for a non-unknown rating.
Native JSON shape compliance is insufficient to establish semantic correctness.

These array bounds are supported in the official
[OpenAI structured-output schema](https://developers.openai.com/api/docs/guides/structured-outputs?api-mode=responses)
and [Gemini GenerateContent schema](https://ai.google.dev/gemini-api/docs/generate-content/structured-output).
They are checked against a separate, versioned source fingerprint and native body
hash before any reviewer call. Do not substitute a version-2 proposal for version 3.

The V39 verification uses one other public IFEval writing request with three
available original answers and one unavailable answer. Three available answers
yield six reviewer attempts; the unavailable executor response remains unknown
and causes no reviewer dispatch. No V37/V38 reviewer case is repeated and no
executor answer is regenerated. This is a bounded wire-contract check on an
exposed public example, not a fresh semantic-quality comparison or independent
family coverage. Production routing, ranking thresholds and P12 admission do not
change.
