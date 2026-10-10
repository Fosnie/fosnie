# Model routing in Core

The catalog, policy API, rules/classifier/hybrid strategies, shared settings,
supported chat callers and opt-in API alias are implemented behind explicit
default-off gates. See [onboarding and release scope](model-routing-release.md)
for local/cloud/mixed setup, off switches, supported callers and accounting limits.
The [evaluation harness](model-routing-evaluation.md) provides reproducible
synthetic replay and separately budgeted text evaluation. Independent deployment-specific quality and financial savings remain unestablished.
See [first-shipment acceptance](model-routing-acceptance.md) for the completed contract scope.
Paired [desktop remote routing](model-routing-desktop.md) now shares the text
composer and regeneration path with original device-key checks.

Routing is Apache-2.0 Core functionality. Enterprise can narrow inference permissions
through `InferencePolicy`; no Enterprise package or licence is required by Core.

## Selection and strategy

`default`, `manual` and `auto` describe the user's selection. Within Auto,
`rules`, `classifier` and `hybrid` describe how requirements are determined.
`economy`, `balanced` and `quality` change preferences only among eligible models.
Missing selection inherits the chat's stored choice. An old null provider UUID is
not a request to reset Auto. Explicit conflicting new and legacy selections fail.

The classifier returns task requirements, never a provider name or endpoint.
The catalog records capabilities, operator declarations, evaluation provenance,
locality and optional prices. Model names are not quality rankings. Unknown price
is not zero. Local inference may still use separately permitted web search tools.
Locality needs administrator attestation to satisfy `local_only`; personal BYOK
profiles cannot grant themselves that attestation.

## Current API

Endpoints require Fosnie authentication. Shared writes use `providers.manage`
through the RBAC seam. Personal profile writes require ownership and enabled BYOK.
No endpoint returns stored credentials.

| Endpoint | Operation |
|---|---|
| `GET /api/routing/catalog` | Visible provider metadata and optional profiles |
| `GET /api/routing/management` | Effective management capabilities |
| `PUT /api/routing/profiles/{id}` | Save profile with `expected_revision` |
| `DELETE /api/routing/profiles/{id}?expected_revision=...` | Remove profile |
| `POST /api/routing/profiles/{id}/probe` | Explicit synthetic compatibility check |
| `GET /api/routing/policies` | Visible deployment and personal policies |
| `PUT /api/routing/policies/{id}` | Create/update policy with caller-generated UUID |
| `DELETE /api/routing/policies/{id}?expected_revision=...` | Remove policy |
| `POST /api/routing/preview` | Preview without inference |
| `POST /api/routing/classifier-probe` | Explicit one-call, budgeted judge test |
| `POST /api/routing/evaluate` | Manager-owned capped text evaluation, no tools or replay |

Creation uses `expected_revision: 0`. Updates/deletion use the last returned revision.
Revisions are opaque increasing generations, not necessarily consecutive numbers.
Delete/recreate cannot make an old execution plan current again. Endpoint/model/key
edits also invalidate earlier profile evidence.

Preview receives declared requirements. It reports `classifier_required` instead
of invoking the judge. `decision.actual` stays unset until dispatch. Shadow keeps
the proposed target separate from the default planned for execution; an ineligible
proposal can be null.

The probe accepts `connectivity`, `tool_call` or `json_response`, an expected
profile revision and optional cost cap. It performs one synthetic request, does
not execute returned tools and does not evaluate answer quality or attest locality.

See [Routing settings](model-routing-settings.md) for deployment/personal forms,
CAS conflict handling, fixed-service provenance, rollout controls and the
distinction between a non-inferencing preview and explicit bounded tests.

## Internal contracts

### Fixed inference roles

`GET /api/routing/fixed-catalog` lists the visible embedding, rerank, OCR, STT,
TTS and verification configurations. `PUT`/`DELETE
/api/routing/fixed-profiles/{provider_id}` manages their separate profiles with
revision checks. Shared profiles and trusted locality require `providers.manage`;
personal OCR/STT/TTS profiles require ownership and enabled BYOK. Personal profiles
cannot self-attest locality. These roles can appear in a policy's allowlist but
cannot become its primary or classifier LLM.

The fixed resolver checks the current role, ownership, enabled state, profile,
locality and policy before using the injected registry. Exact fixed-provider and
active-index registry methods default to an explicit unsupported error, so custom
registries must opt in. No fallback to a different row or process environment is
allowed on this path.

Migration 0117 gives the active embedding index its own routing revision. Active
model, endpoint, encrypted key, dimension or collection changes invalidate its
profile; desired-model changes and reindex progress do not. An embedding profile
references the active index revision and model when an index exists. Resolution
uses that index's credentials through the registry, preserving the live vectors'
provenance. It never promotes the desired model or triggers reindexing.

Migration 0118 adds independent `embed_desired` and `verify_claims` stages beside
the existing `embed` and `verify` profiles. Catalog identity is now
`(provider_id, role)`; PUT selects the stage through `profile.role`, and DELETE
accepts `?role=embed_desired` or `?role=verify_claims` with `expected_revision`.
Omitting the DELETE role keeps the original provider role. Existing profile
contents/revisions survive the migration unchanged. Only provider managers can
attest these deployment-wide stages.

Desired embedding model/endpoint/key/dimension/collection edits increment a
separate `desired_routing_revision`; progress updates change neither index
revision. `resolve_desired_embed` is an explicit injected-registry method with
no active-index/DB fallback. Its target carries `desired_index_revision`, while
active targets carry `active_index_revision`. Each has its own model, key and
collection. Resolving or attesting a target never promotes it.

`ExecutionPlan.fixed_assignments` persists references without endpoint URLs or
keys. `fixed_lease::pin` adds verified stages atomically and revalidates existing
pins instead of replacing stale ones. `fixed_lease::prepare` uses these saved
references under the original actor; missing or changed pins require replanning.
Supported queued document, research and workflow callers persist this contract;
their current source/actor checks are described below. Unadopted callers cannot
consume a routed conversation through a legacy path.

The opt-in strict resolver checks ownership, BYOK, policy and revisions before
credential access through the injected registry. A complete endpoint, model and
credential travel together. An absent key is explicitly empty, never inherited
from another ML environment target.

`InferenceEnvelope` travels through the existing overrides channel. Python installs
each purpose's target in a scoped context, including native Anthropic/Gemini and
OpenAI-compatible/Responses adapters. Concurrent requests and cancellation restore
the previous context. Strict calls do not silently drop tools/effort or switch
adapters after rejection. Internal retries are disabled for these bounded calls.

The classifier makes at most one bounded call. Invalid output uses a checked
default; terminal hybrid rules skip the classifier. Rules mode makes zero classifier
calls. Shared budget reservations prevent concurrent overspending; unknown usage
keeps its reservation. Every chat/helper/background dispatch still needs to be
connected to this ledger before release.

`model_routing::dispatch` now prepares and executes a planned non-streaming or
streaming LLM request. It rechecks actor identity, policy/profile/provider revisions
and capabilities, resolves only the requested purposes, and intersects current
extension restrictions. The ordinary chat orchestrator has not adopted it yet.

Each ML request receives a bounded budget grant: a deadline and per-purpose call,
input and output allowances. Core reserves the complete fan-out atomically. ML
shares one grant across child tasks and checks each call before HTTP dispatch.
Terminal reports seal unused slots against late background calls. Core validates
grant/decision/target IDs, revisions, distinct call IDs and usage arithmetic before
refunding unused tokens or slots. Missing/partial/invalid reports retain reserves;
provider overruns stop subsequent calls. Unknown prices have an explicit counter
and cannot be treated as zero when a monetary cap is applied later.

Streaming EOF after partial output is an error; it never starts a replacement
answer. Cancellation and deadline expiry close the stream and keep unconfirmed
spend reserved. Generic dispatch performs no automatic retry. The stateless
public text API has the explicitly bounded safe-point retry described below.

Current grant limitations: text input uses conservative UTF-8 byte accounting
with a template margin; bounded multimodal calls are rejected until adapter-specific
accounting is implemented. Monetary reconciliation retains conservative estimates
for executed calls; it does not claim to know actual native cache pricing.
Fixed-service pricing and propagation through all product entry points remain
release gates.

Fixed-service transport now has separate `fixed_assignments` and `fixed_budget`
fields in the opt-in inference envelope. `model_routing::fixed_lease::prepare`
rechecks the actor, pinned references and current policy before issuing a grant.
Fixed grants share the LLM run ledger, but bound HTTP attempts, serialized request
body bytes, decoded response bytes and elapsed time. Bytes are not token counts.
Unknown token/cost counters remain explicit, so fixed work currently rejects any
hard token or monetary cap; a later cap cannot treat earlier unknown spend as zero.
Only a complete, validated report refunds unused slots. Lost reports retain them.

ML installs complete endpoint/model/key assignments for embeddings, reranking,
OCR, STT, TTS and verification. Missing assignments or grants fail before model
HTTP, including fixed calls from a request containing only LLM assignments.
Routed calls do not inherit environment credentials, follow redirects, retry
inside the adapter or turn a verifier/reranker failure into an empty success.
Legacy requests keep their existing retry/degradation behaviour. Cancellation
also terminates STT's local transcoding child. Verification batches consume one
slot per HTTP request, and oversized responses close their stream and exhaust
the fixed grant.

Terminal JSON/NDJSON responses carry `fixed_routing_report`. `/speech` preserves
the audio body and sends the report in `X-Fosnie-Fixed-Routing-Report`;
`/transcribe` returns it in JSON. Reports contain IDs/revisions, byte counts,
status and latency, without inputs or keys. Invalid audio override headers are
rejected instead of becoming an unconfigured legacy request. The reference
verifier now validates opt-in `routing_model` and returns its actual primary
model: live verification uses `VERIFY_MODEL`, claims use `VERIFY_FACTCG_MODEL`.
Strict clients reject old sidecars that do not acknowledge that identity.
The `verify` and `verify_claims` profiles pin their respective primary models
independently at the configured verifier endpoint. Claims never inherit the live
model assignment; each HTTP batch consumes its own claims allowance. Local
secondary NLI/HHEM work remains inside the fixed-service HTTP attempt accounting.

Strict `/reindex-embeddings` accepts the routing envelope through `overrides`,
checks active source and desired destination before collection creation, embeds
only with `embed_desired`, and reports usage on its terminal `built` event. It
does not swap aliases. Strict `embed_with` requires explicit fields matching its
desired assignment. Dual-write ingestion validates those fields before extraction,
checks dimensions and source alias, and writes to pinned physical collections.
An alias moving mid-ingest cannot place old-model vectors in the new collection.
`/ingest` returns fixed reports. Legacy reindex/ingest paths retain their behaviour.

`model_routing::mixed::prepare` resolves a pipeline's LLM and fixed stages together,
rechecks provider/profile/index/policy revisions and applies all purpose-specific
edition restrictions. One atomic reservation covers both lanes: rejection of a
fixed allowance cannot leave the LLM lane charged. Read-only fixed references can
carry active-index source provenance without granting an embedding call. Plans
must already contain those pins; preparation never silently selects a new index.

`model_routing::transport::PipelineClient` is the opt-in Core consumer for mixed
JSON, NDJSON and voice responses. Construct it once per ML service with
`PipelineClient::from_state`, then pass the prepared request to `json`, `stream`,
`transcribe` or `speech`. JSON operations cover retrieve, web search, ingest,
verification, embedding dimension, document reading and chat-step; streams cover
retrieve, web search, research, generation and desired-index builds. Unsupported
endpoints still need their own strict contract before adoption. The transport
installs the routing envelope over caller overrides and preserves other settings.
It uses the configured ML shared secret, rejects redirects, bounds response bodies
and stream lines, and closes on cancellation/deadline/error. It never retries.

Both reports are validated before any unused reservation is released, including
cross-lane call-ID uniqueness. Missing/incomplete/invalid reports retain the
entire mixed reservation. Known overruns exhaust the run even in a partial
report. EOF without the expected `done`/`built` event is an error. A terminal
error can reconcile complete accounting but still fails the operation. Speech
reconciles its header report only after the complete audio body arrives.
Invalid payloads or cancellation after preparation conservatively retain the
issued reservation; callers should validate inputs before preparing the grant.

Public chat/voice entry points still do not activate Auto. Internal chat callers
can now pass a prepared runtime with `TurnContext::with_routing` to `run_turn`.
`model_routing::run::RoutedRun` now owns the original authenticated context, an
immutable plan, the same budget used by the classifier, a reusable pipeline
client and sticky cancellation. Clones and `spawn` children share those objects;
they do not rebuild an actor from a user ID or depend on Tokio task locals.
`drive` bridges an existing turn Notify into broadcast cancellation; every active
helper and late child sees cancellation. Deadline and exhausted-budget checks
cover preparation as well as inference. The runtime is deliberately not
deserializable: loading a saved plan must not reset already consumed work on job
resume. Primary/tool/final assignments must remain pinned to the same reference.

Opt-in chat helpers are in `chat::routing`: `prepare` authorizes the chat and reads
its real project/history before invoking the classifier; `spawn_title` passes the
full runtime to the background title task; `history` routes compaction through
`utility_summary`. Both utilities use the saved effort and reconcile reports via
the shared mixed transport. Chat authorization and project scope are checked
again before writes. Title writes preserve a concurrent manual rename. Routed
compaction propagates inference, policy, budget and database failures; the legacy
entry point retains its prior best-effort behaviour.

The opt-in `run_turn` path currently supports existing web chats, including agents
whose selected tools are `web_search` and/or `read_document`, without attachments,
workspace, automation, regeneration or custom reasoning/
provider overrides. It checks the authenticated actor/role, turn and chat against
the runtime before writes, then rechecks the actual project and chat access.
Unsupported paths fail explicitly: they never fall through to legacy inference.
General tool dispatch remains a migration gate. Live groundedness supports unified
library-only answers with model search off and no selected web/document tools.
Other groundedness paths require adoption. The no-op Core moderation hook and the
Enterprise moderation hook support the routed contract; other extensions default
to refusing routed execution until they explicitly adopt it.

Enterprise post-answer moderation now uses the same `RoutedRun`. The optional
`utility_assignments.moderation` policy entry pins its provider independently of
the answering model; without an assignment it inherits the primary target. Local
and cloud targets remain subject to the original actor, provider access and
inference restrictions, including `local_only`. Core does not depend on the
Enterprise classifier: `/classify-prompt` remains an Enterprise ML overlay route.
Deploy matching Core/Enterprise ML versions to use the new `moderation` purpose;
older ML versions reject it and no legacy call is substituted.

The callback runs only after a successful answer. It requires the existing
enabled/notice/basis settings and the moderation entitlement, revalidates chat
access and project binding, and binds the prompt to the persisted user message
and completed assistant in the same turn/decision. Work-hours scoring uses the
user message timestamp. The existing cheap gate, structural scoring, threshold
and 2000-character excerpt are preserved. A single strict classifier call reserves
16384 conservative input tokens and 128 output tokens on the original ledger;
the whole callback has a 30-second ceiling and shares parent cancellation.
Malformed classifications, invalid or missing reports, provider/configuration
failures and cancellation do not become neutral successful classifications.

After inference the callback refreshes access, entitlement and a coherent
configuration snapshot. Row locks recheck prompt/turn identity and serialize
duplicate callbacks. A flag and its Enterprise audit (signed when configured) commit in one
transaction. The audit records the original actor role, target and shared usage
snapshot, without the prompt or credentials. Existing flags skip repeat inference;
concurrent callbacks can both spend but persist one flag/audit. Below-threshold
callbacks save no marker, so repeating one can spend again within the same ledger.
This is an in-process child, not durable resume or final billing. Moderator
visibility and the legacy non-routed behavior are unchanged.

Run `tools/routing/test.ps1 -Lane enterprise-routing` with the isolated
`FOSNIE_ROUTING_TEST_DATABASE_URL` to test the real Enterprise hook, signed audit,
chat handoff and ML overlay. Missing prerequisites fail explicitly.

Attached readable libraries now use strict retrieval. This supports
`rag.model_search_mode=off|gaps_only|always`, unified synthesis and an explicit
`rag.rerank_enabled` boolean. Per-part synthesis is supported with model search off
and no selected web/document tools; dynamic tool evidence still needs attribution
to individual parts. Unknown synthesis modes are refused. These gates are checked during preparation and again
before dispatch; the feature settings are not silently overridden. Preparation
checks KB/source authorization and pins the active embedding index and, when
enabled, reranker. Dispatch reauthorizes the chat, intersects the requested KBs
with current access and merges the current source-document deny-list. It never
uses the desired embedding index for retrieval.

Model-driven `search_library` uses the same runtime in both the pre-stream loop
and midstream continuation (`rag.model_search_locus=loop|midstream`). When model
search is enabled, preparation requires declared or verified tool capability on
the selected profile. Tool decisions use the pinned ToolLoop target/effort, at
most 4096 output tokens and the remaining shared budget. The loop is bounded by
the agent step limit (at most 32), tool batches by 32, and actual searches by the
existing per-turn search limit. Every dispatch obtains the normal authorization
witness and rechecks chat/library/source access and fixed-index provenance.
The pre-stream batch must contain only advertised adopted tools (`search_library`,
`web_search`, `read_document`); streamed calls currently allow only `search_library`
and execute only after a validated terminal report. Invented tool names,
incomplete accounting and shared-budget failures stop the turn. Successful
tool-loop answers record the actual pinned model even without a final stream.
Legacy tools keep their existing path. Other configured tools remain gated until
their nested inference adopts the runtime. The implicit `generate_artefact`
baseline is not advertised. Its immediate non-agentic draft export now has an
explicit gated path described below; tool-agent approval/resume remains refused.

Each retrieval request reserves at most 8 decomposition/reformulation calls,
64 grading/subanswer/gap-check calls, 32 embedding attempts and, when enabled,
16 reranking attempts. Per-call input allowances are bounded by the pinned model's
context capacity minus output allowance; output ceilings are 2048/8192 for the
two LLM purposes, capped by model capacity. Fixed attempts have 1 MiB input and
8 MiB output ceilings. These are initial internal hard limits, not quality-tuned
estimates. They can conservatively refuse a low-budget run before HTTP. Fixed
token/cost accounting remains unknown, so strict monetary/total-token caps may
also refuse retrieval until proven bounds/pricing are implemented.

ML enforces the shared grants across fan-out and returns both accounting reports.
Only validated complete reports release unused reservations. Missing reports,
partial EOF, ACL failures and changed index provenance stop the turn instead of
silently generating without sources. Strict ML retrieval rethrows model/budget/
index errors that legacy retrieval treats as best-effort, and cancels and awaits
all sibling tasks on failure/cancellation. Legacy retrieval behaviour is retained.

`web_search::routed::search` now provides an explicit inline quick/standard web
executor for the next tool-loop migration. It receives the original runtime and
advertised tool set, reauthorizes the chat and native tool, checks the dormant
connector gate, and reads domain/robots restrictions without swallowing DB errors.
It requires pinned Rerank and OCR services (fetched PDFs can invoke OCR). One
request atomically reserves 8 WebPlan calls, 16 WebGrade calls, 32 rerank attempts
and 8 OCR attempts; the shared ledger refunds only validated unused work.
LLM output ceilings are 512/80 tokens, capped by model capacity; rerank input is
bounded to 1 MiB, OCR input to 8 MiB, and both outputs to 8 MiB per attempt.
Fetch count is capped at 8 and tightened by the caller's web budget. Deep jobs
are refused until durable actor/plan/accounting propagation exists. These are
conservative bounds; quick search still reserves the whole request initially.

Strict ML web search propagates planner/grader/reformulation/conflict/OCR errors,
and cancels and awaits failed fan-out siblings. It bypasses the page cache because
cached OCR text has no model provenance. Strict fetch/extraction failures stop the
request; legacy snippet salvage and caching remain available. A disconnected
stream cannot deadlock its producer on a full terminal/progress queue. This helper
returns validated evidence to its caller. The routed chat tool loop now invokes
it alongside dynamic `search_library`, fences web results as untrusted evidence,
persists citations directly against the assistant message, and records the actual
pinned model for a reused tool-loop answer. Web dependencies are pinned during
chat preparation. Native tool overrides are refreshed at dispatch; a change to
agent tools, sampling, prompt or execution limits stops the loop for replanning.

Web agents retain `agent_runs` and their Redis kill-token; routed document agents
also create these even though legacy read-only agents do not. A 250 ms watcher checks
revocation/expiry throughout the routed turn, including active HTTP waits; it begins
observing a row only after token creation succeeds. Failures close running agent
rows and release tokens, while explicit cancelled status is preserved. Step count,
tool messages and a ledger snapshot (including unknown costs/tokens) are recorded.
These checkpoints are explicitly non-resumable; they cannot restore a fresh
budget after a crash. An agent-level token_budget is currently refused because
fixed-service token bounds are unknown. Existing max_steps, sampling output cap,
wall-clock kill-token, deployment tool timeouts and web depth/fetch limits remain enforced. Public Auto and
durable deep jobs remain disabled; other document/MCP/custom/side-effect tools are still
outside the adopted set.

`tools::routed_document::read` is used by the routed chat tool loop and is also
available to explicit callers. It requires the original `RoutedRun`, an advertised
tool witness and a document UUID. It checks the live project link, personal KB
access and Enterprise source-document ACL before ML and again before returning
text. Tool overrides are also refreshed; ACL/database failures propagate. Model
arguments cannot supply a file path or provider. The path comes from the authorized
database row and remains subject to ML storage-root validation.

One mixed reservation grants at most 32 `document` LLM attempts (up to 512 output
tokens each) and one pinned OCR attempt (8 MiB input/output transport limits).
Unknown OCR token/cost bounds are not treated as zero. The ML file limit is 4 MiB;
returned text is capped in UTF-8 bytes at min(16000, one quarter of the pinned
tool-loop context capacity). Native text needs no model call and complete reports
refund all unused slots. Large text with a query uses at most 32 non-overlapping
sections, capped at min(120000, the deployment read_document_max_chars setting).
Every input/section/digest truncation is explicitly reported. This bounded read
must not be described as exhaustive when `truncated=true`.

Images and PDFs with scanned pages use the pinned OCR service; a mixed native/scanned
PDF cannot silently drop its scanned pages. Strict parser/OCR errors propagate.
The strict path never discovers the ML process's default model/context. Parent
cancellation or a failed map cancels and awaits outstanding inference children;
the non-streaming HTTP handler also watches client disconnect. Native parsers run
in a thread: an already running parser may finish locally after cancellation,
but that thread performs no model inference. Runtime/deployment deadlines bound
the request. Missing reports, cancellation or timeout keep unknown reservations.
Only configured agents selecting `read_document` and/or `web_search` (plus the
implicit hidden artefact baseline) are admitted. Document agents require a project
and enabled agents support; preparation requires tool capability and pins OCR.
The returned tool message includes the validated document UUID, mode, truncation
flag and text as untrusted evidence. Truncated reads explicitly warn against
claiming exhaustive coverage. Each completed tool result is checkpointed immediately,
so a failure later in a batch does not discard earlier completed reads. Checkpoints
remain non-resumable. Duplicate calls reuse the existing history without another
document request. Exhausting the library top-up allowance does not stop document
or web tools. Legacy document requests retain their path.

`chat::routed_parts` owns per-part RAG synthesis futures on the original runtime.
It validates all slices against the pinned context/output capacity before starting
generation. Each request retains the conversation and current user question while
receiving only its own retrieved evidence slice. Final model and effort stay pinned;
each part and truncation continuation draws from the same run ledger. There are
at most 16 parts, three concurrent workers and four continuations per part. Queued
work, continuations and ordered delivery share a total synthesis deadline; each
stream also has an idle timeout. Each worker channel holds at most 16 events,
with 64 KiB token-event and 256 KiB per-part answer/reasoning limits.

Output is delivered and saved in part order even when later parts finish first.
Workers are owned futures, not detached tasks: parent cancellation, timeout or any
worker failure drops all pending streams before returning. Completed reports
reconcile reservations normally; unfinished or missing reports retain unknown
spend. Tool calls, unexpected finish reasons, missing terminal reports and changed
response models fail the turn. Source access and configuration are checked again
before each part/continuation. ACL errors or a newly denied source stop remaining
work. Partial output is not marked successfully completed. This closes client-side
streams; it does not assert that an upstream provider can reverse already incurred
work. Legacy per-part behaviour is unchanged. An empty parts list uses unified
synthesis, as before.

`groundedness::routed::verify` uses the original runtime and the pinned fixed
`Verify` slot. It rechecks chat, KB and source-document access before ML and before
releasing results. A newly denied source, changed configuration or ACL/database
failure stops verification. The caller must supply the already-filtered retrieval
context and its source/deny-list snapshot. This executor does not itself persist
or publish results, and does not implement durable background-job recovery.

The routed chat now pins Verify during preparation when supported live
groundedness is enabled and libraries are attached. After a successful answer it
passes the original runtime, the persisted answer snapshot, the actual trimmed
evidence used for generation and the source deny-list to a verification child.
Reasoning is removed from the answer before verification. The child observes the
shared run cancellation/deadline and bridges the original cancellation signal.
This is an in-process child, not a resumable queued job.

`groundedness::routed::verify_message` validates message/chat/turn/decision identity
and unchanged content before inference and again under a row lock before writing.
It refreshes feature settings and source access before persistence. Verification
run, span verdicts (one batch), message summary and the audit event commit in one
transaction through the existing audit sink. A failed write rolls everything back.
Repeated callbacks with an existing summary do not call the verifier again; racing
callbacks may both incur inference but the row lock permits only one result/audit.
The live frame is sent only after commit; missed socket delivery can be recovered
from stored message history. Verification failures emit a metric/log and do not
turn the already completed answer into an error or publish a successful score.
The audit includes fixed-target provenance and a shared-usage snapshot, not a
final billing record. Per-part/tool-top-up grounding remains gated. Workspace
draft verification and repair have explicit internal APIs described below;
their bounded public document-job API and durable queue behavior are described below.

The whole operation has a 30-second ceiling, tightened by the remaining run
deadline. One fixed-service attempt reserves 8 MiB input and 2 MiB output transport
capacity; unknown token/cost bounds stay unknown. Evidence is limited to 1 MiB,
the question to 64 KiB and answer to 256 KiB, with no silent truncation. It requires
one completed inference report and the actual pinned model. Strict ML rejects
malformed/missing spans, overlapping ranges, invalid labels/confidences and text
that disagrees with character offsets. Rust independently validates the result,
including Unicode character positions and the score derived from flagged ranges.
At most 1024 spans are accepted. Invalid spans cannot be filtered away into a
fully grounded score. Strictness and confidence threshold are read without
swallowing configuration errors; HHEM second opinion remains refused until
separately adopted. Strict HTTP verification cancels and awaits model work on
client disconnect. Legacy verification retains its fail-open behavior.

Small memory recall uses SQL only. Compaction uses `utility_summary`; final
streaming and truncation continuations use the same pinned final target and
ledger. Context/output capacity comes from the current pinned profile, with an
explicit output allowance of at most 4096 tokens. No global model-info lookup or
default-provider resolution occurs. The legacy empty-output retry is disabled
on this path until retry eligibility/orchestration is implemented. A successful
dispatch records the routing decision and actual target on the assistant message.
EOF before a validated terminal report fails the turn and retains the reservation;
unfinished assistant rows are marked interrupted. A mismatched response model
cannot be reported as a successful routed answer. The existing chat evidence and
audit flow is retained. A placeholder title can be generated after the answer by
an explicit runtime child, preserving the shared budget and manual renames.

Legacy memory collections have no embedding-model provenance. Strict ML memory
search/upsert therefore reject before Qdrant/model access; a pinned KB embedding
index does not attest a memory collection. Legacy memory remains supported.
Attested memory-index migration is a prerequisite for routing large-memory recall.

Remaining gates include replacing legacy pipeline consumers at those entry points,
durable actor/plan/budget propagation into helpers and jobs, proven fixed-service
pricing/token bounds, and safe job resume/promotion after partially completed
dual writes. Active embeddings check their returned dimension without switching
the collection or index. Alias promotion and the Core reindex scheduler have
not adopted the strict plan yet; do not enable routed jobs until they do.

Usage records contain identifiers, purpose, status, latency and token metadata,
without prompts or credentials. Anthropic input includes separate cache reads/writes,
following its [cache documentation](https://platform.claude.com/docs/en/build-with-claude/prompt-caching).
Gemini thoughts count inside normalized output, following its
[UsageMetadata contract](https://ai.google.dev/api/generate-content#UsageMetadata).
Raw usage is retained for interpretation. Live quality/savings have not been measured.

## Routed draft verification and tracked repair

`groundedness::routed_draft::{verify_document,repair_document,execute}` now support
workspace documents on an existing `RoutedRun`. The caller pins Document plus
Embed, Rerank, OCR and VerifyClaims before dispatch. Claim extraction and repair
use the Document assignment; verification uses the independent fixed VerifyClaims
slot. Both phases spend the same ledger and share cancellation. The returned
verification handle retains the original runtime identity, version, file hash,
evidence permissions and claims. It cannot be deserialized into a fresh budget.

This implementation admits files up to 4 MiB, extracted text up to 16000 UTF-8
bytes, four decomposition sections and 16 claims. Oversized files/text/claim sets
fail explicitly. Empty claim sets retain an unknown score. Work is sequential
within each ML operation and bounded by a 120-second phase ceiling plus the
remaining run deadline. Strict parsing, complete usage reports and actual
VerifyClaims model acknowledgement are required. A verifier outage, malformed
reply or failed generation cannot become a successful score or deletion proposal.
HHEM and nonzero confidence filtering remain unsupported on this path.

Evidence searches use the pinned physical embedding collection, caller-readable
project libraries and the Enterprise source-document deny list. The pipeline
restores the deny-list context after completion or cancellation. Rust revalidates
the evidence document IDs against those libraries, project permissions, feature
settings, current document version and file hash before persistence.

Verification run, claims and audit commit atomically. Repair accepts only unique,
nonoverlapping source spans from that verified version. Rewrites must be verified
against their newly retrieved evidence; all changes remain pending tracked-change
proposals in the existing accept/reject workflow. The document version pointer,
new version row, pending edits, claim outcomes and audit commit together after
locking the document and claims. Changed/deleted documents, repeated repair,
invalid edit application or audit failure cannot publish a partial new version.
As with existing document CAS writes, an aborted file operation can leave an
unreferenced version file; database state stays unchanged. No success frame is
sent before commit. `execute` emits a terminal repair error if its second phase fails.

These internal runtime APIs also back the opt-in public document-job API below.
Generated artefacts use the verification-only path described below. Existing legacy requests retain their
existing path. The existing REST repair endpoint delegates durable public results
to the continuation below; other routed results are refused by the legacy worker,
preventing a new job from silently replacing the original actor, plan or ledger.
The separate browser composer contract is described in its capability document.

### Durable draft jobs

`groundedness::durable::enqueue` consumes exclusive ownership of a planned run
and enqueues one verification, optionally followed by tracked repair, through
the existing scheduler. Outstanding runtime clones or inference reservations
prevent handoff. Migration 0119 stores the actor, immutable plan references,
effective limits, charged usage, source snapshot and absolute deadline in
`routed_jobs`. The task payload contains only the job ID. Credentials are resolved
again at dispatch and are never serialized into this checkpoint.

The worker restores the original ledger, including classifier/earlier helper
spend, and validates that the user remains active with the original role.
Break-glass and enrolment-only principals cannot enqueue this work. Queue time
consumes the deadline; jobs without a shorter policy deadline expire within
24 hours. Existing target revision, injected policy, project/source access and
feature checks still apply. JSON operations serialize their reservation and
complete-report reconciliation around the actual HTTP call. A failed checkpoint
prevents dispatch; uncertain/cancelled requests retain their full reservation.
Durable streaming is explicitly unsupported.

A worker owns a database lease, refreshed every two seconds. Verification and
document-edit transactions lock and validate that lease before publishing.
Cancellation and expiry invalidate it. Duplicate delivery cannot acquire the
same running job, and delivery after success performs no inference or edits.
The scheduler reconciles task status from this authoritative job record.

Recovery distinguishes work that never entered the handler from work already
started. An unstarted claimed task can return to the queue with its original
plan, budget and deadline. An expired running lease becomes `interrupted` and
is not automatically replayed, even when a provider may have completed its
request. This is conservative recovery, not exactly-once provider execution or
resume of an interrupted phase. Successful verification uses the job ID, so
its committed result remains identifiable if repair or worker finalization fails.
Delayed repair of confirmed public verify-only jobs is supported below. Durable
document, research and workflow queue contracts are described later; arbitrary
chat/tool-loop replay remains unsupported. Existing legacy repair
cannot reconstruct a fresh runtime for these verification results.

### Delayed tracked repair

Migration 0120 adds a phase and a private, versioned verified-draft checkpoint.
The extracted document text, validated claims, exact spans and claim IDs are
stored in the same transaction as the verification result and audit event.
They are never included in task payloads or public status responses.

`POST /api/document-verification-jobs/{id}/repair` explicitly continues a
successful public verify-only job. The existing verification-run repair endpoint
uses this same path for durable jobs. The owner must retain the original role,
document write permission and access to all evidence; the launch/repair gates,
settings, source version/hash and original deadline must still permit the work.
A new task is queued atomically with the audit event, retaining the same job ID,
actor, plan, spent usage and absolute deadline. Waiting does not renew the budget.
The worker restores from the database, validates the checkpoint against persisted
claims, and rechecks source access and pinned targets before inference. It runs
repair and reverification only, without repeating decomposition or routing.

Repeated continuation requests for an already queued, running or succeeded
repair are idempotent. Scheduler delivery is fenced by the task ID as well as
the job ID, so an old verification delivery cannot claim the repair phase.
Failed, cancelled or interrupted phases cannot be replayed; combined
verify-and-repair jobs cannot acquire an additional repair phase. Existing jobs
without a verified checkpoint need a new verification. This continuation is
limited by the original deadline; it is not indefinite resumption of an old report.

### Public document-job API

The protected Core router exposes the following endpoints. This bounded path is
off by default (`routing.document_jobs_enabled` absent or false); no migration
enables it. It uses the existing groundedness entitlement and policy extensions.
It does not enable Auto in chat/composer or complete the routing release gates.

| Endpoint | Contract |
| --- | --- |
| `GET /api/routing/document-jobs` | Capability version, available selection modes and current enablement. |
| `PUT /api/routing/document-jobs` | `{ "enabled": true/false }`; requires `providers.manage`, validates and audits the global boolean setting. |
| `POST /api/document-verification-jobs` | `{ "document_id": "UUID", "selection": { "mode": "default" }, "repair": false }`; returns HTTP 202 with `job_id` and `status_url`. |
| `POST /api/artefact-verification-jobs` | `{ "artefact_id": "UUID", "selection": { "mode": "default" } }`; verify a generated Markdown, DOCX or PDF artefact. Uses the same status/cancel endpoints returned in `status_url`. |
| `GET /api/document-verification-jobs/{id}` | Owner-only status, selection, decision reasons, visible planned document target, conservative usage snapshot and available `result_id`. |
| `POST /api/document-verification-jobs/{id}/cancel` | Idempotent stop for the owner; available after project access, entitlement or the launch gate is revoked. |
| `POST /api/document-verification-jobs/{id}/repair` | Continue a completed verify-only job using its original plan and remaining budget; returns HTTP 202 with the same `job_id` and `status_url`. |

Selection is required and uses the shared `default`, `manual` (`provider_id`), or
`auto` (`objective`, optional `policy_id`) contract. Unknown fields/modes are
rejected. Default uses the configured routing-policy default; manual pins the
chosen accessible provider for document work. Neither calls the classifier.
Both retain hard constraints and the independent fixed-service assignments.
Auto uses the existing rules/classifier/hybrid planner: off executes the checked
default, shadow retains the proposal but executes the checked default, and active
uses the selected assignments. This does not change the legacy verify-draft API.

Before a possible classifier call, launch checks the active principal, project
and evidence permissions, file bounds, settings, fixed targets and DOCX repair
support. The classifier receives a bounded task description (verification,
optional repair and pipeline limits), not document content, filename or path.
The decision records `document_metadata_only`; this is not evidence that the
router assessed the document's semantic difficulty. Hard monetary/token caps
remain incompatible with fixed services whose bounds are unknown and are
rejected before inference. The source snapshot is checked again at handoff.

Disabling the launch gate stops public jobs before further inference or document
commit; previously committed results remain readable with current permissions.
Status never returns checkpoint context, source paths, credentials, lease owners
or private provider labels. `planned_document_target` describes the assignment,
and `usage` is labelled `conservative_snapshot`, not a final bill or proof that
all reserved calls ran. If verification committed before repair failed, its
`result_id` remains available. Read the result/export through the existing
verification-run endpoints. Routed report reads now recheck document access,
every referenced evidence library and Enterprise document denials, including
evidence from repairs; being the original creator does not bypass revocation.

Each successful launch POST creates a new job; repair POSTs are idempotent as
described above. Status includes `phase` (`verify` or `repair`). Launch retry
deduplication and direct tracked repair of generated artefacts are not provided by this API.
Interrupted phases keep the conservative recovery behavior above.

### Document viewer controls

When the document-job gate is enabled, the document viewer offers Default,
Manual and Auto in its verification panel. Manual lists accessible enabled
models with current routing profiles; the backend remains authoritative for
capabilities, policy and access. Auto uses the default routing policy with the
balanced objective. Its explanation distinguishes rules/judge selection from
off/shadow execution and does not claim to assess document difficulty. Fixed
evidence/verifier assignments remain independent.

The viewer can queue verification alone or verification with tracked repairs,
poll the job, cancel it, read a committed report after a later failure, and
request delayed repair. Terminal status refreshes document versions, proposed
edits and report data even if a WebSocket completion event was missed. The URL's
`verificationJob` parameter retains the job ID across reloads; it carries no
credentials or checkpoint content. Keep that URL to reopen a queued job. A job
history/list endpoint is not yet implemented. Source access failures hide stale
status/report data while keeping owner cancellation available. A selected old
document version cannot start verification or repair through these controls.

Admin → Providers exposes the existing permission-protected deployment toggle.
It remains off by default and does not enable chat/composer Auto. With a confirmed
disabled gate, the viewer retains legacy verification controls. A failed gate
lookup or routed launch does not silently fall back to legacy inference. Launches
are not automatically retried; an uncertain connection error explicitly states
that a job may already have started. Repair/cancel use their idempotent endpoints.

The isolated browser harness runs the production panel and request layer with
mocked HTTP (no model calls): `npx playwright test --config playwright.document-jobs.config.ts`
from `frontend`. It defaults to installed Edge; set `PW_CHANNEL=chromium` on hosts
with Playwright Chromium. The harness starts its own loopback Vite server on 5187.

### Generated artefact verification

The same default-off gate now advertises `target_types: ["document", "draft"]`.
An explicit artefact verification request creates its own selected plan and
durable ledger; all extraction, decomposition and evidence checks within that
job share it. This does not resume or renew the preceding chat-generation run.
The source snapshot binds the artefact ID, chat, path, MIME, file hash and the
chat's current readable knowledge-base allowlist and document denials. Files
must be nonempty, inside artefact storage and at most 4 MiB; the existing
16 KB extracted-text / 16-claim limits still apply. Unsupported kinds fail before
any classifier call. Artefact contents, names and paths are not classifier input.

The worker checks chat access, source metadata, file hash and evidence scope
again before execution and before publishing. Publication locks the artefact row
and commits the draft report, claims, checkpoint and audit atomically under the
same job lease. Result/export/status reads recheck current chat and evidence
access even for the original report creator. Cancellation remains owner-scoped.
Generated artefacts are not workspace document versions: neither direct nor
delayed tracked repair is accepted for these targets, including DOCX artefacts.

Status adds `target_type` and `target_id`. For document jobs, `document_id` keeps
its previous UUID value; it is null for artefacts. Existing stored source
checkpoints without `kind` default to document, so no database migration is
needed. The internal source field `doc` remains the target UUID for compatibility.

The artefact panel reuses the model-selection/status component for Markdown,
DOCX and PDF. The `artefactVerification=ARTEFACT_ID:JOB_ID` URL parameter scopes
the retained job to that artefact. Unsupported kinds retain their preview/download
behavior. Only a confirmed disabled gate offers the existing legacy Verify
action; gate errors do not trigger legacy inference. Chat/composer Auto remains
disabled pending the remaining caller and activation gates.

## Bounded Research runtime (internal adoption)

`research::routed::execute` accepts an existing `RoutedRun` and an explicit
file/web/hybrid request. It does not select models again, create a new budget, enqueue
work or publish a message. The original actor must still be active with the
same role and have chat access and access to every selected library. Core
applies the edition's document denials, snapshots the complete readable ready
corpus, confines files to storage and hashes their contents. It repeats these
checks before returning the final report; changed sources or permissions fail
the run. The caller must still fence the eventual publication transaction.

The file/hybrid contract accepts one to four documents, each at most 4 MiB and
16 KB of extracted text, with 32 KB of text across the corpus. Core accepts the
four built-in templates and bounded custom templates, with at most eight sections
and a 24 KB final report.
Plan/notes, section writing/coherence, rolling summaries and title generation
use their respective pinned purposes. OCR uses a fixed grant in the same run
ledger. Context and output allowances come from each purpose's pinned profile;
a small context produces a bounded error rather than a switch to another model.
No shared census cache is used. Citation identity and report structure are
validated; optional source grounding and evidence review are described below.

ML requires `research_limits: {"schema_version":1}`, source hashes and both
inference grants on routed `/deep_research` requests. It rejects sampling and
legacy runtime overrides before inference. Optional verification/review require
their additional bounded grants before any work begins.
Oversized/truncated sources, malformed outlines, unsupported citations, missing
writer completion, provider failure and budget exhaustion produce a terminal
error with accounting, never a successful partial report. Cancellation closes
the direct async iterator and active model response; there is no detached writer.
Requests without a routing envelope retain the existing Research path.

Durable `RoutedRun` streaming now checkpoints reservations before HTTP, holds
its serialization lock throughout the stream and checkpoints reconciled usage
before exposing a terminal event. Drop, cancellation, incomplete streams and
lost leases retain conservative persisted charges. Terminal delivery fails when
the worker loses its lease. These mechanics are covered in the explicit database
test lane; they do not themselves enable Research jobs in `routed_jobs`.

## Research jobs API

Migration 0121 adds `deep_research` to `routed_jobs`; existing document rows and
their repair checkpoints are unchanged. `routing.research_jobs_enabled` is a
separate boolean gate, absent/false by default. The document-job gate does not
authorize Research. Authorized provider administrators can inspect/update it
through `GET/PUT /api/routing/research-jobs`. Capabilities advertise files, web
and hybrid, the independent `web_enabled` connector state and the four-page cap.

`POST /api/research-jobs` takes an explicit selection and a bounded request:

```json
{
  "selection": {"mode": "auto", "objective": "balanced"},
  "request": {
    "question": "Compare the evidence in these documents",
    "source": "files",
    "kb_ids": ["LIBRARY_UUID"],
    "template": "freeform",
    "refinements": []
  }
}
```

`template` may also be a visible custom-template UUID when capabilities advertise
`custom_templates`. The backend resolves it using the original actor, without
accepting a client-supplied template body. It freezes the full runnable specification
in the source checkpoint before planning. Editing its executable fields, deleting
it or revoking access stops queued/running work; the final transaction locks its
row through publication. Descriptive fields that do not affect the runnable spec
do not change the snapshot. Archived templates remain runnable for Refine, matching
the existing template visibility rules. Foreign personal templates return 404.

Bounded custom templates support 1–8 fixed sections or free mode with an empty
skeleton. They require non-empty writing instructions, headings and briefs, unique
single-line headings and no expansion/executive-summary placeholder flags. Limits
are 8192 UTF-8 bytes for the serialized spec, 4096 for instructions, 512 for label,
160 per heading and 1024 per brief. The editor's broader template format is unchanged;
unsupported combinations fail preflight rather than silently changing semantics.
ML independently validates the same bounded subset before inference. Stage call
allowances use the actual fixed section count (eight for a free outline), while
model selection, shared ledger and absolute deadline are unchanged. Existing built-in
checkpoints need no migration: their absent template snapshot defaults to null.

`source` defaults to `files` for existing clients and persisted jobs. Web-only
requests require an empty `kb_ids` array; hybrid requires explicit libraries.
Web/hybrid preflight requires the separate web-search connector to be enabled
and a current fixed reranker profile as well as OCR. One primary-provider search
uses only the user's question, not document text, notes or generated queries.
At most eight search results are ranked once and four pages fetched sequentially.
Each page is limited to 4 MiB and 16 KB extracted text; the combined file/web text
cap remains 32 KB. All inference shares the original grant, ledger and deadline.
No SERP/page cache, search-provider fallback, rendered-page escalation, snippet-only
salvage or additional search rounds are used by this bounded collector. Page
fetches retain DNS pinning/SSRF checks and enforce domain policy at every redirect;
there are at most five redirect hops. Existing robots policy semantics apply.

Runtime web restrictions are captured in the job and rechecked during idle work
and before publication. Missing runtime restrictions retain the ML process's
configured defaults. Changing a runtime restriction or disabling egress cancels
further work; publication also holds a configuration table share lock through its
final authorization/commit, including against new restriction inserts. An in-flight
external request can run until the two-second monitor observes the change and
closes the ML request. Empty/failed/oversized web evidence fails the whole job;
hybrid never reports a files-only partial success. Reports must cite each selected
source kind. URLs come from fetched-source metadata, never model-written URLs;
Core validates citation membership, bounds and explicit domain restrictions.

Default and Manual pin all adopted Research child purposes to their selected
model without a judge call. Auto follows
the configured Rules/Classifier/Hybrid policy and rollout; its classifier input
is bounded task metadata, not source text or paths. Preflight checks the actor,
sources and fixed-service restrictions before planning. A successful launch
atomically creates a private research chat, task, immutable source/plan/budget
checkpoint and queue audit, returning `job_id`, `chat_id` and `status_url`.
The task payload contains only the job ID. No model credentials are persisted.
The selection belongs to this job; it does not activate Auto for chat follow-ups.

`GET /api/research-jobs/{id}` exposes owner-scoped status, selection, decision,
conservative usage and the final `message_id`. It rechecks the original actor
and source access and does not expose paths, hashes, credentials or lease state.
`POST /api/research-jobs/{id}/cancel` remains available to the owner after gate
or source access revocation. Other actors cannot read or cancel the job.

### Optional clarification inside the Research run

Migration 0122 adds `awaiting_input` for Research jobs only. Launch accepts an
optional top-level `clarify: true`; absent/false preserves existing launches and
stored jobs. Capabilities advertise `clarification: true`. The UI offers an opt-in
checkbox. Clarification uses one call to the already pinned `ResearchPlan` model
and the original ledger, before evidence collection. It is separate from the
routing judge: it does not choose or change models. ML `/research/routed-triage`
requires an explicit one-call grant and uses no legacy fail-open fallback.

The reviewer receives the question, source kind, selected library count and
existing refinements, without document contents, paths or library names. It
returns either no questions (continue immediately) or up to three prompts with
up to four optional suggestions each. Core and ML validate the shape and byte
limits independently. Incomplete output, tool calls, invalid questions or missing
accounting fail the job; they never mean "no clarification needed".

A waiting checkpoint is committed with its audit and completion of the triage
queue task under the job lease. It publishes no report. Status includes the
original RFC3339 `deadline` and, only while waiting, a `clarification` object with
server-generated `id` and `questions: [{prompt, options}]`. The chat renders these
after reload, accepts suggested or free-text answers, keeps polling and permits
cancellation. Waiting time counts toward the original ten-minute maximum; it
does not extend the deadline. Scheduler recovery fails expired waiting jobs.

`POST /api/research-jobs/{id}/answers` accepts
`{"clarification_id":"UUID","answers":["answer for question 1", "answer for question 2"]}`.
Answer every question in order with non-empty text, at most 256 UTF-8 bytes and no
control characters. Prompts are capped at 240 bytes; combined prompt/answer pairs
fit the existing 512-byte refinement limit. Clarification-enabled launches accept
at most five existing refinements, reserving space for three answers. Answers
cannot modify libraries, source mode, template, selection or routing policy. Web
search still uses only the original question; answers steer report synthesis.

Resume checks original ownership/active role, gate, current access, source hashes,
template and runtime web policy. One transaction saves answers/refinements,
creates a new queue task for the same job and records an audit. The original
execution plan, spent ledger, absolute deadline and source snapshot are retained.
The worker skips triage on continuation. Repeating the same accepted checkpoint
and answers is idempotent; different answers are rejected. A concurrent cancel or
second submission is serialized by the job row lock. Audit failure rolls back the
handoff. Old queue deliveries cannot claim the resumed work. No second clarification
round, automatic scope change or fresh launch is hidden behind this endpoint.

The scheduler passes its actual delivery ID to the Research worker. The worker
restores the original actor, immutable plan, consumed budget and absolute deadline
(at most ten minutes from enqueue, including queue time). It observes the gate,
actor role, chat/library/document permissions, cancellation and worker lease
every two seconds, including while the model is idle. Source identity is checked
before inference and again before publication. Started work is never replayed:
lease/deadline expiry becomes interrupted; expired queued work becomes failed.
Duplicate delivery cannot take ownership or produce a second report.

Publication writes an immutable Markdown file, then commits the assistant message,
ordered citations, artefact pointer, title, audit and succeeded status in one
fenced database transaction, including web_citations for fetched pages. References
are renumbered with documents first, then web, preserving first appearance within
each kind to match the existing history loader. Citation order survives reload. No provisional
tokens are published. An ordinary failed transaction attempts to remove its staged file;
a crash/cancellation during staging may leave an unreferenced private file for
future cleanup, never a database pointer to a partially published report.

The Research screen uses this queue for every source when the independent gate is enabled.
It offers Default / Manual / Auto, current-profile model choices, explicit library
selection and built-in/custom template admission. `GET /api/research-jobs/scope` returns
only readable ready library/project IDs, names, kinds and ready-document counts;
it performs no inference. Counts are a preview: launch revalidates the full inventory,
source access, readiness and file limits. Scope errors block launch. Restricted MFA
sessions cannot read this endpoint. No legacy prepare/triage call precedes a routed job.

The administrator Providers page exposes the independent Research gate.
Missing/failed/unsupported capability responses block launch rather than
selecting a legacy path. An explicitly disabled gate keeps the old flow on a
fresh screen; revoking a gate already observed as enabled blocks that open form.
Web and hybrid offer the same model controls and visibly require the independent
web-search connector. A failed/uncertain routed POST is not retried
automatically and never falls back to legacy; users are directed to Recent research.

The chat restores polling/cancellation from the persisted `routed_job_id` and
refreshes messages, artefacts and titles on terminal status, including without a
WebSocket event. Unreadable/mismatched status data is hidden; owner cancellation
remains possible after a status-access failure or gate revocation. Polling stops on
errors and terminal status; explicit Retry is available. The UI distinguishes
metadata-only Auto and Off/Shadow default execution rather than claiming to assess
the question's difficulty. Default/Manual pin the research LLM stages; OCR retains
its separate configured service.

### Research evidence review and source grounding

The routed request accepts independent optional `verify` and `deepen` booleans,
both false for old callers/checkpoints. Capabilities expose `verification: true`
and `deepening: "existing_sources_v1"`; the UI offers two separate checkboxes.
These options survive clarification/resume. No schema migration is needed.

`deepen` means one additional ResearchPlan call per outline section, before
writing, re-examining all the original extracted source texts. It identifies
missed evidence, contradictions and unresolved gaps; the corresponding review
is passed to that section's writer. It preserves the selected corpus, fetched
pages, headings and section count. It does not issue new searches, add sources
or repeat itself until a judge is satisfied. Up to eight review calls are
reserved from the same ledger (or the fixed custom-template section count).
Malformed/incomplete reviews fail the job, rather than treating evidence as
sufficient. Completion includes a validated section/gap count and sufficient
completed ResearchPlan accounting; unused reservations are released normally.
This is a bounded source-review subset, not adoption of legacy multi-round digs.

`verify` pins the separately configured fixed `Verify` service at launch,
including its profile/revision and original-actor/Enterprise constraints. It
reserves one service call with 512 KiB request/response caps within the same
ledger/deadline. After the report is written, the verifier checks its body
against the complete extracted evidence, not generated notes or a clipped subset.
It uses strict labels, zero confidence filtering and no HHEM second opinion.
Service errors, missing model acknowledgement/accounting, malformed offsets or
skipped verification fail publication. Core validates the model, every Unicode
character span, label, confidence and score consistency before accepting it.

Verification flags are retained for review; this mode does not silently remove
or rewrite unsupported text. The score measures source grounding of the report
body. It does not establish that the sources are true, prove each adjacent
citation, or verify the separately generated title. A successful job may have
flagged passages. The UI states this distinction and shows the flag count.

The existing verification_runs, claim_verdicts and messages.groundedness records
are committed in the same fenced transaction as report/message/citations/artefact
and audits. No best-effort post-publication verification is used. Span offsets
are shifted over the title prefix and their text is read from the final stored
message after citation renumbering; the current eight-source bound guarantees
D1..D8 replacements have equal character widths. A changed-width publication is
rejected. Both the groundedness display and owner-scoped job status survive reload;
status returns the summary without full spans. Audit or insert failure rolls back
the report and verification together. No generated claim is automatically promoted
to verified fact merely because the request asked for verification.

Existing legacy Research launch does not accept this queue's plan or resume its
budget. Additional web search rounds and retrieval-based deepening remain
unadopted. Chat/composer Auto remains disabled.

### Research history and launch recovery

Capabilities advertise `history` and `idempotent_launch`. The queue launch accepts
an optional UUID `request_id`. New UI submissions generate one ID per explicit
launch and preserve it as `research_request` in the page URL before sending POST.
Reload or a lost response triggers only `GET /api/research-launches/{request_id}`;
it never automatically resubmits POST or switches to legacy research. Recovery
continues when new launches have been disabled. No prompt, credential or source
contents are stored in this URL.

Migration 0123 adds owner-scoped launch receipts. Before any classifier/model
planning, a committed receipt reserves the key and hashes the parsed request,
selection and clarification flag. An identical duplicate returns the original
job/chat after current authorization, or 409 while pending/stopped. Different
parameters under the same key return 409. Enqueue commits the receipt, job, chat,
task and audit in one transaction; receipt recovery does not reset the frozen
plan, budget or deadline. Old clients omitting `request_id` keep their existing
behavior and do not gain deduplication automatically.

Receipt states are `planning`, `succeeded`, `failed` and `interrupted`. Here
`succeeded` means enqueue committed, not that the report finished. Planning has a
120-second timeout and a three-minute database deadline fencing late enqueue.
Expired planning is read as interrupted. Failed/expired keys are never reclaimed:
work may have incurred planning costs before failure. The user can explicitly
prepare a separate request with a new key; checking a receipt never calls a model.
Receipts retain the original role and revalidate the active owner. Successful
receipt lookups also enforce the existing job/source/template authorization.

`GET /api/research-jobs?before=<job UUID>` returns owner-only history in descending
UUIDv7 order, at most 20 items and a `next_cursor`. Each entry rechecks current job
access. Readable entries include chat/title, selection and usage; inaccessible
entries contain only job ID, status, creation time and `accessible: false`.
The owner can still cancel active jobs after source access loss. History and
receipt reads remain available with the launch gate off. The UI supports paging,
refresh, opening accessible chats and cancellation, hiding stale details after
a failed refresh. The older Recent research list retains legacy chats.

## Workflow adapter: agents with authorized sources and read tools

`GET/PUT /api/routing/workflow-jobs` exposes the independent, default-off
`routing.workflow_jobs_enabled` gate; changing it requires provider-management
permission. Capabilities identify `scope: agent_delivery_v1` and supported read tools. An agent-run
workflow can explicitly set `action_config.model_selection` to Default, Manual
(provider UUID), or Auto (objective and optional policy UUID). Absence retains
legacy behavior. This API adapter supports the default assistant and custom
general-mode agents, prompts up to 4096 bytes, projects and up to 32 distinct
libraries (explicit `kb_ids`, captured project links and agent-bound libraries).
Agents may use the adopted `web_search` and `read_document` tools. Artefact
generation, MCP/custom/state-changing tools and agent-specific token budgets
remain unsupported. Unsupported paths fail before
inference; they do not fall back. The workflow editor supports Legacy (absent
selection), Default, Manual and Auto. Saving a legacy workflow never silently
opts it into routing. Manual offers enabled models with current routing profiles;
Auto accepts Economy/Balanced/Quality and an explicit policy or the owner's default.
Default and Manual also pin retrieval, web, document and summary helpers
to the selected model. Auto can use the policy's explicit utility assignments.
All purposes retain their own compatibility checks and the shared run ledger.

Migration 0124 captures the firing's configuration and original owner's role in
the same transaction as workflow-run/task creation. Later configuration edits,
including removal of routing, do not rewrite a captured firing. Old queued runs
cannot adopt newly-added routing without a new firing. Project library links and
trigger event IDs are captured with the firing. The actor, workflow enabled
state, global and original-owner feature restrictions, pause switch and routing
gate are rechecked on execution and during the idle-stream monitor.

A custom agent must be shared, owned by the workflow actor, or used by an admin,
and available in general mode. Its instructions, parameters, owner, modes, tools,
library bindings and effective skill metadata are captured in the firing's
transaction. These must remain unchanged before planning, every dispatch and
publication; archiving the agent also stops the attempt. Queued custom-agent
firings without a snapshot require a new firing. Existing assistant-only snapshots
remain compatible. Tool execution reuses the routed chat runtime and its current
tool grants, source ACLs, egress restrictions, fixed-service profiles and shared
accounting. Document reading requires a project; web supports quick/standard
depth. Neither helper creates a fresh routing budget or resumes a failed attempt.

Agent runs retain their normal kill-token and wall-clock controls. If the outer
workflow monitor or deadline drops chat execution, workflow failure cleanup closes
unfinished child runs and releases their tokens. Recovery also closes orphaned
children, including rows created before their routed checkpoint was written.

Source authorization requires current project write access, library read access,
ready/unarchived sources, and the Enterprise document-denial seam. Restricted
libraries must belong to the captured project. The output chat and its library
links are created transactionally. Source scope, denial sets and chat ownership
are checked before/after planning, before each routed dispatch, during idle
streams and before final publication. Changed project links or source access
stop the attempt without replay or a fresh budget.

Document event placeholders are limited to `filename`, `mime`, `kb_id`, `doc_id`
(also `payload.filename`, etc.). They require 1–32 captured `document.ingested`
events whose resource is a ready `kb_document` in the selected libraries and the
same project context. Values come from current authorized database records, never
the arbitrary event payload. Values are JSON-quoted; for batched events the first
event supplies placeholders and the authorized metadata list is appended. Expanded
input is capped at 16384 bytes. Removed documents, changed metadata, unsupported
events and unknown placeholders fail closed. Static prompts do not use event data.

The queued-to-running claim happens before planning and is never replayed. The
selected plan, accounting and deadline transfer into the existing durable runtime
under a `workflow_agent` job. Generation and helpers share the same ledger; lease
checkpoints precede dispatch. A monitor observes disablement during idle streams.
Planning is capped at 120 seconds, execution at eight minutes after planning, and
the outer attempt at ten minutes. Expired or interrupted work becomes terminal;
recovery does not supply a fresh budget. Workflow/job completion is transactional
with its routing audit, and task state follows the workflow rather than a duplicate
handler's return value.

The final assistant-message write uses the durable lease fence. Chat output and
workflow completion remain separate transactions: a later completion/audit failure
can leave a completed chat answer alongside a failed workflow. This adapter does
not claim atomic publication of the entire chat and workflow history. Further
tool adoption and voice/telephony/API adoption remain subsequent P07 work.

The editor explains Rules (no judge), Judge and Hybrid strategies and displays
the assigned judge from the current catalog. Off skips routing evaluation;
Shadow evaluates a proposal (possibly spending judge tokens) but still answers
with the default model; Active uses the routing decision. The objective is a
preference among eligible models, not a promise that the best answer is known.
The gate can remain off while a configuration is saved; saving never enables it
or invokes a model. Runtime permissions and compatibility checks remain authoritative.

Unavailable settings/catalogs/policies block replacing a selection with an
unresolved choice. Existing unavailable or unknown API selections are preserved
when saving other fields and replaced only on an explicit choice, including
returning to Legacy. Auto objectives, policy IDs and unrelated action fields are
retained. Refetches do not overwrite an in-progress form; unsupported API conditions
are retained too. When editing another owner's workflow, only deployment choices
are offered: the editor's personal default/BYOK catalog must not impersonate the
owner's. An implicit owner default is resolved at execution without a misleading
preview. Run history describes the saved run selection, not the edited draft.

Browser coverage uses the production Workflows screen with mocked HTTP in
`frontend/tests/document-jobs/workflows.spec.ts`. Run it with
`npx playwright test --config playwright.document-jobs.config.ts workflows.spec.ts`.

### Group delivery

`action_config.deliver_group_chat_id` selects an internal group/DM destination.
Its sorted audience (1–128 members, including the workflow owner) is captured
with the firing. Missing/oversized audiences fail that firing without blocking
unrelated event dispatch. Execution and idle monitoring reject membership
changes; retry requires a new firing. Messaging must be enabled for the owner
and every recipient. Active recipients must have project read and library read
access, and no additional Enterprise document denials relative to the original
source scope. These checks run before planning and again before delivery.

Migration 0125 creates a persistent output guard with the chat, before inference.
Ordinary project/share grants cannot open an unpublished output. After delivery,
read/export checks require a captured recipient, current group membership,
messaging availability, a retained share, an unarchived chat and current source
access. A newly added group member cannot read the output. Deleting the workflow
or group does not remove the guard; removing the share revokes recipient access.
Owner/admin access follows existing chat rules. Ordinary sharing cannot extend
the captured audience to another group. If the owner continues the conversation,
recipient access stops: the grant covers the captured workflow result, not later
private turns. Chat-history responses and synchronous exports recheck access
after loading; queued exports check again when building and downloading, so an
old export request cannot bypass a later revocation.

Delivery stores only `Workflow completed — open the output.` and a chat UUID
link in the group; prompt, answer and document metadata stay in the guarded chat.
The group message (including existing encryption/sequence rules), share, receipt,
successful workflow/job status and routing audit commit in the same fenced
transaction. Live notifications follow commit; history covers reconnects.
Terminal replay creates no second message or new inference budget. If delivery
fails after generation, the author may still see the private completed answer,
but recipients cannot open it. No production gate is enabled by this migration.

## Public API: explicit stateless routing

`POST /v1/chat/completions` accepts the opt-in `model_selection` extension for
stateless text requests. Omit `model` (or send an empty string) with this extension:

```json
{
  "model_selection": {"mode": "auto", "objective": "balanced"},
  "messages": [{"role": "user", "content": "Explain this algorithm"}],
  "max_completion_tokens": 1024,
  "stream": false
}
```

Default is `{"mode":"default"}`; Manual is
`{"mode":"manual","provider_id":"<configured-provider-uuid>"}`. Auto also accepts
`policy_id`. The shared policy retains Rules/Classifier/Hybrid and Off/Shadow/Active
semantics; choosing Auto does not activate a policy. Classifier and final calls
consume one execution ledger, including Enterprise inference-policy restrictions.
Absent output limits default to 1024; conflicting or nonpositive limits fail before
planning. Unsupported fields, multimodal content, tools, stop sequences and named
models combined with selection are rejected rather than ignored. The extension
does not accept `x-fosnie-chat-id` and does not store a conversation or run web search.
For an agent request, use the separate contract below.

`GET /v1/routing` uses the same API-key authentication, feature checks and rate
limit and advertises this scope. The independent `routing.public_api_enabled`
boolean defaults to false when absent. Session-authenticated
`GET /api/routing/public-api` returns capabilities; `PUT` with `{"enabled":true}`
requires `providers.manage` and records the setting change in the normal audit
chain. No migration or deployment activation is required by the code change.

Stateless explicit selection and the virtual alias may make **one** technical
fallback when the policy is Active Auto and `technical_fallback=true`. Core reads
the first ML event before exposing the response. Typed connection/transport-timeout,
429 and 500/502/503/504 failures before that event can choose a different visible,
Auto-enabled model. ML emits a bounded `routing_failure` marker only for typed
upstream errors before any event; Core also requires a complete, valid failed-call
report. Exception messages, malformed reports, 400/401/403, incompatible APIs,
cancel/deadline, output/reasoning/tool events and mid-stream errors never authorize
a fallback. Generic helpers, agent/browser/voice turns and durable jobs retain
their pinned execution without automatic replay.

Fallback preserves effective rule/classifier tier, capabilities and effort; it
does not invoke the judge again. The current deployment and injected purpose
restrictions intersect the original limits; the policy revision and original
API key, principal, alias and gates are rechecked. Both attempts share one
deadline and ledger, including conservative unconfirmed spend. Insufficient
remaining call/token/money allowance fails before resolving the alternate key.
`fosnie_routing.decision.proposed` keeps the initial proposal; `actual` identifies
the completed model. Optional `fallback` metadata and the signed-chain
`routing.fallback` event record the failed/planned references, revisions and reason
without prompts or credentials. The top-level response model identifies the final
model for explicit selection and keeps the configured name for a virtual alias.
`GET /v1/routing` advertises the safe point and maximum retry count.

Without `model_selection`, existing model labels and unadopted `agent/<uuid>`
conversations keep their legacy behavior. An adopted agent conversation requires
explicit selection on every request. Explicit null/malformed selections return an OpenAI-shaped 400.
The extension does not reserve any provider name. An administrator can separately
configure an opt-in virtual Auto alias for SDKs requiring a nonempty `model`;
see [API Auto alias](model-routing-api-alias.md). Existing `auto` and `fosnie/auto`
providers retain their meaning. Alias activation and subsequent provider writes
reject collisions atomically; the migration leaves the alias disabled.

For extension requests, response `model` is the pinned configured model ID.
`fosnie_routing` on the JSON response or terminal SSE chunk contains the run ID,
selection, decision (proposed vs actual, policy/revisions/reasons) and full run
usage. Ordinary `usage` describes the final completion only, from the validated
inference report; it excludes classifier tokens, which appear in routing usage.
An incomplete/invalid report, unexpected tool call or premature EOF is a failure,
never a successful zero-token response or a retry through a legacy provider.
The generation transport rejects empty successful call reports before refunding
reserved work. No inference is automatically replayed.

Key revocation/expiry, actor deactivation/role change, public-API feature access
and this gate are checked during planning, upstream connection and idle response
consumption (250 ms polling), and again before emitting each result. Dropping the
response closes upstream and preserves unknown reservations. Completed and failed
planned runs write `api.completion.routed` through the existing audit chain, with
decision/usage and no prompt or credentials. Disconnect audit is best effort on
the running server; it is not a durable job and process crashes cannot guarantee
that record. The shared classifier now writes `routing.classifier.started` before
dispatch and `routing.classifier.finished` when it settles normally. These events
carry attempt/run/lease IDs, target references and a ledger snapshot, never the
prompt or credentials. A start without a finish is conservatively unsettled
spend, including cancellation before a runtime exists and process crashes. It
does not authorize replay. These snapshots are included in run totals; summing
them with completion totals would double-count the same work.

### Agent requests and continuation

Use `model: "agent/<uuid>"` together with `model_selection` for a routed agent
turn. Send exactly one nonempty user message with string content. Agent settings
own sampling, instructions and tools; request sampling, client tools, system
messages and replayed message arrays are rejected. Default/Manual/Auto use the
same planner as stateless requests. The response keeps the requested agent model
label, and `fosnie_routing` identifies the downstream target and full run usage.
Generation and tool-step counters use validated lease reports; contradictory
top-level usage is ignored, and unavailable final counts cannot become a
successful zero-token API response.
`x-fosnie-chat-id` carries the server-owned conversation for continuation.

The supported scope is general-mode text, authorized agent libraries and adopted
read tools (`web_search`, plus `read_document` where the chat already has a valid
project). Unsupported tools, token-budget controls, artefacts and other modes
remain refused. API creation does not bind a project. Internal helper calls share
the plan and ledger; tracked post-answer tasks settle before final API accounting.
No client-side approval or tool execution is exposed.

Migration 0126 adds `api_routed_chats`, a persistent adoption/source witness. A
continuation must belong to the key's owner, have API origin, retain its original
agent and remain unarchived. Agent configuration/skills, project/library scope
and Enterprise document denials are captured and compared again before planning,
at every model dispatch, during idle execution and before answer publication.
The witness also protects older history: changed sources/configuration require
a new conversation. Existing nonempty legacy conversations cannot be adopted
without starting fresh. Omitting selection on an adopted conversation is an error;
disabling routing never sends that history through the legacy path.

All API agent turns use a per-conversation database advisory lock; concurrent
requests receive 409. The worker retains the lock through cancellation cleanup,
even if the client disconnects. Failed/unfinished assistant and agent-run rows
follow the existing routed cleanup path. `api.agent.routed` records the settled
run through the normal audit chain; cancellation and failed execution are not
successful completions. This is request execution, not a resumable durable job.

## Fixed voice REST adoption

`GET /api/routing/voice` describes the opt-in `fixed_voice_rest_v1` scope.
`PUT /api/routing/voice` accepts `{ "enabled": true|false }`, requires
`providers.manage` and records the setting through the normal runtime-config
audit path. `routing.voice_rest_enabled` defaults to false. The ordinary voice
feature and current user's group restrictions must also allow voice.

- `POST /api/voice/routed/transcribe?mime=audio/wav&language=en-GB` accepts raw
  audio (maximum 16 MiB). The response contains `text` and `fosnie_routing` with
  run/decision IDs, the pinned STT reference and conservative run usage.
- `POST /api/voice/routed/speech` accepts `{ "text": "...", "voice": "..." }`
  (voice is optional; text maximum 64 KiB in UTF-8). It returns bounded audio,
  `x-fosnie-routing-run-id`, `x-fosnie-routing-calls` and
  `x-fosnie-routing-accounting: tokens_and_cost_unknown`.

These operations use the actor's configured fixed STT/TTS profiles and require
no default chat model. They do not accept Auto/Manual/model-selection fields:
audio-engine selection is separate from choosing the LLM that answers a spoken
question. Existing legacy endpoints and WebSocket frames retain their protocol.
Capabilities explicitly report live voice and telephony as not adopted.

`RoutedRun::transcribe` and `speech` also work on an existing selected runtime
when its original plan pins those roles. They share its ledger, deadline and
cancellation. Standalone `fixed_voice` runtimes have no LLM assignments and cannot
be frozen as durable jobs. Profile/policy revisions, deployment and Enterprise
constraints are checked at dispatch; no substitute provider or replay is allowed.
Calls cost one reserved attempt, with unknown token/currency usage. Hard token or
currency ceilings without proven bounds reject the fixed call before dispatch.

Provider wire-byte grants include conversion and serialization overhead: STT
allows 32 MiB input / 1 MiB output, TTS 512 KiB input / 16 MiB output, one call
each, at most 120 seconds per call within the remaining run deadline. Expansion
beyond the grant fails. Successful audio requires exactly one completed,
matching report; speech bytes must match its reported output size. Empty,
missing, malformed or failed reports retain the conservative reservation.

REST access checks the original active actor/role, current voice feature and
gate before planning, during idle inference every 250 ms and before publishing.
The shared runtime writes `routing.audio.started` before dispatch and
`routing.audio.finished` when the operation settles. Audit includes only
attempt/run/lease IDs, pinned references, ledger snapshots and validated byte
counts, never audio, transcript, speech text or credentials. A start without a
finish after cancellation/disconnect/crash is unsettled spend, not permission to
replay. Snapshots are included in the run total and must not be added again.

The fixed REST gate does not activate conversational voice. Bounded browser voice
has the separate opt-in described below. Realtime STT, speculative retrieval,
streaming TTS remain unsupported. Telephone batch PCM has its own explicit
adoption and original authenticated line/call checks described below.

### Planning after speech recognition

For a conversational caller, create `RoutedRun::pending_voice` with the original
authorized chat/project, run ID, desired Default/Manual/Auto selection and STT/TTS
roles before dispatching audio. This phase pins the policy and engines without
calling an LLM. Install the caller's access witness and bridge its cancellation
signal while running transcription; a telephone/device caller must preserve its
authenticated provenance rather than reconstructing a user context.

After the final transcript, `chat::routing::prepare_voice` consumes that same
runtime, authorizes its chat/history/project, derives requirements and fixed
retrieval/tool roles, then selects the LLM. Internally `select_voice` transfers
the original ledger into planning, so classifier spend is added to STT spend.
The run ID, actor, cancellation state, access witness, audio references and
original deadline survive. Prior restrictions can only tighten, including when
Enterprise returns different restrictions for STT and the primary LLM purpose.
Current policy ID/revision and existing audio pins must still match. There is no
fallback to a fresh budget or changed audio engine.

The handoff requires exclusive runtime/ledger ownership and no active child or
inference lease. It cannot run twice on an already selected plan or take a
durable job. A failed/cancelled handoff consumes the pending runtime; the durable
audio/classifier receipts preserve conservative accounting. Default/Manual do
not call a judge. With Auto/classifier the minimal full chain is four calls:
STT, classifier, final generation and TTS, with two fixed calls whose token and
currency usage remains unknown. Retrieval, tools and extra speech clauses consume
the same additional allowance.

The bounded browser session below uses this handoff API. Legacy
`voice/session.rs`, partial transcripts/speculation, realtime output and phone
transport remain separate. Do not enable them by relabelling a legacy session
or by running a second fresh planner.

### Bounded browser voice sessions

`GET/PUT /api/routing/voice-live` discovers or changes
`routing.voice_live_enabled` (default false; writes require `providers.manage`
and use the audited runtime-config path). Both `voice` and `voice_live` feature
access must be granted to the current user. Capabilities identify
`browser_pcm_batch_v1`, with explicit exclusions for phone and realtime engines.

Send `voice.routed.start` with required `model_selection` (Default/Manual/Auto),
optional `chat_id`, `project_id`, `agent_id`, and `aec`. Omit chat ID to start a
new conversation. Continuation accepts only previously adopted voice chats with
the same sources and agent configuration. Existing legacy chats are refused.
The new frame requires browser authentication; device-authenticated sockets
cannot acquire Web provenance through it. The legacy start frame stays legacy.
The frontend `useLiveVoice` hook accepts an explicit `modelSelection` option and
sends this new frame only when supplied; the public composer selector is still
a P08 deliverable.

Audio uses existing `voice.audio.chunk` frames: PCM16 little-endian mono at
16 kHz, at most 4096 decoded bytes per frame and 30 seconds per utterance.
Noise below the configured speech floor is discarded; trailing silence ends the
utterance using browser voice knobs. No semantic detector, realtime recognizer
or speculative retrieval is called. Final STT, selection/classification, normal
chat inference/helpers and one complete-answer TTS clip share the same runtime.
Text streams as ordinary chat frames; audio is emitted after the complete answer
and validated TTS report. `voice.final`, `voice.tts.chunk`, `voice.tts.end` and
voice state/error frames retain their existing shapes. There is no silent
fallback to a legacy engine.

Migration 0127 stores `voice_routed_chats.source_scope`. The original active
actor/role, current feature/gate, chat owner/origin, project, agent configuration,
libraries and Enterprise document denials are checked before STT, throughout
inference and before publication. Changed scope requires a new conversation.
An advisory lock allows one voice session per chat and remains held through
cancellation cleanup. Adopted chats reject legacy sends and regeneration before
history mutation, including when routing is disabled.

Explicit `voice.barge_in`, matching `chat.cancel`, sustained loud input with AEC,
`voice.stream.end` and disconnect cancel in-flight work, including before/after
classifier handoff. The routed hook sends session end on unmount; each outgoing
frame has a five-second backpressure timeout that cancels a stalled session.
Disconnect does not detach an unbounded reply. No successful audio end is emitted
for an interrupted request. Shared audio/classifier receipts retain unknown
spend; completed `voice.turn.routed` audit records the total ledger without
content/credentials. A failed early turn refers to its correlated attempt receipts
instead of inventing zero usage. Audio completion means synthesis completed,
not proof that a listener heard it.

### Routed PCM output for telephone transports

`RoutedRun::speech_pcm` requests `response_format: "wav"` on the same fixed TTS
assignment and ledger. The ML speech route forwards this explicit format for
that request only; absent format preserves existing clip behavior. This is a
transport constraint, not another model-selection stage or another inference
call. Providers which cannot produce the required WAV fail without fallback.

Before exposing samples, `PcmReply` requires a complete bounded RIFF/WAVE file
with exactly one PCM format chunk and one nonempty data chunk: signed PCM16,
mono, little-endian, 24 kHz. It verifies chunk lengths/padding, byte rate, block
alignment and duration (at most 120 seconds). Compressed audio, raw unlabelled
PCM, other rates/channels, duplicate chunks, truncation and trailing garbage
are rejected. Valid bounded metadata chunks are skipped. Provider accounting
uses full container bytes, not the smaller decoded sample count; malformed
audio does not refund a completed inference call.

`speech_to_samples` accepts only a sink declaring 24 kHz samples, before any
inference. It delivers aligned 20 ms chunks to the existing telephone codecs
and pacer (G.711 mu-law or 8 kHz PCM16), then awaits the sink's completion. It
retains the run's cancellation, original deadline and access witness through
playback; an additional duration-plus-five-second timeout bounds a stalled
transport. Cancellation/error clears queued audio; dropping the future schedules
bounded best-effort clear in the current process. This is not crash-durable
playback or proof that a caller heard the response.

Browser/telephone capture (16 kHz) is independent of this synthesis output
(24 kHz). The telephone caller opt-in is described below; browser and standalone
REST capability documents continue to exclude telephony from their own scope.

### Authenticated telephone caller opt-in

`GET/PUT /api/admin/telephony/numbers/{id}/routing` requires both
`telephony.manage` and `providers.manage`. PUT accepts required
`model_selection`: Default/Manual/Auto to opt in, or explicit null to disable.
Migration 0128 stores a revision and selection in `phone_routing`. Absence is
legacy; every edit creates a new revision. Writes and audit are atomic. This
is an internal administrative API; a public phone-routing form is outside the
current remote-text composer scope.

After their existing signature/shared-secret checks, Twilio and AudioSocket
answers pin this selection into the one-use call ticket. The common starter
compares it with the current setting, so an opt-in/out or revision change
cannot silently downgrade or reinterpret a ticket. Routed calls require an
existing in-progress call log and a 24 kHz sample sink; they use fixed routed
engines instead of requiring the legacy streaming synthesizer.

The session keeps the original owner principal, line, agent, carrier call ID
and call log ID. It creates a new Phone-origin conversation and binds it to the
call before inference; a second session/replay cannot adopt it. An advisory
lock lasts through cleanup. Its private access witness rechecks active role,
voice/telephony features, provider configuration, enabled line and original
binding, notice/recording settings, call status, agent configuration and exact
library/document-denial scope. Mutation closes the call and clears queued audio.
The shared chat guard accepts Phone only with that in-process witness and the
same call ID. Web/legacy sends cannot continue its saved conversation.

The opening notice uses its own fixed-only routed run (one TTS call, no judge).
Audio capture stays muted until its synthesis/playback and audit finish; the
notice cannot be replayed in the session. Each subsequent utterance owns a
separate STT → selection → chat → PCM TTS run with one shared budget. Selection,
policy revision and fixed audio references stay pinned to session startup;
stale configuration fails instead of substituting an engine. There is no new
call-wide monetary cap: existing telephone rate/concurrency/idle controls still
apply alongside per-run limits.

This bounded implementation supports general-mode answers, agent libraries and
adopted web_search. Phone write/transfer/booking tools, skills, project/device
sources and the owner's personal memory are excluded. The runtime fails before
STT for unsupported agent tools. It uses acoustic endpoints, batch STT and
complete-answer PCM TTS; legacy realtime/speculative paths are not adopted.
Existing carrier recording/notice and teardown mechanisms remain in the transport.
Audio receipts include call/chat correlation; `voice.notice.routed` records the
opening ledger, and `voice.turn.routed` records each completed utterance. A
synthesis receipt is not proof of listening, and a disconnected call is never
automatically retried.

## Non-destructive routed regeneration

`GET/PUT /api/routing/regenerate` exposes a default-off gate; changing it requires
`providers.manage`. The explicit browser frame `chat.routed.regenerate` takes
`chat_id` and `from_message_id` (a completed assistant answer). It creates a new
Web chat and emits the existing `chat.created`, `chat.started`, token and terminal
frames. Supported browser/remote desktop composer controls use this explicit
non-destructive branch path; branch continuation remains unsupported.

Migration 0129 saves `messages.routing_selection` for new routed answers separately
from their decision. Auto repeats classification/selection against the current
policy and profiles; Manual keeps the provider intent and resolves its current
eligible profile, without a judge or fallback to Auto. Default resolves the
current default. The mutable chat preference and the old actual model are never
used to infer the answer's intent. Old answers without saved intent fail explicitly.

The branch copies only history before the anchoring question, with new message
and citation IDs. It regenerates that question once; the old answer, decision and
later turns stay unchanged. Cached summaries, tool checkpoints, budgets, attachments
and artefacts are not adopted. History is bounded to 256 messages / 1 MiB of text;
all preceding assistant answers must have routed intent and be completed.

Only owner Web/general conversations with adopted read tools are supported.
Voice, API, device and durable workflow outputs require their original caller
witness and are refused here. Current actor, project, agent, library/source ACLs,
citations and the selected history prefix are checked before planning and throughout
execution. Source edits, access revocation, gate changes, cancellation or disconnect
stop the attempt. Output backpressure is bounded, and the worker finishes its
cancellation cleanup. Failed attempts may leave an inspectable empty/partial branch.

`routed_chat_branches` records lineage and source scope. Branches require an
in-process regeneration witness; legacy sends and destructive regeneration are
blocked. Repeating a completed branch answer is supported while its scope remains
valid; ordinary branch continuation and prompt editing remain for later UI adoption.
Deleting an original chat clears lineage references, without deleting its branches.
There is no automatic retry or replay of tool side effects.

## Routed draft export

`GET/PUT /api/routing/artefacts` exposes a default-off gate (writes require
`providers.manage`). Explicit document requests to a non-agentic Web/general
drafter can create Markdown, DOCX or PDF from the routed answer. This covers the
existing immediate drafter path. Agents with other tools still require separate
approval/resume adoption; voice, API, regeneration branches and durable jobs are
refused. This path has its independent caller gate and does not broaden the
general browser composer's execution scope.

Preparation captures the original owner, role, agent and source scope before
selection. Its private request intent stays on the same RoutedRun, survives caller
access-guard installation and prevents freezing into a durable job. The gate,
actor, source snapshot and native generate_artefact permission/override are checked
through selection, generation and publication. Auto and Manual use the same
selected model and shared ledger as the answer. Title extraction/fallback is
deterministic and never calls a process-default naming model.

The ML `POST /render-routed-artefact` contract accepts version, format, title and
bounded draft text only. It rejects paths, overrides and unsupported formats.
Markdown writes text; DOCX uses the structural python-docx builder; PDF uses
escaped Markdown with images disabled and a resource-denying WeasyPrint fetcher.
These text-only renderers do not use deployment templates, pandoc, LibreOffice,
external resources or model calls. PDF fails explicitly when its native renderer
is unavailable. This lane does not promise the richer legacy template formatting.
ML removes its temporary output and returns bounded bytes/MIME/hash. Core validates
the response before writing a unique local file. Rendering remains inside the run
deadline but does not consume an inference-call/token grant.

Migration 0130 records a consumed render attempt per turn **before** dispatch.
Concurrent attempts cannot render twice. A repeated completed request can return
its existing artefact ID; a started/uncertain attempt cannot be resumed or replayed,
including with a newly prepared runtime. Recovery requires a new explicit turn.
This is at-most-once dispatch, not crash-proof exactly-once completion. The saved
answer is validated before export, and a changed body cannot reuse the receipt.
Artefact metadata, publication status and audit commit atomically after a final
access check. Cancellation may leave a non-public orphan file; no download pointer
is published on failure. Rendering failures leave the completed draft text and
emit chat.error, with no chat.completed or legacy fallback. Audit receipts contain
run/decision/selection references and sizes, not document content.

## Verification

The P07 call-site acceptance map is in `tools/routing/baseline.md`, including
unsupported combinations and their explicit guards. Legacy callers cannot
downgrade a saved Auto conversation, routed history, API/voice adoption marker,
regeneration branch or durable routed job into default-provider execution.
This check runs before credential resolution, including an explicitly requested
legacy provider. Pending legacy tool approvals are refused if the conversation
has adopted routing; routed/non-resumable checkpoints cannot dispatch effects
through legacy resume. Compatibility-triggered Manual selections in ordinary
legacy chats remain supported. The old picker cannot overwrite routed intent:
ownership is checked first and a chat row lock fences concurrent selection
updates. Migration 0131 indexes the durable-job chat witness. Public
composer entry is now capability-discovered behind the separate default-off
browser gate; see [Browser chat routing](model-routing-browser-chat.md). Shared
[routing administration](model-routing-settings.md) and the collision-safe
[API Auto alias](model-routing-api-alias.md) have separate explicit controls.

Run `tools/routing/test.ps1 -Lane fast`, `-Lane ml`, `-Lane integration` and
`-Lane enterprise`. Integration requires `FOSNIE_ROUTING_TEST_DATABASE_URL` naming
an isolated database with the `routing_` prefix. `tools/routing/compose.yml` creates
disposable PostgreSQL/Redis on loopback ports 15439/16389. Its data is temporary;
reapply migrations after container restart. Missing services fail, never pass as
a skipped test. These lanes do not contact paid model endpoints.

`tools/routing/setup.ps1 -RunIntegration` starts only this isolated Compose project,
applies migrations from `backend/migrations` and runs the integration lane. It
restores the caller's database environment variables afterward.


## Provider failure recovery

The [failure contract](model-routing-errors.md) adds typed provider errors,
revision-bound cooldowns, safe pre-output browser/paired-desktop text failover and
persisted product feedback. The original stateless guard remains separate. Manual
selection and emitted/tool/durable work never replay automatically.
