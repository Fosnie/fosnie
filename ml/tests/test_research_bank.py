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

"""Memory bank: stable W# allocation, URL dedup, capped top_sources ordering,
and notes building (mocked LLM) with stub degradation."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import llm, reranker
from app.research import bank as bank_mod
from app.research import notes as notes_mod
from app.research.bank import Bank, DocSource, Note
from app.research.budgets import budgets
from app.web.pipeline import _Source


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _src(url: str, snippet_only: bool = False, chunks: list[str] | None = None) -> _Source:
    return _Source(
        url=url, title=f"T {url}", domain=url.split("/")[2], published_date=None,
        fetched_at="2026-06-10T00:00:00+00:00", snippet_only=snippet_only,
        chunks=chunks if chunks is not None else [f"content of {url} " * 10],
    )


def test_stable_ids_and_url_dedup():
    b = Bank()
    a = b.add_source(_src("https://a.example/1"))
    c = b.add_source(_src("https://b.example/2"))
    again = b.add_source(_src("https://a.example/1"))
    assert (a, c) == ("W1", "W2")
    assert again == "W1", "re-adding the same URL returns the existing ID"
    assert b.sids() == ["W1", "W2"]
    assert b.resolve(["W2", "W9", "W1"]) == [b.get("W2"), b.get("W1")], "unknown IDs dropped"


def test_top_sources_cap_and_snippet_ordering(monkeypatch):
    async def equal_rerank(query, docs):
        return [0.0] * len(docs)  # degraded reranker — ordering falls to the tiebreak

    monkeypatch.setattr(reranker, "rerank", equal_rerank)
    sources = [
        _src("https://snip.example/1", snippet_only=True),
        _src("https://full.example/2"),
        _src("https://full.example/3"),
    ]
    b = _run(bank_mod.from_pool_sources("q", sources, cap=2))
    assert len(b.records) == 2
    assert all(not r.source.snippet_only for r in b.records), "fetched beat snippet-only at equal score"


def test_sources_without_chunks_excluded():
    b = _run(bank_mod.from_pool_sources("q", [_src("https://e.example/1", chunks=[])], cap=10))
    assert b.records == []


def test_build_notes_parses_and_stubs(monkeypatch):
    calls = {"n": 0}

    async def fake_llm(system, user, max_tokens=0):
        calls["n"] += 1
        if "W1" in user:
            return '{"claims": ["Alpha is 42 [fact]"], "quotes": ["the answer is 42"]}'
        return "completely unparseable output"

    monkeypatch.setattr(llm, "complete", fake_llm)
    b = Bank()
    b.add_source(_src("https://a.example/1"))
    b.add_source(_src("https://b.example/2"))
    _run(notes_mod.build_notes("the question", b, budgets(65_536)))
    assert calls["n"] == 2
    assert b.get("W1").note.claims == ["Alpha is 42 [fact]"]
    assert b.get("W1").note.quotes == ["the answer is 42"]
    stub = b.get("W2").note
    assert stub.claims and "T https://b.example/2" in stub.claims[0], "unparseable degrades to a stub note"


def test_build_notes_llm_down_stubs_everything(monkeypatch):
    async def dead_llm(system, user, max_tokens=0):
        raise RuntimeError("LLM unreachable")

    monkeypatch.setattr(llm, "complete", dead_llm)
    b = Bank()
    b.add_source(_src("https://a.example/1"))
    _run(notes_mod.build_notes("q", b, budgets(32_768)))
    assert b.get("W1").note is not None, "never raises — stub note instead"


# --- Phase 2: the D# (document) namespace ------------------------------------


def _doc(doc_id: str, kb_name: str = "Contracts") -> DocSource:
    return DocSource(doc_id=doc_id, kb_id="kb1", kb_name=kb_name, filename=f"{doc_id}.docx")


def test_doc_ids_separate_namespace_and_dedup():
    b = Bank()
    w = b.add_source(_src("https://a.example/1"))
    d1 = b.add_doc_source(_doc("doc-a"))
    d2 = b.add_doc_source(_doc("doc-b"))
    again = b.add_doc_source(_doc("doc-a"))
    assert w == "W1"
    assert (d1, d2) == ("D1", "D2"), "documents number independently of web sources"
    assert again == "D1", "re-adding the same doc_id returns its existing ID"
    assert set(b.sids()) == {"W1", "D1", "D2"}
    assert [r.sid for r in b.web_records()] == ["W1"]
    assert [r.sid for r in b.doc_records()] == ["D1", "D2"]


def test_mixed_resolve_and_meta_line():
    b = Bank()
    b.add_source(_src("https://a.example/1"))
    b.add_doc_source(_doc("doc-a", kb_name="Matter Alpha"))
    got = b.resolve(["D1", "W1", "Dz"])
    assert [r.sid for r in got] == ["D1", "W1"], "order preserved, unknown dropped"
    assert b.get("D1").meta_line() == "[D1] doc-a.docx — Matter Alpha (your documents)"
    assert b.get("W1").meta_line().startswith("[W1] T https://a.example/1 — a.example")


def test_note_full_text_overrides_claims_quotes():
    # The stuff-whole-corpus fast path: the writer reads full text directly.
    n = Note(claims=["ignored"], quotes=["ignored"], full_text="the entire document body")
    assert n.text() == "the entire document body"
    # Without full_text it renders claims + quotes as before.
    n2 = Note(claims=["c1"], quotes=["q1"])
    assert "Claims:" in n2.text() and "c1" in n2.text() and "q1" in n2.text()
