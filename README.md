<!-- TODO: replace with a centred logo/banner linked to https://fosnie.dev -->
<p align="center">
  <img src="frontend/public/logo.svg" alt="Fosnie" height="80">
</p>

<h1 align="center">Fosnie</h1>

<p align="center">
  <strong>The open-source, self-hosted private AI platform.</strong><br>
  Model-agnostic, and built so your data stays on your own infrastructure.
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/Licence-Apache_2.0-blue.svg" alt="Licence: Apache 2.0"></a>
  <a href="https://github.com/Fosnie/fosnie/stargazers"><img src="https://img.shields.io/github/stars/Fosnie/fosnie?style=flat" alt="Stars"></a>
  <a href="https://docs.fosnie.dev"><img src="https://img.shields.io/badge/docs-fosnie.dev-0a7?style=flat" alt="Docs"></a>
  <!-- <a href="https://discord.gg/…"><img src="https://img.shields.io/discord/…?label=discord" alt="Discord"></a> -->
</p>

<p align="center">
  <a href="https://docs.fosnie.dev">Docs</a> ·
  <a href="https://fosnie.dev">Website</a> ·
  <a href="#editions--licensing">Enterprise</a> ·
  <a href="#contributing">Contributing</a>
</p>

<!-- TODO: add a short demo GIF here: it is the single highest-converting element of the README -->

---

**Fosnie** is an open-source, self-hosted **private AI platform**. This
repository is the open-source (Apache-2.0) edition; **Fosnie Enterprise** is the commercial
tier (one product, one tier). It runs entirely inside
your own infrastructure: no data, prompts, or documents leave it unless you
explicitly enable an external connector. Bring your own model, a local LLM (vLLM,
Ollama, llama.cpp) or an external API key (Anthropic, OpenAI, Gemini), and Fosnie
gives you retrieval-augmented chat, agents, document work, research, and voice on top.

Built for teams that hold other people's confidential data under a duty of care
(legal, audit, finance, healthcare), where *"where does my data go?"* is the first question.

## Features

- **🔒 Offline by default**: no data, prompt, or document leaves your infrastructure unless you enable a connector; every outbound connector ships dormant and opt-in.
- **🧠 Agentic RAG**: decompose → hybrid (dense + sparse) search → rerank → grade → answer, with inline citations.
- **🔌 Model-agnostic + BYOK**: point each capability (LLM, embeddings, rerank, OCR, STT/TTS) at a local engine or any external API, configured in the UI. Native Anthropic adapter (tools + extended thinking).
- **🤖 Agents & workflows**: action-taking agent runs with human-in-the-loop approval, durable resume, and event-driven automations.
- **🔎 Deep Research**: multi-step research over your documents, the web, or both, with a fully cited report.
- **📄 Document work**: generate DOCX/PDF/XLSX/HTML, tracked-change accept/reject, tabular review.
- **🗣️ Voice**: speech-to-text and text-to-speech, including live streaming with barge-in.
- **✅ Groundedness**: verify answers against their sources and flag unsupported claims.
- **🛠️ Tools & MCP**: built-in tools plus a Model Context Protocol host for your own connectors.
- **👥 Teams & messaging**: group and project chats and direct messages, with at-rest encryption.
- **🧾 Tamper-detecting audit**: a SHA-256 hash-chained, append-only audit log.

## Quickstart

> **Requirements:** Docker + Docker Compose **v2 (≥ 2.24)**. ~8 GB RAM with an external model
> API; **~16 GB** for the fully-local stack (`--local`). Only one port is published (`8080`).

**Option A: one-line install** (downloads a pinned release, generates secrets, starts the stack):

```bash
curl -fsSL https://get.fosnie.dev | sh
# fully local: no API keys, all inference on this host:
curl -fsSL https://get.fosnie.dev | sh -s -- --local
```

> Prefer to read before you run: download `install.sh`, review it, then `sh install.sh`.

**Option B: manual (Docker Compose):**

```bash
mkdir fosnie && cd fosnie
curl -fsSL -O https://github.com/Fosnie/fosnie/releases/latest/download/docker-compose.yml
curl -fsSL https://github.com/Fosnie/fosnie/releases/latest/download/example.env -o .env
# edit .env: set POSTGRES_PASSWORD, MESSAGE_ENCRYPTION_KEY (openssl rand -base64 32), ML_SHARED_SECRET
docker compose up -d
```

Open **http://localhost:8080** and create the first account, which **becomes the admin**. In the
default (external-inference) mode an onboarding checklist walks you through adding a model
provider under **Settings → Providers** (a local engine or an API key). With `--local` the
Ollama + reranker stack is wired up automatically, so chat works immediately.

- **Fully-local models:** `--local` adds Ollama (LLM + embeddings) and a llama.cpp reranker; the
  backend auto-configures them. First run pulls the models (a few GB).
- **Managed Postgres** (Supabase, Neon, RDS, Cloud SQL, Azure): set `PAI__DATABASE_URL` in `.env`
  and drop the bundled `postgres` service (see the comments in `example.env`).
- **Bare-metal (systemd), the offline air-gap bundle, backups, upgrades, and reverse-proxy TLS:**
  see **[`backend/deploy/README.md`](backend/deploy/README.md)** and the
  **[documentation](https://docs.fosnie.dev)**.

Upgrade: `docker compose pull && docker compose up -d` (pin `APP_VERSION` in `.env` for
reproducible upgrades).

## Editions & Licensing

- **Fosnie** (this repository) is free and open-source under **Apache-2.0**, and is a
  complete, self-contained product: everything above, with nothing locked behind a paywall.
- **Fosnie Enterprise** is a separate commercial edition for larger and regulated
  organisations. It depends on Core and adds compliance, governance, and scale features.
  See the comparison below or **[talk to us](https://fosnie.dev)**.

### Fosnie vs Fosnie Enterprise

| | **Fosnie** (Apache-2.0) | **Fosnie Enterprise** |
|---|:---:|:---:|
| Chat, agentic RAG, document generation | ✅ | ✅ |
| Deep Research, web search, code interpreter | ✅ | ✅ |
| Agents + workflows (human-in-the-loop) | ✅ | ✅ |
| Voice (incl. live streaming) | ✅ | ✅ |
| Groundedness / verification | ✅ | ✅ |
| Model-agnostic providers + per-user BYOK | ✅ | ✅ |
| MCP host + connectors | ✅ | ✅ |
| Projects / knowledge bases / sharing, roles + groups | ✅ | ✅ |
| Local auth + basic OIDC | ✅ | ✅ |
| Audit log | Hash-chained (tamper-**detection**) | Tamper-**evident** crown: Ed25519 signing, signed checkpoints, per-interaction evidence, GDPR crypto-shred, offline verification |
| Federated SSO / SAML + SCIM | – | ✅ |
| Custom roles, delegated admin, fine-grained access | Fixed roles | ✅ |
| Moderation & accountability | – | ✅ |
| Review & Approve (per-message human sign-off) | – | ✅ |
| Legal holds & retention | – | ✅ |
| Data-owner group approval | – | ✅ |
| White-label branding | – | ✅ |
| DMS connectors (iManage, NetDocuments) | – | ✅ |
| Certified air-gap (signed SBOM, BYOK/HSM) + bare-metal tuning | Runs offline | ✅ |
| Support, SLA, DPA | Community | ✅ |

## Architecture

- **`backend/`** is Rust (axum + tokio): chat orchestration, WebSocket transport, auth, scheduler, audit, config. `sqlx` compile-time-checked queries; forward-only migrations.
- **`ml/`** is Python (FastAPI), the only LLM client: agentic RAG, extraction/OCR, chunking, embeddings, reranking, generation, voice, Deep Research, web search.
- **`frontend/`** is a React 19 + Vite single-page app.

External inference, Qdrant, Redis, and PostgreSQL are reached as configured services; the
platform is their client. Full map in the **[documentation](https://docs.fosnie.dev)**.

## Roadmap

See the **[roadmap](https://fosnie.dev/roadmap)**.

## Contributing

Contributions are welcome. Please read **[CONTRIBUTING.md](CONTRIBUTING.md)**. We use a
Developer Certificate of Origin sign-off on every commit (`git commit -s`) and follow our
**[Code of Conduct](CODE_OF_CONDUCT.md)**. Security issues: **[SECURITY.md](SECURITY.md)**.

## Community

Questions and ideas → **[GitHub Discussions](https://github.com/Fosnie/fosnie/discussions)** ·
Bugs → **[Issues](https://github.com/Fosnie/fosnie/issues)**.

## License

Fosnie is licensed under the **[Apache License 2.0](LICENSE)**. "Fosnie" and "Private AI"
are trademarks of Private AI Ltd (see **[NOTICE](NOTICE)**); the licence does not grant rights
to the name or branding.

<!-- TODO: add a star-history chart once the repo has traction (https://star-history.com) -->
