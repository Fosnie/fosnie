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
# Assemble what gets published for a desktop release: the installer under its
# versioned name, the same file under a stable name people can link to, and the
# update manifest installed clients read.
#
# The point of a script rather than a checklist is that the release built on a
# signing machine and the one assembled in CI are the same bytes arranged the
# same way, by construction. Nothing here uploads: it writes a directory whose
# contents go to object storage, in the order printed at the end.
#
# Usage:
#   publish-desktop.sh --bundle DIR --version X.Y.Z --out DIR [options]
#
#   --bundle DIR      where `tauri build` left its output (…/bundle/msi)
#   --version X.Y.Z   the release being published
#   --out DIR         where to assemble the upload set
#   --base-url URL    public base the manifest points at
#                     (default https://get.fosnie.dev)
#   --notes-file F    release notes for the manifest (optional)
#   --conf F          tauri configuration to cross-check the version against
#                     (default desktop/src-tauri/tauri.conf.json beside this repo)

set -euo pipefail

BUNDLE=""
VERSION=""
OUT=""
BASE_URL="https://get.fosnie.dev"
NOTES_FILE=""
CONF=""

die() { echo "publish-desktop: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --bundle)     BUNDLE="${2:-}"; shift 2 ;;
    --version)    VERSION="${2:-}"; shift 2 ;;
    --out)        OUT="${2:-}"; shift 2 ;;
    --base-url)   BASE_URL="${2:-}"; shift 2 ;;
    --notes-file) NOTES_FILE="${2:-}"; shift 2 ;;
    --conf)       CONF="${2:-}"; shift 2 ;;
    -h|--help)    sed -n '17,35p' "$0"; exit 0 ;;
    *)            die "unknown argument: $1" ;;
  esac
done

[ -n "$BUNDLE" ]  || die "--bundle is required"
[ -n "$VERSION" ] || die "--version is required"
[ -n "$OUT" ]     || die "--out is required"
[ -d "$BUNDLE" ]  || die "no such bundle directory: $BUNDLE"

# Python rather than jq: this script runs both in CI and on the signing machine,
# which is a Windows box with Git Bash, and Python is the JSON tool present in
# both. Escaping release notes by hand in shell is not worth the saving.
PY=""
for c in python3 python; do
  if command -v "$c" >/dev/null 2>&1; then PY="$c"; break; fi
done
[ -n "$PY" ] || die "python is required (for reading and writing JSON)"

BASE_URL="${BASE_URL%/}"
if [ -z "$CONF" ]; then
  CONF="$(cd "$(dirname "$0")/.." && pwd)/desktop/src-tauri/tauri.conf.json"
fi

# --- The installer and its signature -----------------------------------------
#
# Exactly one of each. Several means an earlier build was left in the directory
# and there is no safe guess as to which release is being published; none means
# the build did not produce what this script exists to publish.

find_one() {
  local pattern="$1" label="$2" found count
  found="$(find "$BUNDLE" -maxdepth 1 -name "$pattern" -type f | sort)"
  count="$(printf '%s' "$found" | grep -c . || true)"
  [ "$count" = "1" ] || die "expected exactly one $label in $BUNDLE, found $count"
  printf '%s' "$found"
}

MSI="$(find_one '*.msi' 'installer (*.msi)')"
SIG="$(find_one '*.msi.sig' 'update signature (*.msi.sig)')"
MSI_NAME="$(basename "$MSI")"

# --- The signature must be for THIS file -------------------------------------
#
# A signature carries the name of what it signed. A manifest whose URL points at
# a differently-named file downloads happily and then fails verification on every
# machine at once, which is the failure this check exists to make impossible.

SIG_TEXT="$(tr -d '\r\n' < "$SIG")"
[ -n "$SIG_TEXT" ] || die "the signature file is empty: $SIG"
SIGNED_FILE="$(printf '%s' "$SIG_TEXT" | base64 -d 2>/dev/null | sed -n 's/.*file:\([^[:space:]]*\).*/\1/p' | head -n1 || true)"
if [ -n "$SIGNED_FILE" ] && [ "$SIGNED_FILE" != "$MSI_NAME" ]; then
  die "the signature is for '$SIGNED_FILE' but the installer here is '$MSI_NAME' — do not publish this pair"
fi
[ -n "$SIGNED_FILE" ] || echo "publish-desktop: note — the signature does not name the file it signed; carrying on" >&2

# --- The version must agree everywhere ---------------------------------------

case "$MSI_NAME" in
  *"$VERSION"*) ;;
  *) die "the installer is named '$MSI_NAME', which is not version $VERSION" ;;
esac

if [ -f "$CONF" ]; then
  CONF_VERSION="$("$PY" -c 'import json,sys; print(json.load(open(sys.argv[1], encoding="utf-8"))["version"])' "$CONF")"
  [ "$CONF_VERSION" = "$VERSION" ] \
    || die "the client is configured as version $CONF_VERSION but $VERSION is being published"
fi

# --- Assemble -----------------------------------------------------------------

DEST="$OUT/desktop"
mkdir -p "$DEST"

cp "$MSI" "$DEST/$MSI_NAME"
# The stable name, so a download link in a document does not go stale every
# release. Same bytes; the manifest deliberately points at the versioned one, so
# a client never downloads something other than what it was offered.
cp "$MSI" "$DEST/Fosnie-Setup.msi"

NOTES=""
[ -n "$NOTES_FILE" ] && [ -f "$NOTES_FILE" ] && NOTES="$(cat "$NOTES_FILE")"

VERSION="$VERSION" NOTES="$NOTES" \
PUB_DATE="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
SIGNATURE="$SIG_TEXT" URL="$BASE_URL/desktop/$MSI_NAME" \
"$PY" - "$DEST/latest.json" <<'PYEOF'
import json, os, sys

manifest = {
    "version": os.environ["VERSION"],
    "notes": os.environ["NOTES"],
    "pub_date": os.environ["PUB_DATE"],
    "platforms": {
        "windows-x86_64": {
            "signature": os.environ["SIGNATURE"],
            "url": os.environ["URL"],
        }
    },
}
with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
PYEOF

SHA="$(sha256sum "$DEST/$MSI_NAME" | cut -d' ' -f1)"

cat <<EOF

Assembled version $VERSION in $DEST
  $MSI_NAME
  Fosnie-Setup.msi
  latest.json
  SHA-256 of the installer: $SHA

Upload IN THIS ORDER. The manifest is what tells every installed client that a
new version exists, so it goes last: published first, it points them all at a
file that is not there yet.

  1. desktop/$MSI_NAME
  2. desktop/Fosnie-Setup.msi
  3. desktop/latest.json
EOF
