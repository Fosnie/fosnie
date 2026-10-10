# Unified Auto pilot

This pilot compares final answer utility of Manual, Rules, Classifier and Hybrid.
No command changes production settings, imports evidence or promotes profiles.

## Data

`pilot_corpus.py` reconstructs75 cases:25 each from revision-pinned GSM8K,
SQuAD1.1 and MERA ruOpenBookQA. Selection is deterministic, before model outputs:
5 calibration and20 test source groups per dataset. Original inputs, licence/
attribution, references, source hashes/revisions and proofs remain bound.

GSM8K groups use calculation skeletons. Bounded exact rational arithmetic checks
published calculations/final numbers, not the semantic reasoning of the problem.
SQuAD groups are whole articles; every accepted answer must match a published
context span. MERA groups use normalised question stems; the source describes
automatic translation and human validation. The Russian questions are not
claimed to be originally human-authored. Wrapper instructions contain no answer.

Package: dataset.json, answer-bank.json, source-manifest.json, provenance.json,
upstream-sources.json and licence/attribution files. blind-review/index.html and
review-draft.json are incomplete, with no predictions, chosen tiers or answer
bank. Family independence/tier labels are not fabricated. Public model-training
exposure is unknown. Newly selected groups are not automatic P12 samples.

The validator uses whole-answer equality after outer trimming, against the
finite published answer list: integer, exact span or A/B/C/D. No extraction,
repair or model grader. Missing/failed/incomplete answers are unknown. Source
groups cannot span calibration/test. Old V7–V14 versions are not re-scored.

## Frozen profiles and phases

Pilot tiers are explicit hypotheses: local Qwen4B/none economy, GPT-5.4 Mini/high
advanced, Gemini3.1 Flash-Lite/high balanced. Model names do not prove quality.
Saved profiles/policies/revisions are checked before each evaluation. The Gemini
profile also supplies the fixed judge with independent low effort, native schema,
1024 output and confidence threshold0.75. Judge low does not inherit executor high.

Phase1:15 calibration cases ×3 Manual candidates =45 primary calls. Freeze the
best single by complete source-group pass, then case pass, then stable ID.
Selection never reads test outcomes. Unknown calibration attempts remain counted.

Phase2:60 test cases ×frozen best-single/Rules/Classifier/Hybrid =240 primary
calls. Same input/language/caller hints across modes. Supplied-document workflow
metadata sets extraction for SQuAD; other task kinds stay unknown. No answer or
tier labels enter routing. Rules selects local extraction and balanced for other
requests. Classifier describes requirements and cannot choose a provider/URL.
Hybrid skips the judge for known extraction and classifies otherwise. Policies,
presets, thresholds and pools stay fixed throughout test.

Ceiling:285 primary+120 judge=405 generation slots. The frozen Hybrid rule skips
20 judges; full synthetic replay actually uses385 slots, including100 judges.

## Budget and execution

One proposed allocation: maximum390 cloud calls; per-provider240 OpenAI and375
Gemini;2million conservative cloud token reserve,1.5million per model. Output
2048 primary/1024 judge, Core30s, whole pilot2h, Gemini interval10s. Core reserves
50000microUSD per evaluation, summed upper reserve14.25USD. Declared prices/free
credits/account quotas are not verified invoices. All judge/primary/failure/
checked-default spend stays in the same ledger; unknown spend is not refunded.

Core reservations fsync before POST, native reservations before upstream bytes.
The guard binds exact model/case/purpose/body and current judge instruction/schema.
Only official OpenAI Responses, Gemini generateContent and loopback Ollama11435
are allowed. No redirect, proxy-env, extra helper/judge/primary, tools, retries,
fallback or escalation. Auth/quota failure blocks the provider. Results preserve
safe targets/timings/usage and bounded judge fields, not raw answers/thoughts,
provider error bodies, headers or keys.

Workspace run_unified_pilot.py: offline proposal, read-only preflight, synthetic
Core/ML replay, or separately approved execute. Mock all-pass proves transport/
accounting only and is labelled synthetic. Live needs exact whole-stage approval,
one exclusive claim and hidden-memory credentials; earlier consumed budgets
cannot be reused. Drift/contract/deadline failures stop later dispatch.

```powershell
python tools/routing/eval/pilot_corpus.py check --sources PUBLIC_SOURCES --output CORPUS
python run_unified_pilot.py proposal --output NEW_PLAN.json
python run_unified_pilot.py preflight --proposal NEW_PLAN.json --output NEW_PREFLIGHT_DIRECTORY
python run_unified_pilot.py simulate --proposal NEW_PLAN.json --output NEW_SYNTHETIC_DIRECTORY
python run_unified_pilot.py execute --proposal NEW_PLAN.json --approval APPROVAL.json --output NEW_LIVE_DIRECTORY
```

## Coverage and admission

Supplied SQuAD context measures extraction, not retrieval. This answer-quality
lane does not exercise real web/KB lookup, generated code, multi-turn continuation
or production Enterprise source access. Those remain explicit gaps.

Separate existing Enterprise acceptance exercises real global/scoped RBAC, BYOK,
licences, local-only/pools, source ACL, durable worker revocation and signed audit
with synthetic models. It is permission/accounting evidence, not75-case quality
through Enterprise. Earlier V6 live local web receipts remain separate, not new
Auto quality evidence.

P12 needs20 independently reviewed comparable test families per workload/effort
group. These60 candidate groups mix tasks, languages and context ranges; grouping
and review are incomplete. They do not automatically admit P12. Judge label
accuracy remains unknown until independent review; final answer utility can be
reported as exploratory. No fitted confidence, policy promotion or savings claim.

Sources: [GSM8K](https://github.com/openai/grade-school-math),
[SQuAD/data licence](https://rajpurkar.github.io/SQuAD-explorer/),
[MERA card](https://huggingface.co/datasets/MERA-evaluation/MERA).

## Exploratory live V16 result

The whole-stage allocation was authorised after the prepared proposal. Actual
Core/ML/native execution completed45 calibration and240 test evaluations.
Calibration selected gemini-lite; test did not select
the baseline or change thresholds/profiles. Test60-case counts: best_single 53pass/3fail/4unknown, rules 49pass/7fail/4unknown, classifier 38pass/20fail/2unknown, hybrid 41pass/18fail/1unknown.
There were 263 cloud and 122 local upstream
attempts, including 100 judge attempts. Failed requests
and unknowns were retained without retries. Native usage is incomplete for
22 attempts; invoice cost and savings are not established.

Workspace report.v16.md/report.v16-summary.json preserve source/target counts,
judge overhead and paired outcomes. No production policy or quality evidence was
promoted. Independent group/requirement review and broader workflow quality
remain required. A guard regression also verifies that cancellation during
Gemini spacing keeps the reservation and prevents a late upstream send.
