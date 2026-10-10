# A separate instruction following quality lane

`tools/routing/eval/instruction_following.py` adds bounded scoring for 20
deterministic instruction types from the official Google Research IFEval code.
The immutable upstream revision is e6890f85757dd84e27ca6df2dd30651dafad28e0.
The Apache-2.0 notice and licence are retained in the vendored checker subset.
Optional language detection and NLP tokenisation models are excluded. The word
counter uses the same Unicode word regex as upstream's RegexpTokenizer.

All instruction arguments must be explicit and bounded. Unsupported instructions,
random/default arguments and unsafe keyword regex patterns are refused. The
assessor checks the original response against every supported instruction, without
inventing a golden answer or rewriting the prompt. Failed, truncated, oversized or
unattested responses remain unknown. A supplied pass label cannot replace scoring.
The family comparison keeps missing observations in the denominator and rejects
duplicate/retried observations and mixed profile/effort snapshots.

The original source bytes, exact published prompt and kwargs, evaluator bytes and
frozen corpus hashes are checked before scoring. Rehashing an edited prompt/kwargs
does not bypass the upstream binding. Model content is omitted from comparison
reports; the caller must preserve the original private receipt separately.

This is a strict supported subset of IFEval's instruction checks, not the official
full leaderboard score. Some upstream rules permit fences or match a substring;
their behaviour is retained rather than silently tightened. Instruction following
does not establish semantic correctness, useful answers, accurate web facts or
safe tool execution. Public training exposure and prompt/kwargs consistency must
still be reviewed. Source instruction taxonomy is only a candidate family grouping.

The V32 workspace cohort freezes40 unmodified public prompts, two per type, with
no overlap against the checked earlier BBH/V15/V17 prompt manifests. No calibration
generation or answer selected these samples. The model baseline is predeclared
from earlier calibration; it is not selected on new responses. All source/evaluator
references and blind review artefacts are preserved locally.

Validate that frozen cohort without inference:

```powershell
python tools/routing/eval/instruction_following.py --cohort C:/path/to/cohort --output C:/path/to/new-readiness.json
```

Compare observations with schema_version1, dataset_sha256, evaluator_version,
evaluator_sha256, fixed configurations (id/model/effort/profile_sha256) and records
(configuration_id/case_id/profile_sha256/effort/response/contract_status/finish_reason/
target_matches). The stored response is rescored; there is no trust in verdict:

```powershell
python tools/routing/eval/instruction_following.py --cohort C:/path/to/cohort --observations C:/path/to/observations.json --output C:/path/to/new-comparison.json
```

The normal `eval-test` command includes offline regressions for these contracts.
V32 passed98 existing+new offline tests, then two additional source binding tests
(100 unique tests).640 differential comparisons on the40 frozen prompts matched
the original upstream checker methods. No ordinary test command downloads data
or calls a provider; V32 used zero model/cloud calls and started no services.

The native pilot has not run. The fixed V15 exact-answer runner is incompatible
with this cohort and must receive a separate constraint-scoring adapter before
dispatch. Read workspace instruction-following-proposal.v32.md/JSON for the frozen
whole experiment:200primary+80judge cap, separate exclusive native accounting and
no retry/promotion. Runtime must freeze current profiles, policies, classifier
contract, effort restrictions and limits before observing any answer.

P12 remains unadmitted: reviewed family independence, workload labels, source
consistency and comparable live results are absent. Do not mark a machine review
as human-attested, equate20 instruction types with20 independent families in every
workload, or weaken Core's evidence admission gates.

Primary sources: [IFEval source](https://github.com/google-research/google-research/tree/e6890f85757dd84e27ca6df2dd30651dafad28e0/instruction_following_eval),
[paper](https://arxiv.org/abs/2311.07911).

V33 adds the [Core/native adapter and recorded live limitations](model-routing-instruction-pilot.md). Its manual phases completed; the Auto comparison was interrupted and is not admitted.
