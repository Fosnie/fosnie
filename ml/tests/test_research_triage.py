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

"""Ambiguity triage: ≤3 questions × ≤4 options enforced, out-of-range scope
indices dropped, and any failure / unparseable output degrades to no questions
(never blocks the run)."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import llm
from app.research import triage as triage_mod


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


_SCOPE = [
    {"index": 0, "name": "Alpha", "kind": "project", "doc_count": 10},
    {"index": 1, "name": "Beta", "kind": "library", "doc_count": 5},
]


def test_ambiguous_questions_capped_and_indices_validated(monkeypatch):
    async def fake_llm(system, user, max_tokens=0):
        # 4 questions (one over the cap) and an option with 5 options + a bad index.
        return (
            '{"ambiguous": true, "questions": ['
            '{"id":"q1","prompt":"Which project?","options":['
            '{"label":"Alpha","scope_indices":[0]},'
            '{"label":"Beta","scope_indices":[1]},'
            '{"label":"All","scope_indices":[]},'
            '{"label":"Bogus","scope_indices":[9]},'
            '{"label":"Fifth","scope_indices":[0]}]},'
            '{"id":"q2","prompt":"Timeframe?","options":[{"label":"Last year","scope_indices":[]}]},'
            '{"id":"q3","prompt":"x","options":[{"label":"y","scope_indices":[0]}]},'
            '{"id":"q4","prompt":"too many","options":[{"label":"z","scope_indices":[]}]}]}'
        )

    monkeypatch.setattr(llm, "complete", fake_llm)
    out = _run(triage_mod.triage("research alpha", "files", _SCOPE))
    assert out["ambiguous"] is True
    assert len(out["questions"]) == triage_mod.MAX_QUESTIONS, "questions capped at 3"
    q1 = out["questions"][0]
    assert len(q1["options"]) == triage_mod.MAX_OPTIONS, "options capped at 4"
    # The bad index [9] was dropped from its option's scope_indices.
    bogus = next(o for o in q1["options"] if o["label"] == "Bogus")
    assert bogus["scope_indices"] == [], "out-of-range scope index dropped"


def test_not_ambiguous_returns_no_questions(monkeypatch):
    async def fake_llm(system, user, max_tokens=0):
        return '{"ambiguous": false, "questions": []}'

    monkeypatch.setattr(llm, "complete", fake_llm)
    out = _run(triage_mod.triage("q", "files", _SCOPE))
    assert out == {"ambiguous": False, "questions": []}


def test_unparseable_and_failure_degrade_to_no_questions(monkeypatch):
    async def junk(system, user, max_tokens=0):
        return "not json at all"

    monkeypatch.setattr(llm, "complete", junk)
    assert _run(triage_mod.triage("q", "files", _SCOPE))["questions"] == []

    async def dead(system, user, max_tokens=0):
        raise RuntimeError("LLM down")

    monkeypatch.setattr(llm, "complete", dead)
    out = _run(triage_mod.triage("q", "files", _SCOPE))
    assert out == {"ambiguous": False, "questions": []}, "never blocks the run"


def test_empty_options_question_dropped(monkeypatch):
    async def fake_llm(system, user, max_tokens=0):
        return '{"ambiguous": true, "questions": [{"id":"q1","prompt":"?","options":[]}]}'

    monkeypatch.setattr(llm, "complete", fake_llm)
    out = _run(triage_mod.triage("q", "files", _SCOPE))
    assert out["questions"] == [], "a question with no usable options is dropped"
