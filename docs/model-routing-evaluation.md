# Routing evaluation

The evaluation tools are Core functionality. They use the same routing types,
filters, rules, classification merge, retry eligibility and run budget as Fosnie.
Evaluation never enables a public caller gate, changes a saved policy or installs
a model. Real model quality and billing are separate from routing contract checks.

## Offline replay

From the Core checkout on Windows, with the existing ML Python environment:

```powershell
./tools/routing/test.ps1 -Lane eval-offline
```

This builds `backend/examples/routing_replay.rs`, checks the harness and replays
the frozen dataset twice to confirm identical JSON output. No PostgreSQL, Redis,
ML server, API credentials or network access are required. Output goes to
`tools/routing/eval/reports/offline/report.{md,json}`; generated reports are ignored
by Git. The versioned cases and manifest remain source files.

Portable commands (Python uses only its standard library):

```sh
SQLX_OFFLINE=true cargo build --offline --manifest-path backend/Cargo.toml --example routing_replay
python tools/routing/eval/runner.py offline --output /tmp/fosnie-eval --verify-replay
python tools/routing/eval/runner.py offline --output /tmp/fosnie-smoke --smoke
```

Use `--replay-bin` if Cargo's target directory differs. The example accepts bounded
JSONL on stdin and emits one deterministic result per line, with no DB/network
or product-side effects. It is an adapter to production primitives, not another
ranking implementation. It supports Active proposals and simulated accounting;
the service/HTTP fixtures separately verify authorization, streaming and actual
target dispatch. Offline `actual_target`, actual cost, latency and TTFT stay null.

`tools/routing/eval/fixtures/cases.jsonl` contains **420 synthetic variants, 70
dialogue/task families and 14 semantic template groups**, with English/Russian
and all three objectives. The 28-case smoke subset covers every category in both
languages. Categories include frozen historical weather, ordinary recipe scaling,
editing, short proof certificates, coding, long simple extraction, RAG/document
facts, tables, supplied tool results, continuation, unknown profiles/prices,
local-only and pre-output vs non-retryable failures. Documents/tools are frozen
data; no real document renderer, web search or side effect is executed here.

Entire semantic template groups stay in a single train/calibration/test split,
including both languages, parameter variants and objective variants. This avoids
pretending that near-duplicate prompts are independent held-out samples. It is a
small contract corpus, not a statistical guarantee of model performance. No
threshold is fitted. Judge self-confidence is not a calibrated probability.

To intentionally revise fixtures, edit `corpus.py`, regenerate them and review
the changed manifest/hash and source facts together. The runner checks the
frozen SHA-256, count, schema, origins, IDs and family/template split boundaries.
It never silently regenerates expected results during verification.

The six comparisons are always-economy, a single fixture model selected only on
calibration, simple rules, classifier, hybrid and a post-answer fixture oracle.
The oracle has perfect availability knowledge and no judge overhead; it is an
ideal synthetic upper bound. Mock answers, classifier output, declared tiers
and tariffs are explicit fixtures. Their pass counts and estimate reductions
are **not real model accuracy, calibrated routing quality or financial savings**.
An unknown response or price is never counted as pass or zero total cost.

Validators check exact JSON keys/types/facts/source IDs, arithmetic totals,
Euler certificates, topological orders and absence of external tool actions.
The report separates routing contract failures, fixture validator outcomes,
under-routing, advanced choices for ordinary tasks, all-stage cost estimates,
classifier/helper/retry overhead, unknown costs and evaluator costs. The JSON
contains category/strategy, language, objective and split aggregates, held-out
comparisons, revisions and per-case results. It contains no raw prompts,
credentials or raw upstream responses. JSON/Markdown output is reproducible.

## Explicit live evaluation

Live mode uses authenticated `POST /api/routing/evaluate` on the authorized
Fosnie deployment. It requires the effective global `providers.manage` permission
and the original saved actor; scoped-only permission is insufficient under the
Enterprise RBAC implementation. Server authorization/policy checks run before
planning, periodically during the one pinned future and before returning bytes.
Current target revisions and injected restrictions are checked again at completion.

The endpoint accepts saved selection/policy/revision, bounded plain-text messages,
optional known requirements and explicit output/cost/time limits. It measures
serialized UTF-8 input itself and never trusts a smaller client token estimate.
Known effort/tier/capabilities survive classification. Per-case local-only can
only tighten saved policy/deployment/Enterprise constraints. The new request-owned
ledger is installed **before the classifier**. At most one judge and one Primary
answer share its cost, call, token and deadline allowance. No tool schema, fixed
services, source lookup, helper fan-out, retries or durable replay are available.
Missing/partial/failed reports or attempted tool calls cannot return an answer.

The endpoint records started/finished audit receipts and actual target metadata
only after a validated completed answer. On failure the response never claims
zero spend; the client keeps the complete reserved request upper bound. Pricing
is the saved profile estimate, not an invoice. Actual billing remains unknown,
including unmodelled native cache-price differences.

With no authorized endpoint/budget, this writes an explicit `not_run` report and
sends **zero network requests**:

```powershell
python tools/routing/eval/runner.py live --output tools/routing/eval/reports/live-not-run
```

`live-config.example.json` is a non-executable placeholder. When an operator
authorizes endpoints and a budget, the implementing AI prepares a separate config
with actual visible provider UUIDs and existing saved policy revisions, and obtains
the authenticated manager credential via the named environment variable. Never
put a token into the config or command line. Saved Auto policies must already be
Active for the explicit evaluation; the runner never activates them. Manual
baselines use the caller's effective default policy. The configured single-model
baseline is not assumed to have measured quality merely because it is named.

```powershell
python tools/routing/eval/runner.py live --live-config <authorized-config.json> --output <private-report-directory>
# The previous command remains a zero-network preview.
# Run only after endpoint and budget authorization:
python tools/routing/eval/runner.py live --live-config <authorized-config.json> --execute-live --output <private-report-directory>
```

The runner preflights effective management permission, policies/revisions,
strategies and visible profiles. HTTPS or loopback HTTP is required; credential
URLs and redirects are refused. Sampling is deterministic and stratified across
categories/languages. Before each evaluation, the full request cap is reserved
from the total run cap; failed, timed-out or unknown-spend requests are not
refunded or retried. A smaller remaining run cap stops further requests.

Live mode grades the same frozen text/source facts and emits the same result
format. It does not inject offline provider/profile/pool/fault mutations into
the real deployment. Unknown fixtures remain unscored. A live RAG/tools/document
product-workflow quality evaluation needs separately authorized fixture adapters;
the text endpoint does not claim that coverage. External LLM evaluators are not
invoked; their cost is separately zero because none ran.

Real quality-preservation confidence bounds and net actual-cost savings remain
`insufficient_evidence`/`not_run` until an adequate independent sample and billing
data exist. No global Active rollout or release gate is inferred from mock results.

## Accounting boundary for this release

Routing ledgers normalize per-stage input/output/reasoning/cache counts without
adding reasoning twice. Text grants use conservative input/template bounds and
profile tariffs. `cost_microusd` is a known estimate subtotal when unknown-cost
calls exist; it is not a claim about actual provider charges. Missing native
cache-specific tariffs and invoice reconciliation remain explicit unknowns.

Fixed embed/rerank/OCR/STT/TTS attempts use their separate byte/call/deadline
grants. Their unproven token/money accounting is recorded as unknown. Such work
is refused before dispatch when a hard token/output/money cap needs a proven
bound; it is not assigned an invented zero price. Bounded multimodal inference
continues to be refused until adapter-specific accounting exists. These are
supported-scope restrictions, not successful implementation of missing pricing.
Preserve them when adding evaluations or broadening the release scope.

See [unified Auto pilot](model-routing-unified-pilot.md) for the new public EN/RU
corpus, calibration-only baseline selection, shared judge budget and explicit
separation of answer quality from workflow/Enterprise contract coverage.
