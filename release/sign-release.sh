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
# Release signer (ts-enterprise-airgap §1, D1). Builds a SHA-256 manifest over an
# assembled release tree and signs it via a DUAL PATH so a customer's security team
# can verify with OR without our tooling — offline, no Rekor/OIDC:
#
#   * openssl  — a detached RSA/ECDSA signature of the manifest (always, when a key
#                is provided). The universally-available path.
#   * minisign — an alternative detached signature (when `minisign` + a key present).
#   * cosign   — self-managed keypair (NOT keyless): `cosign sign-blob` the manifest
#                and `cosign attest --type cyclonedx` each SBOM (when `cosign` + a key
#                present). For OCI/Sigstore-native customers.
#
# The PRIVATE keys come from the release environment (env vars below) and are NEVER
# committed. The PUBLIC key is copied into the tree (also publish it in the docs and
# on the website — three sources to cross-check).
#
# Usage:
#   sign-release.sh RELEASE_DIR
#   sign-release.sh --gen-test-key DIR     # RSA keypair for local/CI verification ONLY
#
# Env (all optional; a missing key skips that path with a note):
#   PAI_RELEASE_OPENSSL_KEY   private PEM (RSA or ECDSA) for the openssl signature
#   PAI_RELEASE_OPENSSL_PUB   matching public PEM (copied into the tree as SIGNING-PUBKEY.pem)
#   PAI_RELEASE_MINISIGN_KEY  minisign secret key file
#   PAI_RELEASE_COSIGN_KEY    cosign private key file (COSIGN_PASSWORD in env)

set -uo pipefail

MANIFEST="MANIFEST.sha256"

gen_test_key() {
  dir="$1"; mkdir -p "$dir"
  command -v openssl >/dev/null 2>&1 || { echo "openssl required" >&2; exit 2; }
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out "$dir/release-test.key" 2>/dev/null
  openssl rsa -in "$dir/release-test.key" -pubout -out "$dir/release-test.pub" 2>/dev/null
  echo "test keypair written to $dir/release-test.{key,pub}"
  echo "use: PAI_RELEASE_OPENSSL_KEY=$dir/release-test.key PAI_RELEASE_OPENSSL_PUB=$dir/release-test.pub sign-release.sh RELEASE_DIR"
}

if [ "${1:-}" = "--gen-test-key" ]; then
  gen_test_key "${2:?usage: sign-release.sh --gen-test-key DIR}"
  exit 0
fi

RELEASE_DIR="${1:?usage: sign-release.sh RELEASE_DIR}"
[ -d "$RELEASE_DIR" ] || { echo "not a directory: $RELEASE_DIR" >&2; exit 2; }

SHA="sha256sum"; command -v sha256sum >/dev/null 2>&1 || SHA="shasum -a 256"

echo "signing release tree: $RELEASE_DIR"
cd "$RELEASE_DIR"

# 1. Manifest over every file EXCEPT the manifest + signature artefacts themselves.
find . -type f \
  ! -name "$MANIFEST" \
  ! -name "$MANIFEST.sig" \
  ! -name "$MANIFEST.minisig" \
  ! -name "$MANIFEST.cosig" \
  ! -name "SIGNING-PUBKEY.pem" \
  | LC_ALL=C sort \
  | sed 's#^\./##' \
  | while IFS= read -r f; do $SHA "$f"; done > "$MANIFEST"
echo "  ✓ manifest: $(wc -l < "$MANIFEST") files → $MANIFEST"

signed=0

# 2a. openssl detached signature (RSA/ECDSA).
if [ -n "${PAI_RELEASE_OPENSSL_KEY:-}" ] && command -v openssl >/dev/null 2>&1; then
  if openssl dgst -sha256 -sign "$PAI_RELEASE_OPENSSL_KEY" -out "$MANIFEST.sig" "$MANIFEST"; then
    echo "  ✓ openssl signature: $MANIFEST.sig"
    signed=1
    [ -n "${PAI_RELEASE_OPENSSL_PUB:-}" ] && cp "$PAI_RELEASE_OPENSSL_PUB" SIGNING-PUBKEY.pem \
      && echo "  ✓ public key: SIGNING-PUBKEY.pem"
  else
    echo "  ✗ openssl signing failed" >&2
  fi
else
  echo "  .. openssl path skipped (set PAI_RELEASE_OPENSSL_KEY)"
fi

# 2b. minisign alternative.
if [ -n "${PAI_RELEASE_MINISIGN_KEY:-}" ] && command -v minisign >/dev/null 2>&1; then
  if minisign -Sm "$MANIFEST" -s "$PAI_RELEASE_MINISIGN_KEY" -x "$MANIFEST.minisig" >/dev/null 2>&1; then
    echo "  ✓ minisign signature: $MANIFEST.minisig"
    signed=1
  else
    echo "  ✗ minisign signing failed" >&2
  fi
else
  echo "  .. minisign path skipped (needs minisign + PAI_RELEASE_MINISIGN_KEY)"
fi

# 2c. cosign (self-managed keypair) — blob signature + CycloneDX attestations.
if [ -n "${PAI_RELEASE_COSIGN_KEY:-}" ] && command -v cosign >/dev/null 2>&1; then
  if cosign sign-blob --yes --key "$PAI_RELEASE_COSIGN_KEY" --output-signature "$MANIFEST.cosig" "$MANIFEST" >/dev/null 2>&1; then
    echo "  ✓ cosign signature: $MANIFEST.cosig"
    signed=1
  else
    echo "  ✗ cosign sign-blob failed" >&2
  fi
  # Attest each CycloneDX SBOM present (predicate = the SBOM file).
  for sbom in sbom/*.cdx.json; do
    [ -e "$sbom" ] || continue
    cosign attest-blob --yes --key "$PAI_RELEASE_COSIGN_KEY" --type cyclonedx \
      --predicate "$sbom" --output-attestation "$sbom.att" "$sbom" >/dev/null 2>&1 \
      && echo "  ✓ cosign attest: $sbom.att"
  done
else
  echo "  .. cosign path skipped (needs cosign + PAI_RELEASE_COSIGN_KEY)"
fi

if [ "$signed" -eq 0 ]; then
  echo "WARNING: manifest built but NOT signed (no signing key provided)" >&2
  exit 1
fi
echo "done."
