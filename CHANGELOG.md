# Changelog

All notable changes to Fosnie are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/) (pre-1.0: `0.MINOR.PATCH`; a batch of features/fixes bumps the
patch, a milestone or breaking change bumps the minor).

Releases are cut by pushing a `vX.Y.Z` tag. Changes merged to `main` land under **[Unreleased]** until the
next tag; a plain merge ships nothing to users.

## [Unreleased]

## [0.7.0] - 2026-10-10

### Added

- Added a frozen instruction following comparison through the existing Core evaluation endpoint, with manual baselines, Classifier/Hybrid accounting and separate unknown results for incomplete provider responses.
- Added a bounded, revision-pinned instruction following evaluator for routing experiments, keeping constraint compliance separate from semantic quality and production evidence admission.
- Added reproducible Manual and Auto evaluation plans for audited public cohorts, with saved profile/policy checks, durable finite reservations and separate unknown results; completed evaluations expose their terminal contract and native profile fingerprint.
- Added finite native cloud evaluation allocations and explicit temperature for protected text evaluations, preserving prior defaults and rejecting invalid values before dispatch.
- Added isolated evaluation support for automatic model selection across web search, library retrieval and continued conversations.
- Added offline blind-review tooling for frozen judge datasets, family/exposure checks and a revision-pinned public benchmark importer. Diagnostics now count dialogue families separately from language variants. These tools do not promote evidence, change routing policies or make model calls automatically.
- Added an explicitly isolated loopback development demo using the actual chat and routing settings components, separate test data and operator-declared local profiles. It does not enable production routing or connect cloud credentials.
- Added optional model ranking from reviewed held-out evaluation records and bounded quality escalation for non-streaming stateless Auto API requests. Includes offline evidence aggregation, profile import controls and shared Core/Enterprise admission and budget checks. Both options default to disabled.
- Added explicit task exclusions to model profiles so automatic selection and fallback can avoid models unsuitable for a workload.
- Added a revision-bound public EN/RU corpus and calibration-first comparison of Manual, Rules, Classifier and Hybrid, with judge accounting, whole-stage bounds and explicit incomplete evidence/coverage status.
- Added configurable automatic model selection across local and cloud providers, with routing rules, a selectable judge and shared usage limits.
- Added bounded embedding rebuilds with pinned models, source access checks, persistent usage and explicit fresh attempts after interruptions.
- Added safe quota, billing, rate-limit and timeout diagnostics, revision-bound provider cooldowns and bounded pre-output failover for eligible plain-text Auto chat turns. Failed attempts share the original allowance and access checks. Chat error messages now retain received output and offer model selection or a prepared explicit retry; tools, durable work and emitted output never replay.
- Added an optional quick search mode using search-engine excerpts without page fetching, OCR or reranking inference.

### Changed

- Renamed the settings, service units, backup bundles and file locations that a deployment is configured with so that they carry the product name, with the previous spellings still accepted for settings, backups and the link between the two services, and a note in the deployment guide covering the few that have to be moved by hand.
- Enabled technical recovery before output in the isolated local routing demo.
- Add per-model Auto effort presets and an independent judge effort setting, with explicit request/rule precedence and per-candidate fallback resolution. Preserve legacy defaults, shared budgets and evaluation fingerprints without presets; invalidate reviewed evidence when a preset changes. Translate Gemini 3 reasoning levels natively, expose the judge's abstention threshold, and add offline field-level diagnostics for saved judge observations.
- The private routing example shares its fixture with an installed Enterprise consumer for source-ACL, revocation and signed-provenance evaluation. Core production keeps its edition seams and does not depend on Enterprise.
- Uploaded documents can opt into the effective Auto policy, pinned embedding/OCR services and a durable shared inference budget. Interrupted ingestion is not automatically replayed; source/actor access and the deployment gate are checked again during work and at publication. Immutable bounded file snapshots prevent changed upload bytes entering the original job. The default-off control appears in routing settings. Legacy and connector ingestion retain their existing path.
- Model profiles can restrict JSON output to specific declared API efforts. Auto selection, rules, explicit targets, classifiers and dispatch enforce the restriction without changing effort. Settings expose the restriction and evaluation fingerprints change when it changes. Legacy profiles retain their existing fingerprints and behaviour.
- Quick web search now reranks at most eight verbatim excerpts and returns at most four, reducing work for local models. Routed chat uses the original current question as a bounded ranking hint without sending it to the search engine. Partial coverage is explicit and citations keep their original source metadata. Standard and deep search retain their existing evidence processing.
- Add opt-in connector document ingestion with pinned models, a durable shared ledger, original importer/sync principal, source-ACL revision checks and explicit reindexing. Unauthorised copies remain stored without model egress; old legacy tasks cannot bypass the gate.
- Improved model routing upgrade verification and recovery instructions.

### Fixed

- Prevented Auto's judge from consuming the last answer call, and added an optional answer time reserve with explicit skipped-judge reasons and tightened dispatch deadlines.
- Added model-free recovery for interrupted document index writes and retained the previous embedding index until promotion was confirmed.
- Preserved existing indexed content when an ingestion model failed, removed stale chunks from both indexes during a rebuild, and rejected contextual workloads that exceeded their routing allowance before starting summaries.
- Separated provider pacing from the Core request deadline and added request-owned native timeouts and explicit availability stops for bounded routing experiments.
- Bounded model evaluations now require a normal stop and non-blank final text for plain answers as well as structured answers. Truncated, filtered or missing finish metadata cannot become a successful quality measurement. Failures retain the conservative request cap and do not trigger a retry.
- Fixed internal retrieval helpers exceeding a routed chat's output allowance before inference could start.
- Fixed browser routing so its internal generative response ceiling no longer rejects fixed web services with unknown token usage; retained explicit policy caps, prevented documentation keywords from triggering unrequested artefact generation and added an isolated live WebSocket pilot.
- Fixed learned quality comparisons across model-specific reasoning presets while retaining each model's effort and evaluation provenance checks.
- Added optional native structured responses for routing judges and bounded evaluations, and clarified when supplied information requires external tools.
- Fixed native tool-history replay for Responses and Gemini, and reserved generation room for reasoning-based retrieval graders within existing grants.
- Added safe reasons for rejected native JSON outputs in routed ML calls and Core, plus a private local calibration and weather-tool probe with pinned targets and shared accounting. Rejected responses remain unrepaired and are not replayed. Added an optional loopback SearXNG dependency profile for isolated live-search validation; ordinary offline checks do not start it.
- Fixed non-streaming model calls to retain their explicitly assigned purpose and token allowance, including primary evaluation requests and final answers.
- Preserved xhigh in legacy Responses streaming, stopped requesting disabled reasoning summaries and translated Gemini thinking controls for the actual request model, with unsupported controls rejected before dispatch.
- Added explicit cancellation for an administrator's embedding rebuild and prevented an older stopped worker from overwriting a newer attempt's status.
- Prevented concurrent rebuilds from replacing the same desired index, and rechecked the built corpus and requesting user's access before publishing it. A proven pre-publication refusal now permits an explicit fresh attempt without replaying inference.
- Added bounded page-image processing for image-only OCR services, preserving PDF page numbers and checking the remaining OCR allowance before sending scanned pages.
- Fixed explicit judge tests being refused by Auto's answer reserve. A standalone probe uses its existing single-call allowance and deadline without reserving a nonexistent answer; normal Auto still preserves the last answer call and time.

## [0.6.0] - 2026-08-02

### Added

- Added the ability to take voice calls on telephone numbers, each answered by an agent you choose on behalf of an account you choose, registered and switched on in the interface, with a call log linking to the transcript of every call, and with speech recognition, the reply and the conversation all running on your own infrastructure.
- Added separate turn-taking settings for calls arriving on a telephone line, timing for every stage of a spoken reply, and a measurement of what the caller actually heard rather than what was sent.
- Added the ability for the agent answering a telephone line to write down what a caller wanted and pass it on, with a ready-made Receptionist agent, an optional announcement into a team chat, and a rule that whoever may wire a line still cannot read what callers said on it.
- Added the ability for a telephone line to hand a caller to a person, with a written handover for whoever picks up, and for the agent to finish a call itself; no part of it makes an outbound connection to the telephone network.
- Added a list of names a telephone line checks callers against before offering them anything, enforced so that no caller is put through to a person until they have been checked and found clear, with the result recorded against the call and never disclosed to the caller.
- Added a diary with real opening hours and a real time zone, so a telephone line can offer times, book one, move it and cancel it, with two callers unable to take the same slot and a caller ringing back identified by two independent things before anything changes.
- Added a spoken notice every caller hears before anything they say is acted on, per line retention for conversations and call records, deletion of a single call's conversation on request, and a record of what the lines do with what they are told, assembled from the settings themselves.
- Added a rule for tools that reach outside during a telephone call, so nothing waits on an approval nobody can give while a caller listens, and outward notifications that post a line into Slack, Teams or any address that accepts a message when a line takes something.
- Added a second way for a line to be answered: a telephone system on the practice's own network hands the audio straight to the deployment, so a call reaches nobody else at all.
- Added the telephone settings to the interface and a readiness check that asks the questions a call asks, including a real test request to the speech engine, so a line can be proved to work before anybody rings it.
- Added call recording per line, kept as a two channel sound file you can play back from the call log, with the caller told the call is recorded before they say anything and a compulsory period after which the audio is deleted.

### Changed

- Live voice can now be carried over something other than a browser tab: the conversation, its turn taking and its interruptions are no longer tied to one kind of connection, and the narrowband audio handling a telephone line needs is in place ready for it.

### Fixed

- Fixed wide tables in the admin console giving the whole page a horizontal scrollbar: a table with more columns than the page is wide now scrolls inside its own box, leaving the headings and prose beside it where they were.

## [0.5.0] - 2026-07-25

### Added

- Added a desktop application for Windows and macOS: pair it with your instance using a code from your profile, then work in it with reliable streaming, system notifications and signed updates you approve before they install.
- Added connected folders: connect a folder on your computer to the desktop app, and an agent in a chat can work in it, with reading allowed outright and every change, deletion or command shown for you to agree to first and reversible afterwards.
- Added automations that run against a connected folder on your own machine, with missed runs surfaced and file changes paused for your approval.
- Added a pinned plan above the stream, an end-of-turn summary of the files changed and commands run, an actions-only view, and made a waiting approval impossible to miss: a decision taken on one device settles the request on the others at once, the desktop app carries a taskbar count of approvals waiting, and a failed task raises a notification.
- Added device pairing, so a native client can be signed in to an instance with its own per-machine token, listed and revoked from your profile.
- Added support for the app to run against an instance it is not served by, signing in with a paired device token instead of a browser session.
- Added three ways to get the desktop application: download it from us, have your own instance hand it out and update it, or build it from source.

### Changed

- On computers that confine the desktop agent's commands, an everyday command that stays in the connected folder and does not need the internet now runs without asking each time, while commands that reach the network or change files elsewhere are still confirmed.
- A command run by the desktop agent that overruns its time limit is now stopped together with any programs it started, rather than leaving them running.
- The desktop client now carries the Fosnie mark everywhere Windows shows it, sends notifications under the Fosnie name and icon, draws its own title bar on Windows, and opens filling the screen the first time before remembering the size and position you leave it at.

### Fixed

- Fixed the sidebar being squeezed into a stub column with its labels cut off when the window is narrow, such as a desktop window snapped to half a screen: it now steps aside and opens over the page on request, as it already did on a phone.

## [0.4.0] - 2026-07-21

### Added

- Added speculative library search to live voice: the knowledge-base search now starts from the partial transcript while the speaker is still talking, so a grounded reply begins sooner.
- Added an OpenAI-compatible API at `/v1`, authenticated by platform API keys minted in Profile: address a configured model directly, or an agent to answer from your own libraries.
- Added an artefact panel: generated documents open beside the conversation with a preview per file type, and download stays one click away.

## [0.3.0] - 2026-07-20

### Added

- Added PowerPoint (.pptx) generation: editable 16:9 decks with native text, tables, charts and speaker notes.
- Deep Research now checks each report section's evidence before writing and runs a targeted search to fill the gaps, so under-supported sections get real sources instead of padding.

## [0.2.0] - 2026-07-17

### Added

- Retrieval now runs as many rounds as a question needs until the evidence is exhausted, and the answering model can search the library again itself when the first pass falls short.
- Remote MCP servers can now authenticate each user individually through OAuth 2.1, so an admin adds a server by URL and every user connects once under their own identity.
- Deep Research report types are now user-definable: duplicate one of the four built-ins or start from scratch to set a report's section structure, per-section briefs, outline mode and writing style, personal by default or published deployment-wide under a permission.

### Fixed

- Internal scaffolding calls (history compaction, skill dry-run, report-to-page rendering) no longer inherit the model's default reasoning effort, which on reasoning-heavy models wasted the token budget, inflated cost and latency, and could return nothing.
- Fixed incremental history compaction silently stopping after the first summary on long conversations.

### Security

- Agents can now be granted individual MCP tools, and every MCP call is checked against one authorisation gate, closing grant-bypass gaps.
- Built-in and custom tool calls now pass a single authorisation check before they run, an admin-disabled tool genuinely refuses, and custom tools can be granted to an agent.

## [0.1.0] - 2026-07-13

### Added

- Initial public release of Fosnie Core (Apache-2.0): a self-hosted, model-agnostic private AI platform.
- Chat with agentic RAG over your own documents: hybrid retrieval, reranking, and inline citations.
- Deep Research: multi-step, fully cited reports over your documents, the web, or both.
- Document work: DOCX/PDF/XLSX/HTML generation, tracked-change accept/reject review, and tabular review.
- Agents and event-driven workflows with human-in-the-loop approval and durable resume.
- Multiple LLM providers with per-chat switching and per-user BYOK; local engines or any OpenAI-compatible
  API; native Anthropic adapter.
- Self-hosted, zero-egress code interpreter (Firecracker microVM on KVM hosts, gVisor on KVM-less hosts).
- Voice: speech-to-text and text-to-speech, including live streaming.
- Groundedness verification of answers against their sources.
- MCP host and custom HTTP tools.
- Local auth and basic OIDC; projects, knowledge bases, sharing, roles and groups.
- Hash-chained, append-only audit log (tamper-detection).
- One-line installer and Docker Compose deployment; health and Prometheus metrics endpoints.

[Unreleased]: https://github.com/Fosnie/fosnie/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/Fosnie/fosnie/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/Fosnie/fosnie/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/Fosnie/fosnie/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Fosnie/fosnie/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Fosnie/fosnie/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Fosnie/fosnie/releases/tag/v0.1.0
