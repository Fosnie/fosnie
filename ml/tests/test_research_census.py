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

"""Corpus census: cache hit/miss partitioning, stuff-vs-map_reduce routing,
the stuff-whole-corpus fast path (zero note LLM calls), deadline clipping →
unreviewed, and stub-on-failure. All deps mocked — no Qdrant, no LLM, no I/O."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio
import time

from app import extract, llm, map_reduce, qdrant_store
from app.config import settings
from app.research import census as census_mod
from app.research.bank import Bank
from app.research.budgets import budgets


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _docs(n):
    return [
        {"doc_id": f"d{i}", "kb_id": "kb", "kb_name": "KB",
         "filename": f"f{i}.txt", "path": f"/x/{i}", "mime": "text/plain"}
        for i in range(1, n + 1)
    ]


def _no_cache(monkeypatch):
    async def get_notes(ids):
        return {}

    async def upsert_notes(notes):
        return None

    monkeypatch.setattr(qdrant_store, "get_notes", get_notes)
    monkeypatch.setattr(qdrant_store, "upsert_notes", upsert_notes)


def test_cache_hit_uses_payload_no_extract_or_llm(monkeypatch):
    hit = {
        "doc_id": "d1", "knowledge_base_id": "kb", "schema_version": census_mod.SCHEMA_VERSION,
        "model_id": "m", "note": {"doc_type": "memo", "claims": ["cached claim"], "quotes": ["cq"]},
    }

    async def get_notes(ids):
        return {"d1": hit}

    monkeypatch.setattr(qdrant_store, "get_notes", get_notes)
    extracted = {"n": 0}

    def fake_extract(path, mime=None):
        extracted["n"] += 1
        return "should not be read"

    monkeypatch.setattr(extract, "extract", fake_extract)

    async def dead_llm(system, user, max_tokens=0):
        raise AssertionError("cache hit must not call the LLM")

    monkeypatch.setattr(llm, "complete", dead_llm)

    bank = Bank()
    b = budgets(65_536, "files")
    res = _run(census_mod.run_census(_docs(1), bank, b, time.monotonic() + 600, "m"))
    assert res.reviewed == 1
    assert extracted["n"] == 0, "cache hit reads neither disk nor the LLM"
    note = bank.get("D1").note
    assert "cached claim" in note.claims and note.quotes == ["cq"]


def test_stuff_fast_path_no_note_calls(monkeypatch):
    _no_cache(monkeypatch)
    monkeypatch.setattr(extract, "extract", lambda path, mime=None: "tiny doc body")
    monkeypatch.setattr(settings, "stuff_fraction", 0.45)  # generous ⇒ small corpus fits

    async def dead_llm(system, user, max_tokens=0):
        raise AssertionError("the fast path must not call the LLM")

    monkeypatch.setattr(llm, "complete", dead_llm)

    bank = Bank()
    b = budgets(65_536, "files")
    res = _run(census_mod.run_census(_docs(2), bank, b, time.monotonic() + 600, "m"))
    assert res.reviewed == 2 and res.stuffed_corpus is True
    assert bank.get("D1").note.full_text == "tiny doc body", "writer reads full text directly"


def test_note_building_stuff_route(monkeypatch):
    _no_cache(monkeypatch)
    # Force the per-document note path (not the fast path) with a near-zero stuff
    # fraction, and a small doc so it is stuffed (no map-reduce).
    monkeypatch.setattr(settings, "stuff_fraction", 0.0001)
    # Bigger than the (near-zero) stuff budget so the fast path is declined, but
    # smaller than the census window so it is stuffed (not map-reduced).
    monkeypatch.setattr(extract, "extract", lambda path, mime=None: "a modest document body " * 10)
    mr_calls = {"n": 0}

    async def fake_mr(text, prompt):
        mr_calls["n"] += 1
        return {"mode": "map_reduce", "sections": [], "text": "digest"}

    monkeypatch.setattr(map_reduce, "map_reduce", fake_mr)
    llm_calls = {"n": 0}

    async def fake_llm(system, user, max_tokens=0):
        llm_calls["n"] += 1
        return '{"doc_type": "report", "claims": ["a built claim"], "quotes": ["q"]}'

    monkeypatch.setattr(llm, "complete", fake_llm)
    upserted = {}

    async def upsert_notes(notes):
        upserted["notes"] = notes

    monkeypatch.setattr(qdrant_store, "upsert_notes", upsert_notes)

    bank = Bank()
    b = budgets(65_536, "files")
    res = _run(census_mod.run_census(_docs(1), bank, b, time.monotonic() + 600, "m"))
    assert res.reviewed == 1 and res.stuffed_corpus is False
    assert llm_calls["n"] == 1, "one note call for the single miss"
    assert mr_calls["n"] == 0, "small doc is stuffed, not map-reduced"
    assert "a built claim" in bank.get("D1").note.claims
    # Fresh note cached with the schema version + model id.
    assert upserted["notes"][0]["schema_version"] == census_mod.SCHEMA_VERSION
    assert upserted["notes"][0]["model_id"] == "m"


def test_large_doc_uses_map_reduce(monkeypatch):
    _no_cache(monkeypatch)
    monkeypatch.setattr(settings, "stuff_fraction", 0.0001)
    big = "word " * 20_000  # ~20k tokens ≫ census_input_tokens floor (6k)
    monkeypatch.setattr(extract, "extract", lambda path, mime=None: big)
    mr_calls = {"n": 0}

    async def fake_mr(text, prompt):
        mr_calls["n"] += 1
        return {"mode": "map_reduce", "sections": [{"x": 1}], "text": "the digest"}

    monkeypatch.setattr(map_reduce, "map_reduce", fake_mr)

    async def fake_llm(system, user, max_tokens=0):
        return '{"claims": ["c"], "quotes": []}'

    monkeypatch.setattr(llm, "complete", fake_llm)

    bank = Bank()
    b = budgets(65_536, "files")
    _run(census_mod.run_census(_docs(1), bank, b, time.monotonic() + 600, "m"))
    assert mr_calls["n"] == 1, "a doc over the census window is map-reduced first"


def test_deadline_clips_to_unreviewed(monkeypatch):
    _no_cache(monkeypatch)
    monkeypatch.setattr(extract, "extract", lambda path, mime=None: "body")

    async def fake_llm(system, user, max_tokens=0):
        return '{"claims": ["c"], "quotes": []}'

    monkeypatch.setattr(llm, "complete", fake_llm)

    bank = Bank()
    b = budgets(65_536, "files")
    # Deadline already passed ⇒ every uncached doc goes to the unreviewed list.
    res = _run(census_mod.run_census(_docs(3), bank, b, time.monotonic() - 1, "m"))
    assert res.reviewed == 0
    assert len(res.unreviewed) == 3, "all documents reported as not reviewed"


def test_per_doc_failure_degrades_to_stub(monkeypatch):
    _no_cache(monkeypatch)
    monkeypatch.setattr(settings, "stuff_fraction", 0.0001)
    # Large enough to decline the fast path so the (failing) note call is made.
    monkeypatch.setattr(extract, "extract", lambda path, mime=None: "some body text " * 10)

    async def boom_llm(system, user, max_tokens=0):
        raise RuntimeError("LLM down")

    monkeypatch.setattr(llm, "complete", boom_llm)

    bank = Bank()
    b = budgets(65_536, "files")
    res = _run(census_mod.run_census(_docs(1), bank, b, time.monotonic() + 600, "m"))
    assert res.reviewed == 1
    note = bank.get("D1").note
    assert note is not None and note.claims, "failure degrades to a stub note, never raises"
    assert "f1.txt" in note.claims[0]
