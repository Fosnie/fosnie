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

"""Deterministic structure checks + unresolved-marker stripping."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app.research.bank import Bank
from app.research.budgets import budgets
from app.research.checks import run_checks, strip_unresolved
from app.research.outline import Outline, OutlineSection
from app.web.pipeline import _Source

_B = budgets(65_536)


def _bank() -> Bank:
    b = Bank()
    for i in (1, 2):
        b.add_source(_Source(
            url=f"https://s{i}.example/x", title=f"S{i}", domain=f"s{i}.example",
            published_date=None, fetched_at="t", snippet_only=False, chunks=["c"],
        ))
    return b


_OUTLINE = Outline(sections=[
    OutlineSection("Findings", "f", ["W1"]),
    OutlineSection("Conclusions", "c", ["W2"]),
])

_GOOD_BODY = ("word " * 450).strip()


def test_clean_report_passes():
    report = (
        f"## 1. Findings\n{_GOOD_BODY} [W1]\n\n"
        f"## 2. Conclusions\n{_GOOD_BODY} [W2]"
    )
    assert run_checks(report, _OUTLINE, _bank(), _B) == []


def test_missing_heading_and_order():
    report = f"## 2. Conclusions\n{_GOOD_BODY} [W2]"
    kinds = {v.kind for v in run_checks(report, _OUTLINE, _bank(), _B)}
    assert "missing_heading" in kinds


def test_word_band_violation():
    report = (
        f"## 1. Findings\nTiny body. [W1]\n\n"
        f"## 2. Conclusions\n{_GOOD_BODY} [W2]"
    )
    vs = run_checks(report, _OUTLINE, _bank(), _B)
    assert any(v.kind == "word_band" and v.section == "1. Findings" for v in vs)


def test_unresolved_and_density():
    report = (
        f"## 1. Findings\n{_GOOD_BODY} [W7]\n\n"
        f"## 2. Conclusions\n{_GOOD_BODY}"
    )
    vs = run_checks(report, _OUTLINE, _bank(), _B)
    kinds = {v.kind for v in vs}
    assert "unresolved_id" in kinds
    assert "no_citations" in kinds


def test_strip_unresolved():
    report = "Alpha [W1] beta [W9] gamma [W2]."
    cleaned, stripped = strip_unresolved(report, _bank())
    assert stripped == ["W9"]
    assert "[W9]" not in cleaned
    assert "[W1]" in cleaned and "[W2]" in cleaned


def test_placeholder_section_exempt():
    outline = Outline(sections=[
        OutlineSection("Executive summary", "s", [], placeholder="[[X]]"),
        OutlineSection("Findings", "f", ["W1"]),
    ])
    report = (
        "## 1. Executive summary\nShort summary, no citations.\n\n"
        f"## 2. Findings\n{_GOOD_BODY} [W1]"
    )
    assert run_checks(report, outline, _bank(), _B) == []
