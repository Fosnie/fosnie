# Copyright 2026 Private AI Ltd (SC881079)
# Licensed under the Apache License, Version 2.0 (the "License").

"""Build the topic→section TOC index from the corpus running headers.

Scrolls the KB collection, parses each chunk's "Part N / Chapter M — <title>" header (via
chunker.toc_header), groups by (kb, part, chapter) accumulating the section-number RANGE from
each chunk's `section_num`, and upserts one searchable point per chapter into `pai_kb_toc`.
Idempotent (deterministic point ids). Non-statute KBs (no Part/Chapter headers) yield nothing.

Usage (from ml/, Qdrant reachable):  python scripts/backfill_toc.py [--dry-run]
"""

import argparse
import asyncio
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from app import qdrant_store  # noqa: E402
from app.chunker import toc_header  # noqa: E402


async def backfill(dry_run: bool) -> int:
    coll = qdrant_store.COLLECTION
    c = qdrant_store.client()
    if not await c.collection_exists(coll):
        print(f"collection {coll!r} does not exist — nothing to build")
        return 0

    # (kb, part, chapter) -> {title, lo, hi}
    acc: dict[tuple, dict] = {}
    scanned = 0
    async for points in qdrant_store.scroll_all(coll):
        for p in points:
            scanned += 1
            pl = p.payload or {}
            kb = pl.get("knowledge_base_id")
            num = pl.get("section_num")
            hdr = toc_header(pl.get("chunk_text") or "")
            if not kb or num is None or not hdr:
                continue
            key = (kb, hdr.get("part"), hdr.get("chapter"))
            e = acc.get(key)
            if e is None:
                acc[key] = {"title": hdr["title"], "lo": num, "hi": num}
            else:
                e["lo"] = min(e["lo"], num)
                e["hi"] = max(e["hi"], num)

    # Group rows per KB for upsert.
    by_kb: dict[str, list[dict]] = {}
    for (kb, part, chapter), e in acc.items():
        by_kb.setdefault(kb, []).append(
            {"part": part, "chapter": chapter, "title": e["title"], "num_lo": e["lo"], "num_hi": e["hi"]}
        )

    total = sum(len(v) for v in by_kb.values())
    # Show a sample (esp. the allotment/pre-emption chapters we care about).
    sample = [r for rows in by_kb.values() for r in rows]
    for r in sorted(sample, key=lambda x: x["num_lo"])[:40]:
        if 500 <= r["num_lo"] <= 600 or "allot" in r["title"].lower() or "pre-emption" in r["title"].lower():
            print(f"  Ch {r['chapter']}: {r['title'][:48]!r:52} s{r['num_lo']}-s{r['num_hi']}")

    if not dry_run:
        for kb, rows in by_kb.items():
            await qdrant_store.upsert_toc(kb, rows)

    verb = "would build" if dry_run else "built"
    print(f"backfill TOC: scanned={scanned} chapters {verb}={total} across {len(by_kb)} KB(s)")
    return total


def main() -> None:
    ap = argparse.ArgumentParser(description="Build the topic→section TOC index.")
    ap.add_argument("--dry-run", action="store_true", help="report without writing")
    args = ap.parse_args()
    asyncio.run(backfill(args.dry_run))


if __name__ == "__main__":
    main()
