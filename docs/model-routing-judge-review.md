# Frozen judge data and public benchmark checks

The offline tools under `tools/routing/eval` prepare data and diagnose results.
They do not call models, write policies, attach P12 evidence or enable routing.
Core and Enterprise use the same files. No new runtime API is introduced.

## Public examples with published answers

`public_benchmark.py --download --output NEW_DIRECTORY` explicitly downloads
allowlisted JSON data and the MIT licence from the authors' [BIG-Bench Hard
repository](https://github.com/suzgunmirac/BIG-Bench-Hard). Default invocation
without `--download` makes zero network calls. The importer resolves main once,
then fetches all files through that immutable commit; `--revision SHA` repeats
a known revision. Downloads have size/time bounds, no redirects or model calls.
It does not execute upstream code or load upstream model-output files.

The initial cohort has 46 examples: two deterministically selected examples
from each of 23 task families. Logical-deduction and shuffled-object size variants
are grouped as one family and represented by their three-object task. Boolean
expressions, date understanding and word sorting are calibration families;
the other 20 families are test data. These choices precede model observations.
Canonical hashes choose samples; successful answers are never used to select them.
All examples remain English. Input text and published targets are preserved;
the sole prompt change asks for only the final answer without an explanation.

`prompts.json`, `answer-bank.json` and `provenance.json` bind the sample to its
revision, original row indices, source URLs and file/input hashes. Raw source
JSON retains the benchmark canary and `LICENSE-BBH.txt` retains its licence and
copyright. Keep these files together outside tracked fixture directories.
Targets are absent from the judge input and blind-review page. The answer
validator compares the whole final text after trimming outside whitespace;
there is no answer extraction, Markdown repair or partial-credit rewriting.

Published data are independent of Fosnie's AI-authored fixtures, but may already
be in a model's training data. Task names and reference answers do not label the
minimum required model tier. English static reasoning/language benchmarks do
not cover live weather, Russian, visual prompts, tools or Enterprise workflows.
A small real run on them does not establish production routing quality.

## Freeze an operator dataset

Create a local input with the following shape. IDs may use letters, digits,
underscore, hyphen, dot, colon and slash; no ID becomes a filesystem path.

```json
{
  "schema_version": 1,
  "dataset_id": "cohort-001",
  "source": {"kind": "independent", "reference": "origin or local provenance reference"},
  "cases": [{
    "id": "case-001", "family_id": "family-001", "split": "test",
    "language": "en", "conversation": "The original conversation to classify."
  }]
}
```

`source.kind` is `independent` or `synthetic`; it is an operator declaration.
Synthetic source remains synthetic after human review. Use real dataset origins;
hashes cannot verify that a reference or human attestation is truthful.
The input has no labels, model names, predictions, confidence or model responses.
Maximums are 4 MiB, 1,000 cases and 64,000 UTF-8 bytes per conversation. Families
cannot cross calibration/test splits; normalised duplicate conversations fail.

Run from Core with an existing Python runtime:

```powershell
python tools/routing/eval/judge_review.py prepare --dataset INPUT.json --output NEW_DIRECTORY
```

Known AI calibration seed families and prompts are automatically excluded from
new test data. Supply each other exposed dataset with repeated `--exposed PATH`.
An exposure registry may instead contain `schema_version: 1` and `records` of
`family_id`/`prompt_sha256` pairs. The manifest freezes hashes of supplied exposure
sources. These are exact identity/normalised-text guards, not semantic detection
of renamed or paraphrased templates. Declare all related variants in one family
and include all prior exposure sources. Public test observations become exposure
for any future tuning/hold-out cohort; preserve their family IDs.

The new directory contains frozen dataset/manifest, a blank review draft and
`review.html`. Existing output directories/files are never overwritten. Prompt
content uses inert JSON and text rendering; the page has no network or external
assets. It can be opened directly in a browser. Review all fields, give a rationale
and identify the human reviewer. The page hides predictions, targets and proposed
labels; it neither records nor infers them. No unchecked flag defaults to false.
Save a draft before closing; reloads do not silently retain unfinished work.
Drafts can be restored only into the same frozen dataset/manifest.

## Finalise and report

```powershell
python tools/routing/eval/judge_review.py finalise --bundle BUNDLE_DIRECTORY --review HUMAN_REVIEW.json --output NEW_LABELS.json
python tools/routing/eval/judge_review.py report --bundle BUNDLE_DIRECTORY --review HUMAN_REVIEW.json --observations OBSERVATIONS.json --output NEW_REPORT.json
```

Finalisation requires one complete, typed review per case, no foreign/duplicate
case, rationales, frozen hashes and a human attestation of blindness to model
observations. Unknown task requires uncertainty. The exported label format feeds
the existing `judge_diagnostics.py`; review metadata is separate from classification
targets and contains no conversations. This is attested label review, not proof
of semantic correctness or administrator-reviewed P12 live model evidence.
AI-generated labels must not claim `review_kind: human`.

Diagnostics retain case counts but also report family counts. A family matches
only if every variant is an exact accepted requirement match; field errors,
abstention or missing/failed output prevent a family match. The conservative
Wilson 95% lower bound uses families, not English/Russian variants as independent
samples. This metric does not reward appropriate abstention; inspect raw field
errors and abstention counts separately. Confidence is never treated as a
calibrated probability. Thresholds are not fitted by reporting.

All outputs retain `quality_evidence: false`. Answer reference checks and judge
labels are distinct. P12 still requires real, complete, comparable held-out
measurements for current saved model configurations and explicit administrator
review through its existing workflow. These utilities do not automatically
translate benchmark/classification scores into model-profile evidence.


## Full answer-cohort comparisons

`cohort_report.py --cohort PUBLIC_DIRECTORY --observations OBSERVATIONS.json
--output NEW_REPORT.json` is an offline report utility. It reads the frozen
public source files, reconstructs the original deterministic sample, and checks
the prompt/answer/provenance bindings before comparison. It downloads nothing
and makes no model, policy or profile call.

An observation file has schema_version1, dataset_sha256, configurations and
records. Each configuration contains id, model, effort and the saved profile's
canonical profile_sha256. Each record binds configuration_id, case_id,
profile_sha256 and effort, plus verdict (pass/fail/unknown), contract_status
(completed/failed), finish_reason and target_matches. A scored answer requires
a normal stop, matching target and complete contract. There is exactly one
record per configuration/case; retries cannot replace failures. Missing records
count as unknown. Mixing profile/effort revisions or foreign cases is refused.

Reports keep calibration and test counts separate. A family passes only when
every frozen variant passes, including variants that are missing or unknown.
The Wilson 95% lower bound uses families. The best-single descriptive baseline
uses calibration reference-pass counts only, with stable ID ties; test results
never choose it. Pairwise test counts are descriptive. A post-answer oracle
shows only a hindsight ceiling from published references and measured answers;
it cannot be deployed as a prompt router.

A complete case ledger is not a validated model ranking. The cohort mixes task
types and input sizes and does not supply reviewed minimum-tier/capability labels.
Comparisons record exact model/effort configurations; they are not automatically
comparable P12 workload records. Actual financial costs, calibrated confidence
and semantic labels are not inferred. Every report keeps quality_evidence=false
and leaves production settings/profile evidence untouched.


## Diagnose answer shape without changing the validator

`answer_diagnostics.py` provides `diagnose(content, reference, question)` for an
explicit local evaluation observer. It records only a final-answer hash, lengths
and finite flags: strict/casefold/whitespace matches, known prefixes, a single
fence, a JSON string or the official option text. Other enumerated option/Boolean
values are kept distinct from formatting hints. Thinking-marker presence is a
Boolean; reasoning bodies are not retained. Inputs are bounded text.

These flags are diagnostic only. They never rewrite a response, convert its
strict failure into pass, promote model evidence or implement a runtime repair.
A prefix or equivalent-option hint is not a reviewed semantic assessment.
Truncated or unaccounted provider text remains unsuccessful even if its observer
sees a reference fragment. A calibration-only change of token cap, effort or
model is a new request configuration; preserve the original held-out receipts
and do not retune them into success. Claiming an untouched test cohort after
such tuning requires new, family-disjoint data and exposure checks.


## Audit references before further evaluation

The [versioned reference audit](model-routing-reference-audit.md) recomputes a
supported subset from frozen source problems and derives a separate cohort with
explicit formats, source lineage and correction proofs. Published references
can be wrong; the pilot found a date whose labelled year contradicted the stated
anniversary. Unsupported references stay unverified. Old datasets/metrics and
exposure are preserved; no corrected or format-normalised result is promoted.
