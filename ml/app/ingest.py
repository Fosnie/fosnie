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

"""Ingest a document into a Project Knowledge collection: extract → chunk →
(optional context) → embed (dense + BM25) → upsert. Re-index replaces the
document's chunks (PK is not versioned, 08).

Layered chunker, each toggle default-off:
  L0+L1  recursive split + page/clause metadata (always).
  L2     parent–child: embed small children, store enclosing parents by id.
  L3     contextual retrieval: prepend an LLM blurb before embedding (the text
         embedded changes; the stored `chunk_text` stays the verbatim original
         so citations quote the source). Pooling-agnostic — compatible with
         Qwen3-Embedding's last-token pooling."""

import asyncio
import logging
import uuid

from . import chunker, contextual, embeddings, extract, metadata, qdrant_store, sparse
from .config import settings

_EMBED_BATCH = 64
_log = logging.getLogger("pai.ingest")


async def ingest_document(
    doc_id: str,
    kb_id: str,
    path: str,
    mime: str | None,
    dimension: int,
    dual: dict | None = None,
) -> dict:
    from .rag_ctx import cfg

    pages = await extract.extract_pages_ocr(path, mime)

    # The document's own effective date (best-effort) — lets retrieval filter by
    # the document's date, not just when it was ingested.
    effective_date = await metadata.extract_effective_date(pages, mime, path)

    # Per-KB parent–child chunking: the backend passes the KB's
    # flag as a `parent_child` override; unset ⇒ the service default (settings).
    parent_child = cfg("parent_child", settings.parent_child)
    if parent_child:
        parents, children = chunker.chunk_hierarchy(
            pages,
            settings.child_chunk_size,
            settings.child_chunk_overlap,
            settings.parent_chunk_size,
            lambda: str(uuid.uuid4()),
        )
    else:
        parents, children = [], chunker.chunk_pages(pages)
    if not children:
        return {"chunks": 0, "effective_date": effective_date}

    await qdrant_store.ensure_collection(dimension)
    await qdrant_store.delete_doc(kb_id, doc_id)  # replace on re-index

    # L3: per-child situating blurb (bounded by a cost guard). The blurb is
    # prepended only to the EMBEDDED text; the stored chunk_text is the original.
    use_ctx = settings.contextual_retrieval
    if use_ctx and len(children) > settings.contextual_max_chunks:
        _log.warning(
            "contextual_retrieval skipped for doc %s: %d chunks > cap %d",
            doc_id, len(children), settings.contextual_max_chunks,
        )
        use_ctx = False
    doc_text = "\n\n".join(t for _, t in pages)

    # Contextualise concurrently (bounded) instead of one awaited LLM round-trip
    # per chunk — a 100-chunk document was 100 serial calls.
    if use_ctx:
        ctx_sem = asyncio.Semaphore(max(1, settings.contextual_concurrency))

        async def _ctx(text: str) -> str:
            async with ctx_sem:
                return await contextual.contextualise(doc_text, text)

        contexts: list[str] = list(await asyncio.gather(*[_ctx(ch["text"]) for ch in children]))
    else:
        contexts = ["" for _ in children]
    embed_texts: list[str] = [
        contextual.augment(blurb, ch["text"]) for blurb, ch in zip(contexts, children)
    ]

    # Dense (batched) + BM25 over the AUGMENTED text (contextual embeddings AND
    # contextual BM25 — the combination behind the ~67% error reduction).
    dense: list[list[float]] = []
    for i in range(0, len(embed_texts), _EMBED_BATCH):
        dense.extend(await embeddings.embed(embed_texts[i : i + _EMBED_BATCH]))
    sp = sparse.sparse_embed(embed_texts)

    if parent_child and parents:
        await qdrant_store.ensure_parents()
        await qdrant_store.upsert_parents(
            [
                {"parent_id": p["parent_id"], "doc_id": doc_id, "knowledge_base_id": kb_id, "text": p["text"]}
                for p in parents
            ]
        )

    rows = []
    for i, ch in enumerate(children):
        payload = {
            "knowledge_base_id": kb_id,  # immutable; the only access key (no grants in payload)
            "doc_id": doc_id,
            "chunk_index": i,
            "page_number": ch["page_number"],
            "clause_section_ref": ch["clause_section_ref"],
            "chunk_text": ch["text"],  # verbatim original — citations quote this
        }
        # Deterministic retrieval expansion: the cross-reference set
        # and the numeric section for ±N neighbour ranges. Absent when the chunk names none.
        if ch.get("refs_out"):
            payload["refs_out"] = ch["refs_out"]
        if ch.get("section_num") is not None:
            payload["section_num"] = ch["section_num"]
        # follow-up: all sections this chunk owns (incl. a mid-chunk inner
        # section like s564 inside the s563 chunk) — for exact by-number fetch/neighbour matching.
        if ch.get("section_nums"):
            payload["section_nums"] = ch["section_nums"]
        if contexts[i]:
            payload["context"] = contexts[i]
        if ch.get("parent_id"):
            payload["parent_id"] = ch["parent_id"]
        rows.append({"id": str(uuid.uuid4()), "payload": payload})

    await qdrant_store.upsert(rows, dense, sp)

    # build the topic→section TOC index for this doc's chapters from
    # the running headers (chapter title → section-number range). Statute-only; a non-statute
    # doc yields nothing. Best-effort — never fail an ingest on the TOC.
    try:
        toc_acc: dict[tuple, dict] = {}
        for ch in children:
            num = ch.get("section_num")
            hdr = chunker.toc_header(ch["text"])
            if num is None or not hdr:
                continue
            # key on (part, chapter) ONLY — the SAME grain as
            # `upsert_toc`'s deterministic point id (uuid5 of `{kb}|{part}|{chapter}`) and
            # `backfill_toc.py`. Keying on the title too let a chapter whose running-header
            # title drifted across chunks (OCR/mojibake) split into several `toc_acc` rows
            # that then COLLIDE on one id → last-write-wins → a truncated num range. First-seen
            # title is kept; the range merges across all header chunks of the chapter.
            key = (hdr.get("part"), hdr.get("chapter"))
            e = toc_acc.get(key)
            if e is None:
                toc_acc[key] = {"part": hdr.get("part"), "chapter": hdr.get("chapter"), "title": hdr["title"], "num_lo": num, "num_hi": num}
            else:
                e["num_lo"] = min(e["num_lo"], num)
                e["num_hi"] = max(e["num_hi"], num)
        if toc_acc:
            await qdrant_store.upsert_toc(kb_id, list(toc_acc.values()))
    except Exception as e:  # noqa: BLE001 — the TOC index is an optional retrieval aid
        _log.warning("TOC index build failed for doc %s: %s", doc_id, e)

    # Dual-write during a blue-green re-index: also embed with
    # the NEW model and upsert into the rebuilt collection, so a doc uploaded mid-
    # migration is present in both indexes (BM25 is model-independent → reuse `sp`).
    if dual:
        from . import reindex

        new_coll = reindex.collection_name(int(dual["dim"]), dual["model"])
        await qdrant_store.create_kb_collection(new_coll, int(dual["dim"]))
        dense2: list[list[float]] = []
        for i in range(0, len(embed_texts), _EMBED_BATCH):
            dense2.extend(await embeddings.embed_with(
                embed_texts[i : i + _EMBED_BATCH], dual.get("base_url"), dual.get("model"), dual.get("api_key")
            ))
        await qdrant_store.upsert_named(new_coll, rows, dense2, sp)

    return {"chunks": len(children), "effective_date": effective_date}
