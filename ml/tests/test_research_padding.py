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

"""Padding detection. Mocks embeddings.embed — no network. Asserts
near-duplicate sections flag, the citation-density floor, and fail-open on
embed failure / past deadline."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import embeddings
from app.research import padding
from app.research.bank import Bank
from app.research.budgets import budgets


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


class _Outline:
    class _S:
        def __init__(self, heading, placeholder=None):
            self.heading = heading
            self.placeholder = placeholder

    def __init__(self, sections):
        self.sections = sections


_OUTLINE = _Outline([_Outline._S("Findings"), _Outline._S("Analysis")])
_B = budgets(65_536)


def test_near_duplicate_sections_flagged(monkeypatch):
    async def fake_embed(texts):
        # Two identical unit vectors → cosine 1.0 > threshold.
        return [[1.0, 0.0] for _ in texts]
    monkeypatch.setattr(embeddings, "embed", fake_embed)
    report = ("## 1. Findings\n" + "alpha [W1]. " * 40 + "\n\n## 2. Analysis\n" + "beta [W2]. " * 40)
    out = _run(padding.detect_padding(report, _OUTLINE, Bank(), _B, 1e18))
    assert any(v.kind == "padding_similarity" for v in out), "duplicate sections flagged"


def test_distinct_sections_not_flagged(monkeypatch):
    async def fake_embed(texts):
        return [[1.0, 0.0], [0.0, 1.0]][: len(texts)]  # orthogonal → cosine 0
    monkeypatch.setattr(embeddings, "embed", fake_embed)
    report = ("## 1. Findings\n" + "alpha [W1]. " * 40 + "\n\n## 2. Analysis\n" + "beta [W2]. " * 40)
    out = _run(padding.detect_padding(report, _OUTLINE, Bank(), _B, 1e18))
    assert not any(v.kind == "padding_similarity" for v in out), "orthogonal sections not flagged"


def test_citation_density_floor(monkeypatch):
    async def fake_embed(texts):
        return [[0.0, 1.0] for _ in texts]
    monkeypatch.setattr(embeddings, "embed", fake_embed)
    # A long section with a single citation → below the 1/250 floor.
    report = "## 1. Findings\n" + "word " * 400 + "[W1]."
    out = _run(padding.detect_padding(report, _OUTLINE, Bank(), _B, 1e18))
    assert any(v.kind == "citation_density" for v in out), "thin section flagged"


def test_embed_failure_fails_open(monkeypatch):
    async def boom(texts):
        raise RuntimeError("embed down")
    monkeypatch.setattr(embeddings, "embed", boom)
    # Two short sections (no density violation) + embed raises → no similarity, [] overall.
    report = "## 1. Findings\nshort [W1].\n\n## 2. Analysis\nshort [W2]."
    out = _run(padding.detect_padding(report, _OUTLINE, Bank(), _B, 1e18))
    assert out == [], "embed failure ⇒ no similarity signal, fail-open"


def test_past_deadline_returns_empty(monkeypatch):
    async def fake_embed(texts):
        raise AssertionError("must not embed past the deadline")
    monkeypatch.setattr(embeddings, "embed", fake_embed)
    report = "## 1. Findings\nbody [W1]."
    assert _run(padding.detect_padding(report, _OUTLINE, Bank(), _B, 0.0)) == []
