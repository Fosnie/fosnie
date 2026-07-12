#!/usr/bin/env sh
# Fosnie one-line installer. Downloads the pinned compose + env for a release,
# generates fresh secrets, and brings the stack up. Safe to re-run (idempotent —
# it will not overwrite an existing .env).
#
#   curl -fsSL https://github.com/Fosnie/fosnie/releases/latest/download/install.sh | sh
#   # fully local (no API keys, all inference on this host):
#   curl -fsSL .../install.sh | sh -s -- --local
#
# The safe form is: download → read it → run it. All logic lives in functions and
# `main` is called at the very end, so a truncated download cannot half-execute.
set -eu

REPO="Fosnie/fosnie"                 # ← swapped at repo-publish time
VER="${FOSNIE_VERSION:-latest}"          # a release tag (e.g. v1.2.0) or "latest"
DIR="${FOSNIE_DIR:-fosnie}"
LOCAL=0
# LLM + embedding models pulled into Ollama for --profile local. Small, CPU-friendly.
OLLAMA_LLM="${OLLAMA_LLM:-qwen3:4b}"
OLLAMA_EMBED="${OLLAMA_EMBED:-hf.co/ggml-org/bge-m3-Q8_0-GGUF:Q8_0}"

log()  { printf '\033[1;36m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33m!! \033[0m %s\n' "$1" >&2; }
die()  { printf '\033[1;31mxx \033[0m %s\n' "$1" >&2; exit 1; }

asset_url() {
  if [ "$VER" = "latest" ]; then
    echo "https://github.com/$REPO/releases/latest/download/$1"
  else
    echo "https://github.com/$REPO/releases/download/$VER/$1"
  fi
}

fetch() { # fetch <asset> <dest>
  url=$(asset_url "$1")
  if command -v curl >/dev/null 2>&1; then curl -fsSL "$url" -o "$2"
  elif command -v wget >/dev/null 2>&1; then wget -qO "$2" "$url"
  else die "need curl or wget"; fi
}

require() {
  command -v docker >/dev/null 2>&1 || die "Docker is not installed — see https://docs.docker.com/engine/install/"
  docker compose version >/dev/null 2>&1 || die "Docker Compose v2 not found (need 'docker compose', ≥ 2.24)"
  docker info >/dev/null 2>&1 || die "the Docker daemon is not running (or you lack permission — try: sudo usermod -aG docker \$USER)"
  command -v openssl >/dev/null 2>&1 || die "need openssl to generate secrets"
}

parse_args() {
  for a in "$@"; do
    case "$a" in
      --local) LOCAL=1 ;;
      --version=*) VER="${a#*=}" ;;
      --dir=*) DIR="${a#*=}" ;;
      -h|--help) echo "usage: install.sh [--local] [--version=vX.Y.Z] [--dir=PATH]"; exit 0 ;;
      *) warn "ignoring unknown argument: $a" ;;
    esac
  done
}

download() {
  mkdir -p "$DIR"; cd "$DIR"
  log "downloading pinned compose + env ($VER)…"
  fetch docker-compose.yml docker-compose.yml
  if [ -f .env ]; then
    warn ".env already exists — keeping it (delete it to regenerate secrets)"
    KEEP_ENV=1
  else
    fetch example.env .env
    KEEP_ENV=0
  fi
}

set_kv() { # set_kv KEY VALUE  — replace or append KEY=VALUE in .env (| is safe: none of our values contain it)
  if grep -q "^$1=" .env; then
    sed -i.bak "s|^$1=.*|$1=$2|" .env && rm -f .env.bak
  else
    printf '%s=%s\n' "$1" "$2" >> .env
  fi
}

secrets() {
  [ "${KEEP_ENV:-0}" = "1" ] && { log "reusing existing secrets in .env"; return; }
  log "generating secrets…"
  set_kv POSTGRES_PASSWORD "$(openssl rand -hex 24)"
  set_kv MESSAGE_ENCRYPTION_KEY "$(openssl rand -base64 32)"   # base64(32B) — required format
  set_kv ML_SHARED_SECRET "$(openssl rand -hex 32)"
  # Image tags are SemVer-normalised by the release workflow (v1.2.0 → 1.2.0), but
  # the release-asset URLs (asset_url) need the raw v-prefixed tag. Strip a single
  # leading "v" here so the image tag resolves; "latest"/unprefixed pass through.
  set_kv APP_VERSION "${VER#v}"
  chmod 600 .env
}

bring_up() {
  if [ "$LOCAL" = "1" ]; then
    set_kv LOCAL_STACK 1
    log "starting stack (--profile local: Ollama + reranker)…"
    docker compose --profile local pull
    docker compose --profile local up -d
    pull_models
  else
    log "starting stack (external inference)…"
    docker compose pull
    docker compose up -d
  fi
}

pull_models() {
  log "waiting for Ollama…"
  i=0; while [ "$i" -lt 60 ]; do
    docker compose exec -T ollama ollama list >/dev/null 2>&1 && break
    i=$((i+1)); sleep 2
  done
  log "pulling local models (first run only; this can take a while)…"
  docker compose exec -T ollama ollama pull "$OLLAMA_LLM"   || warn "failed to pull $OLLAMA_LLM — pull it later: docker compose exec ollama ollama pull $OLLAMA_LLM"
  docker compose exec -T ollama ollama pull "$OLLAMA_EMBED" || warn "failed to pull $OLLAMA_EMBED — pull it later"
}

wait_healthy() {
  port=$(grep -E '^HOST_PORT=' .env | cut -d= -f2); port="${port:-8080}"
  log "waiting for the backend on http://localhost:$port …"
  i=0; while [ "$i" -lt 90 ]; do
    if command -v curl >/dev/null 2>&1; then
      curl -fsS "http://localhost:$port/health" >/dev/null 2>&1 && { ready=1; break; }
    else
      wget -qO- "http://localhost:$port/health" >/dev/null 2>&1 && { ready=1; break; }
    fi
    i=$((i+1)); sleep 2
  done
  echo
  if [ "${ready:-0}" = "1" ]; then
    log "Fosnie is up 🎉"
    echo "   Open  http://localhost:$port  and create the first account — it becomes the admin."
    [ "$LOCAL" = "1" ] && echo "   Local models are wired up; just start chatting." \
                       || echo "   Then add a model provider under Settings → Providers to enable chat."
  else
    warn "backend did not report healthy in time. Check logs:  (cd $DIR && docker compose logs -f backend)"
  fi
}

main() {
  parse_args "$@"
  require
  download
  secrets
  bring_up
  wait_healthy
}

main "$@"
