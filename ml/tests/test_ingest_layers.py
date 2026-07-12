"""Ingest wiring for the L2/L3 toggles — fully monkeypatched (no Qdrant, no
model). Captures what would be embedded + upserted to assert the layer behaviour."""

import asyncio

import pytest

from app import contextual, embeddings, extract, ingest, qdrant_store, sparse
from app.config import settings


def _patch_common(monkeypatch, captured):
    monkeypatch.setattr(
        extract, "extract_pages",
        lambda path, mime=None: [(1, "1. TERM\n" + ("alpha " * 200) + "\n\n2. FEES\n" + ("beta " * 200))],
    )

    async def fake_embed(texts):
        captured["embedded"].extend(texts)
        return [[0.1, 0.2, 0.3] for _ in texts]

    monkeypatch.setattr(embeddings, "embed", fake_embed)
    monkeypatch.setattr(sparse, "sparse_embed", lambda texts: [{"indices": [], "values": []} for _ in texts])

    async def noop(*a, **k):
        return None

    monkeypatch.setattr(qdrant_store, "ensure_collection", noop)
    monkeypatch.setattr(qdrant_store, "delete_doc", noop)
    monkeypatch.setattr(qdrant_store, "ensure_parents", noop)

    async def fake_upsert(rows, dense, sp):
        captured["rows"] = rows

    async def fake_upsert_parents(parents):
        captured["parents"] = parents

    monkeypatch.setattr(qdrant_store, "upsert", fake_upsert)
    monkeypatch.setattr(qdrant_store, "upsert_parents", fake_upsert_parents)


def _run(monkeypatch, parent_child, contextual_on):
    monkeypatch.setattr(settings, "parent_child", parent_child)
    monkeypatch.setattr(settings, "contextual_retrieval", contextual_on)
    if contextual_on:
        async def fake_ctx(doc_text, chunk):
            return "BLURB"
        monkeypatch.setattr(contextual, "contextualise", fake_ctx)

    captured = {"embedded": [], "rows": [], "parents": []}
    _patch_common(monkeypatch, captured)
    asyncio.run(ingest.ingest_document("doc1", "kb1", "/x.txt", "text/plain", 3))
    return captured


def test_l0_flat_default(monkeypatch):
    cap = _run(monkeypatch, parent_child=False, contextual_on=False)
    assert cap["rows"], "chunks upserted"
    assert all("parent_id" not in r["payload"] for r in cap["rows"]), "no parent_id when L2 off"
    assert cap["parents"] == [], "no parents upserted"
    # Embedded text == stored chunk_text (no blurb) for the flat path.
    assert set(cap["embedded"]) == {r["payload"]["chunk_text"] for r in cap["rows"]}


def test_l2_parent_child(monkeypatch):
    cap = _run(monkeypatch, parent_child=True, contextual_on=False)
    assert cap["parents"], "parents upserted under L2"
    pids = {p["parent_id"] for p in cap["parents"]}
    assert cap["rows"] and all(r["payload"].get("parent_id") in pids for r in cap["rows"]), \
        "every child references a real parent"
    assert all(p["doc_id"] == "doc1" for p in cap["parents"])


def test_l3_contextual_embeds_blurb_keeps_original(monkeypatch):
    cap = _run(monkeypatch, parent_child=False, contextual_on=True)
    assert cap["embedded"] and all(t.startswith("BLURB") for t in cap["embedded"]), \
        "embedded text carries the situating blurb"
    assert all(not r["payload"]["chunk_text"].startswith("BLURB") for r in cap["rows"]), \
        "stored chunk_text stays the verbatim original"
    assert all(r["payload"].get("context") == "BLURB" for r in cap["rows"])
