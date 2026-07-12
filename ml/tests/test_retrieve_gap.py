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

"""Bounded LLM gap-check + deterministic fill before synthesis. The LLM names
what a part still needs; a deterministic fetch tops up the slice from a non-evictable budget.
No network: `llm.complete` and the `qdrant_store` look-ups are monkeypatched."""

import asyncio
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import llm
from app import qdrant_store
from app import retrieve as retrieve_mod
from app.config import settings


def _slice(title, sections=None, subq_indices=(0,)):
    return {"title": title, "context": "Sub-question 1: x\nAnswer: partial.", "sections": sections or [],
            "n_blocks": 0, "has_evidence": True, "subq_indices": list(subq_indices)}


def _sa(subq="x", status="ok"):
    return {"subq": subq, "scope": "", "status": status, "answer": "a", "cited": [], "ranked": [],
            "best_rerank": 0.5, "retried": False}


def _payload(section, text="operative text"):
    return {"doc_id": "d1", "chunk_index": 1, "page_number": 2, "parent_id": None,
            "clause_section_ref": section, "section_nums": [int("".join(c for c in section if c.isdigit()) or 0)],
            "chunk_text": f"{section} {text}"}


def _wire(monkeypatch, gap_response, *, by_sections=None, search=None, sections_raise=False):
    """Install the fakes; return a dict of call-logs."""
    calls = {"complete": 0, "by_sections": [], "search": 0}

    async def fake_complete(system, user, max_tokens=512, **kw):
        calls["complete"] += 1
        if isinstance(gap_response, Exception):
            raise gap_response
        return json.dumps(gap_response)

    async def fake_by_sections(kb_ids, section_ids, limit=24):
        calls["by_sections"].append(list(section_ids))
        if sections_raise:
            raise RuntimeError("qdrant down")
        return (by_sections or {}).get(tuple(section_ids), [])

    async def fake_search_one(query, kb_ids, sem, **kw):
        calls["search"] += 1
        return search or []

    async def fake_toc(kb_ids, text, limit=2):
        return []

    async def fake_parents(ids):
        return {}

    monkeypatch.setattr(llm, "complete", fake_complete)
    monkeypatch.setattr(llm, "set_stage", lambda *a, **k: None)
    monkeypatch.setattr(llm, "set_guided", lambda *a, **k: None)
    monkeypatch.setattr(qdrant_store, "fetch_by_sections", fake_by_sections)
    monkeypatch.setattr(qdrant_store, "toc_search", fake_toc)
    monkeypatch.setattr(qdrant_store, "retrieve_parents", fake_parents)
    monkeypatch.setattr(retrieve_mod, "_search_one", fake_search_one)
    return calls


def _run(parts, citations, pool, *, prompt="1. A?\n2. B?"):
    async def go():
        est = retrieve_mod._ExpandStat(8)
        retrieve_mod._expand_stat.set(est)
        sem = asyncio.Semaphore(4)
        ctx = await retrieve_mod._gap_round(prompt, parts, [_sa()], "ctx", citations, [], pool, ["kb"], sem)
        return est, ctx

    return asyncio.new_event_loop().run_until_complete(go())


def test_missing_section_is_filled_into_slice(monkeypatch):
    calls = _wire(monkeypatch,
                  {"sufficient": False, "missing": [{"need": "ratification", "query": "s239", "sections": ["239"]}]},
                  by_sections={("239",): [_payload("239")]})
    part = _slice("Ratification and derivative claims")
    citations = []
    est, _ = _run([part], citations, [])
    assert calls["by_sections"] == [["239"]]
    assert "239" in part["sections"] and part["n_blocks"] == 1 and part["has_evidence"]
    assert "[D1] 239" in part["context"] and len(citations) == 1
    assert est.gap_sections_added == 1 and est.gap_rounds == 1 and est.gap_queries == 1


def test_sufficient_skips_fill(monkeypatch):
    calls = _wire(monkeypatch, {"sufficient": True, "missing": []})
    part = _slice("Well-covered part", sections=["171"])
    est, _ = _run([part], [], [])
    assert calls["by_sections"] == [] and calls["search"] == 0
    assert est.gap_sections_added == 0 and part["n_blocks"] == 0


def test_disabled_is_noop(monkeypatch):
    calls = _wire(monkeypatch, {"sufficient": False, "missing": [{"need": "x", "query": "x", "sections": ["1"]}]})
    monkeypatch.setattr(settings, "gap_round_enabled", False)
    part = _slice("Part")
    est, _ = _run([part], [], [])
    assert calls["complete"] == 0 and est.gap_sections_added == 0


def test_reserve_caps_appended_blocks(monkeypatch):
    # The model names 3 items each yielding a distinct section; reserve=2 bounds the appends.
    missing = [{"need": f"n{i}", "query": f"q{i}", "sections": [str(500 + i)]} for i in range(3)]
    by = {(str(500 + i),): [_payload(str(500 + i))] for i in range(3)}
    _wire(monkeypatch, {"sufficient": False, "missing": missing}, by_sections=by)
    monkeypatch.setattr(settings, "gap_reserve", 2)
    part = _slice("Multi-gap part")
    citations = []
    est, _ = _run([part], citations, [])
    assert est.gap_sections_added == 2 and len(citations) == 2 and part["n_blocks"] == 2


def test_missing_capped_at_three(monkeypatch):
    # Five named items must be truncated to 3 by the parser.
    missing = [{"need": f"n{i}", "query": f"q{i}", "sections": [str(600 + i)]} for i in range(5)]
    by = {(str(600 + i),): [_payload(str(600 + i))] for i in range(5)}
    _wire(monkeypatch, {"sufficient": False, "missing": missing}, by_sections=by)
    part = _slice("Over-named part")
    est, _ = _run([part], [], [])
    assert est.gap_sections_added == 3


def test_gapcheck_error_is_fail_soft(monkeypatch):
    calls = _wire(monkeypatch, RuntimeError("llm down"))
    part = _slice("Part")
    est, _ = _run([part], [], [])
    assert calls["by_sections"] == [] and est.gap_sections_added == 0


def test_fetch_error_falls_back_to_query(monkeypatch):
    # fetch_by_sections raises, but the BM25 query path still yields a payload → gap still filled.
    _wire(monkeypatch,
          {"sufficient": False, "missing": [{"need": "x", "query": "x", "sections": ["9"]}]},
          search=[{"payload": _payload("994"), "_rerank": 0.9}], sections_raise=True)
    part = _slice("Part")
    est, _ = _run([part], [], [])
    assert est.gap_sections_added == 1 and "994" in part["sections"]


def test_already_pooled_chunk_is_deduped(monkeypatch):
    pooled = _payload("239")
    _wire(monkeypatch,
          {"sufficient": False, "missing": [{"need": "x", "query": "x", "sections": ["239"]}]},
          by_sections={("239",): [pooled]})
    part = _slice("Part")
    est, _ = _run([part], [], [pooled])  # already in the pool
    assert est.gap_sections_added == 0 and part["n_blocks"] == 0


def test_unified_mode_appends_to_context(monkeypatch):
    # No parts → one gap-check on the turn; recovered block joins the unified context string.
    _wire(monkeypatch,
          {"sufficient": False, "missing": [{"need": "x", "query": "x", "sections": ["239"]}]},
          by_sections={("239",): [_payload("239")]})
    citations = []
    est, ctx = _run([], citations, [])
    assert est.gap_sections_added == 1 and "[D1] 239" in ctx and len(citations) == 1
