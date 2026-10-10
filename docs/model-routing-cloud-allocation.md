# Bounded native cloud comparison

`tools/routing/eval/cloud_allocation.py` prepares a separate finite proposal and
guards the two native provider requests used by the audited pilot. It does not
renew earlier allocations, fetch credentials or activate a policy.

The workspace `run_audited_cloud.py` provides `proposal`, `preflight`, `simulate`
and `execute`. Proposal is offline. Preflight uses read-only private Core routes
without ML/generation. Simulation uses real Core and ML with an explicit
`httpx.MockTransport`; provider responses are fixtures, labelled synthetic.
Synthetic passes verify transport/format/accounting only, never model quality.
Live execution requires an exact prepared proposal, a separate human approval
receipt, hidden in-memory provider keys and an exclusive allocation claim.
The claim cannot be resumed or reset by selecting another output directory.

The guard fixes `gpt-5.4-mini` at `https://api.openai.com/v1/responses` and
`gemini-3.1-flash-lite` at
`https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-lite:generateContent`.
There is no model substitution, tool, system-message rewrite, schema rewrite,
reasoning override, redirect, environment proxy or fallback/retry. Complete
native JSON payloads must match the frozen original question and parameters;
extra fields, destinations and duplicate slots are refused before egress.

The reviewed proposal contains12 audited cases in6 families, with6 calibration
and6 inherited test cases, and24 total Manual requests, at most12 per model.
Each request has at most512 generated tokens and30s Core duration. The whole
run has a15-minute deadline and a50000-token conservative reserve ceiling per
model. The native input body is at most16000 bytes. Gemini calls are separated
by at least10 seconds. Failed/unknown usage keeps its full reserve. Authentication
or quota failure blocks later dispatches to that provider.

The guard fsyncs each allocation/model/case/request-hash reservation before
upstream bytes. Its ledger contains bounded usage/status fields, not prompts,
answers, error bodies, reasoning or credentials. Keys are only attached to the
allowlisted outbound headers; the private Core/ML fixtures receive no real keys.
Response bodies are bounded to1MiB and are passed to the native adapters and
Core's existing terminal completion checks. Unknown spend is never refunded.

The public question+explicit format contract matches V12's local run; there is
no additional generic system message. The corrected date label and prompt
version remain new data and never re-score the original public run. Differences
in provider sampling/settings are recorded; this is a comparison of these
fixed configurations, not identical stochastic runs or a fresh test set.

## Explicit sampling

The protected text evaluation request accepts optional `temperature`, finite
from0 through2. Omission retains the earlier0 setting. The audited configuration
can pin temperature per configuration; it is bound by the plan/configuration
hash. Invalid values fail before dispatch. Ordinary routed chat is unchanged.

The [official Gemini 3 guide](https://ai.google.dev/gemini-api/docs/generate-content/gemini-3#temperature)
recommends1.0; this plan explicitly forwards1.0 to Gemini. It preserves that
model's default thinking behaviour. The
[thinking guide](https://ai.google.dev/gemini-api/docs/generate-content/thinking#token-limits-and-max_output_tokens)
describes the combined generated/thought token cap and MAX_TOKENS behaviour;
truncated answers remain unknown in the evaluation. No thinking text is retained.

The native Responses adapter currently omits temperature, so the OpenAI request
uses its provider default even though Core's sampling field defaults to0.
The guard verifies its actual JSON rather than claiming a temperature was applied.
Reasoning effort is omitted. The
[official GPT-5.4 Mini page](https://developers.openai.com/api/docs/models/gpt-5.4-mini)
lists none as its default reasoning effort. The actual target retains null for
the omitted override; the report does not fabricate a measured effective setting.

## Reproduction

```powershell
python run_audited_cloud.py proposal --output NEW_PROPOSAL.json
python run_audited_cloud.py preflight --proposal NEW_PROPOSAL.json --output NEW_PREFLIGHT_DIRECTORY
python run_audited_cloud.py simulate --proposal NEW_PROPOSAL.json --output NEW_SYNTHETIC_DIRECTORY
# Only after a new human allocation and approval receipt:
python run_audited_cloud.py execute --proposal NEW_PROPOSAL.json --approval APPROVAL.json --output NEW_LIVE_DIRECTORY
```

Run these workspace commands from the pilot directory using Core's existing ML
Python environment and freshly built private pilot/evidence utilities. Keep the
original/exhausted ledgers untouched. Never put API keys on the command line,
in a URL, JSON configuration, source, report or environment file.

Original cloud allowance27/28 and the later approved12/12 run are historical.
Neither authorises this new24-call plan. The human approval receipt is a record
of actual consent, not cryptographic authentication; the AI/operator must obtain
that consent rather than inventing it. Conservative cost/token reserves do not
verify free credits, current account limits or invoice pricing.

Successful live runs would provide descriptive Manual baselines. They would
not establish judge-tier accuracy, calibrated Auto confidence, source/tool/RU
coverage or P12 admission. The inherited test subset has only3 families and was
already exposed. `quality_evidence`, policy changes and evidence promotion remain
false. Core and Enterprise retain their existing manager/licence/restriction checks.

See [reasoning fidelity and controlled comparison](model-routing-reasoning-check.md)
for effective-model translation, disabled-summary privacy and a separately
budgeted default/high experiment with matching output ceilings.
