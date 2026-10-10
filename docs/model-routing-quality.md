# Reviewed ranking and bounded quality escalation (P12)

Both features are open source Core policy options, available with the Enterprise
overlay. Existing profiles and policies read with empty evidence and both options
disabled. No migration, live evaluation, production activation or paid call is
performed by importing evidence or opening settings.

## Auto, explicit rules and measured ranking

`learned_ranking` applies after the existing visibility, current revision,
capability, tier, locality, rule pool and budget filters. Explicit selection rules,
Manual, checked Default and uncertain classification keep their existing semantics.
Rules-only policies without a terminal selection still use their checked default.
Learned ranking therefore normally accompanies classified Auto requirements.

Records describe a task **and call purpose**, minimum tier, required capabilities, language,
effort and token sizes. Retrieval requires the same categorical features and input
and output sizes within a factor of two. It never reads previous user prompts or
embeddings. The nearest record is used; equally near records prefer the lower
score. Matching a similar workload is a bounded heuristic, not semantic expertise.

Every eligible candidate must have a live, reviewed, current record with at least
20 independent held-out dialogue families. The selected records must have the
same dataset hash, family manifest hash and count, workload and evaluator version.
Incomplete or incomparable coverage retains ordinary routing and reports
`learned_insufficient_evidence`. Synthetic, unreviewed and stale results cannot
affect it. Repeated turns from one family must not be counted as independent cases.

For comparable records, a Wilson 95% lower bound below 0.80 excludes the candidate.
If all candidates fail this floor, Auto refuses instead of disregarding the
measurements. Economy/Balanced retain their existing cost/tier preferences among
survivors; Quality prefers the measured lower bound, then existing tie-breakers.
The chosen evidence UUID appears in decision reasons. This is a summary of the
recorded observations, **not a calibrated probability of the next answer**, a
quality guarantee or evidence of savings. Minimum samples are admission criteria,
not a claim of statistical equivalence to a stronger model.

Purpose-aware ranking applies where a new Auto execution plan is made. Explicit
helper assignments and already pinned stages do not change mid-run. There is no
production exploration, automatic promotion of observations, embedding similarity
store or self-reported model score.
With learned ranking enabled, the browser's single-candidate shortcut is disabled:
classification is still needed to check the relevant measured quality floor. A
sole candidate with comparable failing evidence must not bypass that floor.
The settings dry preview does not run a classifier and shows its checked default
and `classifier_required`; it cannot certify a learned choice for an unclassified request.

## Prepare and import evidence offline

1. Save the model profile first. In Providers → Routing → profile → Held-out model
   evaluations, download the **saved** profile. Model/provider configuration,
   capabilities, effort support and task tiers must describe the evaluated setup.
2. Build the offline utility from `backend`:
   `cargo build --offline --example routing_evidence`.
   Run `target/debug/examples/routing_evidence PROFILE.json` (append `.exe` on
   Windows) to get its configuration fingerprint. Provider revisions invalidate
   evidence when settings change. Profile CAS revision alone does not.
3. Supply a frozen evaluation batch with the shape below. The manifest must contain
   independent **test** families unused in training/calibration/tuning. Produce one
   aggregate outcome per family for the same provider configuration. Include every
   manifest family; omitted observations and `unknown` count as failures. Do not
   select only successful cases. This utility accepts no prompts or answers.

```json
{
  "schema_version": 1,
  "source": "synthetic",
  "dataset_sha256": "<64 hex characters identifying the frozen dataset>",
  "profile_sha256": "<fingerprint from the saved profile>",
  "evaluator_version": "rubric-v1",
  "purpose": "primary",
  "workload": {
    "task": "general", "minimum_tier": "economy",
    "fresh_information": false, "tools": false, "vision": false,
    "structured_output": false, "input_tokens": 512, "output_tokens": 256,
    "language": "en", "effort": null, "uncertain": false
  },
  "families": [{"id": "family-001", "split": "test"}],
  "observations": [{"family_id": "family-001", "outcome": "pass"}]
}
```

4. Run `target/debug/examples/routing_evidence PROFILE.json BATCH.json NEW_OUTPUT.json`.
   It refuses overwritten outputs, oversized inputs, unknown fields, train or
   calibration splits, duplicate/foreign family IDs and stale fingerprints. It
   preserves `source` and outputs an **unreviewed** record array. `live` is appropriate
   only for real measured results; the utility cannot prove the operator's claim.
5. Import that array in the profile editor, inspect results and provenance, mark
   reviewed only after validating the manifest/evaluator, and Save. Importing or
   reviewing synthetic records does not make them live. Editing capabilities, tier
   or adapter clears imported records; stale server fingerprints are rejected.
   The editor limits files to 128 KiB/64 records and uses existing CAS protection.
6. Only holders of `providers.manage` may attach evidence, including BYOK evidence.
   Normal BYOK profile editing removes it. Enable `learned_ranking` only on the
   intended policy after comparable records exist. Use Shadow/preview first; the
   importer does not enable policies, gates or quality verification flags.

Review is an authorised administrator attestation. Hashes bind records to a
configuration and cohort, but do not authenticate the honesty of external
measurements. No real model evidence is bundled by this implementation.
An upstream vendor changing a model behind an unchanged API alias cannot be
detected by a configuration hash. Operators must re-evaluate that model and
remove or unreview obsolete records when the underlying implementation changes.

## Quality contracts for the stateless API

Set `quality_escalation: true` on an Active Auto policy and positive run limits:
`max_calls <= 8`, `max_output_tokens <= 8192`, `max_total_tokens <= 262144`,
`max_duration_ms <= 120000`. Allocate calls/tokens for the judge and both attempts.
Each request must explicitly opt in using `stream: false` and one contract:

```json
{
  "model_selection": {"mode": "auto", "objective": "balanced"},
  "stream": false,
  "max_tokens": 256,
  "quality_contract": {"kind": "json_object", "required_fields": ["answer"]},
  "messages": [{"role": "user", "content": "Return a JSON object with an answer field: what is 2 + 2?"}]
}
```

`{"kind":"nonempty"}` checks for non-whitespace text. `json_object` additionally
requires an exact JSON object with the listed top-level keys (maximum 32). A null
value is present; types, meaning, factual correctness, schema conformance beyond
these keys and task quality are not checked. The contract does not rewrite prompts.
The caller should request the desired format in its messages.

The server buffers text and reasoning until a fully accounted `stop` completion.
Buffer limits are 1 MiB and 4,096 events. Empty answer, invalid JSON or missing
fields are typed server failures. The rejected answer is discarded. At most one
higher task-tier compatible model may answer the original messages. The original
effort, rule pool, principal, policy revision, total budget and deadline remain.
Provider visibility and Core/Enterprise constraints are checked again.

Technical fallback and quality escalation share **one** retry allowance. Neither
path can trigger a third executor. Both attempts and any classifier are accounted
in `fosnie_routing.usage`; the normal response `usage` describes the final answer.
`quality_escalation` metadata/audit records targets and the typed failure;
`quality_rejection` describes the rejected attempt even when the later answer
succeeds. Rejected answer text and reasoning are not recorded in these events.

Truncation, filtering, malformed accounting, tool events, interrupted streams,
revoked credentials, stale policy/profile, exhausted budgets and buffer overflow
fail the request; they do not trigger quality escalation. A second invalid answer
fails without exposing either rejected answer. No available higher tier also
fails. Streaming, agent/tool loops, durable jobs, browser chat and fixed services
do not use this extension. No external action is replayed. Structural validation
is deliberately narrower than a semantic quality judge.

## Verification and rollout

From Core, `tools/routing/test.ps1 -Lane quality -WithEnterprise` (or the matching
`test.sh quality --with-enterprise`) runs Core all-targets, routing units,
synthetic technical/quality HTTP acceptance, offline utility build, frontend
units/build/browser and the actual Enterprise build/routing integration/overlay.
It requires the dedicated loopback `FOSNIE_ROUTING_TEST_DATABASE_URL` and existing
isolated Redis. Use `-Output`/`--output` for a separate P12 receipt.

Disable the two policy options to restore ordinary ranking and refuse new quality
contracts. Saved records can remain inert. Changing policy/profile revisions
invalidates in-flight escalation. Before downgrading to a server predating P12,
remove the new JSON policy fields and `quality_evidence` using a reviewed backup
procedure: older strict deserialisers cannot read unknown fields. Do not downgrade
binary-only against P12-written profiles/policies.

Live pilot budgets, claims about model quality/savings, semantic evaluators and
production activation require their own evaluation and rollout decisions.


## Preparing independent labels

The [offline judge review tools](model-routing-judge-review.md) prepare a frozen,
blinded human label workflow and a small revision-pinned public benchmark cohort.
They keep label review, classification accuracy and answer-quality measurements
separate. Their output is not a P12 evidence import and cannot enable measured
ranking. Public benchmarks may have been used to train the evaluated models.

## Explicit task exclusions

The optional model-profile `task_exclusions` set lets an operator exclude a
model configuration from particular Auto tasks. An empty or omitted set keeps
existing behaviour. The exclusion is a hard filter before tier/cost ranking:
explicit Auto rules, checked active defaults, technical fallback and quality
escalation cannot bypass it. No eligible model produces an explicit error.
Manual selection and fixed assignments retain their separate capability checks.

`unknown` excludes unclassified tasks. A classifier policy with one visible
model still classifies an unknown task when that model has task exclusions.
The classifier cannot clear these restrictions. Profile updates use existing
permissions/revision checks, and nonempty exclusions change the quality-evidence
fingerprint. Empty legacy fingerprints remain unchanged. The settings editor
clears old evaluation records when exclusions change.

These are operator restrictions, not verified model ratings or prompt matching
by benchmark ID. They apply to the model configuration, including its Auto
effort preset. Changing the effort does not automatically remove a restriction;
the operator must review suitability before enabling the task again.

## Comparable model-specific efforts

Learned ranking can compare configurations such as a local model with the
`none` preset and a cloud model with `high`. Each candidate must first have
current reviewed live evidence for its own resolved effort. Explicit caller
effort overrides the preset and cannot use observations for another effort.
`null` (provider default) and `none` (explicit disable) remain distinct.

After that individual check, cohort comparison requires the same dataset,
held-out families, evaluator, purpose, sample count and workload fields. Only
the candidate-specific effort field is excluded from cross-model equality.
The minimum reviewed sample count, quality floor, profile fingerprint and
all capability/policy filters remain unchanged. Incomparable or missing data
keeps the existing declared ranking and reports insufficient evidence.

The V17 exploratory pilot was not imported into learned ranking. Its ten
candidate groups per source remain unreviewed and do not satisfy P12 admission.
The mixed-preset contract was checked with synthetic unit fixtures and the
installed Enterprise consumer, with no additional model calls. Rebuild the
private pilot examples before a subsequent experiment after engine changes.
