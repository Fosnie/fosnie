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

"""Qdrant access (Python-only, topology). ONE collection, payload-
partitioned (Qdrant's multitenancy shape for many small tenants):
`pai_kb` with named vectors `dense` (Cosine) + `bm25` (sparse, server-side IDF).
Each chunk payload carries the immutable `knowledge_base_id` (set at ingest by
the backend) and NO denormalised access grants — authorisation is a query-time
pre-filter `knowledge_base_id IN <allow-list>` (Libraries). Hybrid query
= dense + BM25 prefetch fused with RRF, KB-filtered (ACORN)."""

import re
import uuid
from contextvars import ContextVar

from qdrant_client import AsyncQdrantClient, models

from .config import settings

_client: AsyncQdrantClient | None = None

# Per-request source-ACL deny-list: `doc_id`s that the
# caller is not entitled to under an `enforce` connector mapping. Set once at the
# top of `retrieve()` and read by EVERY query filter below, so a denied document
# can surface through no path (hybrid search, section/neighbour scroll, TOC, or
# the by-id parent fetch). Task-local via ContextVar — asyncio.gather children
# inherit the value set before the fan-out; requests never see each other's list.
# Empty (the default) ⇒ no `must_not` clause ⇒ byte-identical to before.
_deny_docs: ContextVar[frozenset[str]] = ContextVar("deny_docs", default=frozenset())


def set_deny_docs(ids) -> object:
    """Set the retrieval deny-list for the current request. Returns a token to pass
    to `reset_deny_docs` in a finally."""
    return _deny_docs.set(frozenset(str(i) for i in (ids or ())))


def reset_deny_docs(token: object) -> None:
    _deny_docs.reset(token)


def _kb_must(kb_ids: list[str]) -> list:
    """The KB allow-list pre-filter (`knowledge_base_id IN kb_ids`) shared by every
    query method."""
    return [models.FieldCondition(key="knowledge_base_id", match=models.MatchAny(any=kb_ids))]


def _deny_must_not() -> list | None:
    """The source-ACL deny clause (`doc_id IN <deny>`) or None when empty."""
    deny = _deny_docs.get()
    if not deny:
        return None
    return [models.FieldCondition(key="doc_id", match=models.MatchAny(any=list(deny)))]

# Single shared collection (+ sibling by-id parent store for L2).
COLLECTION = "pai_kb"
PARENTS = "pai_kb_parents"
# Deep Research per-document notes cache (Phase 2): question-independent
# structured notes keyed by doc_id, reused across runs. Same by-id shape as the
# L2 parents (dummy vector, never searched) — derived data, not source of truth.
NOTES = "pai_doc_notes"
# Topic→section table-of-contents index: one point per
# (KB, Part, Chapter), sparse-searched by chapter title → section-number range.
TOC = "pai_kb_toc"


def client() -> AsyncQdrantClient:
    global _client
    if _client is None:
        _client = AsyncQdrantClient(url=settings.qdrant_url, timeout=settings.qdrant_timeout)
    return _client


async def ensure_collection(dim: int) -> None:
    """Create the single shared collection once (at the deployment embedding
    dimension) with a payload index on `knowledge_base_id` so the `IN
    <allow-list>` pre-filter is instant. Idempotent. `COLLECTION` (`pai_kb`) may be
    a literal collection (fresh deploy) or a Qdrant alias (after the first
    blue-green re-index) — `collection_exists` is true for both, so this no-ops
    once an index exists; only the very first ingest creates the literal."""
    c = client()
    if not await c.collection_exists(COLLECTION):
        await create_kb_collection(COLLECTION, dim)


async def create_kb_collection(name: str, dim: int) -> None:
    """Create a KB vector collection (dense `dim` Cosine + `bm25` sparse) with the
    `knowledge_base_id`/`doc_id` payload indexes. Used for the live collection and
    for the blue-green re-index target."""
    c = client()
    if await c.collection_exists(name):
        return
    await c.create_collection(
        collection_name=name,
        vectors_config={"dense": models.VectorParams(size=dim, distance=models.Distance.COSINE)},
        sparse_vectors_config={"bm25": models.SparseVectorParams(modifier=models.Modifier.IDF)},
    )
    await c.create_payload_index(name, "knowledge_base_id", models.PayloadSchemaType.KEYWORD)
    await c.create_payload_index(name, "doc_id", models.PayloadSchemaType.KEYWORD)
    await _create_section_indexes(c, name)


async def _create_section_indexes(c: AsyncQdrantClient, name: str) -> None:
    """Payload indexes for deterministic retrieval expansion:
    section identity (`clause_section_ref`, `refs_out` KEYWORD) + numeric adjacency
    (`section_num` INTEGER). Idempotent — Qdrant treats a repeat create as a no-op."""
    await c.create_payload_index(name, "clause_section_ref", models.PayloadSchemaType.KEYWORD)
    await c.create_payload_index(name, "refs_out", models.PayloadSchemaType.KEYWORD)
    await c.create_payload_index(name, "section_num", models.PayloadSchemaType.INTEGER)
    # follow-up: multi-valued owned sections (a chunk that swallowed a mid-
    # section heading owns >1) — indexed so the by-number fetch/neighbour filters below stay fast.
    await c.create_payload_index(name, "section_nums", models.PayloadSchemaType.INTEGER)


async def ensure_section_indexes(name: str = COLLECTION) -> None:
    """Add the section indexes to an EXISTING collection (created before this
    feature). Idempotent; used by the refs_out backfill so range/keyword filters are fast."""
    await _create_section_indexes(client(), await alias_target(name))


async def alias_target(alias: str) -> str:
    """Resolve which collection `alias` points to. When `alias` is itself a literal
    collection (pre-migration deploys), it IS the target. Falls back to the name."""
    c = client()
    try:
        for a in (await c.get_aliases()).aliases:
            if a.alias_name == alias:
                return a.collection_name
    except Exception:
        pass
    return alias


async def is_alias(name: str) -> bool:
    """True when `name` is a Qdrant alias (vs a literal collection)."""
    c = client()
    try:
        return any(a.alias_name == name for a in (await c.get_aliases()).aliases)
    except Exception:
        return False


async def scroll_all(name: str, batch: int = 256):
    """Yield batches of points (id + payload, no vectors) from a collection — the
    re-index re-embeds from the payload text, so vectors aren't fetched."""
    c = client()
    offset = None
    while True:
        points, offset = await c.scroll(
            collection_name=name, limit=batch, offset=offset, with_payload=True, with_vectors=False
        )
        if points:
            yield points
        if offset is None:
            break


async def set_payload(name: str, point_ids: list, payload: dict) -> None:
    """Overwrite payload FIELDS on existing points WITHOUT touching vectors
    (backfill) — merges the given keys into each point's payload.
    Cheap: no re-embedding, unlike upsert."""
    if not point_ids:
        return
    c = client()
    await c.set_payload(collection_name=name, payload=payload, points=list(point_ids))


async def fetch_by_sections(kb_ids: list[str], section_ids: list[str], limit: int = 24) -> list[dict]:
    """Chunk PAYLOADS whose owning section (`clause_section_ref`) is one of `section_ids`,
    within the KB allow-list — a deterministic look-up of a required or cross-referenced
    section's operative text. No vectors, no rerank — a plain filtered
    scroll. Returns payload dicts (same shape retrieve.py's pool consumes)."""
    if not kb_ids or not section_ids:
        return []
    c = client()
    # Match the chunk's PRIMARY label (`clause_section_ref`, string) OR any section it OWNS
    # (`section_nums`, follow-up) — the latter reaches a section whose
    # heading landed mid-chunk (s564 inside the s563 chunk). `should` = must-KB AND (either match),
    # so pre-backfill points (no `section_nums`) still resolve via `clause_section_ref`.
    nums = sorted({int(m.group()) for s in section_ids if (m := re.match(r"\d+", str(s)))})
    flt = models.Filter(
        must=_kb_must(kb_ids),
        must_not=_deny_must_not(),
        should=[
            models.FieldCondition(key="clause_section_ref", match=models.MatchAny(any=section_ids)),
            models.FieldCondition(key="section_nums", match=models.MatchAny(any=nums)),
        ],
    )
    points, _ = await c.scroll(COLLECTION, scroll_filter=flt, limit=limit, with_payload=True, with_vectors=False)
    return [p.payload for p in points if p.payload]


async def fetch_neighbours(kb_ids: list[str], section_nums: list[int], span: int, limit: int = 24) -> list[dict]:
    """Chunk PAYLOADS for the numeric neighbours of the given sections (±`span`), within
    the KB allow-list — a provision's operative context often spills into the adjacent
    section. Expands each num to [n-span, n+span] and matches
    `section_num` exactly, so s443A+s444+s445 come back in one pass. No vectors/rerank."""
    if not kb_ids or not section_nums or span < 0:
        return []
    targets = sorted({n + d for n in section_nums for d in range(-span, span + 1) if n + d > 0})
    if not targets:
        return []
    c = client()
    # Match the scalar `section_num` OR the multi-valued owned `section_nums` so a mid-chunk
    # inner section is reached by the TOC/neighbour sweep too. `should` keeps pre-backfill
    # points (scalar only) matching.
    flt = models.Filter(
        must=_kb_must(kb_ids),
        must_not=_deny_must_not(),
        should=[
            models.FieldCondition(key="section_num", match=models.MatchAny(any=targets)),
            models.FieldCondition(key="section_nums", match=models.MatchAny(any=targets)),
        ],
    )
    points, _ = await c.scroll(COLLECTION, scroll_filter=flt, limit=limit, with_payload=True, with_vectors=False)
    return [p.payload for p in points if p.payload]


async def upsert_named(name: str, rows: list[dict], dense: list[list[float]], sparse: list[dict]) -> None:
    """Like [`upsert`] but into an explicit collection (the re-index target)."""
    c = client()
    points = []
    for row, dv, sv in zip(rows, dense, sparse):
        pairs = sorted(zip(sv["indices"], sv["values"]))
        s_idx, s_val = (list(x) for x in zip(*pairs)) if pairs else ([], [])
        points.append(
            models.PointStruct(
                id=row["id"],
                vector={"dense": dv, "bm25": models.SparseVector(indices=s_idx, values=s_val)},
                payload=row["payload"],
            )
        )
    for i in range(0, len(points), 128):
        await c.upsert(name, points=points[i : i + 128])


async def count(name: str) -> int:
    """Exact point count of a collection (for the re-index sanity check)."""
    c = client()
    if not await c.collection_exists(name):
        return 0
    return (await c.count(name, exact=True)).count


async def swap_alias(alias: str, new_name: str) -> None:
    """Atomically point `alias` at `new_name`. Handles the one-time transition where
    `alias` is still a LITERAL collection (fresh deploys): drop the literal first,
    then create the alias (a collection + alias of the same name cannot coexist).
    Subsequent swaps are a single atomic delete+create alias action."""
    c = client()
    if await c.collection_exists(alias) and not await is_alias(alias):
        # Literal collection occupies the name — remove it, then alias the name.
        await c.delete_collection(alias)
        await c.update_collection_aliases(
            change_aliases_operations=[
                models.CreateAliasOperation(
                    create_alias=models.CreateAlias(collection_name=new_name, alias_name=alias)
                )
            ]
        )
    else:
        # Already an alias — atomic repoint (delete old + create new in one call).
        await c.update_collection_aliases(
            change_aliases_operations=[
                models.DeleteAliasOperation(delete_alias=models.DeleteAlias(alias_name=alias)),
                models.CreateAliasOperation(
                    create_alias=models.CreateAlias(collection_name=new_name, alias_name=alias)
                ),
            ]
        )


async def drop_collection(name: str) -> None:
    """Best-effort drop of a (now-superseded) collection."""
    c = client()
    try:
        if await c.collection_exists(name):
            await c.delete_collection(name)
    except Exception:
        pass


async def delete_doc(kb_id: str, doc_id: str) -> None:
    """Remove a document's chunks (re-index-replaces; KBs are not versioned),
    plus its L2 parents. Scoped to the KB and document."""
    c = client()
    by_doc = models.FilterSelector(
        filter=models.Filter(
            must=[
                models.FieldCondition(key="knowledge_base_id", match=models.MatchValue(value=kb_id)),
                models.FieldCondition(key="doc_id", match=models.MatchValue(value=doc_id)),
            ]
        )
    )
    if await c.collection_exists(COLLECTION):
        await c.delete(COLLECTION, points_selector=by_doc)
    if await c.collection_exists(PARENTS):
        await c.delete(PARENTS, points_selector=by_doc)
    # Invalidate the cached Deep Research note (point id == doc_id). This is the
    # single funnel for both re-index (ingest replaces) and document deletion, so
    # co-locating the note drop here keeps the cache honest with no extra wiring.
    if await c.collection_exists(NOTES):
        await c.delete(NOTES, points_selector=models.PointIdsList(points=[doc_id]))


# --- L2 parent store ---------------------------------------------------------


async def ensure_parents() -> None:
    """Parents live in a tiny by-id store (size-1 dummy vector, never searched)."""
    c = client()
    if await c.collection_exists(PARENTS):
        return
    await c.create_collection(
        collection_name=PARENTS,
        vectors_config=models.VectorParams(size=1, distance=models.Distance.DOT),
    )
    await c.create_payload_index(PARENTS, "doc_id", models.PayloadSchemaType.KEYWORD)


async def upsert_parents(parents: list[dict]) -> None:
    """`parents[i]` = {parent_id, doc_id, knowledge_base_id, text}. Stored by id
    for read-time fetch."""
    if not parents:
        return
    c = client()
    points = [
        models.PointStruct(
            id=p["parent_id"],
            vector=[0.0],
            payload={
                "parent_id": p["parent_id"],
                "doc_id": p["doc_id"],
                "knowledge_base_id": p["knowledge_base_id"],
                "text": p["text"],
            },
        )
        for p in parents
    ]
    await c.upsert(PARENTS, points=points)


async def retrieve_parents(ids: list[str]) -> dict[str, str]:
    """Fetch parent texts by id (no search; ids are globally-unique). Returns
    {parent_id: text}. This path fetches by point id, not by filter, so the
    source-ACL deny-list is applied as a post-filter on each parent's `doc_id` —
    a denied document's parent (L2) block must not arrive here as a back door
    around the chunk-level `must_not`."""
    if not ids:
        return {}
    c = client()
    if not await c.collection_exists(PARENTS):
        return {}
    deny = _deny_docs.get()
    points = await c.retrieve(collection_name=PARENTS, ids=ids, with_payload=True)
    return {
        str(p.payload["parent_id"]): p.payload["text"]
        for p in points
        if p.payload and (not deny or str(p.payload.get("doc_id")) not in deny)
    }


# --- Topic→section TOC index ------------------------


async def ensure_toc() -> None:
    """The statute table-of-contents index: one point per (KB, Part, Chapter), searched by a
    BM25 sparse vector of the chapter TITLE so a topical, numberless sub-question ("authority
    to allot shares… pre-emption") maps to the chapter's section-number range. Sparse-only
    (titles are short); tiny (hundreds of rows per statute)."""
    c = client()
    if await c.collection_exists(TOC):
        return
    await c.create_collection(
        collection_name=TOC,
        vectors_config={},
        sparse_vectors_config={"bm25": models.SparseVectorParams(modifier=models.Modifier.IDF)},
    )
    await c.create_payload_index(TOC, "knowledge_base_id", models.PayloadSchemaType.KEYWORD)


async def upsert_toc(kb_id: str, rows: list[dict]) -> None:
    """`rows[i]` = {part, chapter, title, num_lo, num_hi}. Deterministic id per
    (kb, part, chapter) so a re-ingest/backfill replaces rather than duplicates."""
    if not rows:
        return
    from . import sparse

    c = client()
    await ensure_toc()
    points = []
    for r in rows:
        sv = sparse.sparse_one(r["title"])
        pairs = sorted(zip(sv["indices"], sv["values"]))
        idx, val = (list(x) for x in zip(*pairs)) if pairs else ([], [])
        pid = str(uuid.uuid5(uuid.NAMESPACE_URL, f"{kb_id}|{r.get('part')}|{r.get('chapter')}"))
        points.append(
            models.PointStruct(
                id=pid,
                vector={"bm25": models.SparseVector(indices=idx, values=val)},
                payload={"knowledge_base_id": kb_id, **r},
            )
        )
    for i in range(0, len(points), 128):
        await c.upsert(TOC, points=points[i : i + 128])


async def toc_search(kb_ids: list[str], text: str, limit: int = 2) -> list[dict]:
    """BM25-match `text` (a sub-question) against chapter TITLES within the KB allow-list →
    top chapter payloads {part, chapter, title, num_lo, num_hi}. Fail-soft: empty when the
    TOC collection doesn't exist (non-statute KB) — the caller then does nothing."""
    if not kb_ids or not text.strip():
        return []
    from . import sparse

    c = client()
    if not await c.collection_exists(TOC):
        return []
    sv = sparse.sparse_one(text)
    flt = models.Filter(must=_kb_must(kb_ids), must_not=_deny_must_not())
    res = await c.query_points(
        collection_name=TOC,
        query=models.SparseVector(indices=sv["indices"], values=sv["values"]),
        using="bm25",
        query_filter=flt,
        limit=limit,
        with_payload=True,
    )
    return [p.payload for p in res.points if p.payload]


# --- Deep Research per-document notes cache (Phase 2) ------------------------


async def ensure_notes() -> None:
    """The notes cache: a by-id store (size-1 dummy vector, never searched),
    point id == doc_id so a note is fetched/replaced/dropped by document."""
    c = client()
    if await c.collection_exists(NOTES):
        return
    await c.create_collection(
        collection_name=NOTES,
        vectors_config=models.VectorParams(size=1, distance=models.Distance.DOT),
    )
    await c.create_payload_index(NOTES, "doc_id", models.PayloadSchemaType.KEYWORD)
    await c.create_payload_index(NOTES, "knowledge_base_id", models.PayloadSchemaType.KEYWORD)


async def upsert_notes(notes: list[dict]) -> None:
    """`notes[i]` = full payload incl. `doc_id`/`knowledge_base_id`/`note`. Best
    effort — a failed cache write must never sink the run (caller guards)."""
    if not notes:
        return
    await ensure_notes()
    c = client()
    points = [
        models.PointStruct(id=n["doc_id"], vector=[0.0], payload=n) for n in notes
    ]
    await c.upsert(NOTES, points=points)


async def get_notes(doc_ids: list[str]) -> dict[str, dict]:
    """Batch-fetch cached note payloads by doc_id (no search). Returns
    {doc_id: payload}; missing/absent collection ⇒ empty (all cache misses)."""
    if not doc_ids:
        return {}
    c = client()
    if not await c.collection_exists(NOTES):
        return {}
    points = await c.retrieve(collection_name=NOTES, ids=doc_ids, with_payload=True)
    return {str(p.payload["doc_id"]): p.payload for p in points if p.payload}


async def upsert(
    rows: list[dict],
    dense: list[list[float]],
    sparse: list[dict],
) -> None:
    """`rows[i]` carries point id + payload (incl. `knowledge_base_id`); vectors
    come from `dense`/`sparse`."""
    c = client()
    points = []
    for row, dv, sv in zip(rows, dense, sparse):
        # Qdrant requires sparse vector indices to be sorted ascending.
        # BM25 tokenisers return indices in arbitrary order → sort here.
        pairs = sorted(zip(sv["indices"], sv["values"]))
        s_idx, s_val = (list(x) for x in zip(*pairs)) if pairs else ([], [])
        points.append(
            models.PointStruct(
                id=row["id"],
                vector={
                    "dense": dv,
                    "bm25": models.SparseVector(indices=s_idx, values=s_val),
                },
                payload=row["payload"],
            )
        )
    # Qdrant resets the connection for large payloads — upload in small batches.
    batch_size = 128
    for i in range(0, len(points), batch_size):
        await c.upsert(COLLECTION, points=points[i : i + batch_size])


async def hybrid_search(
    kb_ids: list[str],
    dense: list[float],
    sparse: dict,
    limit: int,
) -> list[dict]:
    """KB-pre-filtered dense+BM25 hybrid over the shared collection. The
    allow-list is resolved server-side by the backend; an EMPTY list must never
    reach here (fail-closed is the caller's job). Returns [{score, payload}]."""
    if not kb_ids:
        return []
    c = client()
    if not await c.collection_exists(COLLECTION):
        return []
    flt = models.Filter(must=_kb_must(kb_ids), must_not=_deny_must_not())
    from .rag_ctx import cfg

    over = max(limit * cfg("over_retrieval", settings.over_retrieval), limit)
    res = await c.query_points(
        collection_name=COLLECTION,
        prefetch=[
            models.Prefetch(query=dense, using="dense", limit=over, filter=flt),
            models.Prefetch(
                query=models.SparseVector(indices=sparse["indices"], values=sparse["values"]),
                using="bm25",
                limit=over,
                filter=flt,
            ),
        ],
        query=models.FusionQuery(fusion=models.Fusion.RRF),
        limit=limit,
        with_payload=True,
    )
    return [{"score": p.score, "payload": p.payload} for p in res.points]
