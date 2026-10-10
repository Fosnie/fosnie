# Auto effort presets and judge diagnostics

Routing remains a Core feature; Enterprise consumes the same profiles, policy,
revision checks, permissions and budget ledger. These optional JSON fields need
no database migration. Existing profiles and policies retain provider defaults
and the classifier confidence threshold of 0.75.

## Configuration and precedence

Set `ModelProfile.auto_effort` to an API value declared in
`capabilities.efforts`, or leave it absent/null for the provider default. For
example, an Ollama model that supports disabling thinking can declare `none`
and use `"auto_effort":"none"`; a different executor can use `high`. These are
operator settings, not universal capability or quality ratings.

For an Auto executor, the order is explicit request effort, explicit terminal
rule effort, model preset, provider default. Conflicting explicit request and
rule efforts fail. Capability and budget checks resolve the effective effort
separately for every candidate. A terminal rule without an effort inherits that
model's preset. Auto Off/Shadow checked-default execution uses its model preset;
the Shadow proposal remains a separate decision. Manual and Default selections
do not inherit Auto presets. They retain provider defaults unless the request
supplies an effort.

The primary, tool loop and final answer keep the selected target and effort.
Inherited helpers keep that target; explicitly assigned Auto helpers use their
assigned model's preset. Helpers still pass their own capability and policy
checks at dispatch. Presets cannot increase any output, context, duration,
cost or call allowance.

Technical fallback and quality escalation resolve the new candidate's preset.
Only an explicitly requested/rule effort constrains every attempt. This avoids
turning a local model's `none` preset into a demand that a cloud fallback also
supports `none`. All attempts retain the existing shared budget and retry limit.

Set `RoutingPolicy.classifier.effort` independently. It never inherits an
executor preset, even when the judge and executor are the same provider.
Unsupported declared judge efforts fail before resolving a credential or
reserving an inference call; Auto can then use its checked default. Provider
rejection of a declared effort does not silently remove it or retry the judge.

`classifier.min_confidence` accepts finite values from 0 to 1. A result below
the threshold becomes uncertain and uses the checked default. Equality is
accepted. Known workload requirements cannot be relaxed. The default remains
0.75. Raising this number cannot fix a confidently incorrect classification.

## Adapter contracts and evidence

OpenAI Chat/Ollama receive `reasoning_effort`; OpenAI Responses receives
`reasoning.effort`. Native Gemini 3 and later use `thinkingLevel` for supported
`minimal`, `low`, `medium` and `high` values. The existing budget translation is
retained for earlier Gemini models. Unsupported mappings are rejected before
HTTP, without guessing an equivalent setting. API family recognition is solely
wire translation; ranking never uses a model name as a quality score.

Changing an Auto effort preset changes the evaluation fingerprint and requires
fresh reviewed evidence. Fingerprints without a preset remain byte-for-byte
compatible with the original version. Learned ranking retrieves evidence for
each candidate's effective effort and retains its conservative requirement for
comparable cohorts; evidence from different efforts is not automatically
treated as comparable. The settings editor clears evidence when presets change.

## Offline judge diagnostics

`tools/routing/eval/judge_diagnostics.py` compares saved, explicitly labelled
classification observations. It does not call models, mutate policies, fit a
threshold, score final answers or produce reviewed P12 model evidence.

Labels contain `schema_version:1` and `cases`. Each case contains `id`,
`family_id`, `split` (`calibration` or `test`), allowed `tasks`, the labelled
`minimum_tier`, and expected booleans for `fresh_information`, `tools`, `vision`,
`structured_output` and `uncertain`. Semantic families cannot span splits.

Observations contain `schema_version:1`, a list of `models`, and `records`.
Each record contains `model`, `case_id`, `status` (`completed` or `failed`) and
the raw eight-field `classification`, or null for a failed response. Duplicate
observations are rejected. Missing, failed and malformed responses remain
unknown. Tier estimates above and below labels are reported separately from
task/flag errors. Confidence buckets expose confidently incorrect fields;
abstention never erases those diagnostic errors.

```powershell
python tools/routing/eval/judge_diagnostics.py --labels labels.json --observations observations.json --output judge-report.json --min-confidence 0.75
```

Keep calibration and held-out test results separate. Labels are operator
judgements, particularly tier estimates; a label mismatch does not by itself
prove a model incapable of answering. Use an independently reviewed corpus and
actual answer evaluations before changing production routing or quality claims.

Sources: [OpenAI model effort support](https://developers.openai.com/api/docs/models/gpt-5.4-mini),
[Gemini ThinkingConfig](https://ai.google.dev/api/generate-content#ThinkingConfig),
[Ollama OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility).


## Native output rejection diagnostics

`/chat-step` retains HTTP 422 and the existing safe detail `output schema
contract rejected`. It additionally returns `X-Fosnie-Output-Contract` with a
fixed category: `unsupported_contract`, `incomplete_response`, `invalid_content`,
`output_too_large`, `markdown_fence`, `invalid_json`, `duplicate_keys`,
`fields_mismatch`, `scalar_type_mismatch`, `enum_mismatch` or `bounds_mismatch`.
Core recognises these values only on 422 and returns a fixed operator-facing
failure message. Unknown headers are ignored. Categories include no generated
text, field names, schema values, prompt, reasoning or credential.

Rejection still fails the requested contract: no Markdown removal, JSON repair,
schema downgrade, replay or budget refund is introduced. Sending a native schema
does not prove that a particular local server/model enforces it. A syntactically
valid answer is also not proof that its classification or final facts are right.

## Private local calibration and tool probe

`tools/routing/eval/fixtures/judge-calibration.v1.json` is a 12-case, AI-authored
diagnostic seed. Its `reviewed:false` and `quality_evidence:false` flags are
deliberate. All families remain in calibration; none is a held-out quality test.
Use the frozen prompts and field labels for comparing saved judge observations,
and retain failures and abstentions. Obtain independent review and a separate
family-disjoint corpus before promoting results to quality evidence.

The loopback-only `backend/examples/routing_pilot.rs` accepts an optional
`local_judge_name` (`llama3b` or `qwen4b`) and independent `judge_efforts`. It exposes
the private `/api/routing/tool-weather-probe` endpoint for its manager identity and
isolated `routing_pilot_*` schema. This is absent from deployed Core routes.

Use the returned `tool-local` policy ID/revision. The probe advertises exactly one
`lookup_weather` call for Glasgow, validates its arguments, rechecks the original
actor and policy before egress, and makes at most one fixed Open-Meteo GET. The
tool loop and final answer reuse one routed run, target, effort and two-call
ledger; no judge, fallback or automatic replay is permitted. Data must use Celsius
and a UTC timestamp within one hour. The final strict flat JSON answer is compared
with the public tool evidence. Source bounds are 16 KiB and eight seconds.

Optional request `ground_schema:true` selects a separately labelled prompt
contract which also includes the exact schema and a no-Markdown instruction.
Default remains the original contract. Neither option removes native validation
or extends any allowance. Failed final contracts preserve the public evidence,
failure phase and accounted/reserved run usage, without exposing raw model text.

This checks native tool calling and a public weather read, not the full Fosnie
`web_search`/SearXNG pipeline. Open-Meteo current conditions use weather-model
data; they should not be labelled direct station observations. See the
[Open-Meteo API documentation](https://open-meteo.com/en/docs).

## Browser generative ceiling and fixed services

The browser's internal 1024-token response ceiling is enforced on generative
stage requirements before JSON/stream dispatch and reflected in the runtime's
reported output capacity. It is not injected into the shared policy budget.
This distinction permits byte-accounted fixed web services while preserving
explicit deployment/user token, output and money caps: fixed calls with unknown
usage still cannot satisfy those caps. Their unknown counters remain non-zero.
The same server-owned browser witness covers adopted remote desktop text.

Artefact intent checks match whole keywords rather than substrings, so a request
to make a search against API documentation does not demand a file-export gate.
Explicit file exports and plural document requests retain the existing path.


### Quick web evidence limits

The routed inline `quick` path uses the original current-turn question from chat
execution as a ranking hint (at most 2048 Unicode characters), separately from
model-authored tool arguments. It never sends this hint to the search engine.
Both the lexical prefilter and the assigned fixed reranker use a bounded
combination of the engine query and this question; the reranker's existing
provider, locality restrictions and shared byte/call accounting still apply.
The hint carries no instructions, permissions, source scope or new target.

The deterministic lexical prefilter favours distinct query words, with a
rarity weight, and keeps a verbatim window of at most 800 characters from each
chunk. It sends at most eight excerpts to the fixed reranker. Both selection
steps prefer one excerpt per source before filling remaining places by score;
the digest holds at most four excerpts with original source identifiers,
citations, timestamps and snippet-only flags. Source pools remain intact.
The digest explicitly warns about partial coverage whenever material is omitted.
Truncated context cannot establish that something is absent. A malformed score array or
non-finite score fails; a routed budget/target failure is not salvaged
into apparently valid evidence. No new inference call is introduced.

These are deliberately small quick-search limits, not token accounting or a
recall guarantee. Exact word matching may miss paraphrases; broad questions
should use standard search or further targeted searches. Standard and deep
search retain their existing full candidate processing and digest limits.
Existing domain, robots, SSRF, integration and actor checks precede selection;
strict routed requests still skip unattested caches. Enterprise uses the same
Core execution path; this change introduces no separate tenant policy or API.

See [reasoning fidelity and controlled comparison](model-routing-reasoning-check.md)
for effective-model translation, disabled-summary privacy and a separately
budgeted default/high experiment with matching output ceilings.
