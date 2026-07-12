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

"""Outline building: JSON parse + clamp, unknown-ID drop, junk → skeleton
fallback with rerank-distributed notes, constrained headings preserved."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import llm, reranker
from app.research import outline as outline_mod
from app.research import templates
from app.research.bank import Bank, Note
from app.research.budgets import budgets
from app.web.pipeline import _Source


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _bank(n: int = 3) -> Bank:
    b = Bank()
    for i in range(1, n + 1):
        sid = b.add_source(_Source(
            url=f"https://s{i}.example/x", title=f"Source {i}", domain=f"s{i}.example",
            published_date=None, fetched_at="2026-06-10T00:00:00+00:00",
            snippet_only=False, chunks=[f"chunk text {i}"],
        ))
        b.get(sid).note = Note(claims=[f"claim {i}"], quotes=[])
    return b


def _equal_rerank(monkeypatch):
    async def fr(query, docs):
        return [0.0] * len(docs)

    monkeypatch.setattr(reranker, "rerank", fr)


def test_parse_clamp_and_unknown_id_drop(monkeypatch):
    _equal_rerank(monkeypatch)
    raw = (
        '[{"heading": "Alpha", "brief": "a", "note_ids": ["W1", "W99"]},'
        '{"heading": "Beta", "brief": "b", "note_ids": ["W2"]},'
        '{"heading": "Gamma", "brief": "c", "note_ids": ["W3"]},'
        '{"heading": "Delta", "brief": "d", "note_ids": []},'
        '{"heading": "Epsilon", "brief": "e", "note_ids": []}]'
    )

    async def fake_llm(system, user, max_tokens=0):
        return raw

    monkeypatch.setattr(llm, "complete", fake_llm)
    o = _run(outline_mod.build("q", templates.get("freeform"), _bank(), budgets(32_768)))
    assert [s.heading for s in o.sections][:3] == ["Alpha", "Beta", "Gamma"]
    assert "W99" not in o.sections[0].note_ids, "unknown note IDs dropped"
    assert "W1" in o.sections[0].note_ids


def test_junk_output_falls_back_to_skeleton(monkeypatch):
    _equal_rerank(monkeypatch)

    async def junk(system, user, max_tokens=0):
        return "I cannot make an outline, sorry."

    monkeypatch.setattr(llm, "complete", junk)
    t = templates.get("exploration")
    o = _run(outline_mod.build("q", t, _bank(), budgets(32_768)))
    assert [s.heading for s in o.sections] == [s.heading for s in t.skeleton]
    all_bound = {sid for s in o.sections for sid in s.note_ids}
    assert all_bound == {"W1", "W2", "W3"}, "notes rerank-distributed so evidence is reachable"


def test_constrained_template_preserves_headings(monkeypatch):
    _equal_rerank(monkeypatch)

    async def renames_everything(system, user, max_tokens=0):
        return '[{"heading": "Totally Different", "brief": "x", "note_ids": ["W1"]}]'

    monkeypatch.setattr(llm, "complete", renames_everything)
    t = templates.get("formal")
    o = _run(outline_mod.build("q", t, _bank(), budgets(65_536)))
    headings = [s.heading for s in o.sections]
    for spec in t.skeleton:
        assert spec.heading in headings, f"skeleton heading '{spec.heading}' must survive"
    exec_s = next(s for s in o.sections if s.heading == "Executive summary")
    assert exec_s.placeholder is not None, "placeholder flag carried through"


def test_no_evidence_section_left_empty(monkeypatch):
    _equal_rerank(monkeypatch)

    async def sparse(system, user, max_tokens=0):
        return (
            '[{"heading": "One", "brief": "a", "note_ids": []},'
            '{"heading": "Two", "brief": "b", "note_ids": []},'
            '{"heading": "Three", "brief": "c", "note_ids": []},'
            '{"heading": "Four", "brief": "d", "note_ids": []}]'
        )

    monkeypatch.setattr(llm, "complete", sparse)
    o = _run(outline_mod.build("q", templates.get("freeform"), _bank(3), budgets(32_768)))
    bound = {sid for s in o.sections for sid in s.note_ids}
    assert bound == {"W1", "W2", "W3"}, "every source bound somewhere"


def test_empty_section_borrows_when_all_notes_already_bound(monkeypatch):
    # The LLM crams every note into one section, leaving the others empty with NO
    # unbound notes left over → only the rebalance pass can fill them. No evidence
    # section may be left empty when the bank has notes.
    _equal_rerank(monkeypatch)

    async def lopsided(system, user, max_tokens=0):
        return (
            '[{"heading": "One", "brief": "a", "note_ids": ["W1", "W2", "W3"]},'
            '{"heading": "Two", "brief": "b", "note_ids": []},'
            '{"heading": "Three", "brief": "c", "note_ids": []}]'
        )

    monkeypatch.setattr(llm, "complete", lopsided)
    o = _run(outline_mod.build("q", templates.get("freeform"), _bank(3), budgets(32_768)))
    for s in o.sections:
        assert s.note_ids, f"section '{s.heading}' must not be evidence-empty"
