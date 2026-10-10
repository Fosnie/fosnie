# Versioned reference audit

`tools/routing/eval/reference_audit.py` audits a frozen public cohort offline and
creates a separate, checked subset with explicit answer formats. It makes zero
network/model calls and never changes policies or profile evidence.

```powershell
python tools/routing/eval/reference_audit.py create --parent PUBLIC_V1_DIRECTORY --output NEW_DERIVED_DIRECTORY --version public-bbh-audited-v2
python tools/routing/eval/reference_audit.py check --parent PUBLIC_V1_DIRECTORY --derived NEW_DERIVED_DIRECTORY
```

The parent must pass the original BBH source/hash reconstruction. Its inputs,
published targets, raw sources and licence are retained unchanged. Supported
deterministic oracles cover bounded Boolean/integer syntax trees, ASCII word
sorting, bracket stacks, restricted calendar grammars and restricted navigation.
There is no eval/exec, arbitrary code, implicit date inference, model grading or
claim that unsupported tasks are correct. Unsupported/ambiguous syntax stays
unverified. Leap-day birthday conventions, duplicate options, invalid prefixes
and oversized expressions are refused. These are deliberately limited oracles.

Each source case receives verified, correction_proposed or unverified, with its
source target/input binding, computed target/value and proof. The verifier source
hash and audit version bind the result. A correction is programmatic, not a
human review attestation. Before derivation/check the audit is recomputed; stale,
foreign or edited results are refused.

The derived directory contains prompts.json, answer-bank.json, manifest.json,
reference-audit.json, source-lineage.json and the original LICENSE-BBH.txt.
Only deterministically checked cases enter the subset. Selection uses verifier
coverage, not model outputs. Every excluded case is listed in the manifest.
Original IDs/families/splits/source URLs/file hashes/row indices remain. Corrected
targets and explicit-format prompts receive new hashes/version. No answers or
case-specific hints are inserted into the generic format directives.

Format instructions distinguish exact True/False, integer, option label, sorted
space-separated words, missing space-separated brackets and Yes/No. This resolves
the earlier ambiguity between a multiple-choice reference and a question asking
for a literal date. Scoring remains whole-answer equality after trimming outside
whitespace. It does not extract answers, strip Markdown or accept comma lists.

Output cannot be the parent or inside it, and existing output directories/files
are never overwritten. Check reconstructs the exact derived package, lineage,
proof and licence from the parent. Models/API keys/responses are not inputs.
Old reports are never re-scored against the derived answers. Observation hashes
must match the new dataset; old-dataset observations are rejected.

Compare future matching observations with the existing offline CLI:

```powershell
python tools/routing/eval/cohort_report.py --cohort NEW_DERIVED_DIRECTORY --parent PUBLIC_V1_DIRECTORY --observations NEW_OBSERVATIONS.json --output NEW_COMPARISON.json
```

All exposure is inherited. Previously observed/tuned families do not become fresh
hold-out because the reference/prompt version changes. The new verified subset
is smaller and is not directly comparable to the original full cohort. Human
requirement labels, semantic review, real completed model measurements and
deployment-specific P12 criteria remain separate. quality_evidence and
human_reviewed stay false. No automatic evidence import/promotion is implemented.

For reproducible future Manual/Rules/Classifier/Hybrid requests, use the
[audited Core run workflow](model-routing-audited-run.md). Offline planning
makes no network calls; execution requires a matching finite plan.
