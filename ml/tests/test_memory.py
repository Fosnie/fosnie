"""Memory recall index: upsert + relevance search ranking. Embeddings are
monkeypatched (deterministic vectors) so ordering is asserted without a model;
the Qdrant collection is real (dev stack)."""

import asyncio

import pytest

from app import embeddings, memory


def test_memory_upsert_and_search(monkeypatch):
    # Deterministic 3-d vectors keyed by a marker word in the content/query.
    table = {
        "ocean": [1.0, 0.0, 0.0],
        "mountain": [0.0, 1.0, 0.0],
        "desert": [0.0, 0.0, 1.0],
    }

    def vec_for(text: str) -> list[float]:
        for k, v in table.items():
            if k in text.lower():
                return v
        return [0.1, 0.1, 0.1]

    async def fake_embed(texts):
        return [vec_for(t) for t in texts]

    async def fake_dim():
        return 3

    monkeypatch.setattr(embeddings, "embed", fake_embed)
    monkeypatch.setattr(embeddings, "dimension", fake_dim)

    import uuid

    # Unique scope per run to avoid cross-test contamination. Fact ids are UUIDs
    # (as in production — db::new_id), since Qdrant point ids must be UUID/uint.
    scope = f"test_{uuid.uuid4().hex}"
    ocean_id = str(uuid.uuid4())

    async def run():
        facts = [
            (ocean_id, "I love the ocean and the sea"),
            (str(uuid.uuid4()), "The mountain trail was steep"),
            (str(uuid.uuid4()), "A hot desert at noon"),
        ]
        for fid, content in facts:
            await memory.upsert(scope, fid, content)
        return await memory.search(scope, "tell me about the ocean", limit=3)

    ids = asyncio.run(run())
    assert ids, "search returns ranked fact ids"
    assert ids[0] == ocean_id, f"most relevant first, got {ids}"
