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

"""Coherence pass guards: new-citation revert, length-collapse revert,
structural mismatch keeps draft, placeholder replaced (or stripped on
failure)."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import llm
from app.research import cohere
from app.research.budgets import budgets
from app.research.templates import EXEC_SUMMARY_PLACEHOLDER

_DRAFT = (
    "## 1. Executive summary\n" + EXEC_SUMMARY_PLACEHOLDER + "\n\n"
    "## 2. Findings\nAlpha is 42 [W1]. Beta follows from alpha in several measured ways "
    "that the sources document at length and in detail [W2].\n\n"
    "## 3. Conclusions\nIt all adds up [W1]."
)


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def test_happy_path_fills_exec_summary(monkeypatch):
    async def fake_llm(system, user, max_tokens=0):
        return _DRAFT.replace(EXEC_SUMMARY_PLACEHOLDER, "The findings show alpha is 42 and it adds up.")

    monkeypatch.setattr(llm, "complete", fake_llm)
    out = _run(cohere.run(_DRAFT, "q", budgets(65_536)))
    assert EXEC_SUMMARY_PLACEHOLDER not in out
    assert "The findings show alpha" in out
    assert "[W1]" in out and "[W2]" in out


def test_new_citation_reverts_that_section(monkeypatch):
    async def sneaky(system, user, max_tokens=0):
        return _DRAFT.replace("It all adds up [W1].", "It all adds up [W1], and also [W9].").replace(
            EXEC_SUMMARY_PLACEHOLDER, "Summary."
        )

    monkeypatch.setattr(llm, "complete", sneaky)
    out = _run(cohere.run(_DRAFT, "q", budgets(65_536)))
    assert "[W9]" not in out, "a section that gained a citation reverts"
    assert "Summary." in out, "untainted sections keep their edits"


def test_length_collapse_reverts(monkeypatch):
    async def chopper(system, user, max_tokens=0):
        return _DRAFT.replace(
            "Alpha is 42 [W1]. Beta follows from alpha in several measured ways "
            "that the sources document at length and in detail [W2].",
            "Alpha [W1].",
        ).replace(EXEC_SUMMARY_PLACEHOLDER, "Summary.")

    monkeypatch.setattr(llm, "complete", chopper)
    out = _run(cohere.run(_DRAFT, "q", budgets(65_536)))
    assert "Beta follows from alpha" in out, "over-aggressive cut reverted"


def test_structure_change_keeps_draft(monkeypatch):
    async def restructurer(system, user, max_tokens=0):
        return "## Brand New Heading\nDifferent text entirely [W1]."

    monkeypatch.setattr(llm, "complete", restructurer)
    out = _run(cohere.run(_DRAFT, "q", budgets(65_536)))
    assert "## 2. Findings" in out, "changed headings ⇒ whole draft kept"
    assert EXEC_SUMMARY_PLACEHOLDER not in out, "placeholder never ships"


def test_llm_failure_keeps_draft_strips_placeholder(monkeypatch):
    async def dead(system, user, max_tokens=0):
        raise RuntimeError("down")

    monkeypatch.setattr(llm, "complete", dead)
    out = _run(cohere.run(_DRAFT, "q", budgets(32_768)))
    assert "## 2. Findings" in out
    assert EXEC_SUMMARY_PLACEHOLDER not in out
