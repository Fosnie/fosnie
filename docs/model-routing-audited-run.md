# Audited cohort runs through Core

`tools/routing/eval/audited_run.py` runs a versioned, deterministically checked
public subset through `/api/routing/evaluate`. It supports fixed Manual baselines
and saved Auto policies with Rules, Classifier or Hybrid. It does not create
providers, edit settings, import evidence or change a policy.

Prepare the cohort with [reference audit](model-routing-reference-audit.md).
The parent, derived package, verifier hash, answers, prompts, lineage and licence
are reconstructed before planning or execution. A changed dataset or reference
requires a new plan. Original reports are never re-scored.

## Configuration

Create a JSON file with these exact top-level fields:

| Field | Value |
| --- | --- |
| `schema_version` | `1` |
| `dataset_sha256` | Canonical hash from the derived manifest |
| `base_url` | Core HTTPS origin or loopback HTTP origin, without a path/query/credentials |
| `auth_env` | Name of an environment variable containing the **Core** bearer token |
| `local_only` | Explicit boolean; applies to both judge and executor |
| `case_ids` | Unique ordered IDs from this dataset, fixed before generation |
| `profiles` | Saved executor and judge snapshots, each with its native fingerprint |
| `policies` | Complete saved `/api/routing/policies` envelopes |
| `configurations` | Fixed Manual/Auto selections described below |
| `limits` | Explicit finite ceilings described below |

No provider API key belongs in this configuration. Core resolves its existing
providers. Tokens are read only when preflight/run is explicitly selected and
are not written to output files. HTTP redirects and environment proxies are
disabled; HTTPS uses normal certificate verification.

Each profile entry is `{"snapshot": SAVED_PROFILE, "profile_sha256": SHA256}`.
Use the **inner** `profile` from `/api/routing/catalog`, not its catalogue wrapper.
Pin current provider/profile revisions, model, capabilities, preset and all other
saved profile fields. For the native fingerprint use the existing Rust utility:

```powershell
backend/target/debug/examples/routing_evidence.exe SAVED_PROFILE.json
```

Do not substitute the JSON file hash for this fingerprint. It is the same
fingerprint used by P12; pinning a fingerprint is not evidence of quality.
Local-only plans require trusted local profiles, including the judge.

Each configuration requires `id`, `mode`, `policy_id`, `provider_ids`, `efforts`
and `objective`, with optional `temperature` (finite0–2; omitted keeps0). IDs are distinct short names. `objective` is `economy`, `balanced`
or `quality`. Manual needs one executor ID and one effort (`null` preserves the
provider default), using the caller's saved effective default policy. Auto names
one active saved policy; `provider_ids` and `efforts` are explicit acceptance
sets, not a per-request override of its rules or preset. Rules/Classifier/Hybrid
come from that saved policy. The judge profile is pinned independently.

All Auto executors possible under the visible catalogue and saved policy must
be in the acceptance set. The check conservatively includes default/rule targets
and all enabled Auto profiles, intersected with saved allowed-provider constraints.
It does not attempt to prove arbitrary conditional rule pools unreachable.
Narrow the saved policy or explicitly include/pin the additional executors.
Core retains its own authorisation, locality and current revision checks.

`limits` has exactly `max_requests`, `max_model_calls`, `total_budget_microusd`,
`request_budget_microusd`, `max_duration_ms`, `run_duration_ms` and
`max_output_tokens`. Use positive integers, not booleans. Core requests support
50–30000ms and at most2048 output tokens. A run supports at most1000 requests,
2000 model calls and one hour. Select a deadline allowing at least one complete
request plus transport overhead.

The entire ordered case×configuration plan must fit all ceilings. Manual reserves
one model call per evaluation; every Auto mode conservatively reserves two,
including the possible judge, even when Rules does not actually invoke one.
The full request cost cap is reserved, including failures. Reserve is an upper
bound, not an invoice or actual spend. This script cannot authorise or reset a
provider/account allocation; supply only a separately authorised remaining budget.

## Commands

```powershell
# Offline: validates source/configuration and writes a new immutable plan.
python tools/routing/eval/audited_run.py plan --cohort DERIVED --parent PUBLIC_V1 --config CONFIG.json --output PLAN.json

# Three read-only Core calls, no inference. Needs the Core token environment.
python tools/routing/eval/audited_run.py preflight --cohort DERIVED --parent PUBLIC_V1 --config CONFIG.json --plan PLAN.json --output NEW_PREFLIGHT_DIRECTORY

# Explicit inference. The prepared plan must exactly match the current inputs.
python tools/routing/eval/audited_run.py run --cohort DERIVED --parent PUBLIC_V1 --config CONFIG.json --plan PLAN.json --output NEW_RUN_DIRECTORY --execute
```

Existing outputs and paths within source cohorts are refused. Execution cannot
resume an old directory or select a successful retry. An interrupted reservation
remains consumed. Do not work around an exhausted allocation with a new directory.

Before each evaluation, preflight checks effective `providers.manage`, the
effective default for Manual, complete policy snapshots and enabled catalogue
profile/revision snapshots. Drift or revocation stops further dispatch. A
reservation containing request/evaluation hashes and ceilings is appended and
fsynced **before** POST. The run deadline stops before another request starts.
There are no retries, helpers, sources, tools, streamed answers or escalation.
Core's text evaluation contract remains separate from normal routed chat.

Core success now explicitly returns `contract_version: completed_text_v1`,
`finish_reason: stop` and the actual saved `profile_sha256`, after its existing
completed text/accounting and current-profile checks. These are additive fields;
clients using the earlier response remain compatible. The runner checks them,
the evaluation ID, saved policy revision, actual target/model/revisions/effort,
one primary stage and shared judge/primary accounting. An older server or an
unattested/mismatched success becomes unknown and stops the batch. A normal Core
failure or transport error is unknown, retains reserve and never retries.

## Results

Outputs are `allocation.json`, durable `reservations.jsonl`, atomic
`report.partial.json` and final `report.json`. They retain hashes, safe target
metadata, timings, strict verdicts and accounting. They retain no raw prompts,
answers, provider errors, headers, API keys or reasoning text. Strict grading
compares the whole answer after trimming outside whitespace; it extracts or
repairs nothing. Missing/failed variants are unknown and cannot pass a family.
All variants must pass for a family to pass. Rules and judge modes are grouped
separately by configuration, with calibration/test splits intact.

Manual runs additionally emit `manual-observations.json` and
`manual-comparison.json` compatible with the existing offline cohort comparator.
Its comparison covers the entire frozen dataset; cases omitted by a limited run
remain unknown. The run report explicitly groups only the selected cases.
Auto results remain policy observations, not fabricated fixed-model baselines.

This cohort inherits previous exposure, has limited task coverage and is not
fresh held-out data. `quality_evidence`, promotion and policy changes remain false.
Wilson intervals describe observations; they do not prove routing quality,
calibrated judge confidence, P12 admission or financial savings. Enterprise uses
the same endpoint and continues enforcing its own RBAC, licence and restrictions.

Run the affected offline contracts with:

```powershell
python -m unittest discover -s tools/routing/eval -p test_audited_run.py
```

For guarded native OpenAI/Gemini comparisons and zero-egress simulations, see
[bounded cloud allocation](model-routing-cloud-allocation.md).
