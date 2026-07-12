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
# PAI Platform restore — the inverse of backup.sh. Restore ORDER MATTERS:
# Postgres first (source of truth + audit chain), then Qdrant, then storage.
# After restore, START THE BACKEND and verify the audit hash-chain before
# cutover (GET /api/admin/audit/export → status, or `verify_chain`); a bad chain
# means the dump is tampered/corrupt — DO NOT go live.
#
# Config (env): PAI_DB_URL, PAI_QDRANT_URL  (same as backup.sh).
#
# Usage:  restore.sh <bundle-dir> [--force]
#   <bundle-dir>  a pai-backup-* directory produced by backup.sh
#   --force       proceed even if the target Postgres already has app tables
#
# Refuses to clobber a populated database unless --force is given.

set -euo pipefail

BUNDLE="${1:-}"
FORCE="${2:-}"
[ -n "$BUNDLE" ] && [ -d "$BUNDLE" ] || { echo "usage: restore.sh <bundle-dir> [--force]" >&2; exit 2; }

DB_URL="${PAI_DB_URL:-postgres://pai:pai@localhost:5432/pai}"
QDRANT_URL="${PAI_QDRANT_URL:-http://localhost:6333}"

log() { printf '[restore] %s\n' "$*" >&2; }
die() { printf '[restore] ERROR: %s\n' "$*" >&2; exit 1; }

command -v pg_restore >/dev/null || die "pg_restore not found on PATH"
command -v psql       >/dev/null || die "psql not found on PATH"
command -v curl       >/dev/null || die "curl not found on PATH"

# 0. Integrity — the bundle must match its own checksums before we touch anything.
log "verifying checksums…"
( cd "$BUNDLE" && sha256sum --check --quiet sha256sums ) || die "checksum mismatch — bundle is corrupt"

# 1. Safety — refuse a non-empty DB unless forced.
EXISTING_TABLES="$(psql "$DB_URL" -tAc \
  "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'" 2>/dev/null || echo 0)"
if [ "${EXISTING_TABLES:-0}" -gt 0 ] && [ "$FORCE" != "--force" ]; then
  die "target database already has $EXISTING_TABLES tables; re-run with --force to overwrite"
fi

# 2. Postgres — clean restore (drops + recreates objects from the dump).
log "pg_restore…"
pg_restore --clean --if-exists --no-owner --no-privileges --dbname "$DB_URL" "$BUNDLE/postgres.dump"

# 3. Qdrant — recover each collection from its snapshot via the upload API.
if [ -d "$BUNDLE/qdrant" ] && [ -n "$QDRANT_URL" ] && [ "$QDRANT_URL" != "skip" ]; then
  for snap in "$BUNDLE"/qdrant/*.snapshot; do
    [ -e "$snap" ] || continue
    c="$(basename "$snap" .snapshot)"
    log "qdrant recover: $c"
    curl -fsS -X POST "$QDRANT_URL/collections/$c/snapshots/upload?priority=snapshot" \
      -H 'Content-Type:multipart/form-data' -F "snapshot=@${snap}" >/dev/null \
      || die "qdrant recover ($c) failed"
  done
else
  log "qdrant: nothing to restore"
fi

# 4. Storage — extract the file roots back to their absolute paths.
if [ -e "$BUNDLE/storage.tar.gz" ]; then
  log "storage extract…"
  tar --extract --gzip --absolute-names --file "$BUNDLE/storage.tar.gz"
else
  log "storage: nothing to restore"
fi

cat >&2 <<'NEXT'
[restore] DONE.
[restore] NEXT: start the backend, then VERIFY the audit chain before go-live:
[restore]   curl -s "$BASE/api/admin/audit/export" | head   # status must be ok
[restore]   (or run the offline verifier). A bad chain aborts cutover.
NEXT
