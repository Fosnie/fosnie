# Fresh-server acceptance checklist (Docker deploy gate)

Run this on a **freshly rented** box before trusting a release. It is the release
gate for the simple-deploy path: "any person can install it and play." Run it on
two tiers and record the results in the README's system-requirements section.

- **CPU tier** — a plain VPS, **8 vCPU / 16 GB** RAM, Ubuntu 24.04 LTS. Uses
  `--profile local` with `qwen3:4b`. This is the honest minimum.
- **GPU tier** — a **24 GB-class** GPU instance. Point the LLM at a larger model
  (8–14B) via a provider row, or a GPU Ollama. Record TTFT + tokens/sec.

The mechanical checks (health, port exposure, secrets perms, restart survival,
airgap-lint, upgrade idempotence) are automated by
[`fresh-server-smoke.sh`](fresh-server-smoke.sh); the interactive UX checks below
are manual (they need a browser + a logged-in admin).

## 0. Provision

- [ ] Fresh Ubuntu 24.04 LTS; install Docker Engine + Compose v2 (≥ 2.24).
- [ ] `docker info` works as your user (added to the `docker` group).

## 1. Install (target: ≤ 15 min to a working local-model chat)

```bash
curl -fsSL https://github.com/Fosnie/fosnie/releases/latest/download/install.sh | sh -s -- --local
```

- [ ] Finishes with "Fosnie is up" and a URL. No manual file edits were needed.
- [ ] `.env` exists with mode **600** and freshly generated secrets.
- [ ] `docker compose ps` — every container `healthy`/`running`.

## 2. Network surface (fail = release blocker)

- [ ] `ss -tlnp` (or `docker compose ps`) shows **only** port 8080 published to the
      host. Postgres/Qdrant/Redis/ML/Ollama/reranker are **not** on `0.0.0.0`.

## 3. First-run + auth (browser)

- [ ] Open the URL → register the first account → it is made **admin** (ClientAdmin).
- [ ] With `--profile local`: the onboarding checklist is **absent** (a provider was
      auto-seeded) and chat answers with the local model.
- [ ] 2FA: enrol a TOTP factor from **Profile → Security**; log out/in with the code.

## 4. RAG + documents (browser)

- [ ] Create a Project, upload a document → it ingests (KB) without error.
- [ ] Ask a question about it → the answer streams with an inline **citation**.
- [ ] Run a small **Deep Research** query → a cited report is produced.

## 5. Surfaces load

- [ ] Tools catalogue opens (Admin/Tools).
- [ ] Workflows toggle flips (Admin) and the Workflows UI appears.
- [ ] Agents and Skills are listed and usable.

## 6. Integrity + resilience (automated by the smoke script)

- [ ] `docker compose logs` — no Rust panics / Python tracebacks at boot.
- [ ] `airgap-lint.sh` PASS in `--profile local` (SearXNG is the only allowed
      egress, and only if `--profile search` is on).
- [ ] **Reboot the host** → the stack comes back up on its own
      (`restart: unless-stopped`) with data intact.
- [ ] `docker compose pull` is idempotent (no surprise recreation on an unchanged tag).

## 7. Record for the README

- [ ] Time-to-first-working-chat.
- [ ] TTFT and tokens/sec for the local model on this tier.
- [ ] Peak RAM/CPU during a chat + an ingest.
- [ ] Any rough edges hit during setup (fix them in-scope — that is the point).

## Windows Docker Desktop (one pass)

- [ ] `install.ps1` quickstart works end-to-end.
- [ ] Note the known gap: the Firecracker code-interpreter is Linux/KVM only and is
      unavailable on Windows; everything else works.
