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

"""Deterministic late-anchor guardrail. Before a per-part synthesis
can call a section 'not reproduced', a section whose NUMBER is named in the part but absent
from its slice is recovered — reused from the pool (attribution) or fetched. No network:
`qdrant_store.fetch_by_sections` is monkeypatched."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import qdrant_store
from app import retrieve as retrieve_mod
from app.config import settings


def _slice(title, context="", sections=None, n_blocks=0, has_evidence=False):
    return {"title": title, "context": context, "sections": sections or [],
            "n_blocks": n_blocks, "has_evidence": has_evidence,
            "subq_indices": [], }


def _payload(section, text="operative text"):
    return {"doc_id": "doc1", "chunk_index": 1, "page_number": 3,
            "clause_section_ref": section, "section_nums": [int("".join(c for c in section if c.isdigit()))],
            "chunk_text": f"{section} {text}"}


def _run(parts, citations, doc_meta, fetch_impl, monkeypatch, cap=None):
    """Run the guardrail with a mocked fetch, a fresh expand-stat; returns the stat."""
    calls = []

    async def fake_fetch(kb_ids, section_ids, limit=24):
        calls.append(list(section_ids))
        return fetch_impl(section_ids)

    monkeypatch.setattr(qdrant_store, "fetch_by_sections", fake_fetch)
    if cap is not None:
        monkeypatch.setattr(settings, "late_anchor_cap", cap)

    async def go():
        est = retrieve_mod._ExpandStat(8)
        retrieve_mod._expand_stat.set(est)
        await retrieve_mod._late_anchor_slices(parts, citations, doc_meta, ["kb1"])
        return est

    est = asyncio.new_event_loop().run_until_complete(go())
    return est, calls


def test_named_section_absent_is_fetched(monkeypatch):
    part = _slice("What remedy exists for unfair prejudice under s.994?",
                  context="Sub-question 1: unfair prejudice\nAnswer: See below.")
    citations = [{"clause_section_ref": "996"}]
    est, calls = _run([part], citations, [], lambda ids: [_payload("994")], monkeypatch)
    assert calls == [["994"]], "fetched exactly the named-but-absent section"
    assert "994" in part["sections"] and part["n_blocks"] == 1 and part["has_evidence"]
    assert "[D2] 994" in part["context"], "appended as the next turn-global [D#]"
    assert len(citations) == 2 and est.late_anchor_fetch == 1


def test_pooled_elsewhere_is_attributed_without_fetch(monkeypatch):
    # s.994 is already a pooled [D#] (owned by another part) → reuse it, DO NOT fetch.
    part = _slice("Remedy for unfair prejudice under section 994?")
    doc_meta = [{"block": "[D1] 994 Unfair prejudice.", "subqs": {0}, "section": "994",
                 "section_nums": [994]}]
    citations = [{"clause_section_ref": "994"}]
    est, calls = _run([part], citations, doc_meta, lambda ids: [], monkeypatch)
    assert calls == [], "no fetch — the section was already pooled"
    assert "994" in part["sections"] and "[D1] 994" in part["context"]
    assert len(citations) == 1, "attribution adds no new citation"
    assert est.late_anchor_fetch == 1


def test_genuinely_absent_leaves_honest_refusal(monkeypatch):
    part = _slice("Duty under s.9999 (not in corpus)?", context="Answer: Not found in the library.")
    citations = []
    est, calls = _run([part], citations, [], lambda ids: [], monkeypatch)
    assert calls == [["9999"]]
    assert part["sections"] == [] and part["n_blocks"] == 0 and not citations
    assert est.late_anchor_fetch == 0, "nothing recovered → honest refusal preserved"


def test_cap_bounds_fetches_per_part(monkeypatch):
    part = _slice("Consider ss.10, 20, 30, 40, 50 and 60 of the Act.")  # 6 named, none present
    citations = []
    est, calls = _run([part], citations, [], lambda ids: [_payload(ids[0])], monkeypatch, cap=4)
    assert len(calls) == 4 and est.late_anchor_fetch == 4, "capped at late_anchor_cap"


def test_cap_zero_is_noop(monkeypatch):
    part = _slice("Remedy under s.994?")
    est, calls = _run([part], [], [], lambda ids: [_payload("994")], monkeypatch, cap=0)
    assert calls == [] and est.late_anchor_fetch == 0 and part["sections"] == []


def test_no_parts_is_noop(monkeypatch):
    est, calls = _run([], [], [], lambda ids: [_payload("994")], monkeypatch)
    assert calls == [] and est.late_anchor_fetch == 0


def test_fetch_error_is_fail_soft(monkeypatch):
    # First part's fetch raises; the guardrail must swallow it and still process the second.
    p1 = _slice("Section 111 duty?")
    p2 = _slice("Section 222 duty?")
    citations = []

    def impl(ids):
        if ids == ["111"]:
            raise RuntimeError("qdrant down")
        return [_payload("222")]

    est, calls = _run([p1, p2], citations, [], impl, monkeypatch)
    assert ["111"] in calls and ["222"] in calls
    assert p1["sections"] == [] and "222" in p2["sections"], "part 1 fail-soft, part 2 recovered"
    assert est.late_anchor_fetch == 1
