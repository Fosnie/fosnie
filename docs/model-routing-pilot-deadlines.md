# Cloud-only instruction pilot with shared request deadlines

V34 keeps the V32 instruction grader and predeclared Gemini/high baseline. It
selects40 other original IFEval prompts, excluding every V32 prompt, with the same
20 instruction types. Those types are already exposed and are not new independent
held-out families. Source consistency, workload labels and independence remain
unreviewed; instruction compliance still does not establish semantic accuracy.

The executor pool is fixed before inference to OpenAI and Gemini. Qwen's earlier
timeout was an availability limitation for the30s budget; this cloud-only run does
not claim new local-model capability or change production selection settings.
Manual responses are generated once and the Gemini answer is reused for paired
Classifier/Hybrid comparison. In this unknown-task lane Hybrid has no terminal
rule, so its observed difference cannot establish a benefit from explicit rules.

`eval/request_timing.py` separates initial Gemini pacing from the Core deadline.
The client waits before submitting Core, then stores that request's start time
on its own active record. A native judge is capped at12s; primary receives the
remaining30s budget with a1s margin and a25s upper bound. An intra-request Gemini
rate wait cannot extend the absolute Core deadline. Late handlers keep their own
provider, purpose and original start time; no global last-record attribution.

A single transport timeout consumes its reservation and remains unknown. A later
new case may dispatch; the failed case is never retried. Three consecutive failures
for a provider/purpose, or401/403/429, stop the fixed experiment instead of filling
the remaining sample with requests to an unavailable provider. A successful
response resets only that provider/purpose counter. Missing, failed and truncated
responses remain in report denominators.

The whole bound is160Core requests,240generations/cloud calls,2M conservative
reserved cloud tokens,2048primary/1024judge output tokens and8USD Core reserve.
The reserve is not an invoice or a promise about free credits. Each run has its
own non-resumable claim and no automatic retry or evidence/policy promotion.

41 affected offline tests and the complete160Core/240native synthetic protocol
passed with zero model calls. The live run completed on the same fixed controls;
read workspace report.v34.md/summary for actual pass/fail/unknown counts, provider
attempts and judge usage. API HTTP2xx is distinct from a complete attested response.
Native timeouts and generation caps can create unknowns without proving an
incorrect answer. Native usage is preserved; actual invoice/savings remain unknown.

Final private schemas0, own services/indexes removed and temporary ports free.
No GPU/model server was started, no model was downloaded, no old claim was replayed
and V32/V33 inputs and interrupted evidence were preserved. No unrelated backend,
ML inference, frontend or Enterprise tests were repeated for this offline adapter.

This stage provides a complete operational/instruction comparison, not P12
semantic admission. Next work should address independently reviewed requirements
and source families, semantic answer rubrics and the routing overhead shown by
the results, using a separate predeclared dataset before changing thresholds.
