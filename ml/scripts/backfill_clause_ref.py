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

"""Idempotent backfill of `clause_section_ref` on existing chunks.

The old extractor captured commencement dates (`1.10.2007`) and neighbouring numbers
instead of the statute section. This recomputes the ref from the stored `chunk_text`
with the fixed `chunker._clause_ref` and writes it back via a payload-only update —
NO re-embedding, so it is cheap and safe to run against a live collection.

Only points whose ref actually CHANGES are written, so re-running reports `changed=0`.

Usage (from the `ml/` directory, with Qdrant reachable via QDRANT_URL):
    python scripts/backfill_clause_ref.py            # apply
    python scripts/backfill_clause_ref.py --dry-run  # report only, no writes
"""

import argparse
import asyncio
import sys
from pathlib import Path

# Make the `app` package importable when run as a bare script from ml/.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from app import qdrant_store  # noqa: E402
from app.chunker import _clause_ref  # noqa: E402


async def backfill(dry_run: bool) -> tuple[int, int]:
    coll = qdrant_store.COLLECTION
    c = qdrant_store.client()
    if not await c.collection_exists(coll):
        print(f"collection {coll!r} does not exist — nothing to backfill")
        return 0, 0

    scanned = 0
    changed = 0
    groups: dict[str | None, list] = {}  # new_ref -> [point ids]
    samples: list[tuple] = []            # (id, old, new) for a short before/after report

    async for points in qdrant_store.scroll_all(coll):
        for p in points:
            scanned += 1
            payload = p.payload or {}
            new = _clause_ref(payload.get("chunk_text") or "")
            old = payload.get("clause_section_ref")
            if new != old:
                changed += 1
                groups.setdefault(new, []).append(p.id)
                if len(samples) < 20:
                    samples.append((p.id, old, new))

    if samples:
        print("sample corrections (id: old -> new):")
        for pid, old, new in samples:
            print(f"  {pid}: {old!r} -> {new!r}")

    if not dry_run:
        for ref, ids in groups.items():
            # set_payload merges the key; batch per new-ref value.
            for i in range(0, len(ids), 256):
                await qdrant_store.set_payload(coll, ids[i : i + 256], {"clause_section_ref": ref})

    verb = "would change" if dry_run else "changed"
    print(f"backfill clause_section_ref: scanned={scanned} {verb}={changed}")
    return scanned, changed


def main() -> None:
    ap = argparse.ArgumentParser(description="Backfill clause_section_ref.")
    ap.add_argument("--dry-run", action="store_true", help="report corrections without writing")
    args = ap.parse_args()
    asyncio.run(backfill(args.dry_run))


if __name__ == "__main__":
    main()
