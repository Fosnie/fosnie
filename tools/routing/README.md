# Model routing development

Routing is a Core feature. Enterprise consumes the same implementation through Core's
public extension interfaces. Keep automatic selection disabled until its contracts
and deployment-specific model profiles have been validated.

Release checks and evaluation commands are available below. An unavailable
required dependency is a failed prerequisite, never a passing skipped test.

From the Core checkout, in PowerShell:

```powershell
./tools/routing/setup.ps1 -RunIntegration
./tools/routing/test.ps1 -Lane fast
./tools/routing/test.ps1 -Lane ml
./tools/routing/test.ps1 -Lane classifier
./tools/routing/test.ps1 -Lane eval-offline
./tools/routing/test.ps1 -Lane fallback
./tools/routing/test.ps1 -Lane enterprise
./tools/routing/test.ps1 -Lane enterprise-acceptance
./tools/routing/test.ps1 -Lane all -WithEnterprise -Output tools/routing/reports/release
./tools/routing/test.ps1 -Lane e2e
./tools/routing/test.ps1 -Lane native
./tools/routing/test.ps1 -Lane native-release
```

Setup requires Docker Desktop and `cargo sqlx`. It starts the dedicated routing
Compose project on loopback ports 15439/16389 and migrates its disposable database.
It never uses the caller's `DATABASE_URL` for migrations. PostgreSQL data is tmpfs;
run setup again after stopping the test container. Tests use synthetic mock
inference only. See `docs/model-routing.md` for current implementation limitations.

`classifier` covers strict nested rule/config CRUD, executor pools, cloud-only
and local judge assignments, multilingual and shortened continuation input,
uncertainty/schema/tool/report errors, timeout/cancellation and the existing
management probe and technical fallback fixtures. All providers are synthetic;
these contracts do not measure live classifier quality. Settings browser coverage:
`cd frontend; npx playwright test -c playwright.document-jobs.config.ts routing-settings.spec.ts`.

`eval-offline` builds the production-engine JSONL adapter and checks/replays 420
frozen synthetic cases with six comparisons and explicit unknown metrics. It
needs Python/Cargo but no database, network or models. Live evaluation is an
explicit separate mode with server-side request caps; it stays `not_run` without
authorized configuration. See [evaluation contracts](../../docs/model-routing-evaluation.md).

`fallback` uses that isolated database for synthetic stateless HTTP acceptance
plus routing library and ML adapter/lease tests. It covers one technical retry,
original constraints/effort, shared spend, classifier count, stream boundaries,
credential/alias revocation and audit metadata. It makes no live model calls.

`enterprise-acceptance` requires the sibling Enterprise checkout and
`FOSNIE_ROUTING_TEST_DATABASE_URL` naming the dedicated `routing_` database. It runs
the real extension's library/licence tests, existing audit/edition regressions,
all-targets build and isolated routing consumer fixture:
global/scoped management, BYOK, injected inference restrictions, source revocation
on queued jobs and signed export verification. The extension's provider policies
can only narrow Core routing; they do not introduce a routing licence capability.

PowerShell and `bash tools/routing/test.sh <lane>` share `checks.py`. `all` is
Core-only unless `-WithEnterprise` / `--with-enterprise` is supplied. `-List` /
`--list` prints the queue without execution; `-Only <name>` / `--only <name>`
repeats only an affected check. Prerequisites fail before other commands run.
`-StartAt <name>` / `--start-at <name>` continues the untouched queue remainder;
targeted/continued runs merge receipts by check name instead of discarding earlier
passes. Re-run every check affected by a source change explicitly.
Logs/`checks.json` record actual command and Rust test counts, including ignored
tests separately. Runtime credentials and database URLs are not command arguments.

`e2e` includes a real loopback Rust HTTP/WS server, real Core ML process and the
production browser composer communicating through real transport with synthetic
upstream providers. It uses the existing Python/Node/Playwright environments,
strict Vite port 5189 and an isolated schema cleaned on completion. Its injected
test identity exists only in that test executable; deployed authentication is
unchanged. The test proves saved Auto/actual metadata/reload, policy Off and
reenable preserving local-only, and independent caller-gate refusal.
Set `FOSNIE_ROUTING_PYTHON` for a different existing Python runtime and `PW_CHANNEL`
for an installed Playwright browser. Linux defaults to Chromium, Windows to Edge.
The existing mocked product form tests remain a separate browser command.

`native` checks desktop compilation; `native-release` builds the unsigned
installer when native connectivity/client code changes. The latter needs the
existing desktop lockfile's project-local CLI and platform bundler tools. It
builds Core frontend assets, excludes updater signing keys and uses Cargo offline.
It never installs, signs or publishes the generated installer. Frontend builds
and this lane must run sequentially because they share the same dist directory.

See [release scope](../../docs/model-routing-release.md) and the
[R01–R38 matrix](../../docs/model-routing-release-matrix.md). No queue launches
paid inference, publishes a release or activates a production setting.


## Optional live-search dependency

`compose.live-web.yml` adds a disposable SearXNG service on loopback 18888 to the
existing dedicated routing project. It is absent from the ordinary check queue
and starts only with the `live-web` profile. Its configuration keeps a small
engine basket, enables JSON, disables the private instance's limiter, sets short
outgoing timeouts and stores cache data in tmpfs. No API key is needed.

From the Core repository root:

```powershell
docker compose -p fosnie-routing-dev -f tools/routing/compose.yml -f tools/routing/compose.live-web.yml --profile live-web up -d --no-deps searxng
```

Use `http://127.0.0.1:18888` as `SEARXNG_BASE_URL` only in the isolated ML child
process. Do not change user/global/production configuration. Check that 18888 is
unused before starting; a healthy `/healthz` does not prove that engines return
results. Exercise one bounded, public, fixed query first. Do not send private
conversation text during the dependency preflight. Search engines can fan out
inside SearXNG; an HTTP search count is not an upstream-engine request count.

The image defaults to the same official `searxng/searxng:latest` used by the
existing dev deployment. Set `FOSNIE_ROUTING_SEARXNG_IMAGE` to a verified tag or
digest for a frozen run, and record the actual image digest in its receipt. The
5 October pilot used
`searxng/searxng@sha256:76b0bf285aca014c7191fc4d9234c4bfb358624ac33d8883833d496c059ec072`.
See [official container installation](https://docs.searxng.org/admin/installation-docker.html)
and [settings inheritance](https://docs.searxng.org/admin/settings/settings.html).

After validating ownership by Compose project/service labels, remove only this
temporary dependency; the routing PostgreSQL/Redis services remain available:

```powershell
docker compose -p fosnie-routing-dev -f tools/routing/compose.yml -f tools/routing/compose.live-web.yml --profile live-web rm --stop --force searxng
```

The preflight validates SearXNG, not the production routed chat tool boundary,
web pipeline, fixed reranker/OCR profiles, shared stage accounting, citations or
final answer. Those require a separate actual Core/ML dialogue with the original
authenticated caller, tool authorisation, isolated settings and one shared run.
Do not remove a source/permission check or inject default models to make it pass.

## Optional existing-GGUF reranker and authenticated web example

`compose.live-rerank.yml` adds the official CPU llama.cpp server on loopback
18091. Set `FOSNIE_ROUTING_RERANK_GGUF` to a verified existing Qwen3-Reranker
GGUF; the file is mounted read-only and no model is downloaded. The optional
container uses one slot, context/batch/ubatch 2048, four CPUs and 2 GiB. Its
healthcheck targets the configured port 8091. This is a short HTML-chunk pilot
configuration, not a verified capacity for long documents or PDFs. Freeze the
image with `FOSNIE_ROUTING_RERANK_IMAGE` and record its actual digest. The measured
5 October image was
`ghcr.io/ggml-org/llama.cpp@sha256:559ac229adefe0f7e2d4e32f5222f26927b6bb8ba44b9db08e2544afebf41984`.
See the [official server documentation](https://github.com/ggml-org/llama.cpp/tree/master/tools/server).

```powershell
docker compose -p fosnie-routing-dev -f tools/routing/compose.yml -f tools/routing/compose.live-web.yml -f tools/routing/compose.live-rerank.yml --profile live-web up -d --no-deps searxng reranker
```

The operator-only `routing_pilot` example accepts an optional `web_pilot` object
with loopback `rerank_url` and `ocr_url`. It creates fixed profiles and an owner
chat/agent, enables the browser/web connector in its disposable schema, and uses
the genuine Core HTTP ticket and browser WebSocket handlers. The bearer token is
required for protected routes; ticket redemption re-loads the original user.
Its extra owner-only receipt endpoint exists only in the example executable.
All deployment-default changes are confined to that isolated schema. A default
policy is a hard parent constraint: its allowed IDs must include the fixed roles
and its caps must permit their unknown usage. Do not relax real tenant caps.

The model fixture's optional `context_tokens` defaults to 8192, with an allowed
range 2048–32768. Match this declaration to the actual model server configuration
and confirm it from its runtime API; it is not an automatic model benchmark.
Two completed answers do not prove successful search. Receipts must establish
native web invocation, real fixed rerank, source citations, actual target/effort,
saved routing intent, shared per-turn accounting and the original caller checks.
Explicit search arguments diagnose infrastructure separately from model quality.
Do not promote this smoke or AI-authored seeds into reviewed learned evidence.

After checking Compose ownership, remove only the optional dependencies:

```powershell
docker compose -p fosnie-routing-dev -f tools/routing/compose.yml -f tools/routing/compose.live-web.yml -f tools/routing/compose.live-rerank.yml --profile live-web rm --stop --force searxng reranker
```

Temporary Core/ML/Ollama processes and private schemas must also be cleaned.
Keep the user-owned Ollama and routing PostgreSQL/Redis running. No ordinary
test queue starts these services or contacts paid providers.


## Bounded quick-search comparison

The V6 comparison repeats the frozen V5 public documentation question and
follow-up through the same authenticated ticket/WebSocket fixture. Keep both
raw receipts, image/model digests, hardware settings, actual inference counts,
source citations and cleanup evidence. The optional reranker and temporary
Ollama must not overlap heavy compilation. No cloud keys are needed: cloud
profiles point at a forbidden loopback sentinel, and the durable cloud ledger
must stay unchanged. Do not use caches or promote this fixed smoke into judge
or answer-quality evidence. Comparing one run with an earlier run measures an
observed latency difference, not a general benchmark or independently reviewed
quality result. Quick-search excerpts intentionally have partial coverage.


## Public judge cohorts and blind review

See [frozen judge data](../../docs/model-routing-judge-review.md) for the explicit
revision-pinned BBH download, published answer bank and offline review page.
`judge_review.py prepare/finalise/report` freezes dataset/family/exposure hashes,
rejects incomplete or foreign reviews and never calls models or activates a policy.
`eval-test` now includes its offline contract tests. No ordinary test command
downloads benchmarks. Language variants count as one independent family in
judge diagnostics; public examples or human label review alone do not establish
P12 live model quality.

See the [separate instruction following evaluator](../../docs/model-routing-instruction-following.md) for revision-bound constraints, unknown handling and frozen public cohorts. This offline lane does not admit P12 quality or activate Auto.


## Bounded first-shipment acceptance

`./tools/routing/test.ps1 -Lane acceptance -WithEnterprise` (or
`bash tools/routing/test.sh acceptance --with-enterprise`) runs the shared ten-check
queue for protocol, routing library, classifier/fallback, standalone judge probe,
ML adapters, frontend fields/types, settings/chat browsers, actual Core/ML/browser
stack and the actual Enterprise consumer. Use an isolated `routing_` PostgreSQL and
Redis as above. It makes no external model API calls and starts no real model server.
Without the Enterprise switch the queue is Core-only. See
[first-shipment acceptance](../../docs/model-routing-acceptance.md) for scope and
the separate independent P12 workstream; do not automatically resume quality pilots.
