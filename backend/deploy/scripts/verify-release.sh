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
# Offline release verifier. Ships INSIDE the bundle;
# a customer's security team runs it on a machine with NO network to confirm the
# release is intact and authentically signed, in one pass:
#
#   [1] every artefact matches MANIFEST.sha256 (integrity)
#   [2] the manifest signature verifies against the shipped public key —
#       openssl (always checkable) and, when the tools are present, minisign / cosign
#   [3] the SBOM manifest(s) match (backend/frontend/ml + enterprise if present)
#
# Corrupting any byte of any artefact fails step 1; a forged manifest fails step 2.
# Exit 0 = PASS; non-zero = the number of failed checks.
#
# Co-located with airgap-lint.sh in deploy/scripts/ (deploy/verify/ is the
# groundedness sidecar, not a release verifier).
#
# Usage: verify-release.sh [BUNDLE_DIR]   (default: the script's grandparent, i.e.
#        the bundle root when this script sits at <bundle>/deploy/scripts/)

set -uo pipefail

BUNDLE_DIR="${1:-$(cd "$(dirname "$0")/../.." && pwd)}"
MANIFEST="MANIFEST.sha256"
FAILS=0

pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; FAILS=$((FAILS + 1)); }
info() { printf '  ..    %s\n' "$1"; }

SHA="sha256sum"; command -v sha256sum >/dev/null 2>&1 || SHA="shasum -a 256"

cd "$BUNDLE_DIR" || { echo "not a directory: $BUNDLE_DIR" >&2; exit 2; }
echo "verify-release — offline integrity + authenticity ($BUNDLE_DIR)"
echo

# ---------------------------------------------------------------------------
echo "[1] Artefact integrity (MANIFEST.sha256)"
if [ ! -f "$MANIFEST" ]; then
  fail "$MANIFEST missing — cannot verify integrity"
else
  if $SHA -c "$MANIFEST" >/tmp/vr_sha.$$ 2>&1; then
    pass "$(grep -c ': OK$' /tmp/vr_sha.$$ 2>/dev/null || wc -l < "$MANIFEST") files match the manifest"
  else
    fail "manifest check failed:"; grep -v ': OK$' /tmp/vr_sha.$$ | sed 's/^/        /'
  fi
  rm -f /tmp/vr_sha.$$
fi

# ---------------------------------------------------------------------------
echo "[2] Manifest signature (authenticity)"
verified_any=0
# openssl (RSA/ECDSA) — the always-available path.
if [ -f "$MANIFEST.sig" ] && [ -f "SIGNING-PUBKEY.pem" ] && command -v openssl >/dev/null 2>&1; then
  if openssl dgst -sha256 -verify SIGNING-PUBKEY.pem -signature "$MANIFEST.sig" "$MANIFEST" >/dev/null 2>&1; then
    pass "openssl signature valid (SIGNING-PUBKEY.pem)"; verified_any=1
  else
    fail "openssl signature INVALID — manifest not authentic"
  fi
else
  info "openssl signature not present/checkable (need $MANIFEST.sig + SIGNING-PUBKEY.pem)"
fi
# minisign.
if [ -f "$MANIFEST.minisig" ] && [ -f "SIGNING-PUBKEY.minisign" ] && command -v minisign >/dev/null 2>&1; then
  if minisign -Vm "$MANIFEST" -x "$MANIFEST.minisig" -p SIGNING-PUBKEY.minisign >/dev/null 2>&1; then
    pass "minisign signature valid"; verified_any=1
  else
    fail "minisign signature INVALID"
  fi
else
  info "minisign signature not checked (tool/key/sig absent)"
fi
# cosign (self-managed key, offline).
if [ -f "$MANIFEST.cosig" ] && [ -f "SIGNING-PUBKEY.cosign" ] && command -v cosign >/dev/null 2>&1; then
  if cosign verify-blob --offline --key SIGNING-PUBKEY.cosign --signature "$MANIFEST.cosig" "$MANIFEST" >/dev/null 2>&1; then
    pass "cosign signature valid (offline)"; verified_any=1
  else
    fail "cosign signature INVALID"
  fi
else
  info "cosign signature not checked (tool/key/sig absent)"
fi
[ "$verified_any" -eq 0 ] && fail "NO signature could be verified — supply at least one public key + signature"

# ---------------------------------------------------------------------------
echo "[3] SBOM manifests"
for m in sbom/SBOM-MANIFEST.sha256 sbom/ENTERPRISE-SBOM-MANIFEST.sha256; do
  if [ -f "$m" ]; then
    if ( cd "$(dirname "$m")" && $SHA -c "$(basename "$m")" >/dev/null 2>&1 ); then
      pass "SBOM manifest OK: $m"
    else
      fail "SBOM manifest mismatch: $m"
    fi
  else
    info "not present: $m"
  fi
done

echo
if [ "$FAILS" -eq 0 ]; then
  echo "RESULT: PASS — release is intact and authentically signed"
  exit 0
else
  echo "RESULT: FAIL — $FAILS check(s) failed; do NOT trust this release"
  exit "$FAILS"
fi
