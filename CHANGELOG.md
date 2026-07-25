# Changelog

All notable changes to Fosnie are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/) (pre-1.0: `0.MINOR.PATCH`; a batch of features/fixes bumps the
patch, a milestone or breaking change bumps the minor).

Releases are cut by pushing a `vX.Y.Z` tag. Changes merged to `main` land under **[Unreleased]** until the
next tag; a plain merge ships nothing to users.

## [Unreleased]

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

[Unreleased]: https://github.com/Fosnie/fosnie/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/Fosnie/fosnie/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/Fosnie/fosnie/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Fosnie/fosnie/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Fosnie/fosnie/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Fosnie/fosnie/releases/tag/v0.1.0
