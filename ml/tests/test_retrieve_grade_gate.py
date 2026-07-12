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

"""The rerank grade-gate. When the top
cross-encoder score clears `grade_skip_threshold` the per-sub-question LLM grade
is skipped (the dominant TTFT saving on good retrieval); below it, or with the
gate off (threshold 0, the default), the LLM grade runs exactly as before. The
gate only skips the grade — it never changes which passages are retrieved.
Backends stubbed — no network."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import pytest

from app import embeddings, llm, qdrant_store, reranker
from app import retrieve as retrieve_mod
from app.config import settings


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


@pytest.fixture
def stubbed(monkeypatch):
    """Stub the search pipeline; `reranker.rerank` returns a controllable score. `llm.complete`
    is a spy: it distinguishes the GRADE call (yes/partial/no) from a REFORMULATE call so a
    test can assert the gate skips the grade without suppressing a reformulate round."""
    grade_calls = {"n": 0}
    search_calls = {"n": 0}

    def set_score(score: float):
        async def fake_rerank(query, docs):
            return [score for _ in docs]

        monkeypatch.setattr(reranker, "rerank", fake_rerank)

    async def fake_complete(system, user, max_tokens=4):
        if "one word" in system or "yes, partial" in system:  # the grade prompt
            grade_calls["n"] += 1
            return "no"  # never resolve via grade → lets reformulate rounds run
        return "reformulated query"  # the reformulate prompt

    async def fake_embed(texts):
        return [[0.1, 0.2] for _ in texts]

    async def fake_hybrid(kb_ids, qd, qs, k):
        search_calls["n"] += 1
        return [{"payload": {"doc_id": "d", "chunk_index": 0, "chunk_text": "the term is 12 months"}}]

    monkeypatch.setattr(llm, "complete", fake_complete)
    monkeypatch.setattr(embeddings, "embed", fake_embed)
    monkeypatch.setattr(retrieve_mod.sparse, "sparse_one", lambda t: {"indices": [], "values": []})
    monkeypatch.setattr(qdrant_store, "hybrid_search", fake_hybrid)
    set_score(0.9)  # default: a confident top hit
    return grade_calls, set_score, search_calls


def _resolve(rounds=1):
    item = {"subq": "q", "queries": ["q"]}
    return _run(retrieve_mod._resolve_subq(item, ["kb1"], asyncio.Semaphore(4), rounds))


def _skip_count() -> float:
    return retrieve_mod._RAG_GRADE_SKIP._value.get()


def test_gate_fires_skips_llm_grade(stubbed, monkeypatch):
    grade_calls, _, _ = stubbed
    monkeypatch.setattr(settings, "grade_skip_threshold", 0.5)  # below the 0.9 score
    before = _skip_count()
    found = _resolve()
    assert grade_calls["n"] == 0, "confident rerank score skips the LLM grade CALL"
    assert found, "the sub-question still returns its hits"
    assert _skip_count() == before + 1, "the skip is counted"


def test_gate_below_threshold_still_grades(stubbed, monkeypatch):
    grade_calls, set_score, _ = stubbed
    set_score(0.3)  # below the threshold → ambiguous
    monkeypatch.setattr(settings, "grade_skip_threshold", 0.5)
    before = _skip_count()
    _resolve()
    assert grade_calls["n"] == 1, "low rerank confidence falls back to the LLM grade"
    assert _skip_count() == before, "no skip counted"


def test_gate_off_by_default(stubbed):
    grade_calls, _, _ = stubbed
    # The gate ships OFF (no safe universal reranker scale) — the LLM
    # grade always runs until an operator calibrates a threshold from the eval distribution.
    assert settings.grade_skip_threshold == 0.0
    _resolve()
    assert grade_calls["n"] == 1, "default 0 ⇒ gate off, LLM grade always runs"


def test_skip_does_not_suppress_reformulate_round(stubbed, monkeypatch):
    # Even when the gate fires every round (confident score),
    # it only skips the grade CALL — it must NOT mark coverage resolved, so the reformulate
    # round still runs. Two rounds ⇒ two searches (the second on the reformulated query).
    grade_calls, _, search_calls = stubbed
    monkeypatch.setattr(settings, "grade_skip_threshold", 0.5)  # fires on the 0.9 score
    _resolve(rounds=2)
    assert grade_calls["n"] == 0, "grade call skipped both rounds"
    assert search_calls["n"] == 2, "reformulate round still ran — the gate did NOT short-circuit it"


def test_search_one_annotates_rerank_score(stubbed):
    hits = _run(retrieve_mod._search_one("q", ["kb1"], asyncio.Semaphore(1), qdense=[0.1, 0.2]))
    assert hits and hits[0]["_rerank"] == pytest.approx(0.9)
