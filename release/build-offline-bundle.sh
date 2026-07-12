#!/usr/bin/env bash
# Copyright 2026 Private AI Ltd (SC881079)
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
#
# Offline bundle builder (ts-enterprise-airgap §2, D2). Assembles a single tarball
# that stands the stack up on a BARE-METAL host with NO network — see the companion
# install guide docs/deploy/airgap-install.md.
#
# Bundle contents (bare-metal): release binaries, migrations, systemd units, config
# examples, frontend dist, vendored ML wheels, pre-provisioned offline model cache
# (the fastembed Qdrant/bm25 blocker), SBOMs + manifest + signatures, verify-release
# + airgap-lint, and the air-gap docs. The LLM is NOT bundled — the customer brings
# their own Ollama/vLLM (documented).
#
# Run in an ONLINE build environment (it fetches wheels + the model cache once), then
# ship the resulting tarball to the air-gapped host.
#
# Usage:
#   build-offline-bundle.sh [--edition core|enterprise] [--docker] [OUT_DIR]
#
# Env:
#   PAI_QDRANT_PIN   Qdrant image tag to record for the (future) docker path (default v1.12)

set -uo pipefail

EDITION="core"
DO_DOCKER=0
OUT_DIR="./dist-bundle"
while [ $# -gt 0 ]; do
  case "$1" in
    --edition) EDITION="$2"; shift 2 ;;
    --docker)  DO_DOCKER=1; shift ;;
    -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
    *) OUT_DIR="$1"; shift ;;
  esac
done

CORE_ROOT="$(cd "$(dirname "$0")/.." && pwd)"                 # fosnie-core/
ENT_ROOT="$(cd "$CORE_ROOT/../fosnie-enterprise" 2>/dev/null && pwd || true)"
VERSION="$(grep -m1 '^version' "$CORE_ROOT/backend/Cargo.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
[ -n "$VERSION" ] || VERSION="0.0.0"
STAGE="$OUT_DIR/pai-offline-$EDITION-$VERSION"

echo "building $EDITION offline bundle v$VERSION → $STAGE"
rm -rf "$STAGE"; mkdir -p "$STAGE"

warn() { echo "  ! $1" >&2; }
ok()   { echo "  ✓ $1"; }

stage_file() { # src dst-dir
  if [ -e "$1" ]; then mkdir -p "$2"; cp -R "$1" "$2/" && ok "staged $(basename "$1")"; else warn "missing (skipped): $1"; fi
}

# --- binaries ----------------------------------------------------------------
echo "[bin]"
stage_file "$CORE_ROOT/backend/target/release/fosnie-backend"     "$STAGE/bin"
stage_file "$CORE_ROOT/backend/target/release/fosnie-backend.exe" "$STAGE/bin"
if [ "$EDITION" = "enterprise" ]; then
  if [ -n "$ENT_ROOT" ]; then
    stage_file "$ENT_ROOT/target/release/fosnie-enterprise"     "$STAGE/bin"
    stage_file "$ENT_ROOT/target/release/fosnie-enterprise.exe" "$STAGE/bin"
  else
    warn "enterprise edition requested but ../fosnie-enterprise not found"
  fi
fi

# --- schema + service units + config -----------------------------------------
echo "[deploy]"
stage_file "$CORE_ROOT/backend/migrations"                       "$STAGE"
stage_file "$CORE_ROOT/backend/deploy/systemd"                   "$STAGE/deploy"
stage_file "$CORE_ROOT/backend/deploy/config.linux.example.toml" "$STAGE/config"
stage_file "$CORE_ROOT/ml/.env.linux.example"                    "$STAGE/config"
# ship the offline verifier + airgap lint INSIDE the bundle
mkdir -p "$STAGE/deploy/scripts"
stage_file "$CORE_ROOT/backend/deploy/scripts/verify-release.sh" "$STAGE/deploy/scripts"
stage_file "$CORE_ROOT/backend/deploy/scripts/airgap-lint.sh"    "$STAGE/deploy/scripts"
stage_file "$CORE_ROOT/backend/deploy/scripts/perf-probe.sh"     "$STAGE/deploy/scripts"

# --- frontend ----------------------------------------------------------------
echo "[frontend]"
if [ -d "$CORE_ROOT/frontend/dist" ]; then
  stage_file "$CORE_ROOT/frontend/dist" "$STAGE/frontend"
else
  warn "frontend/dist absent — run 'npm ci && npm run build' (VITE_EDITION as needed) first"
fi

# --- ML wheels (vendored for offline pip install) ----------------------------
echo "[ml wheels]"
if command -v uv >/dev/null 2>&1; then
  mkdir -p "$STAGE/wheels"
  if ( cd "$CORE_ROOT/ml" && uv export --no-hashes -o "$STAGE/wheels/requirements.txt" >/dev/null 2>&1 ); then
    if uv pip download -r "$STAGE/wheels/requirements.txt" -d "$STAGE/wheels" >/dev/null 2>&1 \
       || pip download -r "$STAGE/wheels/requirements.txt" -d "$STAGE/wheels" >/dev/null 2>&1; then
      ok "vendored $(ls "$STAGE/wheels"/*.whl 2>/dev/null | wc -l) wheels"
    else
      warn "wheel download failed — vendor on a matching Linux/arch build host"
    fi
  else
    warn "uv export failed"
  fi
else
  warn "uv absent — cannot vendor ML wheels here (do it on the Linux build host)"
fi

# --- offline models: fastembed Qdrant/bm25 cache (the ONE runtime download) ---
echo "[models]"
MODELS="$STAGE/models/fastembed-cache"; mkdir -p "$MODELS"
if command -v python >/dev/null 2>&1 && python - <<'PY' >/dev/null 2>&1
import importlib.util, sys
sys.exit(0 if importlib.util.find_spec("fastembed") else 1)
PY
then
  if FASTEMBED_CACHE_PATH="$MODELS" python - <<'PY' >/dev/null 2>&1
from fastembed import SparseTextEmbedding
# Instantiating downloads Qdrant/bm25 into FASTEMBED_CACHE_PATH.
SparseTextEmbedding(model_name="Qdrant/bm25")
PY
  then ok "pre-provisioned fastembed Qdrant/bm25 into models/fastembed-cache"
  else warn "fastembed pre-provision failed — populate models/fastembed-cache on the build host"; fi
else
  warn "fastembed not importable here — pre-provision Qdrant/bm25 on the Linux build host"
fi
echo "  note: set FASTEMBED_CACHE_PATH=<install>/models/fastembed-cache + HF_HUB_OFFLINE=1 at install"

# --- SBOMs -------------------------------------------------------------------
echo "[sbom]"
mkdir -p "$STAGE/sbom"
bash "$CORE_ROOT/backend/deploy/scripts/generate-sbom.sh" "$STAGE/sbom" || warn "core SBOM generation incomplete"
if [ "$EDITION" = "enterprise" ] && [ -n "$ENT_ROOT" ] && [ -f "$ENT_ROOT/deploy/scripts/generate-enterprise-sbom.sh" ]; then
  bash "$ENT_ROOT/deploy/scripts/generate-enterprise-sbom.sh" "$STAGE/sbom" || warn "enterprise SBOM generation incomplete"
fi

# --- docs --------------------------------------------------------------------
echo "[docs]"
for d in deploy/airgap-install.md deploy/byok.md security/egress-inventory.md security/airgap-certification.md; do
  stage_file "$CORE_ROOT/docs/$d" "$STAGE/docs"
done

# --- docker hook (documented, not built until prod GHCR images land) ---------
if [ "$DO_DOCKER" -eq 1 ]; then
  echo "[docker]"
  warn "docker-save NOT implemented: prod GHCR images (frontend nginx / backend / ml) from"
  warn "ts-docker-deploy do not exist yet. When they land, 'docker save' them + pinned"
  warn "postgres:17 / qdrant/qdrant:${PAI_QDRANT_PIN:-v1.12} / redis:7 into $STAGE/images/."
fi

# --- version marker ----------------------------------------------------------
{
  echo "edition: $EDITION"
  echo "version: $VERSION"
  echo "built:   $(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$STAGE/BUNDLE-INFO.txt"

# --- manifest + signatures (delegates to sign-release.sh) --------------------
echo "[sign]"
bash "$CORE_ROOT/release/sign-release.sh" "$STAGE" || warn "signing incomplete (no key?) — manifest still written"

# --- tarball -----------------------------------------------------------------
echo "[tar]"
TARBALL="$OUT_DIR/pai-offline-$EDITION-$VERSION.tar.gz"
( cd "$OUT_DIR" && tar -czf "$(basename "$TARBALL")" "$(basename "$STAGE")" ) && ok "bundle: $TARBALL"
SHA="sha256sum"; command -v sha256sum >/dev/null 2>&1 || SHA="shasum -a 256"
( cd "$OUT_DIR" && $SHA "$(basename "$TARBALL")" > "$(basename "$TARBALL").sha256" )
ok "checksum: $TARBALL.sha256"
echo "done. verify on the target with: deploy/scripts/verify-release.sh"
