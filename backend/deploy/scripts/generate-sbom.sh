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
# PAI Platform SBOM generator. Emits a CycloneDX software bill of materials per
# ecosystem (Rust backend / React frontend / Python ML) for a release, into an
# output directory, plus a SHA-256 manifest of the produced files. Part of the
# signed-release supply-chain story (signed tarball + SHA-256 manifest + SBOM per release).
#
# Degrades gracefully: a missing generator is warned and skipped, never fatal —
# so a partial SBOM is still produced. Exits non-zero only if NOTHING was made.
#
# Generator tools (install in the release/CI environment):
#   - Rust     : cargo install cargo-cyclonedx
#   - Frontend : npm >= 9 (built-in `npm sbom`); needs a consistent install tree
#                (run `npm ci` first — a broken/partial node_modules makes npm
#                 refuse with ESBOMPROBLEMS).
#   - Python   : uv (uses `uvx cyclonedx-bom` — no permanent install), or a
#                system `cyclonedx-py`.
#
# Usage: generate-sbom.sh [OUTPUT_DIR]   (default: <repo>/sbom)

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
OUT_DIR="${1:-$REPO_ROOT/sbom}"
mkdir -p "$OUT_DIR"
echo "SBOM output: $OUT_DIR"

MADE=()

note_ok()   { echo "  ✓ $1"; MADE+=("$2"); }
note_skip() { echo "  ✗ skip $1 — $2"; }

# --- Rust (backend) ----------------------------------------------------------
echo "[rust] backend"
if command -v cargo-cyclonedx >/dev/null 2>&1; then
  if ( cd "$REPO_ROOT/backend" && cargo cyclonedx --format json >/dev/null 2>&1 ); then
    # cargo-cyclonedx writes <crate>.cdx.json beside Cargo.toml; collect it.
    src="$(ls "$REPO_ROOT"/backend/*.cdx.json 2>/dev/null | head -1)"
    if [ -n "${src:-}" ]; then
      mv "$src" "$OUT_DIR/backend.cdx.json" && note_ok "rust" "$OUT_DIR/backend.cdx.json"
    else
      note_skip "rust" "cargo-cyclonedx produced no .cdx.json"
    fi
  else
    note_skip "rust" "cargo-cyclonedx failed"
  fi
else
  note_skip "rust" "install: cargo install cargo-cyclonedx"
fi

# --- Frontend (npm) ----------------------------------------------------------
# Uses the dedicated CycloneDX npm tool rather than npm's built-in `npm sbom`,
# which refuses (ESBOMPROBLEMS) when platform-optional deps are pruned — common
# for native bindings' WASM fallbacks (e.g. lightningcss → @napi-rs/wasm-runtime
# → @emnapi). `--ignore-npm-errors` tolerates those `npm ls` warnings.
echo "[frontend]"
if command -v npx >/dev/null 2>&1; then
  if ( cd "$REPO_ROOT/frontend" && npx --yes @cyclonedx/cyclonedx-npm --ignore-npm-errors \
         --output-format JSON --output-file "$OUT_DIR/frontend.cdx.json" >/dev/null 2>&1 ) \
     && [ -s "$OUT_DIR/frontend.cdx.json" ]; then
    note_ok "frontend" "$OUT_DIR/frontend.cdx.json"
  else
    rm -f "$OUT_DIR/frontend.cdx.json"
    note_skip "frontend" "@cyclonedx/cyclonedx-npm failed"
  fi
else
  note_skip "frontend" "needs npx (npm >= 9)"
fi

# --- Python (ml) -------------------------------------------------------------
echo "[python] ml"
REQ="$(mktemp)"
trap 'rm -f "$REQ"' EXIT
if command -v uv >/dev/null 2>&1 && ( cd "$REPO_ROOT/ml" && uv export --no-hashes >"$REQ" 2>/dev/null ); then
  if command -v cyclonedx-py >/dev/null 2>&1; then
    PYGEN=(cyclonedx-py)
  elif command -v uvx >/dev/null 2>&1; then
    PYGEN=(uvx --from cyclonedx-bom cyclonedx-py)
  else
    PYGEN=()
  fi
  if [ ${#PYGEN[@]} -gt 0 ]; then
    if "${PYGEN[@]}" requirements "$REQ" --output-format JSON --output-file "$OUT_DIR/ml.cdx.json" >/dev/null 2>&1; then
      note_ok "python" "$OUT_DIR/ml.cdx.json"
    else
      note_skip "python" "cyclonedx-py failed"
    fi
  else
    note_skip "python" "install: uv tool install cyclonedx-bom"
  fi
else
  note_skip "python" "uv export unavailable"
fi

# --- Manifest (SHA-256) ------------------------------------------------------
if [ ${#MADE[@]} -eq 0 ]; then
  echo "no SBOMs generated — install the generator tools listed above" >&2
  exit 1
fi

SHA="sha256sum"
command -v sha256sum >/dev/null 2>&1 || SHA="shasum -a 256"
( cd "$OUT_DIR" && $SHA $(printf '%s\n' "${MADE[@]##*/}") > SBOM-MANIFEST.sha256 )
echo "manifest: $OUT_DIR/SBOM-MANIFEST.sha256"
cat "$OUT_DIR/SBOM-MANIFEST.sha256"
