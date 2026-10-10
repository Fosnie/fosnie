# Instruction following through the Core evaluation endpoint

`tools/routing/eval/instruction_pilot.py` reuses CoreClient, payload generation,
preflight and completed_text_v1 attestation. It runs the same40 original prompts
through three manual configurations, Classifier and Hybrid. The manual Gemini
answer is reused for baseline comparison, with no second baseline generation.
Classification receives the original prompt and unknown task; unreviewed source
labels never become trusted caller hints.

Only a complete response with matching evaluation ID, provider/profile/policy
revisions, effort, native target and ledgers reaches the instruction grader.
Original answers are retained separately with hashes; the report carries constraint
checks and accounting. Missing/incomplete answers remain unknown. Configuration
changes, duplicate/retried observations and budget overrun cannot become pass.
Every Core attempt reserves before dispatch; interruption never refunds the slot.

The V33 private native guard pins exact payloads and destinations, separates judge
and primary reservations and stops a provider after a failed/unknown send. Failure
ownership now comes from the current HTTP handler's thread-local target, not the
last globally appended row. This matters when a late judge request finishes after
a newer primary request has started. No key text enters saved snapshots or logs.

V33 completed all three manual phases on the frozen public set:

| Configuration | Pass | Fail | Unknown |
|---|---:|---:|---:|
| Qwen4B/none | 3 | 0 | 37 |
| GPT-5.4 Mini/high | 35 | 0 | 5 |
| Gemini Lite/high | 40 | 0 | 0 |

Qwen was stopped after one upstream timeout; its later cases are unavailable
observations, not established content failures. Unknowns remain in denominators.
These counts measure verifiable instruction compliance only, not semantic accuracy
or general model quality. The generated-token cap also includes reasoning output.
No actual invoice, free-credit balance or monetary savings is established.

The live Auto phase exposed the late-response proxy ownership defect and was
interrupted. Original151 recorded Core results,152 Core reservations and135 native
attempts are retained:131cloud/4local, cumulativecloud622. Classifier rows are partial
and affected by incorrect blocking; Hybrid was not run. Unattempted rows count as
unknown, and no router win/loss or P12 quality claim may use this Auto comparison.
The old false cleanup receipt is retained rather than rewritten.

The defect was repaired and verified by two concurrent/late-reply regressions plus
the full200Core/280native synthetic protocol (real calls0).48 affected offline tests
also pass. A zero-dispatch startup failure and a later startup failure while owned
containers were stopped are preserved in their separate output folders.

Final cleanup separately confirms private schemas0, own services/indexes removed,
temporary ports free and the original GPU PID no longer running. The original
interrupt's schema count is unknown because its container had stopped; it is not
retroactively reported as0. No main model store, production policy or evidence
was changed, and no old API attempt was replayed.

Next: freeze a clean experiment after the transport fix, with explicit handling
of provider availability and rate-spacing within Core deadlines. Preserve the
affected live rows as failed operational evidence rather than silently replacing
them. Do not tune grader/profile thresholds on this exposed set or claim fresh
holdout from a replay. Independent family/workload/source review and semantic
quality admission remain open.

Primary contracts: [GPT-5.4 Mini](https://developers.openai.com/api/docs/models/gpt-5.4-mini),
[reasoning output limits](https://developers.openai.com/api/docs/guides/reasoning),
[Gemini thinking](https://ai.google.dev/gemini-api/docs/models/gemini-3.1-flash-lite).
Workspace report.v33.md/summary, final checks, live receipt and native ledger are
the primary records; the final synthetic run is separate proof of the repair.

V34 completed a [new cloud-only comparison with request deadlines](model-routing-pilot-deadlines.md). It preserves the interrupted V33 result and does not replay it.
