# Deployment profiles

The platform is **one codebase, two host profiles**. The Rust binary and the
Python ML service are identical across profiles — only **config/env** and the
**host service files** differ. The platform is the HTTP client of an
OpenAI-compatible inference server; it is not engine-locked.

| | **Profile A — Linux** | **Profile B — macOS** |
|---|---|---|
| Hardware | bare-metal GPU server (NVIDIA/ROCm) | Mac Studio (Apple Silicon, unified memory) |
| Inference | vLLM | llama.cpp (`llama-server`) |
| Build target | `x86_64-unknown-linux-gnu` | `aarch64-apple-darwin` |
| Service manager | systemd (`systemd/*.service`) | launchd (`launchd/*.plist`) |
| Backend boot config | `config.linux.example.toml` | `config.macos.example.toml` |
| ML env | `../../ml/.env.linux.example` | `../../ml/.env.macos.example` |
| code-interpreter | available (Firecracker/KVM) | **off** (no Firecracker on macOS) |

## Docker deployment (the simple path)

For most self-hosters, the containerised path is the fastest way to a running
platform — no build, no bare-metal setup. It ships as three release assets in this
directory:

- **`docker-compose.yml`** — the standalone stack. Default (no profile) = 5
  containers: `backend`, `ml`, `postgres`, `qdrant`, `redis`. The backend serves
  the API **and** the SPA, so **only port 8080 is published**; the datastores stay
  on the internal network.
- **`example.env`** — the minimal env (secrets + `HOST_PORT`/`APP_VERSION`). The
  installer regenerates the secrets; everything else is defaulted in the images or
  set at runtime in the admin UI.
- **`install.sh`** / **`install.ps1`** — a `curl … | sh` (or `irm … | iex`)
  installer: preflight → download pinned assets → generate secrets → `up -d` →
  wait healthy.

Profiles:

- *(default)* **external inference** — bring an LLM; add a provider key in the
  admin UI after first login.
- **`--profile local`** — fully local, zero keys: adds **Ollama** (LLM +
  embeddings) and a **llama.cpp reranker**; the backend seeds the provider rows on
  boot (`LOCAL_STACK=1`). BM25 is fastembed inside the ml image (cache baked in →
  no runtime download). `install.sh --local` also pulls the Ollama models.
- **`--profile search`** — adds self-hosted **SearXNG** for the web-search
  connector (dormant until enabled in the admin UI — no egress by itself).

```bash
docker compose up -d                     # external inference
docker compose --profile local up -d     # fully local
docker compose pull && docker compose up -d   # upgrade (pin APP_VERSION in .env)
docker compose logs -f backend           # logs
```

Migrations run **inside the backend on boot** (`sqlx::migrate!`), so there is no
separate migrate step. Data lives in named volumes (`pgdata`, `qdrantdata`,
`redisdata`, `appdata`, …) and survives `down`/reboots (`restart: unless-stopped`).

- **Managed Postgres:** set `FOSNIE__DATABASE_URL` in `.env`, drop the `postgres`
  service (see `example.env`). Use a session-mode/direct pooler, never
  transaction-mode.
- **TLS / a public domain:** put a reverse proxy (nginx/Caddy/Traefik) in front and
  set `PUBLIC_URL` to your `https://` URL (a non-loopback `public_url` MUST be
  https, or the backend refuses to boot). Managed TLS is on the roadmap; the proxy
  recipe is the supported path today.
- **Backups & disaster recovery:** [`BACKUP-DR.md`](BACKUP-DR.md).
- **Air-gap / offline install:** `release/build-offline-bundle.sh`.
- **Build the images yourself:** `release/build-images.sh` (no registry needed).
- **Validate a fresh server before you trust it:**
  [`verify/fresh-server-checklist.md`](verify/fresh-server-checklist.md).

Bare-metal + systemd/launchd (below) remains the tuned production path; Docker is
the low-friction one.

## Config layers

1. **Deployment layer** — the external services (vLLM/llama.cpp, embeddings,
   reranker, Qdrant, Redis, Keycloak, Postgres). Own lifecycle; not managed by
   the platform. The platform only *connects* to them.
2. **Boot config** — endpoints, secrets, ports, paths, host capabilities. A TOML
   file selected by `FOSNIE_CONFIG_FILE`, overlaid by `FOSNIE__*` env vars
   (`defaults → file → env`). Profiles above.
3. **Runtime config** — operational tuning (RAG top-k, retention, branding,
   feature toggles) in Postgres `config_settings`, edited from the admin UI,
   audited. Not host-specific.

## Selecting a profile

- **Linux:** `EnvironmentFile=/etc/fosnie/backend.env` (or set `FOSNIE_CONFIG_FILE`) →
  `config.linux.toml`; `fosnie-ml` reads `/etc/fosnie/ml.env`.
- **macOS:** the launchd plists set `FOSNIE_CONFIG_FILE=/etc/fosnie/config.macos.toml`
  and the ML env vars.

## Upgrading from a pre-rename deployment

Everything the platform names after itself used to be spelled `pai`. If your host
was installed before that changed, the following moved:

| Was | Is now | Still works? |
| --- | --- | --- |
| `PAI__*` env vars, `PAI_CONFIG_FILE` | `FOSNIE__*`, `FOSNIE_CONFIG_FILE` | yes — read as a fallback, with a warning at boot; `FOSNIE__` wins where both are set |
| `X-PAI-ML-Key` request header | `X-Fosnie-ML-Key` | yes — the ML service accepts both, so the backend and ML may be upgraded separately |
| `PAI_DB_URL`, `PAI_QDRANT_URL`, `PAI_STORAGE_DIRS`, `PAI_BACKUP_*` | `FOSNIE_*` | yes — `backup.sh` / `restore.sh` read either |
| `pai-backup-<UTC>/` bundles | `fosnie-backup-<UTC>/` | yes — old bundles restore unchanged and are still pruned by retention |
| `pai-ml.service`, `pai-backup.service`/`.timer` | `fosnie-ml.service`, `fosnie-backup.service`/`.timer` | no — install the new units and `systemctl disable --now` the old ones |
| `com.pai.ml`, `com.pai.backend` launchd jobs | `com.fosnie.ml`, `com.fosnie.backend` | no — `launchctl unload` the old plists first |
| `/etc/pai`, `/var/lib/pai`, `/opt/pai`, `/run/pai`, `/var/backups/pai` | the same under `fosnie` | no — move the directories, or keep your own paths in the unit and config |
| Service account `pai` | `fosnie` | no — the unit templates now expect it; `usermod -l fosnie pai` keeps ownership (the uid is unchanged), or edit `User=`/`Group=` back |
| Prometheus job `pai-ml` | `fosnie-ml` | no — the shipped `alerts.yml` matches the new label, so update the scrape config with it |

The Postgres role and database are still called `pai` in the examples: renaming
them is a data migration, not a config change, so it is left to you.

## What makes a host "incapable" of a feature

`[features]` in the boot config. `code_interpreter = false` means the tool is
never advertised to the model and any dispatch is refused — the binary never
*assumes* Firecracker. macOS leaves it off; a Linux+KVM host opts in.

## llama.cpp tool-calling caveat (Profile B)

`llama-server`'s OpenAI endpoint only emits structured `tool_calls` when run with
a **tool-capable chat template** (e.g. `--jinja` plus a template that supports
tools). Without it, the model's tool intent never reaches the platform and tool
calls silently no-op. This is llama.cpp deployment config — the platform reads
standard OpenAI `tool_calls` either way. Verify with a one-tool smoke chat after
bring-up.

## Inference engine is config, not code

`max_model_len` is *learned* from the server (`/v1/models`) with a config
fallback (`LLM_MAX_MODEL_LEN`). The `qwen3_xml` / `--reasoning-parser` flags are a
**vLLM** concern (Profile A) — they live in that server's launch flags, not in
platform code.

## Observability

The backend exposes Prometheus metrics at **`GET /metrics`** (text exposition
format). It is **fail-closed**: disabled (404) until `FOSNIE__OBSERVABILITY__METRICS_TOKEN`
is set, then required (`Authorization: Bearer <token>` or `?token=`, constant-time
checked) — system telemetry is never readable out of the box. The ML service's
`:8090/metrics` is gated by its shared secret (scrape with `Authorization: Bearer
<ml.shared_secret>`). Keep both on loopback; never proxy `/metrics` publicly. Set
`FOSNIE__OBSERVABILITY__LOG_FORMAT=json` for structured logs.

Series exposed: `http_requests_total{method,route,status}`,
`http_request_duration_seconds`, `chat_turns_total`,
`llm_tokens_total{kind,model}`, `ws_connections`,
`task_runs_total{type,outcome}`, `audit_anomaly_total{action}`,
`ws_origin_rejected_total`, plus process resource gauges (refreshed every 10s):
`process_resident_memory_bytes`, `process_virtual_memory_bytes`,
`process_cpu_usage_percent`.

Example Prometheus scrape job (Prometheus/Grafana/Alertmanager are **not**
bundled — they run alongside, inside the perimeter):

```yaml
scrape_configs:
  - job_name: fosnie-backend
    metrics_path: /metrics
    static_configs: [{ targets: ["127.0.0.1:8080"] }]
    # authorization: { credentials: "<METRICS_TOKEN>" }   # if a token is set
```

Suggested alerts: `audit_anomaly_total` rising (flagged security events —
break-glass use, account-state changes), `task_runs_total{outcome="dead_letter"}`
rising (background failures), readiness probe (`/health/ready`) failing. Flagged
events also surface in **Admin → System → Security alerts**.

## Release SBOM (supply chain)

Every release ships a CycloneDX software bill of materials alongside the signed
tarball + SHA-256 manifest. Generate it
with:

```bash
backend/deploy/scripts/generate-sbom.sh [OUTPUT_DIR]   # default: <repo>/sbom
```

It emits one CycloneDX JSON per ecosystem — `backend.cdx.json` (Rust),
`frontend.cdx.json` (npm), `ml.cdx.json` (Python) — plus `SBOM-MANIFEST.sha256`
over them. It degrades gracefully: a missing generator is skipped, not fatal.

Generator prerequisites in the release/CI environment:

- **Rust** — `cargo install cargo-cyclonedx`.
- **Frontend** — npm ≥ 9 (the script runs `npx @cyclonedx/cyclonedx-npm`, no
  permanent install). It tolerates pruned platform-optional deps (e.g. a native
  binding's WASM fallback), which the built-in `npm sbom` rejects.
- **Python** — `uv` (the script runs `uvx cyclonedx-bom`, no permanent install)
  or a system `cyclonedx-py`.
