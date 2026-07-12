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

"""Reranker-resilience. When the reranker is DOWN, _search_one must
keep the hybrid-fusion order (the RRF `score` on each hit) instead of a flat 0.0, and the
per-turn stat must record the degradation. No network."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import pytest

from app import embeddings, qdrant_store, reranker
from app import retrieve as retrieve_mod


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


@pytest.fixture
def hits_with_scores(monkeypatch):
    # Two hits whose RRF fusion scores (hit["score"]) rank B above A.
    async def fake_hybrid(kb_ids, qd, qs, k):
        return [
            {"score": 0.10, "payload": {"doc_id": "a", "chunk_index": 0, "chunk_text": "chunk A"}},
            {"score": 0.90, "payload": {"doc_id": "b", "chunk_index": 1, "chunk_text": "chunk B"}},
        ]

    async def fake_embed_one(q):
        return [0.1, 0.2]

    monkeypatch.setattr(qdrant_store, "hybrid_search", fake_hybrid)
    monkeypatch.setattr(embeddings, "embed_one", fake_embed_one)
    monkeypatch.setattr(retrieve_mod.sparse, "sparse_one", lambda t: {"indices": [], "values": []})


def test_degraded_falls_back_to_hybrid_score(hits_with_scores, monkeypatch):
    # Reranker down: rerank returns a flat 0.0 and degraded_now() is True.
    async def down_rerank(query, docs):
        return [0.0 for _ in docs]

    monkeypatch.setattr(reranker, "rerank", down_rerank)
    monkeypatch.setattr(reranker, "degraded_now", lambda: True)

    stat = retrieve_mod._RerankStat()
    retrieve_mod._rerank_stat.set(stat)
    hits = _run(retrieve_mod._search_one("q", ["kb1"], asyncio.Semaphore(1)))

    # Ordered by the RRF score (B before A), NOT flattened to a 0.0 tie, and _rerank
    # carries the hybrid score so the grade-gate/pool see a real signal.
    assert [h["payload"]["doc_id"] for h in hits] == ["b", "a"]
    assert hits[0]["_rerank"] == pytest.approx(0.90)
    assert stat.degraded == 1 and stat.calls == 1


def test_healthy_uses_rerank_scores(hits_with_scores, monkeypatch):
    # Reranker up: rerank scores dominate; A scored above B here.
    async def up_rerank(query, docs):
        return [0.95 if "A" in d else 0.05 for d in docs]

    monkeypatch.setattr(reranker, "rerank", up_rerank)
    monkeypatch.setattr(reranker, "degraded_now", lambda: False)

    stat = retrieve_mod._RerankStat()
    retrieve_mod._rerank_stat.set(stat)
    hits = _run(retrieve_mod._search_one("q", ["kb1"], asyncio.Semaphore(1)))

    assert [h["payload"]["doc_id"] for h in hits] == ["a", "b"]
    assert hits[0]["_rerank"] == pytest.approx(0.95)
    assert stat.degraded == 0 and stat.calls == 1


def test_activity_summary_wording():
    meta = {"parts_detected": 5, "parts_covered": 5, "subqs_injected": 2}
    stat = retrieve_mod._RerankStat()
    stat.calls, stat.degraded = 8, 3
    line = retrieve_mod._activity_summary(meta, n_subq=8, n_ok=7, n_sources=28, n_sections=14, rstat=stat)
    assert line.startswith("Coverage: 5/5 parts")
    assert "8 sub-questions (1 not found)" in line
    assert "28 documents, 14 sections" in line
    assert "reranker degraded (using hybrid scores)" in line
    assert "2 parts recovered" in line


def test_activity_summary_healthy_singular():
    meta = {"parts_detected": 0, "parts_covered": 0, "subqs_injected": 0}
    stat = retrieve_mod._RerankStat()
    stat.calls, stat.degraded = 1, 0
    line = retrieve_mod._activity_summary(meta, n_subq=1, n_ok=1, n_sources=1, n_sections=1, rstat=stat)
    assert "Coverage:" not in line  # no enumerated parts → omit the parts clause
    assert "1 sub-question" in line and "not found" not in line
    assert "1 document, 1 section" in line
    assert line.endswith("reranker OK")
