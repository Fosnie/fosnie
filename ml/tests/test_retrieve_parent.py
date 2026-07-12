"""Retrieval under L2: the LLM is handed the parent section, citations still
anchor on the child. Fully monkeypatched (no Qdrant, embeddings, reranker, LLM)."""

import asyncio
import json

from app import embeddings, llm, qdrant_store, reranker
from app import retrieve as retrieve_mod
from app.config import settings


def test_parent_expansion_and_child_citations(monkeypatch):
    # Parent expansion + the plain merged-context/citation dedup live on the pre-§1
    # assemble path; the isolation/synthesis path (default on) builds a [D#] pool from
    # mini-answers instead. Exercise the legacy path this test was written for.
    monkeypatch.setattr(settings, "sub_answer_enabled", False)
    # Two children share parent P1; a third is in P2.
    hits = [
        {"score": 0.9, "payload": {"doc_id": "d1", "chunk_index": 0, "page_number": 1,
                                    "clause_section_ref": "1", "chunk_text": "child A about term", "parent_id": "P1"}},
        {"score": 0.8, "payload": {"doc_id": "d1", "chunk_index": 1, "page_number": 1,
                                    "clause_section_ref": "1", "chunk_text": "child B more term", "parent_id": "P1"}},
        {"score": 0.7, "payload": {"doc_id": "d1", "chunk_index": 2, "page_number": 2,
                                    "clause_section_ref": "2", "chunk_text": "child C about fees", "parent_id": "P2"}},
    ]

    async def fake_decompose(system, user, max_tokens=256):
        return '["q"]'

    async def fake_complete(system, user, max_tokens=256):
        # _grade is asked yes/partial/no; answer yes to stop after one round.
        return "yes"

    # _decompose and _grade/_reformulate all go through llm.complete.
    calls = {"n": 0}

    async def llm_complete(system, user, max_tokens=256):
        calls["n"] += 1
        return '["q"]' if calls["n"] == 1 else "yes"

    monkeypatch.setattr(llm, "complete", llm_complete)
    # A3: variants are embedded in one batched `embed(list)` call, not `embed_one`.
    monkeypatch.setattr(embeddings, "embed", lambda ts: _async([[0.1, 0.2, 0.3] for _ in ts]))
    monkeypatch.setattr(retrieve_mod.sparse, "sparse_one", lambda t: {"indices": [], "values": []})

    async def fake_search(kb_ids, qd, qs, k):
        return hits

    monkeypatch.setattr(qdrant_store, "hybrid_search", fake_search)

    async def fake_rerank(query, texts):
        return [1.0 - i * 0.1 for i in range(len(texts))]

    monkeypatch.setattr(reranker, "rerank", fake_rerank)

    async def fake_parents(ids):
        return {"P1": "PARENT ONE full section text", "P2": "PARENT TWO full section text"}

    monkeypatch.setattr(qdrant_store, "retrieve_parents", fake_parents)

    out = asyncio.run(retrieve_mod.retrieve("when does it terminate?", ["kb1"]))

    # Context is built from DISTINCT parents, not the bare children.
    assert "PARENT ONE" in out["context"] and "PARENT TWO" in out["context"]
    assert "child A" not in out["context"], "L2 hands the parent to the LLM"
    assert out["context"].count("PARENT ONE") == 1, "shared parent de-duplicated"

    # Citations still anchor on the children (precise quote + location), deduped by
    # (doc, page, clause): children 0 & 1 share (d1, page 1, clause "1") → one citation
    # (the highest-ranked, chunk 0, kept), child 2 is a distinct (page 2, clause "2").
    quotes = [c["quote_text"] for c in out["citations"]]
    assert any("child A" in q for q in quotes)
    assert {c["chunk_index"] for c in out["citations"]} == {0, 2}


def test_fail_closed_on_empty_allowlist(monkeypatch):
    """An empty KB allow-list ⇒ ZERO retrieval, and Qdrant is never touched
    (Libraries §4.4 / §11). The backend resolves the allow-list; if it's empty,
    we must not 'search everything'."""

    async def boom(*a, **k):  # any Qdrant/LLM call here is a failure
        raise AssertionError("retrieval must not run for an empty allow-list")

    monkeypatch.setattr(qdrant_store, "hybrid_search", boom)
    monkeypatch.setattr(llm, "complete", boom)

    out = asyncio.run(retrieve_mod.retrieve("anything", []))
    assert out == {"context": "", "citations": []}


def test_multi_query_fans_out_concurrently(monkeypatch):
    """Each sub-question's ~3 query variants are searched, and searches across
    sub-questions/variants run CONCURRENTLY (not one-at-a-time), de-duplicating
    by chunk text on a single-threaded merge."""
    # Merge-keeps-every-chunk is a pre-§1 covering-context invariant; run the legacy
    # assemble path (the §1 pool keeps only cited chunks + floor).
    monkeypatch.setattr(settings, "sub_answer_enabled", False)
    decompose_json = json.dumps(
        [
            {"subq": "A", "queries": ["A", "A1", "A2"]},
            {"subq": "B", "queries": ["B", "B1", "B2"]},
        ]
    )

    async def llm_complete(system, user, max_tokens=256):
        if "Decompose" in system:
            return decompose_json
        return "yes"  # grade: stop after the first (variant) round

    embedded: list[str] = []

    def fake_embed(ts):
        embedded.extend(ts)
        return _async([[0.1, 0.2] for _ in ts])

    active = {"now": 0, "max": 0}
    counter = {"n": 0}

    async def fake_search(kb_ids, qd, qs, k):
        active["now"] += 1
        active["max"] = max(active["max"], active["now"])
        await asyncio.sleep(0.02)  # hold the slot so overlap is observable
        active["now"] -= 1
        i = counter["n"]
        counter["n"] += 1
        return [
            {
                "score": 1.0,
                "payload": {
                    "doc_id": "d", "chunk_index": i, "page_number": 1,
                    "clause_section_ref": "1", "chunk_text": f"chunk-{i}",
                },
            }
        ]

    async def fake_rerank(query, texts):
        return [1.0 for _ in texts]

    monkeypatch.setattr(llm, "complete", llm_complete)
    monkeypatch.setattr(embeddings, "embed", fake_embed)
    monkeypatch.setattr(retrieve_mod.sparse, "sparse_one", lambda t: {"indices": [], "values": []})
    monkeypatch.setattr(qdrant_store, "hybrid_search", fake_search)
    monkeypatch.setattr(reranker, "rerank", fake_rerank)

    out = asyncio.run(retrieve_mod.retrieve("the question", ["kb1"]))

    # All six variant phrasings were actually searched (multi-query fan-out).
    assert {"A", "A1", "A2", "B", "B1", "B2"}.issubset(set(embedded))
    # Searches overlapped — proof of concurrency, not a sequential loop.
    assert active["max"] >= 2, f"expected concurrent searches, max in-flight={active['max']}"
    # The concurrent merge keeps every distinct chunk: all of them survive into the
    # context (the real merge check). They collapse to ONE citation because they share
    # (doc, page, clause) — the deliberate citation location-dedup.
    assert all(f"chunk-{i}" in out["context"] for i in range(counter["n"])), "merge keeps every distinct chunk"
    assert len(out["citations"]) == 1, "same (doc, page, clause) → one citation"


def test_concurrent_merge_dedups_shared_chunk(monkeypatch):
    """When variants surface the SAME chunk, the post-gather merge collapses it
    to one (the dedup is race-free because it runs after asyncio.gather)."""

    async def llm_complete(system, user, max_tokens=256):
        if "Decompose" in system:
            return json.dumps([{"subq": "A", "queries": ["A", "A1", "A2"]}])
        return "yes"

    async def fake_search(kb_ids, qd, qs, k):
        return [
            {
                "score": 1.0,
                "payload": {
                    "doc_id": "d", "chunk_index": 0, "page_number": 1,
                    "clause_section_ref": "1", "chunk_text": "same chunk for all variants",
                },
            }
        ]

    monkeypatch.setattr(llm, "complete", llm_complete)
    monkeypatch.setattr(embeddings, "embed", lambda ts: _async([[0.1] for _ in ts]))
    monkeypatch.setattr(retrieve_mod.sparse, "sparse_one", lambda t: {"indices": [], "values": []})
    monkeypatch.setattr(qdrant_store, "hybrid_search", fake_search)
    monkeypatch.setattr(reranker, "rerank", lambda q, texts: _async([1.0 for _ in texts]))

    out = asyncio.run(retrieve_mod.retrieve("q", ["kb1"]))
    assert len(out["citations"]) == 1, "identical chunk from 3 variants → one citation"


def _async(value):
    async def _f():
        return value
    return _f()
