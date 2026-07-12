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

"""Idempotent backfill of `refs_out` + `section_num` on existing chunks.

Deterministic retrieval expansion needs, per chunk: the set of section references it
mentions (`refs_out`, the cross-reference graph) and its numeric section (`section_num`,
for ±N neighbour ranges). Both are recomputed from the stored `chunk_text` and written
back via a payload-only update — NO re-embedding. Also (idempotently) creates the section
payload indexes so range/keyword filters are fast.

Only points whose (refs_out, section_num) actually CHANGE are written, so re-running
reports `changed=0`.

Usage (from the `ml/` directory, with Qdrant reachable via QDRANT_URL):
    python scripts/backfill_refs_out.py            # apply (+ create indexes)
    python scripts/backfill_refs_out.py --dry-run  # report only, no writes
"""

import argparse
import asyncio
import sys
from pathlib import Path

# Make the `app` package importable when run as a bare script from ml/.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from app import qdrant_store  # noqa: E402
from app.chunker import _chunk_meta  # noqa: E402


async def backfill(dry_run: bool) -> tuple[int, int]:
    coll = qdrant_store.COLLECTION
    c = qdrant_store.client()
    if not await c.collection_exists(coll):
        print(f"collection {coll!r} does not exist — nothing to backfill")
        return 0, 0

    if not dry_run:
        await qdrant_store.ensure_section_indexes(coll)

    scanned = 0
    changed = 0
    samples: list[tuple] = []

    async for points in qdrant_store.scroll_all(coll):
        for p in points:
            scanned += 1
            payload = p.payload or {}
            text = payload.get("chunk_text") or ""
            # Recompute the whole section overlay from one place (chunker) so backfill and ingest
            # can never drift: refs_out, the primary label, the numeric section AND the multi-valued
            # owned `section_nums` (follow-up — recovers s551/s564/s566/s568).
            meta = _chunk_meta(text)
            new_refs = meta.get("refs_out") or []
            new_num = meta.get("section_num")
            new_nums = meta.get("section_nums") or []
            new_clause = meta.get("clause_section_ref")
            old_refs = payload.get("refs_out") or []
            old_num = payload.get("section_num")
            old_nums = payload.get("section_nums") or []
            old_clause = payload.get("clause_section_ref")
            if (new_refs == old_refs and new_num == old_num
                    and new_nums == old_nums and new_clause == old_clause):
                continue
            changed += 1
            if len(samples) < 20:
                samples.append((p.id, old_nums, new_nums, old_clause, new_clause))
            if not dry_run:
                # set_payload MERGES, so always write every field (None/[] clears a now-stale value,
                # e.g. an old Part-number label or a section that moved to its own chunk).
                await qdrant_store.set_payload(coll, [p.id], {
                    "refs_out": new_refs, "section_num": new_num,
                    "section_nums": new_nums, "clause_section_ref": new_clause,
                })

    if samples:
        print("sample updates (id: section_nums old->new, clause old->new):")
        for pid, onums, nnums, oclause, nclause in samples:
            print(f"  {pid}: {onums} -> {nnums}, {oclause!r} -> {nclause!r}")

    verb = "would change" if dry_run else "changed"
    print(f"backfill section overlay: scanned={scanned} {verb}={changed}")
    return scanned, changed


def main() -> None:
    ap = argparse.ArgumentParser(description="Backfill refs_out/section_num.")
    ap.add_argument("--dry-run", action="store_true", help="report updates without writing")
    args = ap.parse_args()
    asyncio.run(backfill(args.dry_run))


if __name__ == "__main__":
    main()
