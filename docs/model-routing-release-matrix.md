# Routing release verification

This matrix identifies executable evidence for the supported release scope.
`all` uses isolated schemas, deterministic providers and original actor checks;
it never invokes a paid model or changes a production gate. Evidence locations
refer to Core unless explicitly marked Enterprise. A build alone is not a
behavioural pass. Reports distinguish test executions, ignored tests and live
quality. Final execution receipts are generated in `tools/routing/reports`.

## P12 extension, verified 5 October 2026

The optional [reviewed ranking and quality contract](model-routing-quality.md)
passed the ten-command `quality --with-enterprise` queue. Local receipts are in
`tools/routing/reports/p12/checks.json` and adjacent logs: 54 Core routing units,
one HTTP technical/quality acceptance root, ten real Enterprise integration tests,
48 frontend units and eleven browser scenarios passed. Core/Enterprise all-targets,
both frontend builds and the offline evidence utility build passed. One existing
durable unit is ignored in this lane, not counted as passed; its P11 DB acceptance
remains separate because P12 does not permit durable replay.

The final sole-candidate correction was verified by rerunning the affected HTTP
acceptance root: classification occurs and failing measured evidence refuses the
sole executor. Unrelated passing suites were not repeated. Coverage includes
typed structural failures, shared two-executor limit, budgets, rule pools,
credential/policy/provider changes, no rejected-content exposure, evidence
provenance/CAS/RBAC and browser import/opt-ins. No paid inference or real model
quality/savings measurement was performed. Both P12 options default off.

The P11 matrix below records the earlier complete supported-scope run; it is not
a claim that every P11 lane was repeated after adding P12.

## Observed execution, 3 October 2026

All 26 commands in the shared Core + Enterprise queue passed. R01–R38 passed for
the supported contracts described below, using synthetic providers. Core compiled
from a temporary source snapshot with no Enterprise sibling or licence. The
actual Core HTTP/WS → ML → product browser test passed without transport mocks.
The desktop all-targets check and separate unsigned Windows installer build
also passed. The installer was produced locally, without installation or publication.

| Package | Result |
|---|---|
| Protocol/Core Rust | 79 test executions passed, including the real stack and migration/upgrade fixtures. |
| Enterprise Rust | 97 passed, including the installed extension's routing, moderation, licence, audit and edition tests. |
| ML | 366 Core routing/adapter + 9 artefact + 19 Enterprise overlay tests passed. |
| Frontend | 46 units + 10 product-form browser scenarios + 1 real-stack browser scenario passed; both edition builds passed. |
| Offline evaluation | 10 harness contracts passed; 420 cases × 6 comparisons = 2520 results, identical replay and zero asserted routing-contract failures. |
| Desktop | Offline all-targets check and release compilation/unsigned x64 MSI bundling passed. Installer launch/installation was not performed. |
| Ignored in unit lane | One durable DB test was reported separately, then executed and passed in its required DB step. It is not counted as a passing skip. |

Receipts: [release checks](../tools/routing/reports/release/checks.json),
[native check](../tools/routing/reports/native/checks.json),
[unsigned installer](../tools/routing/reports/native-release/checks.json), with per-command logs
alongside each receipt. Generated receipts are local artefacts, not versioned
release evidence in a fresh checkout. Re-run the queue for a different source.

The final batch repaired an outdated durable schema fixture, Windows redirected
output encoding and two new browser fixture assumptions. It also exposed and
fixed real empty-key HTTP authentication: no invalid empty Bearer header, no
inherited shared-client auth/cookies and no redirected routed LLM call. The
affected ML package and real stack were rerun; already passing packages were
retained. Evaluation source hashes match the current replay implementation.

| ID | Requirement | Executable evidence | Status |
|---|---|---|---|
| R01 | Core without Enterprise/licence | `core-isolation` compiles a temporary Core-only source snapshot; Core build and `model_routing_stack` exercise actual Core HTTP/ML/UI. | pass |
| R02 | Expired/unlicensed Enterprise | Enterprise `tests/model_routing/audit_license.rs`, all four licence states plus actual routed dispatch. | pass |
| R03 | Legacy pointer/default preserved | `model_routing_schema::r03_r18_r35_r38_upgrade_preserves_selection_and_invalidates_profiles`; common admission/browser cases. | pass |
| R04 | Default precedence and uniqueness | Same upgrade/catalog root, common management cases, P10 live preflight default resolution. | pass |
| R05 | Foreign/disabled/stale target | Core schema strict-target/catalog cases; Enterprise access/inference acceptance; ML inference-target contracts. | pass |
| R06 | DB/decrypt failure propagates | `provider_registry_seam`, schema strict-target cases; fixed/target ML contracts. | pass |
| R07 | Empty auth vs invalid credential | `test_inference_target.py`, Core strict target + injected registry, installed Enterprise synthetic keyring. | pass |
| R08 | Rules-only known/no match | `model_routing_classifier`, routing engine unit tests and frozen replay. | pass |
| R09 | Hybrid terminal/uncertain | Classifier acceptance, browser singleton fixture and replay stage counts. | pass |
| R10 | Cloud judge, no local runtime | Core classifier acceptance's cloud-only fixture and native adapter envelopes. | pass |
| R11 | Local-only judge/helper/fallback | Classifier, fallback and fixed integration cases; Enterprise inference restriction; real stack Off/local-only rejection. | pass |
| R12 | Local model and permitted web | Common web/API-agent/workflow-agent cases; `test_routed_web.py`, with explicit local helper/fixed targets. | pass |
| R13 | No grant of forbidden web tools | Common chat/API-agent/admission cases; ML strict tool/workload checks and validator action rejection. | pass |
| R14 | Ordinary weather/recipe | Frozen replay categories and classifiers/rules; synthetic expectations only, not measured model adequacy. | pass |
| R15 | Short complex/long simple/continue | Classifier acceptance and replay proof/coding/long-simple/continuation categories. | pass |
| R16 | Unknown model/price | Engine/budget tests, classifier/probe acceptance, eval unknown scenarios, fixed-service strict refusals. | pass |
| R17 | Capability/context/effort mismatch | Engine, strict target, ML target/native adapter and protocol contracts. Bounded multimodal deliberately refused. | pass |
| R18 | Profile/provider/index revisions | Fresh/upgrade schema root, fixed/index cases, management CAS and current-target dispatch checks. | pass |
| R19 | Concurrent actor/purpose isolation | ML target/fixed/stage concurrency; common chat/API-agent/desktop/workflow fixtures. | pass |
| R20 | Judge schema/timeout/injection | Classifier root and management probe; one accounted judge, uncertainty/checked default and original caps. | pass |
| R21 | Cancellation/barge-in | Classifier, fallback, browser, voice/live/phone fixtures and ML scoped cancellation. | pass |
| R22 | Tool side effect and resume | Common workflow source/agent/delivery/artefact/admission cases; explicit refusal of unadopted approval/resume, no automatic replay. | pass |
| R23 | Partial stream failure | Fallback root; ML `test_inference_failure.py`; streaming/accounting report validation. | pass |
| R24 | All multistage targets/usage | Common chat/web/research/document/fixed cases and corresponding routed ML tests. | pass |
| R25 | Original actor and queue resume | Durable ignored-in-unit test executed separately, common document/research/workflow jobs; Enterprise resumed source denial. | pass |
| R26 | Shared/simultaneous budget | Budget/lease/mixed unit + ML contracts, classifier/fallback/queue fixtures, evaluation cap-before-judge. | pass |
| R27 | Selection and compatible effort | Classifier/fallback/browser/desktop root, reasoning-effort/native adapters and protocol fixtures. | pass |
| R28 | Auto alias namespace collisions | `r08_public_api_alias_collision_and_execution`, migration namespace concurrency and alias/provider CRUD cases. | pass |
| R29 | Raw vs agent API | Common API/API-agent/alias cases, actual JSON/SSE, adopted library/web traces and strict unsupported fields. | pass |
| R30 | Old/new web/desktop wire | Protocol unit/golden tests, desktop/browser roots and actual production composer browser scenarios. | pass |
| R31 | Shadow actual vs proposed/spend | Schema auto-service/catalog, browser and classifier empty-pool Shadow acceptance. | pass |
| R32 | Enterprise RBAC/source ACL | Installed Enterprise access/source/resume/evaluation modules, scoped-only global denial and original sources. | pass |
| R33 | Audit provenance/export/signature | Installed Enterprise audit/licence/evaluation acceptance, existing audit-chain/edition tests and old-hash compatibility. | pass |
| R34 | Injected registry/restrictions | Core provider seam and strict target, actual Enterprise key provider/inference policy. No test requires a physical HSM. | pass |
| R35 | Off/reenable and constraints retained | Real Core/ML/product-browser stack: cloud default rejected under local-only, compatible Off default, Active reenable, caller gate disabled. Existing browser/API/job fixtures retain adoption. | pass |
| R36 | Fixed service/index provenance | Fixed/index/voice/PCM cases and ML fixed/stage tests; hard unknown accounting is refused, index never silently promoted. | pass |
| R37 | Empty/singleton pool | Classifier root and engine pool tests, real browser singleton path and frozen replay unknown/empty scenarios. | pass |
| R38 | Missing DB is not a skip | Shared queue validates loopback `routing_` DB before execution; every integration root independently rejects a missing/wrong DB. Durable unit ignore is reported then executed in its dedicated DB step. | pass |

## Reproduction

PowerShell `test.ps1` and POSIX `bash test.sh` invoke the same `checks.py` queue.
Core-only `all` covers protocol, all-target build, standalone source snapshot,
routing/research units, schema upgrade and every supported caller fixture,
classifier/fallback/durable/PCM, routed ML/native adapters, frozen replay,
frontend units/build and selected browser/real-stack scenarios. Explicit
`-WithEnterprise` / `--with-enterprise` adds the real extension build/library,
consumer/moderation/licence/audit/edition, ML overlay and frontend overlay.
Core and Enterprise frontend builds run sequentially because they share dist.

`native` separately checks the desktop crate with the installed platform
prerequisites. A missing native library/cache is a failed prerequisite, not a
passing skip. `native-release` builds an unsigned installer when native client
or connectivity changes; it never installs or publishes it. Native PDF rendering,
physical HSM/carrier and paid live quality are separate scope checks; no production
key is requested by routing tests. Do not infer these checks from remote-text/PCM mocks.

Logs and `checks.json` identify command results and counts. Use `-Only <name>` /
`--only <name>` after a failure; do not restart the full queue after a fixture
repair. A source change affecting another queue item requires that item too.
The status column records the observed run above. A future matrix remains pending
for any entry whose required command has not passed; source links alone never
establish a successful run.

Real accuracy, calibrated confidence bounds, cache-specific invoice accounting
and financial savings remain `not_run`/insufficient evidence. The bounded text
live runner is available separately after endpoint/budget authorisation. Its
existence does not activate policies or satisfy a quality-preservation gate.
