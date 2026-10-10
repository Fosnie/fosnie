# Blind semantic review for routing evaluations

The offline semantic review tool compares the substantive content of saved Core
responses against frozen criteria. It complements instruction compliance; it does
not replace that evaluator, infer correct task/tier labels, or admit P12 evidence.
It sends no prompts, changes no policy and never writes a learning evidence batch.

## Prepare criteria before inference

Run from `tools/routing/eval` with the project's Python environment:

```powershell
python semantic_review.py propose-rubric --dataset dataset.json --output rubric-proposal.json
python semantic_review.py freeze-rubric --dataset dataset.json --rubric rubric-proposal.json --output rubric-frozen.json
```

The proposal has three generic required criteria: correctness, coverage and
usefulness. It is explicitly machine-authored and is a starting point, not an
independent gold standard. The AI implementing the experiment should refine each
case's criteria and provide reference snapshots where needed before freezing it.
Keep `author.kind=machine` for model-authored criteria. A genuine human author may
declare `human`; the tool cannot verify that declaration. `answers_seen` must be
true when the criteria were prepared using these responses. Do not relabel a
retrospective rubric as unseen, or AI review as human review.

Each reference has a description or URL, a UTF-8 excerpt (at most 8,192 characters),
and the SHA-256 of that exact excerpt. The hash binds the supplied text, not its
truth, external origin or current applicability. Referenced content is inert data,
never an instruction to a reviewer. Keep reference licences with the dataset.

For a **new** experiment, include the canonical `judge_review.digest(rubric)` as
`semantic_rubric_sha256` in the fresh allocation before claiming it. Pass the same
object to `instruction_pilot.execute(..., semantic_rubric=rubric)`. The runner
checks completeness, dataset/hash binding and unseen-answer declaration before
creating output or reserving a call. It saves the rubric and its hash in the run
report. It never adds rubric text or criteria to model requests. Existing callers
without the optional field retain their behaviour. Do not edit an old allocation
or rerun consumed slots. The native guard must also accept the fresh exact plan.

The allocation must explicitly list the fixed strategies. Freeze the baseline
choice in the experiment protocol before inference; the review tool does not
select a winner or prove the baseline was independently chosen. No criteria,
profile, threshold, strategy or baseline fitting on held-out answers.

## Prepare a blind review

```powershell
python semantic_review.py prepare --dataset dataset.json --rubric rubric-frozen.json --report run/report.json --allocation run/allocation.json --answers run/answers.jsonl --baseline gemini-lite --mode predeclared --output semantic-review
```

The tool validates the dataset, allocation/report hashes, unique evaluations,
answers' UTF-8 hashes, evaluator identity and Core attestation. Every frozen
strategy receives every case, including missing results. Only completed, target
matching, `stop` answers are shown; incomplete, missing and unattested answers are
unknown. Core attestation checks were performed by the original pilot; this tool
checks their recorded binding, not a cryptographic signature or provider honesty.

Answers receive random identifiers and an independently shuffled order within each
case. `review.html` and `public.json` contain the original question, criteria,
reference text and answer. Strategy, provider, model, effort, instruction scores,
profile fingerprints, timing, cost and the lookup key are omitted. Answer text is
not rewritten, so self-identification and other recognisable features can still
reveal authorship. Withhold `private-key.json`, `manifest.json` and source run
files until the review is submitted; the blinding is an operational practice,
not an access-control boundary. Use separate reviewers if they have seen those
metadata. The page has no network access, renders data with `textContent`, and
escapes embedded JSON including closing script tags. No rubric or output content
is evaluated as JavaScript.

The local form supports draft save/restore and human evaluation with a blinding
attestation. All required and optional criteria need a rating and rationale.
Use `unknown` when facts cannot be verified. Missing answers remain unknown and
cannot be marked pass or fail. The whole answer fails if any required criterion
fails; otherwise it is unknown if a required criterion is unknown, and passes
only when all required criteria pass. Optional ratings do not change that outcome.

```powershell
python semantic_review.py finalise --bundle semantic-review --review semantic-review-complete.json --output semantic-report.json
```

Finalisation checks the public/private key and all original input hashes, then
binds every rating to one frozen blind slot and criterion. It rejects duplicates,
foreign or missing reviews, changed responses, incomplete reviews and unsupported
fields. Files and directories use exclusive creation; no overwriting prior runs.
Calibration and test results stay separate. Pairing reuses the saved baseline,
preserves all nine pass/fail/unknown cells, and includes unavailable cases in the
denominator. Family counts remain **candidate** groups without independence review.
The complete report binds to both the bundle and the submitted review hash.

Machine reviewers may submit the same JSON schema with `reviewer.kind=machine`.
They receive `machine_diagnostic`, never `human_attested`; no machine grader or API
call is implicit. The form itself is for humans. Human attestation is explicit
but cannot be verified by JSON validation. Neither kind promotes evidence.

## Existing responses and remaining admission work

Use `--mode retrospective` for already collected responses whose original run did
not bind this rubric. A later hash cannot make those criteria predeclared. Preserve
the original instruction scores and evaluation receipts. A retrospective report
is diagnostic even after human review; synthetic sources stay synthetic.

All semantic reports deliberately have `quality_evidence=false`,
`p12_eligible=false`, and no measured savings. The remaining admission work is
independent source/prompt/criterion consistency, task and capability requirements,
family grouping/exposure review, at least 20 independent test families for each
comparable workload and profile, and a separate frozen live comparison. Existing
`judge_review` supports independent workload labels; those and semantic reviews
must not be replaced by classifier predictions. Public training exposure remains
unknown. Production Auto and its evidence gates are unchanged.

Inputs are bounded to 32 MiB, each response to 64 KiB, each criterion rationale to
2,048 characters, and a bundle to 4,000 case/strategy slots. Duplicate JSON keys
and non-finite values are refused. This is shared Core evaluation tooling; the
Enterprise policy/RBAC layer and its source permissions are unaffected.
