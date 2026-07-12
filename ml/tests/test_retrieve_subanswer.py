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

"""Per-sub-question isolation, synthesis from a
consolidated [D#] pool that keeps relevant UNcited chunks (recall), a targeted retry on a
failed sub-answer, 1:1 citations, and strip-and-recite. Fully monkeypatched."""

import asyncio
import json

from app import llm
from app import retrieve as retrieve_mod
from app.config import settings


def _hit(subq: str) -> dict:
    return _mk(f"passage about {subq}", subq, doc=f"d-{subq}")


def _mk(text: str, section: str, doc: str = "d", rerank: float = 0.9, idx: int = 0) -> dict:
    return {
        "payload": {
            "doc_id": doc, "chunk_index": idx, "page_number": 1,
            "clause_section_ref": section, "chunk_text": text,
        },
        "_rerank": rerank,
    }


def _setup(monkeypatch, *, decompose, sub_answers, floor=None, resolve_hits=None, targeted=None):
    """Drive retrieve() with controlled decomposition, per-sub-Q hits, mini-answers, floor
    and targeted-retry hits. `sub_answers` values may be a str or a callable(user)->str (to
    vary by attempt). Captures each mini-answer's user prompt for the isolation assertion."""
    monkeypatch.setattr(settings, "sub_answer_enabled", True)
    captured: dict = {"users": []}

    def _answer_for(user: str):
        for key, ans in sub_answers.items():
            if f"Sub-question to answer: {key}" in user:
                return ans(user) if callable(ans) else ans
        return "NOT IN CONTEXT"

    async def fake_complete(system, user, max_tokens=256, **kw):
        if "Decompose" in system:
            return json.dumps(decompose)
        if "Answer ONE sub-question" in system:
            captured["users"].append(user)
            return _answer_for(user)
        return "yes"

    async def fake_resolve(item, kb_ids, sem, rounds):
        return resolve_hits(item["subq"]) if resolve_hits else [_hit(item["subq"])]

    async def fake_floor(prompt, kb_ids, sem, n):
        return floor or []

    async def fake_search_one(query, kb_ids, sem, *, qdense=None):
        return targeted or []  # the §3 targeted retry; [] ⇒ retry finds nothing

    monkeypatch.setattr(llm, "complete", fake_complete)
    monkeypatch.setattr(retrieve_mod, "_resolve_subq", fake_resolve)
    monkeypatch.setattr(retrieve_mod, "_floor_retrieve", fake_floor)
    monkeypatch.setattr(retrieve_mod, "_search_one", fake_search_one)
    return captured


def test_each_subanswer_sees_only_its_own_passages(monkeypatch):
    captured = _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"]}, {"subq": "B", "queries": ["B"]}],
        sub_answers={"A": "The answer to A is X [1].", "B": "The answer to B is Y [1]."},
    )
    asyncio.run(retrieve_mod.retrieve("A and B?", ["kb1"]))
    a_user = next(u for u in captured["users"] if "Sub-question to answer: A" in u)
    b_user = next(u for u in captured["users"] if "Sub-question to answer: B" in u)
    assert "passage about A" in a_user and "passage about B" not in a_user
    assert "passage about B" in b_user and "passage about A" not in b_user


def test_pool_citations_map_one_to_one(monkeypatch):
    _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"]}, {"subq": "B", "queries": ["B"]}],
        sub_answers={"A": "A holds [1].", "B": "B holds [1]."},
    )
    out = asyncio.run(retrieve_mod.retrieve("A and B?", ["kb1"]))
    for j, _c in enumerate(out["citations"], 1):
        assert f"[D{j}]" in out["context"]
    assert {c["clause_section_ref"] for c in out["citations"]} == {"A", "B"}


def test_pool_keeps_relevant_uncited_chunk(monkeypatch):
    # A sub-answer sees 3 chunks but cites only [1]; the pool must still
    # keep its top UNcited chunks so relevant passages the terse model skipped aren't lost.
    monkeypatch.setattr(settings, "pool_uncited_per_subq", 3)
    three = [
        _mk("cited chunk one", "s1", rerank=0.9, idx=0),
        _mk("uncited chunk two", "s2", rerank=0.8, idx=1),
        _mk("uncited chunk three", "s3", rerank=0.7, idx=2),
    ]
    _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"]}],
        sub_answers={"A": "Only the first matters [1]."},
        resolve_hits=lambda subq: three,
    )
    out = asyncio.run(retrieve_mod.retrieve("A?", ["kb1"]))
    assert "cited chunk one" in out["context"]
    assert "uncited chunk two" in out["context"], "top uncited chunk survives into the pool"
    assert "uncited chunk three" in out["context"]
    assert len(out["citations"]) == 3, "pool (cited + uncited) maps 1:1 to citations"


def test_pool_uncited_off_keeps_cited_only(monkeypatch):
    monkeypatch.setattr(settings, "pool_uncited_per_subq", 0)
    three = [
        _mk("cited chunk one", "s1", rerank=0.9, idx=0),
        _mk("uncited chunk two", "s2", rerank=0.8, idx=1),
    ]
    _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"]}],
        sub_answers={"A": "Only first [1]."},
        resolve_hits=lambda subq: three,
    )
    out = asyncio.run(retrieve_mod.retrieve("A?", ["kb1"]))
    assert "uncited chunk two" not in out["context"], "0 ⇒ cited-only pool"


def test_targeted_retry_recovers_a_failed_subanswer(monkeypatch):
    # First pass has irrelevant chunks → NOT IN CONTEXT → one targeted
    # re-retrieval brings the right section → the sub-answer succeeds (no honest-fail).
    monkeypatch.setattr(settings, "targeted_fallback_enabled", True)
    _setup(
        monkeypatch,
        decompose=[{"subq": "director duties s.171", "queries": ["director duties s.171"]}],
        sub_answers={
            "director duties s.171": lambda user: "Found in s.171 [1]." if "targeted s.171 passage" in user else "NOT IN CONTEXT",
        },
        resolve_hits=lambda subq: [_mk("irrelevant first-pass chunk", "x", rerank=0.4)],
        targeted=[_mk("targeted s.171 passage", "171", rerank=0.95)],
    )
    out = asyncio.run(retrieve_mod.retrieve("director duties?", ["kb1"]))
    assert "targeted s.171 passage" in out["context"], "targeted retry chunk reached synthesis"
    assert "171" in {c["clause_section_ref"] for c in out["citations"]}
    assert "Not found in the library" not in out["context"], "no honest-fail after a successful retry"


def test_targeted_retry_off_still_fails_honestly(monkeypatch):
    monkeypatch.setattr(settings, "targeted_fallback_enabled", False)
    _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"]}],
        sub_answers={"A": "NOT IN CONTEXT"},
        targeted=[_mk("would-have-helped", "z", rerank=0.95)],  # ignored when off
        floor=[],
    )
    out = asyncio.run(retrieve_mod.retrieve("A?", ["kb1"]))
    assert "would-have-helped" not in out["context"]


def test_strip_and_recite_removes_local_numbers(monkeypatch):
    _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"]}],
        sub_answers={"A": "The term is 12 months [1] and renews [1]."},
    )
    out = asyncio.run(retrieve_mod.retrieve("A?", ["kb1"]))
    ctx = out["context"]
    assert "The term is 12 months and renews" in ctx
    assert "[1]" not in ctx.split("Documents:")[0], "sub-answer local citations stripped"
    assert "[D1]" in ctx


def test_failed_subanswer_chunks_still_pool(monkeypatch):
    # A failed mini-answer whose retrieval found chunks must NOT be
    # censored — its chunks enter the pool and the block points to the Documents.
    monkeypatch.setattr(settings, "targeted_fallback_enabled", False)
    _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"]}, {"subq": "B", "queries": ["B"]}],
        sub_answers={"A": "A is answered [1].", "B": "NOT IN CONTEXT"},
    )
    out = asyncio.run(retrieve_mod.retrieve("A and B?", ["kb1"]))
    # B's reranked chunk is in the pool despite the mini-answer's refusal.
    assert "passage about B" in out["context"]
    assert {"A", "B"}.issubset({c["clause_section_ref"] for c in out["citations"]})
    assert "check the Documents below" in out["context"], "failed-with-retrieval block points to Documents"
    assert "Not found in the library" not in out["context"]
    b = next(sq for sq in out["debug"]["sub_questions"] if sq["subq"] == "B")
    assert b["status"] == "failed" and b["pool_contrib"] > 0


def test_failed_subanswer_empty_retrieval_says_not_found(monkeypatch):
    # Only a genuinely empty retrieval keeps the honest "Not found" refusal.
    monkeypatch.setattr(settings, "targeted_fallback_enabled", False)
    _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"]}, {"subq": "B", "queries": ["B"]}],
        sub_answers={"A": "A is answered [1].", "B": "NOT IN CONTEXT"},
        resolve_hits=lambda subq: [] if subq == "B" else [_hit(subq)],
    )
    out = asyncio.run(retrieve_mod.retrieve("A and B?", ["kb1"]))
    assert "Not found in the library" in out["context"]
    assert {c["clause_section_ref"] for c in out["citations"]} == {"A"}


def test_pool_starvation_every_subq_represented(monkeypatch):
    # 6 sub-questions must EACH land in the pool (round-robin tiers +
    # budget that scales with the sub-question count) — no early-sub-Q hogging.
    subqs = [f"q{i}" for i in range(6)]
    _setup(
        monkeypatch,
        decompose=[{"subq": s, "queries": [s]} for s in subqs],
        sub_answers={s: f"answer {s} [1]." for s in subqs},
        resolve_hits=lambda subq: [_mk(f"chunk for {subq}", subq, doc=subq)],
    )
    out = asyncio.run(retrieve_mod.retrieve("six parts?", ["kb1"]))
    contribs = {sq["subq"]: sq["pool_contrib"] for sq in out["debug"]["sub_questions"]}
    assert all(contribs[s] > 0 for s in subqs), f"a sub-question was starved: {contribs}"
    assert out["debug"]["pool_total"] >= 6


def test_parent_dedup_one_block_per_parent(monkeypatch):
    # Two pooled children of ONE parent → ONE [D#] block (the parent
    # section), citations stay 1:1 with blocks, and the block contains the citation quote.
    monkeypatch.setattr(settings, "pool_uncited_per_subq", 3)
    two = [
        {"payload": {"doc_id": "d", "chunk_index": 0, "page_number": 1,
                     "clause_section_ref": "s171", "chunk_text": "child one about duties", "parent_id": "P1"}, "_rerank": 0.9},
        {"payload": {"doc_id": "d", "chunk_index": 1, "page_number": 1,
                     "clause_section_ref": "s171", "chunk_text": "child two more duties", "parent_id": "P1"}, "_rerank": 0.8},
    ]

    async def fake_parents(ids):
        return {"P1": "PARENT SECTION: child one about duties; child two more duties; plus the proviso."}

    monkeypatch.setattr(retrieve_mod.qdrant_store, "retrieve_parents", fake_parents)
    _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"]}],
        sub_answers={"A": "Both matter [1][2]."},
        resolve_hits=lambda subq: two,
    )
    out = asyncio.run(retrieve_mod.retrieve("A?", ["kb1"]))
    docs = out["context"].split("Documents:")[-1]
    assert "[D1]" in docs and "[D2]" not in docs, "two children of one parent collapse to one [D#] block"
    assert "PARENT SECTION" in out["context"], "the parent section (with the proviso) is expanded in"
    assert len(out["citations"]) == 1, "1:1 [D#] ⇔ citations after parent-dedup"
    assert out["debug"]["parents_expanded"] == 1
    quote = out["citations"][0]["quote_text"]
    assert quote and quote in out["context"], "citation quote is present in its [D#] block (click-highlight)"


def test_scope_tag_labels_the_block(monkeypatch):
    _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"], "scope": "scenario-1"}],
        sub_answers={"A": "A holds [1]."},
    )
    out = asyncio.run(retrieve_mod.retrieve("A?", ["kb1"]))
    assert "(scope: scenario-1)" in out["context"]


def test_total_washout_empty_retrieval(monkeypatch):
    # Genuine washout: no sub-answer AND empty retrieval AND no floor → honest "Not found",
    # empty pool, no crash (the round-robin fallback over empty hits yields nothing).
    monkeypatch.setattr(settings, "targeted_fallback_enabled", False)
    _setup(
        monkeypatch,
        decompose=[{"subq": "A", "queries": ["A"]}],
        sub_answers={"A": "NOT IN CONTEXT"},
        resolve_hits=lambda subq: [],
        floor=[],
    )
    out = asyncio.run(retrieve_mod.retrieve("A?", ["kb1"]))
    assert out["citations"] == []
    assert "Not found in the library" in out["context"]
    assert out["debug"]["pool_total"] == 0


def test_targeted_query_extracts_section_refs():
    q = retrieve_mod._targeted_query("What must a director declare under s.177 and section 182?")
    assert "s.177" in q.lower().replace(" ", "") or "s.177" in q
    assert "182" in q
    # No refs → falls back to the sub-question text.
    assert retrieve_mod._targeted_query("plain question") == "plain question"
