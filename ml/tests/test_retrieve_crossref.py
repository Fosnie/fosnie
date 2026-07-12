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

"""Deterministic retrieval expansion: required-section-set
extraction (single/range/list/Schedule), anchor-completeness look-ups before the
mini-answer, and cross-reference + ±N neighbour expansion. No network."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import chunker, qdrant_store
from app import retrieve as retrieve_mod


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


# --- §1 anchor extraction ----------------------------------------------------

def test_required_anchors_single_and_suffix():
    assert retrieve_mod._required_anchors("liability under s443A and section 994") == {"443A", "994"}


def test_required_anchors_range():
    assert retrieve_mod._required_anchors("see ss. 570-577") == {str(n) for n in range(570, 578)}


def test_required_anchors_list_and_schedule():
    assert retrieve_mod._required_anchors("sections 570 and 571; Schedule 3") == {"570", "571", "Schedule 3"}


def test_required_anchors_drops_years():
    # A bare year in "the 2006 Act" must not become a required section.
    assert retrieve_mod._required_anchors("the Companies Act 2006, section 172") == {"172"}


def test_covered_anchors_scans_ref_and_text():
    payloads = [
        {"clause_section_ref": "570", "chunk_text": "570 Authority to allot. See also section 571 below."},
    ]
    covered = retrieve_mod._covered_anchors(payloads)
    assert "570" in covered and "571" in covered


# --- §2 chunker metadata -----------------------------------------------------

def test_section_refs_and_num():
    assert chunker._section_refs("Applies subject to section 444 and s. 445A.") == ["444", "445A"]
    assert chunker._section_num("443A") == 443
    assert chunker._section_num("2.3") == 2
    assert chunker._section_num(None) is None


def test_chunk_meta_attached():
    meta = chunker._chunk_meta("443A Duty to file. See section 444.")
    assert meta["clause_section_ref"] == "443A"
    assert meta["section_num"] == 443
    assert "444" in meta["refs_out"]


# --- §1 anchor-completeness before the mini-answer ---------------------------

def test_anchor_complete_pulls_missing_section(monkeypatch):
    async def fake_fetch(kb_ids, section_ids, limit=24):
        assert "994" in section_ids  # the missing required anchor is looked up
        return [{"clause_section_ref": "994", "chunk_text": "994 Petition by company member.", "doc_id": "d", "chunk_index": 9}]

    monkeypatch.setattr(qdrant_store, "fetch_by_sections", fake_fetch)
    est = retrieve_mod._ExpandStat(8)
    retrieve_mod._expand_stat.set(est)

    payloads = [{"clause_section_ref": "172", "chunk_text": "172 Duty to promote success.", "doc_id": "d", "chunk_index": 1}]
    out = _run(retrieve_mod._anchor_complete("what remedy under section 994?", "s172 and s994", payloads, ["kb1"]))
    assert any(p["chunk_text"].startswith("994") for p in out), "the missing s994 chunk was pulled in"
    assert est.anchors_recovered == 1
    assert est.anchor_budget == 7, "one unit of the shared anchor budget was spent"


def test_anchor_complete_budget_exhausted(monkeypatch):
    called = {"n": 0}

    async def fake_fetch(kb_ids, section_ids, limit=24):
        called["n"] += 1
        return []

    monkeypatch.setattr(qdrant_store, "fetch_by_sections", fake_fetch)
    est = retrieve_mod._ExpandStat(0)  # no budget
    retrieve_mod._expand_stat.set(est)
    out = _run(retrieve_mod._anchor_complete("section 994", "s994", [], ["kb1"]))
    assert out == [] and called["n"] == 0, "no look-up when the budget is spent"


# --- §2 crossref + neighbour expansion --------------------------------------

def test_expand_sections_follows_refs_and_neighbours(monkeypatch):
    seen = {}

    async def fake_by_sections(kb_ids, section_ids, limit=24):
        seen["targets"] = set(section_ids)
        return [{"clause_section_ref": s, "chunk_text": f"{s} operative text", "doc_id": "d", "chunk_index": 0} for s in section_ids]

    async def fake_neighbours(kb_ids, section_nums, span, limit=24):
        seen["nums"] = set(section_nums)
        seen["span"] = span
        return [{"clause_section_ref": "444", "chunk_text": "444 neighbour", "doc_id": "d", "chunk_index": 0}]

    monkeypatch.setattr(qdrant_store, "fetch_by_sections", fake_by_sections)
    monkeypatch.setattr(qdrant_store, "fetch_neighbours", fake_neighbours)
    est = retrieve_mod._ExpandStat(8)
    retrieve_mod._expand_stat.set(est)

    # A top chunk that IS s443A and cross-refers s570; span pulls the numeric neighbours.
    ranked = [{"clause_section_ref": "443A", "refs_out": ["570"], "chunk_text": "443A ... see section 570", "doc_id": "d", "chunk_index": 0}]
    out = _run(retrieve_mod._expand_sections("filing obligations under s443A", ranked, ["kb1"]))
    assert "570" in seen["targets"], "the cross-referenced section is followed (1 hop)"
    assert 443 in seen["nums"] and seen["span"] == 1, "neighbours of the found section fetched"
    assert est.crossref_followed >= 1 and est.neighbors_fetched >= 1
    assert out, "expansion returns payloads for the pool"
