# Routing baseline — 14 September 2026

Core HEAD: `0f857ac47e0ce6c1855d08602a4b2a2db168faf5` (existing uncommitted work retained).
Enterprise HEAD: `ebdd5584166227cc014003ec1cce0ac03e4cdf47`.

| Check | Result before routing integration |
|---|---|
| Core `SQLX_OFFLINE=true cargo check --all-targets --offline` | Passed; existing code-interpreter/test warnings |
| Protocol `cargo test --offline` | 8 golden tests passed |
| ML provider overrides/reasoning/complete/probe tests | 23 passed |
| Frontend `npm test -- --reporter=dot` | 42 passed after installing absent dependencies without rewriting lockfile |
| Enterprise offline check | Existing non-exhaustive `ConnectorKind::Notify` OAuth match; added to non-OAuth arm to restore compatibility |
| Isolated services | `tools/routing/compose.yml`, loopback ports 15439/16389; separate ephemeral PostgreSQL/Redis |

Enterprise's stale lockfile refreshed its Core path dependency from 0.3.0 to 0.6.0
and added the shared protocol crate during baseline compilation. No unrelated
dependency upgrades were requested. Real inference endpoints are not used by these checks.

## Inference inventory

All below require an explicit actor, purpose, constraints and effective target.
The table tracks integration work; listing a row does not claim it is integrated.

| Path | Actor | Purpose | Previous source | Required propagation |
|---|---|---|---|---|
| chat/tool loop/final | turn user | primary/tool_loop/final | resolve_llm + selected overrides | one turn plan |
| title/compaction | turn user; previously sometimes None | utility_title/summary | default overrides | explicit utility or primary |
| RAG | requesting user | retrieval_decompose/grade | request overrides | stage assignment, fixed index model |
| web quick/standard | tool caller | web_plan/grade/synthesis | independently resolved default | inherit plan |
| web deep worker | initiating user | web stages | job/default | durable actor/plan, recheck on resume |
| research/document | initiating user/system job | research_plan/write/document | default overrides, sometimes None | explicit run plan |
| scheduler/workflow/automation | persisted initiator or explicit system | purpose of child stage | independent overrides | durable selection and actor |
| regenerate | original caller/current actor | new primary turn | stored provider pointer | selection intent + new decision |
| voice/telephony | session user/line configuration | voice | own resolve_llm | one decision per semantic turn |
| raw API | API-key user | primary | label to provider | raw semantics, no injected tools |
| agent API | API-key user | agent stages | run_turn | full plan |
| embeddings/rerank/OCR/STT/TTS/verify | capability caller | fixed capability | existing role resolution | constrain without switching index |

Implementation checkpoints and live-quality status are maintained in the user-provided
implementation plan. No live quality or savings measurements have been made.

## P07 acceptance map — 2 October 2026

The routed lane uses one `RoutedRun`, original actor and explicit purpose. Its
transport rejects missing, mismatched or incomplete dispatch reports; default
provider/environment resolution is not an error recovery path. The legacy lane
remains available for conversations that have not adopted routing. Public
composer entry has a separate default-off P08 browser gate. The opt-in API alias
has its own configuration and reserves no existing provider name; see
`docs/model-routing-api-alias.md`.

Paths below are relative to this repository. `common/*` names refer to
`backend/tests/common/`; they run together in `model_routing_schema` against
loopback mock endpoints and isolated PostgreSQL/Redis.

| Product caller | Routed implementation / targets | Evidence and explicitly refused combinations |
|---|---|---|
| Browser and desktop remote composer/reload | `chat/browser.rs`, shared `chat.routed.send`, `Chat.tsx` / `Composer.tsx`; saved intent/options before judge, explicit routed title, owner actual metadata | `*_browser_cases.rs`, `*_desktop_cases.rs`, protocol strict/golden tests and `frontend/tests/document-jobs/chat.spec.ts`. Default-off browser/remote-text gate, sticky local-only/effort, one compatible candidate shortcut, lease/cancellation/revocation, actual shadow target and reload. Desktop preserves original device key through authenticated ticket/resume; stale credentials cannot become a later live key. ClientHello never changes origin. Attachments, folders, device tools, voice/API/jobs and branch continuation refused; unadopted legacy chats stay compatible. See `docs/model-routing-{browser-chat,desktop}.md`. |
| Chat, agent loop, final answer | `chat/routing.rs`, `model_routing/run.rs`, `transport.rs`; primary, tool_loop, final through selected plan | `common/model_routing_chat_cases.rs`, `*_workflow_agents.rs`, `*_api_agent_cases.rs`, `*_desktop_cases.rs`. General remote text follows its caller's original Web/Desktop scope; device identity requires an in-process access witness. Device workspace, attachments, separate reasoning/provider overrides, research-mode turns and unadopted agent tools refused. Entire proposed tool batch validated before dispatch. |
| Title, compaction, artefact naming | `chat/routing.rs::title`, `chat/mod.rs`; utility_title/utility_summary use run assignments; immediate drafter title is deterministic | Chat fixture covers helper targets/capacity/revocation. Routed callers prepare an existing chat, so the legacy new-chat title task is unreachable. `*_artefact_cases.rs` proves no hidden model naming call. |
| RAG and library tool | `chat/routing.rs::Retrieval`, `tools/mod.rs` routed library arm; retrieval_decompose/grade plus fixed active-index embed/rerank | Chat/fixed fixtures check original source ACL, index revision, per-part synthesis, exact evidence and missing terminal reports. Unpartitioned per-part tool top-ups and arbitrary history truncation refused. Desired embedding target never replaces active-index provenance. |
| Web quick/standard | `web_search/routed.rs`, `chat/routing.rs::search_loop`; web_plan/grade/synthesis plus fixed rerank/OCR, same run | `*_web_cases.rs` and workflow/API agent traces verify grants, selected helpers, cancellation and no default fallback. Standalone deep-web tool jobs are explicitly refused before dispatch; use routed bounded research for multi-stage research. Legacy `web_search::run_deep` cannot consume routed chats. |
| Research (web, corpus, hybrid), queued worker | `research/routed.rs`, `research/jobs.rs`, ML `research/routed*.py`; research_plan/write, explicit fixed services | `*_research_cases.rs`, `*_research_jobs.rs` and ML routed research tests. Server-owned frozen context, fenced lease, current actor/source/template checks. Interrupted attempt needs explicit new planning; unsupported template expansion refused. Legacy `research::run_research` cannot consume routed chats. |
| Document read/edit/verify/repair | `tools/routed_document.rs`, `groundedness/{routed,routed_draft,durable,jobs}.rs`, ML `routed_document.py`/`routed_draft.py`; document/fixed OCR/embed/verify stages | Document/chat/verify/draft/job HTTP fixtures cover targets, ACL changes, CAS publication and terminal errors. Legacy verify worker refuses routed verification IDs. Unsupported document/tool formats stop before dispatch. |
| Scheduler/workflow/automation | `workflows/routing.rs`, `model_routing/durable.rs`, `scheduler/mod.rs`; frozen selection, shared run for child stages | Workflow source/agent/delivery fixtures check original owner, immutable firing inputs, lease, scope and group membership. Configuration opt-in never falls back to legacy; workflow effect/delivery receipts prevent replay. Device automation remains unsupported in routed mode. |
| Regenerate | `chat/regenerate.rs`, `chat/routing.rs`; saved selection/options, fresh Auto decision, Manual pin | `*_regenerate_cases.rs`, `*_browser_cases.rs`, `*_desktop_cases.rs`: new branch, original history unchanged, current source/actor/policy, cancellation/gate revocation. Supported remote Desktop branches preserve surface and original device credential. Historical NULL intent, editing/restarting in place, folders/device tools/voice/API/durable histories and branch continuation refused. |
| Voice and authenticated telephone | `voice/routed.rs`, `telephony/routing.rs`, shared `voice/telephony` starter; one semantic-turn selection, fixed STT/TTS in original ledger | Voice REST/live/PCM/phone fixtures and `voice_routed_pcm` cover promotion, original line/call actor, barge-in, deadlines and gate changes. Bounded PCM batch only; realtime, streaming TTS, speculation, legacy-history adoption and write/transfer/booking tools remain unsupported. |
| Raw API | `http/v1/{routed,alias}.rs`, `passthrough.rs`; stateless primary target, requested alias separated from actual metadata | `*_api_cases.rs`, `*_alias_cases.rs`: JSON/SSE, key/policy/alias revocation, report validation, budgets and strict unsupported fields. Explicit extension or enabled collision-safe alias; caller tools and multimodal payload refused. No injected RAG/tools. Migration 0133 guards concurrent alias/provider namespace writes, including REPEATABLE READ. |
| Agent API | `http/v1/agent_routed.rs`; same routed chat/tool helpers and original API principal; alias requires explicit fosnie_agent_id | `*_api_agent_cases.rs`, `*_alias_cases.rs`: library/web traces, adoption marker, source scope, key revocation, alias JSON/SSE/continuation and concurrent requests. Requested alias and actual target are separately audited. A later legacy request cannot downgrade an adopted API conversation. |
| Fixed embed/rerank/OCR/STT/TTS/verify | `model_routing/{fixed,fixed_lease,mixed}.rs`, ML `inference_fixed.py`; explicit role/index targets | Fixed/voice/document/web fixtures and ML tests enforce provider/profile/index revisions, budgets, endpoint restrictions and complete reports. STT/TTS are independent capabilities, never inherited from selected LLM. |
| Immediate drafter export and stateful no-replay boundary | `chat/routed_artefact.rs`, ML `routed_artefact.py`; completed answer rendered without inference | `*_artefact_cases.rs`: consumed attempt before rendering, repeated/concurrent/changed requests, replan cannot replay, gate cancellation and malformed response. `md/docx/pdf` only; tool-agent approval/resume, queued/voice/API export refused. Native PDF renderer remains unverified. |
| Legacy downgrade / approval boundary | `model_routing/admission.rs` called by chat/regenerate, provider resolver (before requested ID), legacy picker update, legacy WS voice, legacy research/deep workers and agent resume | `*_admission_cases.rs`: empty saved Auto, intent/decision history, voice/API/branch/job markers, no credential resolution/inference/history mutation, terminal approval refusal and no replay. Old picker preserves saved intent and checks ownership before admission; its chat row lock fences concurrent selection updates. Frozen non-resumable/routed checkpoint blocks effects even without chat marker. Migrated legacy Manual stays compatible. Migration 0131 indexes durable chat admission. |

P07 acceptance is bounded by these explicit refusals, not by implementation of
every legacy feature. Lifting a refusal requires passing the same actor, plan,
budget, target validation and cancellation contract through that feature first.
Mock results establish execution safety and target propagation; live routing
quality, model superiority and savings remain `not_run`.
