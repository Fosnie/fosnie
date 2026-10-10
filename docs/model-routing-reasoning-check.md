# Reasoning fidelity and controlled comparison

The routed adapter path already preserves explicit OpenAI effort and uses
Gemini3 thinkingLevel. V13 used null effort overrides, so its failures are not
evidence that high reasoning was incorrectly translated. The provider defaults
were the settings actually tested. Declared tier does not prove answer quality.

The [GPT-5.4 Mini documentation](https://developers.openai.com/api/docs/models/gpt-5.4-mini)
lists none/low/medium/high/xhigh, with none as the default. The
[Gemini3 guide](https://ai.google.dev/gemini-api/docs/generate-content/gemini-3#thinking-level)
lists minimal/low/medium/high for Flash-Lite and identifies minimal as its default.
Minimal does not guarantee full thinking-off. These are API controls, not
universal quality ratings. The same guide recommends temperature1.0.

## Adapter corrections

Legacy Responses streaming now preserves xhigh rather than reducing it to high.
The existing legacy maximum still maps to high; routed targets retain their exact
declared API value. An unsupported legacy value is refused before HTTP lookup,
instead of becoming high implicitly. Null effort omits reasoning as before.

Responses streaming requests summary=auto only when trace is enabled. With trace
disabled, the effort remains active, the summary request is omitted and any
unsolicited summary event is not forwarded. Non-streaming reasoning behaviour,
token ceilings and response completion checks are unchanged.

Gemini request translation now receives the actual model selected for that call,
including explicit legacy model arguments. The reasoning dialect no longer comes
from a different default model. Gemini3 legacy controls use thinkingLevel; earlier
models retain their existing thinkingBudget translation. Routed behaviour for
supported settings is unchanged. The
[REST ThinkingConfig](https://ai.google.dev/api/generate-content#ThinkingConfig)
defines the uppercase enum and prohibits using thinkingLevel on older models.

For Gemini3, a missing request preserves the provider default; explicit auto
requests thoughts according to the trace flag without forcing a level. Explicit
minimal is distinct from disabled. Full disable cannot be guaranteed and fails
before HTTP with an instruction to select minimal where supported. Unsupported
levels are not silently converted. The exact documented IDs gemini-3-pro-preview
and gemini-3.1-pro-preview additionally reject their unsupported levels before
HTTP. Unknown aliases remain subject to declared profile capabilities/provider
validation; no capability or quality is guessed from a name.

## Separate controlled experiment

The new make_reasoning_plan in cloud_allocation.py uses only four inherited
audited cases: two arithmetic and two bracket problems which no V12/V13
configuration passed. For each fixed model it compares default versus explicit
high, both with output2048. There are16 maximum attempts:8 per model,4 per
configuration. Same prompts, references, case order, language and30s Core deadline;
15-minute total and at least10s between Gemini requests. No system message, tools,
judge, source lookup, fallback or retry. The output cap also bounds reasoning;
length/incomplete answers remain unknown, not successful partial solutions.

This includes a new default2048 control. Comparing high2048 only with V13
default512 would confound effort with the larger output allowance. The experiment
does not change Auto presets and is not a held-out routing quality evaluation.

Native high payloads are exact: OpenAI reasoning.effort=high; Gemini
thinkingConfig={thinkingLevel:HIGH,includeThoughts:true}. The latter requests
summaries under the existing default trace behaviour, but the text-only pilot
does not store them. Default omits the reasoning override. Gemini default sends
temperature1.0; with explicit thinking, the adapter omits sampling and the
provider default remains1.0. OpenAI Responses continues omitting temperature.
Thought tokens are included in the existing shared accounting. The guard retains
only safe numeric usage metadata, never prompts/answers/thoughts/credentials.

Each model×case×variant is single-use. Default and high are different planned
slots, not successful retries. A changed variant, effort, output or native body
is refused before upstream bytes. The V13 baseline factory/JSON/claim/ledgers
remain unchanged. Historical approvals do not authorise this new16-call plan.

The private routing_pilot example can set evaluation_max_output_tokens from1
to2048; omission retains1024. This affects only its disposable test policies,
not production settings or the deployed evaluation bound. It allows the same
2048 cap in both variants without changing their policy between cases.

Workspace run_reasoning_cloud.py provides offline proposal, read-only preflight,
synthetic native replay and separately approved execution. Simulation checks all
16 slots and that its8 high rows account64 synthetic thought+1 visible token
as65 output tokens. It proves accounting/translation, not model quality. Real
execution needs a new matching human approval and exclusive16-call claim; it
cannot reuse any consumed earlier directory/allocation. Keys use hidden stdin
and memory. Claims, input hashes, profile revisions and native dispatches persist;
failed/unknown reservations are never refunded.

```powershell
python run_reasoning_cloud.py proposal --output NEW_REASONING_PROPOSAL.json
python run_reasoning_cloud.py preflight --proposal NEW_REASONING_PROPOSAL.json --output NEW_PREFLIGHT_DIRECTORY
python run_reasoning_cloud.py simulate --proposal NEW_REASONING_PROPOSAL.json --output NEW_SYNTHETIC_DIRECTORY
# Only after a new human allocation:
python run_reasoning_cloud.py execute --proposal NEW_REASONING_PROPOSAL.json --approval APPROVAL.json --output NEW_LIVE_DIRECTORY
```

This diagnostic has two already exposed test families, no independently reviewed
judge tiers, no tools or Russian coverage. It cannot meet20-family P12 admission,
promote profiles, establish calibrated Auto confidence or demonstrate savings.
All results keep quality_evidence=false and human review is not fabricated.
