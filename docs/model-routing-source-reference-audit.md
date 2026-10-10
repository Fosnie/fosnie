# Source-only reference preflight

Audit public QA questions and their published references before generating executor
answers. `source_reference_audit.py` checks source bindings and offsets, builds
bounded requests for two fixed reviewers, validates exact evidence IDs and suggested
spans, and preserves missing assessments and disagreements. No production provider,
router policy, baseline, original reference or P12 evidence is changed.

The four criteria cover support, unique interpretation, reference completeness and
span boundaries under the existing published-span exact-match metric. Every criterion
must appear once. Pass/fail requires a context evidence ID. Missing alternatives and
boundary variants must be exact case-sensitive source substrings and cannot contradict
the corresponding pass rating. Context presence is an anchor, not semantic proof.

A valid fail from either reviewer excludes a case from machine preflight. A missing
review or uncertainty leaves it unresolved. All-pass agreement is labelled
`machine_preflight_pass`; it never enables executor generation or marks human review.
Suggestions keep their machine authorship and source offsets and are not added to gold.

`source_reference_review.py` produces an offline page with source text, suggestions,
explicit reference selections and a personal human attestation. All decisions start
unset. The page has no network connections and downloads a JSON review. An AI must
never fill in a real human attestation. The importer refuses incomplete, mismatched,
unanchored or machine-authored reviews and retains original source files. It exports
a new dataset and answer bank rather than editing an existing evaluation.

```shell
python tools/routing/eval/source_reference_review.py --bundle <source-bundle> \
  --report <live-audit-report.json> --review <human-source-review.json> \
  --output <fresh-reviewed-corpus-directory>
```

This records an operator attestation, not independently verified reviewer identity.
Human reference adjudication does not certify independent families, minimum tiers,
current model capabilities or the Wilson quality threshold. Those P12 gates remain.
Public SQuAD TRAIN can have appeared in model training. Neither twenty article titles
nor agreement between two models establishes statistical or reviewer independence.

The V42 workspace harness freezes source selection, payloads, input and protocol hashes
and a fresh 40-attempt allocation before dispatch. It uses the existing public-payload
permission for OpenAI Responses and Gemini GenerateContent. Credentials exist only in
memory, reservations precede dispatch, there are no retries or resumed live allocations,
and native incomplete/failed/contract-invalid responses remain unknown. Simulation
uses only a mock transport. The source-only review makes zero executor or routing calls.

Source: [official SQuAD repository at the pinned revision](https://github.com/rajpurkar/SQuAD-explorer/tree/eee5fdbf62f8613a7812b03419e6b29617b74fd1/dataset),
[SQuAD licence and attribution](https://rajpurkar.github.io/SQuAD-explorer/).
