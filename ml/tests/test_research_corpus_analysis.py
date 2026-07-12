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

"""Consensus/contradictions/gaps: JSON → deterministic markdown body, [D#]
markers validated against the bank (unknown stripped), and omit-on-failure."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import llm
from app.research import corpus_analysis as ca
from app.research.bank import Bank, DocSource, Note


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _bank() -> Bank:
    b = Bank()
    for i in (1, 2):
        sid = b.add_doc_source(DocSource(doc_id=f"d{i}", kb_id="kb", kb_name="KB", filename=f"f{i}.docx"))
        b.get(sid).note = Note(claims=[f"claim {i}"], quotes=[])
    return b


def test_renders_sections_and_validates_sids(monkeypatch):
    async def fake_llm(system, user, max_tokens=0):
        return (
            '{"consensus": [{"point": "agree on X", "sids": ["D1"]}],'
            ' "contradictions": [{"point": "clash on Y", "sids": ["D1","D9"]}],'
            ' "gaps": ["nothing covers Z"]}'
        )

    monkeypatch.setattr(llm, "complete", fake_llm)
    body = _run(ca.analyse(_bank()))
    assert body is not None
    assert "Where the documents agree" in body and "[D1]" in body
    assert "Where they disagree" in body
    assert "[D9]" not in body, "unknown sid stripped"
    assert "nothing covers Z" in body


def test_empty_corpus_returns_none():
    assert _run(ca.analyse(Bank())) is None, "no documents → no analysis section"


def test_failure_omits_section(monkeypatch):
    async def dead(system, user, max_tokens=0):
        raise RuntimeError("LLM down")

    monkeypatch.setattr(llm, "complete", dead)
    assert _run(ca.analyse(_bank())) is None, "failure omits the section, never raises"


def test_empty_json_returns_none(monkeypatch):
    async def empty(system, user, max_tokens=0):
        return '{"consensus": [], "contradictions": [], "gaps": []}'

    monkeypatch.setattr(llm, "complete", empty)
    assert _run(ca.analyse(_bank())) is None, "nothing to say → no section"
