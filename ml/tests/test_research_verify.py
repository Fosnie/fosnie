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

"""In-pipeline verification + ground-or-cut. Mocks decompose_claims,
locate and verify_claims — the slow LLM + the :8095 sidecar are never hit.
Asserts: cut contradicted sentences; strip markers off cited-not_mentioned;
leave uncited claims; never empty a section; exemptions untouched;
disabled/deadline ⇒ byte-identical (report, None); fail-open on raise."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import decompose, locate
from app import verify as verify_svc
from app.config import settings
from app.research import verify as rv
from app.research.bank import Bank, Note
from app.research.budgets import budgets
from app.web.pipeline import _Source


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _bank() -> Bank:
    b = Bank()
    for i in (1, 2, 3):
        s = _Source(url=f"https://s{i}.ex/a", title=f"S{i}", domain=f"s{i}.ex",
                    published_date=None, fetched_at="2026-06-10T00:00:00+00:00",
                    snippet_only=False, chunks=[f"raw evidence body for source {i} " * 6])
        sid = b.add_source(s)
        b.get(sid).note = Note(claims=[f"claim {i}"], quotes=[f"quote {i}"])
    return b


class _Outline:
    """Minimal stand-in: only `.sections[i].placeholder` is read by _exempt."""
    class _S:
        def __init__(self, heading, placeholder=None):
            self.heading = heading
            self.placeholder = placeholder

    def __init__(self, sections):
        self.sections = sections


def _enable(monkeypatch):
    monkeypatch.setattr(settings, "verify_enabled", True)
    # cfg reads settings unless a rag_ctx override is set — none here.


def _mock_decompose_one_per_sentence(monkeypatch):
    # Each "sentence" decomposes to a single claim equal to the sentence text,
    # so locate() (also mocked) can bind it back exactly.
    async def fake_decompose(text):
        import re as _re
        return [s.strip() for s in _re.split(r"(?<=[.!?])\s+", text.strip()) if s.strip()]
    monkeypatch.setattr(decompose, "decompose_claims", fake_decompose)

    def fake_locate(claim, body, hint_start=0, min_cover=0.5):
        idx = body.find(claim)
        if idx < 0:
            return None
        return {"start": idx, "end": idx + len(claim), "text": claim}
    monkeypatch.setattr(locate, "locate", fake_locate)


def _outline_findings():
    return _Outline([_Outline._S("Findings"), _Outline._S("Analysis")])


def test_disabled_returns_byte_identical(monkeypatch):
    monkeypatch.setattr(settings, "verify_enabled", False)
    report = "## 1. Findings\nThe sky is blue [W1]."
    out, summary = _run(rv.verify_and_prune(report, _outline_findings(), _bank(), budgets(65_536), 1e18))
    assert out == report and summary is None, "disabled ⇒ no prune, byte-identical"


def test_deadline_returns_byte_identical(monkeypatch):
    _enable(monkeypatch)
    report = "## 1. Findings\nThe sky is blue [W1]."
    out, summary = _run(rv.verify_and_prune(report, _outline_findings(), _bank(), budgets(65_536), 0.0))
    assert out == report and summary is None, "past deadline ⇒ byte-identical"


def test_contradicted_sentence_is_cut(monkeypatch):
    _enable(monkeypatch)
    _mock_decompose_one_per_sentence(monkeypatch)

    async def verdicts(pairs, hhem_filter=False):
        # The sentence carrying [W2] is contradicted; the [W1] one supported.
        return [{"verdict": "contradicted" if "[W2]" in p["text"] else "supported", "score": 0.9} for p in pairs]
    monkeypatch.setattr(verify_svc, "verify_claims", verdicts)

    report = "## 1. Findings\nFirst true thing [W1]. Second false thing [W2]."
    out, summary = _run(rv.verify_and_prune(report, _outline_findings(), _bank(), budgets(65_536), 1e18))
    assert "Second false thing" not in out, "contradicted sentence cut"
    assert "First true thing [W1]" in out, "supported sentence kept"
    assert summary["contradicted"] == 1 and summary["supported"] == 1


def test_not_mentioned_strips_marker_keeps_text(monkeypatch):
    _enable(monkeypatch)
    _mock_decompose_one_per_sentence(monkeypatch)

    async def verdicts(pairs, hhem_filter=False):
        return [{"verdict": "not_mentioned", "score": 0.3} for _ in pairs]
    monkeypatch.setattr(verify_svc, "verify_claims", verdicts)

    report = "## 1. Findings\nA plausible but unsupported claim [W1]."
    out, summary = _run(rv.verify_and_prune(report, _outline_findings(), _bank(), budgets(65_536), 1e18))
    assert "A plausible but unsupported claim" in out, "claim survives uncited"
    assert "[W1]" not in out, "unsupported marker stripped"
    assert summary["not_mentioned"] >= 1
    assert summary["flagged"] and "A plausible but unsupported claim" in summary["flagged"][0]["text"]


def test_never_empties_a_section(monkeypatch):
    _enable(monkeypatch)
    _mock_decompose_one_per_sentence(monkeypatch)

    async def verdicts(pairs, hhem_filter=False):
        return [{"verdict": "contradicted", "score": 0.9} for _ in pairs]  # cut everything
    monkeypatch.setattr(verify_svc, "verify_claims", verdicts)

    report = "## 1. Findings\nOnly sentence here [W1]."
    out, _ = _run(rv.verify_and_prune(report, _outline_findings(), _bank(), budgets(65_536), 1e18))
    assert "Only sentence here" in out, "a section is reverted rather than emptied"


def test_exempt_sections_untouched(monkeypatch):
    _enable(monkeypatch)
    _mock_decompose_one_per_sentence(monkeypatch)

    # Contradict only the sentence carrying [W3] (so section 2 keeps a survivor
    # and isn't reverted by the never-empty guard — proving it WAS verified).
    async def verdicts(pairs, hhem_filter=False):
        return [{"verdict": "contradicted" if "[W3]" in p["text"] else "supported", "score": 0.9} for p in pairs]
    monkeypatch.setattr(verify_svc, "verify_claims", verdicts)

    # Section 1 is a placeholder (exec summary) → exempt; only section 2 verified.
    outline = _Outline([_Outline._S("Executive summary", placeholder="x"), _Outline._S("Findings")])
    report = "## 1. Executive summary\nSummary sentence [W3].\n\n## 2. Findings\nKept sentence [W2]. Cut sentence [W3]."
    out, _ = _run(rv.verify_and_prune(report, outline, _bank(), budgets(65_536), 1e18))
    assert "Summary sentence [W3]" in out, "exempt exec-summary untouched even though it carries [W3]"
    assert "Kept sentence [W2]" in out, "supported sentence kept"
    assert "Cut sentence" not in out, "the contradicted sentence in the non-exempt section was cut"


def test_verifier_outage_all_not_mentioned_zero_is_byte_identical(monkeypatch):
    # verify_claims fails open to all-not_mentioned/score-0 when the sidecar is
    # DOWN (even though verify_enabled is True). The outage guard must NOT strip.
    _enable(monkeypatch)
    _mock_decompose_one_per_sentence(monkeypatch)

    async def fail_open(pairs, hhem_filter=False):
        return [{"verdict": "not_mentioned", "score": 0.0} for _ in pairs]
    monkeypatch.setattr(verify_svc, "verify_claims", fail_open)

    report = "## 1. Findings\nA cited claim [W1]. Another [W2]."
    out, summary = _run(rv.verify_and_prune(report, _outline_findings(), _bank(), budgets(65_536), 1e18))
    assert out == report and summary is None, "no-signal verdicts ⇒ treated as outage, byte-identical"


def test_fail_open_on_verifier_raise(monkeypatch):
    _enable(monkeypatch)
    _mock_decompose_one_per_sentence(monkeypatch)

    async def boom(pairs, hhem_filter=False):
        raise RuntimeError("sidecar down")
    monkeypatch.setattr(verify_svc, "verify_claims", boom)

    report = "## 1. Findings\nA sentence [W1]."
    out, summary = _run(rv.verify_and_prune(report, _outline_findings(), _bank(), budgets(65_536), 1e18))
    assert out == report and summary is None, "a verifier raise ⇒ deliver unverified, byte-identical"
