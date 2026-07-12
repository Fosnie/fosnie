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

"""A sub-question's query variants are embedded
in ONE `embeddings.embed([...])` call per round, not N `embed_one` round-trips,
and each query's dense vector zips back to it by index. Plus the A5 guard: the
retrieval loop's system prompts stay module-level and interpolation-free so the
small per-stage prefixes remain prefix-cacheable."""

import asyncio
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import pytest

from app import retrieve

# Matches an f-string-style `{name}` interpolation slot, but NOT the literal JSON
# braces the decompose prompt legitimately contains (e.g. `{"subq": ...}`).
_INTERP = re.compile(r"\{[A-Za-z_][A-Za-z0-9_]*\}")


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


@pytest.fixture
def stub_backends(monkeypatch):
    """Fake every backend `_resolve_subq` touches; record the embed batching and
    the dense vector each search received so we can prove the zip alignment."""
    embed_calls: list[list[str]] = []
    searched: list[list[float]] = []

    async def fake_embed(texts):
        embed_calls.append(list(texts))
        # A distinct, index-tagged vector per input so we can check alignment.
        return [[float(i)] for i in range(len(texts))]

    async def fake_embed_one(text):
        raise AssertionError("embed_one must not be called on the batched path (A3)")

    def fake_sparse_one(query):
        return {"indices": [], "values": []}

    async def fake_hybrid(kb_ids, qdense, qsparse, k):
        searched.append(qdense)
        return [{"payload": {"chunk_text": f"chunk-{qdense[0]}", "doc_id": "d", "chunk_index": 0}}]

    async def fake_rerank(query, docs):
        return [1.0] * len(docs)

    async def fake_grade_complete(system, user, max_tokens=4):
        return "yes"  # resolve in round 1 → exactly one embed batch

    monkeypatch.setattr(retrieve.embeddings, "embed", fake_embed)
    monkeypatch.setattr(retrieve.embeddings, "embed_one", fake_embed_one)
    monkeypatch.setattr(retrieve.sparse, "sparse_one", fake_sparse_one)
    monkeypatch.setattr(retrieve.qdrant_store, "hybrid_search", fake_hybrid)
    monkeypatch.setattr(retrieve.reranker, "rerank", fake_rerank)
    monkeypatch.setattr(retrieve.llm, "complete", fake_grade_complete)
    return embed_calls, searched


def test_variants_embedded_in_one_call(stub_backends):
    embed_calls, searched = stub_backends
    item = {"subq": "q", "queries": ["a", "b", "c"]}
    sem = asyncio.Semaphore(4)

    found = _run(retrieve._resolve_subq(item, ["kb1"], sem, rounds=2))

    assert len(embed_calls) == 1, "all three variants embedded in a single batched call"
    assert embed_calls[0] == ["a", "b", "c"], "the whole variant list goes in one request"
    # Each variant's index-tagged vector reached hybrid_search → zip held by index.
    assert sorted(searched) == [[0.0], [1.0], [2.0]]
    assert found, "hits returned"


def test_empty_queries_skip_embed(stub_backends):
    embed_calls, _ = stub_backends
    item = {"subq": "q", "queries": []}
    sem = asyncio.Semaphore(4)
    _run(retrieve._resolve_subq(item, ["kb1"], sem, rounds=1))
    assert embed_calls == [], "no variants → no embed request"


def test_retrieve_system_prompts_are_constants():
    # A5: module-level, non-empty, no `{var}` interpolation can slip in and void
    # the prefix cache (literal JSON braces in the prompt are fine).
    for name in ("_DECOMPOSE_SYSTEM", "_GRADE_SYSTEM", "_REFORMULATE_SYSTEM"):
        val = getattr(retrieve, name)
        assert isinstance(val, str) and val.strip(), f"{name} must be a non-empty constant"
        assert not _INTERP.search(val), f"{name} looks interpolated ({{var}}) — voids the prefix cache"
