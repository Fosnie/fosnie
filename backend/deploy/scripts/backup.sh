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
# Fosnie backup. Captures a consistent point-in-time bundle of the three
# stateful stores and writes a manifest + checksums. Designed for the Linux
# deploy profile but runs anywhere with bash, pg_dump, curl, tar, python3.
#
# Targets:
#   - Postgres  : the `pai` database (app data + the audit hash-chain)        [required]
#   - Qdrant    : every collection, via the snapshot API                      [optional]
#   - Storage   : the on-disk file roots (documents / workspace / artefacts…) [optional]
# Redis is intentionally skipped — it holds only ephemeral session/ticket state.
#
# Config (env, with sensible defaults):
#   FOSNIE_DB_URL          postgres://pai:pai@localhost:5432/pai   (or standard PG* vars)
#   FOSNIE_QDRANT_URL      http://localhost:6333                   ("" or "skip" to skip)
#   FOSNIE_STORAGE_DIRS    space-separated dirs to archive         ("" to skip)
#   FOSNIE_BACKUP_DIR      /var/backups/fosnie                        (bundles land here)
#   FOSNIE_BACKUP_RETAIN   14                                      (keep N newest bundles)
#   FOSNIE_BACKUP_AGE_RECIPIENT  age public key                    (encrypt bundle if set)
#
# Usage:  backup.sh            # one-shot backup into a timestamped bundle dir
# Exit:   non-zero on any failure (set -euo pipefail); safe for a systemd timer.

set -euo pipefail

# The PAI_* spellings are the pre-rename ones, still read so an existing
# /etc/fosnie/backup.env (or /etc/pai/backup.env) keeps working unchanged.
DB_URL="${FOSNIE_DB_URL:-${PAI_DB_URL:-postgres://pai:pai@localhost:5432/pai}}"
QDRANT_URL="${FOSNIE_QDRANT_URL:-${PAI_QDRANT_URL:-http://localhost:6333}}"
STORAGE_DIRS="${FOSNIE_STORAGE_DIRS:-${PAI_STORAGE_DIRS:-}}"
BACKUP_DIR="${FOSNIE_BACKUP_DIR:-${PAI_BACKUP_DIR:-/var/backups/fosnie}}"
RETAIN="${FOSNIE_BACKUP_RETAIN:-${PAI_BACKUP_RETAIN:-14}}"
AGE_RECIPIENT="${FOSNIE_BACKUP_AGE_RECIPIENT:-${PAI_BACKUP_AGE_RECIPIENT:-}}"

log() { printf '[backup] %s\n' "$*" >&2; }
die() { printf '[backup] ERROR: %s\n' "$*" >&2; exit 1; }

command -v pg_dump >/dev/null || die "pg_dump not found on PATH"
command -v curl    >/dev/null || die "curl not found on PATH"
command -v python3 >/dev/null || die "python3 not found on PATH"

STAMP="$(python3 -c 'import datetime; print(datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ"))')"
DEST="${BACKUP_DIR%/}/fosnie-backup-${STAMP}"
mkdir -p "$DEST"
log "bundle → $DEST"

# 1. Postgres — custom format (compressed, parallel-restorable, includes audit chain).
log "pg_dump…"
pg_dump --format=custom --no-owner --no-privileges "$DB_URL" --file "$DEST/postgres.dump"

# 2. Qdrant — snapshot every collection through the HTTP API, then download each.
QDRANT_COLLECTIONS=0
if [ -n "$QDRANT_URL" ] && [ "$QDRANT_URL" != "skip" ]; then
  log "qdrant snapshots…"
  mkdir -p "$DEST/qdrant"
  COLLECTIONS="$(curl -fsS "$QDRANT_URL/collections" \
    | python3 -c 'import sys,json; print("\n".join(c["name"] for c in json.load(sys.stdin)["result"]["collections"]))' | tr -d '\r')" || die "qdrant list failed"
  for c in $COLLECTIONS; do
    SNAP="$(curl -fsS -X POST "$QDRANT_URL/collections/$c/snapshots" \
      | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["name"])' | tr -d '\r')" || die "qdrant snapshot ($c) failed"
    curl -fsS "$QDRANT_URL/collections/$c/snapshots/$SNAP" -o "$DEST/qdrant/$c.snapshot" || die "qdrant download ($c) failed"
    # Remove the server-side snapshot so they do not accumulate on the node.
    curl -fsS -X DELETE "$QDRANT_URL/collections/$c/snapshots/$SNAP" >/dev/null || true
    QDRANT_COLLECTIONS=$((QDRANT_COLLECTIONS + 1))
  done
  log "qdrant: $QDRANT_COLLECTIONS collection(s)"
else
  log "qdrant: skipped"
fi

# 3. Storage roots — one tarball per run (paths preserved absolute for restore).
if [ -n "$STORAGE_DIRS" ]; then
  log "storage tar…"
  EXISTING=""
  for d in $STORAGE_DIRS; do [ -e "$d" ] && EXISTING="$EXISTING $d"; done
  if [ -n "$EXISTING" ]; then
    tar --create --gzip --absolute-names --file "$DEST/storage.tar.gz" $EXISTING
  else
    log "storage: none of the configured dirs exist — skipped"
  fi
else
  log "storage: skipped"
fi

# 4. Manifest + checksums (the integrity anchor for restore).
python3 - "$DEST" "$STAMP" "$DB_URL" "$QDRANT_COLLECTIONS" > "$DEST/manifest.json" <<'PY'
import sys, json, os
dest, stamp, db_url, qdrant = sys.argv[1:5]
# Redact any password in the DSN before recording it.
import re
db_redacted = re.sub(r'(://[^:/]+:)[^@]+(@)', r'\1***\2', db_url)
files = sorted(f for f in os.listdir(dest) if f not in ("manifest.json", "sha256sums"))
print(json.dumps({
    "tool": "fosnie-backup", "version": 1, "created_utc": stamp,
    "postgres_dsn": db_redacted, "qdrant_collections": int(qdrant),
    "files": files,
}, indent=2))
PY

( cd "$DEST" && find . -type f ! -name sha256sums -print0 | sort -z | xargs -0 sha256sum > sha256sums )
log "manifest + sha256sums written"

# 5. Optional encryption (zero-egress: the key/recipient stays in the perimeter).
if [ -n "$AGE_RECIPIENT" ]; then
  command -v age >/dev/null || die "FOSNIE_BACKUP_AGE_RECIPIENT set but 'age' not found"
  log "encrypting bundle with age…"
  TARBALL="${DEST}.tar"
  tar --create --file "$TARBALL" -C "$(dirname "$DEST")" "$(basename "$DEST")"
  age --recipient "$AGE_RECIPIENT" --output "${TARBALL}.age" "$TARBALL"
  rm -rf "$TARBALL" "$DEST"
  DEST="${TARBALL}.age"
  log "encrypted → $DEST"
fi

# 6. Retention — keep the N newest bundles, prune the rest.
log "retention: keep $RETAIN"
# Both bundle spellings are pruned: a directory written before the rename is
# still one of this deployment's backups and must age out with the rest.
ls -1dt "${BACKUP_DIR%/}"/fosnie-backup-* "${BACKUP_DIR%/}"/pai-backup-* 2>/dev/null | tail -n +"$((RETAIN + 1))" | while read -r old; do
  log "prune $old"; rm -rf "$old"
done

log "DONE → $DEST"
