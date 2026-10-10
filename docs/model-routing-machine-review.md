# Machine reviewers for semantic routing diagnostics

`semantic_judge.py` adds a blinded machine-review protocol to the offline semantic
review bundle. It does not introduce a production inference transport, semantic
escalation, learned ranking promotion or an alternative provider resolver.
The native dispatcher in the local pilot is bounded separately from Core's saved
execution ledger; reviewer API attempts are counted separately from answer calls.

Two fixed reviewers, `gpt-5.4-mini` and `gemini-3.1-flash-lite`, receive identical
question/answer/criterion/reference JSON. Strategy, actual answer model, effort,
profile, instruction score, costs and latency are omitted. Each request is an
independent conversation; the original answer is reused without regeneration.
Original wording can still reveal authorship. These two reviewers are also answer
candidates and may share training data or exhibit self-preference: agreement is
not independent ground truth or proof of impartiality.

Requests use the documented native schemas: OpenAI Responses `text.format` with
strict JSON schema, `store=false`, low reasoning and 2,048 output tokens; Gemini
GenerateContent `responseMimeType`/`responseJsonSchema`, LOW thinking, and the same
output cap. No tools or retrieval. These fields follow the official
[OpenAI structured-output documentation](https://developers.openai.com/api/docs/guides/structured-outputs?api-mode=responses),
[GPT-5.4 Mini contract](https://developers.openai.com/api/docs/models/gpt-5.4-mini),
[Gemini model contract](https://ai.google.dev/gemini-api/docs/models/gemini-3.1-flash-lite),
and [GenerateContent reference](https://ai.google.dev/api/generate-content).
Account credits, billing and limits are not inferred from documentation.

The fixed proposal binds protocol source, public bundle, full manifest, selected
case IDs, exact endpoints, reviewer models and native body hashes before dispatch.
At most eight selected cases, 64 available reviewer attempts, 32 KiB native input,
2,048 generated tokens per attempt and a 1,200-second window. Input bytes plus
output cap and overhead are a conservative token reservation, not actual billing.
Missing executor answers cause no reviewer call. The diagnostic sample is chosen
from prompt content before obtaining reviewer scores, with prior exposure declared.
It is not a fresh quality holdout.

Every criterion has pass/fail/unknown, a rationale and exact quoted evidence.
Non-unknown ratings require an anchor in the supplied question, answer or reference
snapshot. Out-of-range reference indices and invented quotes are refused. Anchors
bind the reasoning to actual text, but do not prove its correctness. Schema
validation is not a factual grader. Reviewers must use unknown when material facts
need absent sources or expertise. Prompt/answer/reference content is untrusted;
instructions inside it must not redefine the review protocol. This is a prompt
boundary, not a guarantee against model prompt injection.

Only complete native responses are assessed: OpenAI completed assistant message
with output text; Gemini a single STOP model candidate. Refusal, truncation, tools,
invalid/duplicate JSON, missing criteria and quote failures become unknown, without
retry. Thought text is excluded. Returned model identifiers, when present, must
match the requested model or its dated snapshot; absence is recorded as unknown
attestation, rather than a guarantee of the exact serving weights.

The pilot reserves and flushes one slot before each attempt. Credentials stay in
memory, redirects and environment proxies are disabled, and headers/errors are
never persisted. HTTP 400/401/403/404/429 blocks only that reviewer. Three consecutive
soft transport/response failures block subsequent slots for that reviewer.
A timeout consumes its slot; later distinct slots may continue until the block.
Gemini calls are spaced by ten seconds. Expired windows send nothing; the ledger
retains failed/unknown attempts and closes on normal completion or interruption.
Consumed allocations cannot be reopened, resumed or overwritten. Native socket
phase timeouts and checks while consuming response chunks are not a distributed
wall-clock guarantee; no new dispatch is admitted after the run window.

Both individual reviews retain their ratings. Consensus keeps pass or fail only
when both reviewers give that same rating for the same criterion; disagreement or
an unavailable reviewer is unknown. Required-criterion aggregation follows
`semantic_review`: any required fail, otherwise unknown, otherwise pass. The report
retains all nine criterion agreement cells and per-strategy sample counts.
Calibration/test separation and the original full denominator remain in the
underlying reports. Unselected cases are explicitly unknown, not silently dropped.
`machine_review_json_complete` refers to the JSON form, not coverage; the report
states `full_cohort_semantically_assessed=false` for the bounded sample.

All results are `machine_diagnostic`. Synthetic reviewer calls remain labelled
synthetic even when their source answers came from an earlier live run. Live
reviewer attempts do not make retrospectively proposed criteria independent or
predeclared. No human attestation, family/source/workload certification, invoice
savings, Auto superiority or P12 admission is inferred. All outputs retain
`quality_evidence=false`, `p12_eligible=false` and `policy_updated=false`.

Continue with independent source/requirement/family/rubric review and a separately
frozen experiment before using results as measured ranking evidence. Do not tune
production profiles, defaults or thresholds on this exposed diagnostic sample.
