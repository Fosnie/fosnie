# Changelog

All notable changes to Fosnie are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[Semantic Versioning](https://semver.org/) (pre-1.0: `0.MINOR.PATCH`; a batch of features/fixes bumps the
patch, a milestone or breaking change bumps the minor).

Releases are cut by pushing a `vX.Y.Z` tag. Changes merged to `main` land under **[Unreleased]** until the
next tag; a plain merge ships nothing to users.

## [Unreleased]

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

[Unreleased]: https://github.com/Fosnie/fosnie/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Fosnie/fosnie/releases/tag/v0.1.0
