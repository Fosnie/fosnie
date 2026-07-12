# PAI Platform — Test Deploy on 2× A100 80GB

A **containerised, fast-redeploy** bring-up of the *whole* platform — backend +
ML/RAG + React SPA + Keycloak + the full **prod-version** model stack on an
optimised vLLM. Built for the loop: **deploy → click around → log bugs → Claude
Code fixes → `./redeploy.sh` → repeat.**

This is **not** the production deployment. Production is bare-metal, systemd, no
Docker, zero-egress, gateway+FRP tunnel, JIT access.
This test profile deliberately drops all of that — Docker, host networking, plain
http on localhost, the dev Keycloak realm — so you can get everything running in
one command and iterate quickly. **Do not point a real client at it.**

---

## What comes up

**Everything** — the full platform plus every optional subsystem (voice,
groundedness, web-search, workflows, code-interpreter), not a stripped core.

| Layer | Service | Port | GPU | Notes |
|---|---|---|---|---|
| Platform | `fosnie-backend` (Rust + SPA) | 8080 | — | API + WebSocket + React bundle; orchestrates Firecracker |
| Platform | `pai-ml` (Python ML/RAG) | 8090 | — | HTTP client of the inference stack — small image |
| Inference | `vllm-main` — **Huihui-Qwen3.6-27B-abliterated-AWQ** | 8000 | **GPU 0+1** | `--tensor-parallel-size 2`, util 0.75 |
| Inference | `vllm-ocr` — **zai-org/GLM-OCR** | 8002 | GPU 1 | document extraction (0.9B, MIT) |
| Inference | `vllm-embed` — **Qwen3-Embedding-4B** | 8001 | GPU 1 | `/v1/embeddings`, `--runner pooling` |
| Inference | `reranker` — **Qwen3-Reranker-0.6B** | 8091 | GPU 1 | llama.cpp `/v1/rerank` |
| Voice | `stt` — **Qwen3-ASR-1.7B** | 8092 | GPU 0 | dictation; `STT_FORMAT=chat` |
| Voice | `tts` — **kokoro-fastapi** | 8880 | CPU | read-aloud; OpenAI `/v1/audio/speech` |
| Groundedness | `verify` — **LettuceDetect** (+NLI/FactCG/HHEM) | 8095 | CPU | `/v1/verify`; runs post-stream |
| Web search | `searxng` | 8888 | CPU | SERP for `web_search` (dormant until enabled) |
| Identity | `keycloak` (dev realm) | 8081 | — | seed users alice/bob/carol |
| Data | `postgres` 17 / `redis` 7 / `qdrant` | 5432 / 6379 / 6333 | — | backend auto-migrates on boot |

In-process subsystems need no extra container: **workflows** (event-driven engine)
and **code-interpreter** (the backend orchestrates Firecracker microVMs directly —
see [the one caveat](#code-interpreter-firecracker) below).

### GPU budget on 2× A100 (why it fits)

The main model is **tensor-parallel across both cards** at `--gpu-memory-utilization
0.75`, reserving 75 % of *each* 80 GB card and leaving ~20 GB free per card. The
support models are ordered (`depends_on: service_healthy`) so the main model claims
its share **first**, then:

- **GPU 1** (~20 GB free): OCR (~8 GB) + embedding (~8 GB) + reranker (~1.5 GB).
- **GPU 0** (~20 GB free): STT (~1.5 GB).
- **CPU:** TTS, verifier, SearXNG — none are latency-critical.

If `vllm-main` OOMs on startup, nudge its util to `0.72` (or a support util down a
notch); the headroom is deliberately tight to keep the main model big.

### Why these model choices

- **Main = `Huihui-Qwen3.6-27B-abliterated-AWQ`, TP-2, int4 — not bf16/FP8.** The
  A100 is **Ampere — no native FP8 *compute***, so the
  weight-quant lever is **AWQ/GPTQ int4**. Sharding the 27B-AWQ over both cards at
  0.75 util gives a large KV cache *and* room for the support models. `fp8` KV
  cache still helps on Ampere (memory only), so it's on.
  > ⚠ `abliterated` = an **uncensored** fine-tune (safety filtering removed). Fine
  > for internal testing; for a **public** hub demo a stock `Qwen3-…-Instruct/-AWQ`
  > is the safer face — swap `LLM_MODEL` only. See [`PUBLIC-ACCESS.md`](PUBLIC-ACCESS.md).
- **Pinned vLLM tool/reasoning parsers** — `--tool-call-parser qwen3_xml` +
  `--reasoning-parser qwen3` (08 §B.16). A wrong version silently fails to populate
  `tool_calls` while the box *looks* healthy.
- **Embedding = Qwen3-Embedding-4B**, the quality target. Fixed at install —
  changing size means a full re-embed.

---

## Prerequisites (on the GPU box)

1. **Ubuntu 22.04+**, 2× A100 80GB, recent **NVIDIA driver** (`nvidia-smi` works).
2. **Docker Engine** + **Docker Compose v2** (`docker compose version`).
3. **NVIDIA Container Toolkit** so containers see the GPUs:
   ```bash
   nvidia-ctk --version          # installed?
   docker run --rm --gpus all nvidia/cuda:12.4.0-base-ubuntu22.04 nvidia-smi   # smoke test
   ```
   If the smoke test fails, install the toolkit and `sudo nvidia-ctk runtime configure --runtime=docker && sudo systemctl restart docker`.
4. **Disk:** ~80–120 GB free for model weights (cached in a Docker volume, downloaded once).
5. A **Hugging Face token** (read scope) for model pulls.

---

## Bring-up (first time)

```bash
cd backend/deploy/test
cp .env.test.example .env
# edit .env: set HF_TOKEN and ML_SHARED_SECRET (openssl rand -hex 32)

cp ml.test.env.example ml.test.env
# ml.test.env has no secrets — LLM_API_KEY=dummy is correct for vLLM

docker compose --env-file .env -f docker-compose.test.yml up -d --build
```

First boot pulls the model weights (several GB each) — give `vllm-main` **5–15
min**. Watch progress:

```bash
docker compose --env-file .env -f docker-compose.test.yml logs -f vllm-main
# look for: "Application startup complete" / "Uvicorn running on http://127.0.0.1:8000"
```

Then reach it from your laptop over an SSH tunnel (the dev realm's redirect URIs
are `http://localhost/*`, so tunnelling Keycloak too is what makes login work):

```bash
ssh -L 8080:localhost:8080 -L 8081:localhost:8081 user@<box>
# open http://localhost:8080  → log in as alice / alice  (seed user)
```

### Health checklist

```bash
curl -s localhost:8000/health        && echo " ✓ main LLM"
curl -s localhost:8002/health        && echo " ✓ OCR"
curl -s localhost:8001/health        && echo " ✓ embeddings"
curl -s localhost:8091/health        && echo " ✓ reranker"
curl -s localhost:8092/health        && echo " ✓ STT"
curl -s localhost:8880/health        && echo " ✓ TTS"
curl -s localhost:8095/version       && echo " ✓ verifier"
curl -s "localhost:8888/search?q=test&format=json" >/dev/null && echo " ✓ searxng"
curl -s localhost:8081/health/ready  && echo " ✓ keycloak"
curl -s localhost:8090/health        && echo " ✓ ml"
curl -s localhost:8080/health/ready  && echo " ✓ backend (pg+redis ok)"
```

Feature smokes (the UI surfaces each only when its engine answers `/api/whoami`):

- **Tool calls** — ask an Agent something that forces a tool/RAG call; verify it
  retrieves rather than silently answering blank (confirms the pinned parser).
- **Voice** — the mic + read-aloud buttons appear; dictate a prompt, read a reply aloud.
- **Groundedness** — ask a question over an attached KB; a groundedness pill appears.
- **Web search** — first flip it on in **Admin → Integrations → web_search** (it's a
  dormant connector by design), then ask something current.
- **Code interpreter** — see the caveat below; needs the Firecracker rootfs built.

---

## The fast redeploy loop

This is the point of the whole setup. After Claude Code fixes a bug:

```bash
./redeploy.sh            # rebuild + restart backend + ml only
./redeploy.sh backend    # just the Rust backend (+ SPA)
./redeploy.sh ml         # just the Python ML service
./redeploy.sh logs       # tail backend + ml logs
./redeploy.sh all        # full rebuild incl. inference (rare — reloads models)
./redeploy.sh down       # stop everything
```

`redeploy.sh` rebuilds **only the platform images** and restarts them with
`--no-deps`, so vLLM/Keycloak/datastores keep running and **the model weights
stay warm in GPU**. A platform redeploy is **seconds**; you never pay the cold
model-load cost again until you choose `all` or `down`.

Typical iteration:

1. Deploy, click through the UI, hit a bug, jot it down.
2. Hand the repro to Claude Code; it edits `backend/` / `ml/` / `frontend/`.
3. `./redeploy.sh` (or `backend` / `ml` to be even faster).
4. `./redeploy.sh logs` if it misbehaves. Back to step 1.

Docker layer caching means an `ml`-only fix rebuilds in ~10–20 s; a backend Rust
change is a cargo incremental compile.

---

## Tuning vLLM (optional, when you want the real numbers)

The compose ships a sensible optimised baseline (prefix caching on, fp8 KV cache,
FlashInfer backend, `awq_marlin`). To actually *measure* the levers on your
hardware and produce a before/after table for a demo,
change **one** flag at a time and re-run `vllm bench serve`. Relevant knobs live
in the `vllm-main` `command:` block: `--gpu-memory-utilization`,
`--max-num-seqs`, `--max-num-batched-tokens`, `--kv-cache-dtype`,
`VLLM_ATTENTION_BACKEND`.

> **Quality alternative — full bf16.** If you'd rather run the main model in bf16
> (no quant) for max quality, point `LLM_MODEL` at the bf16 repo and drop
> `--quantization awq_marlin`. It still tensor-parallels over both cards; you may
> need to lower `--gpu-memory-utilization` so the support models keep their room.

---

## Everything is ON — and the one caveat

Unlike a stripped "core" bring-up, this stack runs **all** optional subsystems:
voice (STT+TTS), groundedness verification, web-search, workflows, and the
code-interpreter. The feature flags in `config.test.toml` are all `true` and every
external engine is wired in the compose. Two notes:

- **Web search ships dormant by design** (zero-egress posture). The `searxng`
  service is up, but the `web_search` tool stays hidden until you flip
  **Admin → Integrations → web_search** on at runtime — that's the platform's
  intended gate, not a missing piece.

### Code interpreter (Firecracker)

This is the **only** subsystem that doesn't fully "just work" in Docker, and it's
the repo's own constraint, not a shortcut: `deploy/firecracker/README.md` states
**"WSL2 and Docker are not supported KVM hosts."** The backend orchestrates
Firecracker microVMs directly and needs `/dev/kvm` plus a built VM image. What's
already wired here: `features.code_interpreter = true`, the `[code_interpreter_vm]`
config, and the backend container runs `privileged` with `/dev/kvm` passed through.
What **you** still provide (a Pass-2 deployment artefact, per that README):

1. Build `vmlinux` + `rootfs.ext4` (Linux + Python + pandas/numpy/matplotlib/openpyxl)
   + the guest agent, and drop them in `./firecracker/`.
2. Ensure the host exposes **nested KVM** to the container (`ls /dev/kvm` on the box).

If your box can't expose KVM to a container, the clean fallback is to run the
**backend binary natively** on the host (it still talks to the containerised
datastores/models over `127.0.0.1`) — that's the supported KVM path. Everything
*else* (voice, groundedness, web-search, workflows, RAG, documents) runs fully in
the container stack regardless.

---

## Public access — a URL for the hubs

Want `https://chat.example.com` so people just click a link? It's a small
add-on: one Cloudflare Tunnel container + two DNS hostnames + a ~6-line Keycloak
tweak, no open ports. Full steps in **[`PUBLIC-ACCESS.md`](PUBLIC-ACCESS.md)**:

```bash
docker compose --env-file .env \
  -f docker-compose.test.yml -f docker-compose.public.yml up -d
```

---

## Security reality check (so nobody ships this by accident)

This profile **violates the production rules on purpose** and must never face a
client:

- Dev Keycloak realm with **public seed creds** (alice/bob/carol) and a
  **public client secret** (`fosnie-secret`). Production provisions a
  hardened realm — [`../keycloak/PRODUCTION-REALM.md`](../keycloak/PRODUCTION-REALM.md).
- **Plain http on localhost** (only valid because it's loopback behind your SSH
  tunnel; the backend's `validate()` refuses http on a public host).
- Postgres `pai/pai`, no TLS between services, `/metrics` open-by-omission.
- **Docker**, which production client deployments don't use at all.

For the real thing, follow the systemd profile and the production-hardening checklist.

---

## Files in this folder

| File | Purpose |
|---|---|
| `docker-compose.test.yml` | the full stack — datastores + Keycloak + all models + voice/verify/search + platform |
| `docker-compose.public.yml` | overlay: Cloudflare Tunnel + public hostnames (use with the base file) |
| `Dockerfile.backend` | SPA build → Rust build (sqlx offline) → slim runtime |
| `Dockerfile.ml` | ML service + pandoc/LibreOffice/WeasyPrint + ffmpeg + Chromium |
| `Dockerfile.verify` | groundedness verifier sidecar (LettuceDetect + NLI/FactCG/HHEM) |
| `config.test.toml` | backend boot config — all features on, voice-live + Firecracker blocks |
| `ml.test.env` | ML service env (endpoints, models, voice/verify/web settings) — gitignored, copy from example |
| `ml.test.env.example` | template for `ml.test.env` → `cp ml.test.env.example ml.test.env` on a fresh server |
| `.env.test.example` | compose secrets/model/tunnel template → copy to `.env` |
| `redeploy.sh` | the fast fix→redeploy loop |
| `PUBLIC-ACCESS.md` | put the demo on the internet at `chat.example.com` |
| `firecracker/` | drop your built `vmlinux` + `rootfs.ext4` here (code-interpreter) |

---

## Troubleshooting

- **`vllm-main` OOM / won't start.** Lower `--gpu-memory-utilization` to `0.72`,
  or `--max-model-len` to `32768`. Confirm AWQ weights load (`awq_marlin` in the
  logs) and that it sees **both** GPUs (`--tensor-parallel-size 2`).
- **A support model OOMs after main is up.** The ~20 GB/card is tight — trim its
  `--gpu-memory-utilization` (OCR/embed) or drop main to `0.72` to free more.
- **STT returns nothing / odd text.** `STT_FORMAT` must be `chat` for Qwen3-ASR;
  ffmpeg must be in the ML image (it is) so browser Opus/WebM transcodes to WAV.
- **Code interpreter refused.** Build the Firecracker `vmlinux`/`rootfs.ext4` into
  `./firecracker` and confirm `/dev/kvm` exists on the box — see the caveat above.
- **FlashInfer not found.** Drop `VLLM_ATTENTION_BACKEND: FLASHINFER` from
  `vllm-main` — vLLM falls back to its default backend.
- **`vllm-embed` rejects `--runner pooling`.** You're on an older vLLM — swap it
  for `--task embed` (the pre-rename flag). Newer builds may also auto-detect the
  embedding runner with no flag at all.
- **`ml.test.env` not showing in git.** The repo `.gitignore` ignores `*.env`
  (only `*.env.example` is kept). It has no secrets — `git add -f
  backend/deploy/test/ml.test.env` if you want it tracked.
- **Tool calls silently do nothing.** Check `vllm-main` logs for `qwen3_xml`
  parser load; a mismatched vLLM version is the usual cause (pin `VLLM_TAG`).
- **Login redirect fails.** You must tunnel **8081** too; the dev realm's
  redirect URIs are `http://localhost/*`.
- **`reranker` rejects long chunks.** `-b/-ub` are already raised to the 8192
  context in the compose; if you raise context further, raise them to match.
- **Backend `database_url is required` / won't boot.** The `PAI__DATABASE_URL`
  env is set in compose — check the `.env` was passed (`--env-file .env`).
- **A model re-downloads every restart.** It shouldn't — weights live in the
  `pai_hf_cache` volume. Don't `docker compose down -v` (that wipes volumes).
