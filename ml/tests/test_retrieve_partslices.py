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

"""Per-part synthesis slices: the sub-question→part map, and the
per-part context slices over the turn-global [D#] pool (own sub-answers + own blocks only,
no contamination). Pure functions, no network."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import retrieve as retrieve_mod

FIVE_PART = (
    "1. Can a director be liable for wrongful trading under section 214?\n"
    "2. What statutory duties does a company secretary owe?\n"
    "3. How is a special resolution validly passed by the members?\n"
    "4. What remedy exists for unfair prejudice under section 994?\n"
    "5. When must a company deliver its annual confirmation statement?\n"
)


def test_part_map_aligns_tagged_and_injected():
    # LLM tagged parts 1-2; guardrail injects 3,4,5. part_map aligns to the returned items:
    # kept LLM items first (their own tags), then the injected parts in order.
    items = [
        {"subq": "director liability wrongful trading section 214", "queries": ["q"], "scope": "", "part": 1},
        {"subq": "statutory duties owed by a company secretary", "queries": ["q"], "scope": "", "part": 2},
    ]
    combined, meta = retrieve_mod._ensure_coverage(FIVE_PART, items)
    pm = meta["part_map"]
    assert len(pm) == len(combined)
    assert pm[0] == 0 and pm[1] == 1, "LLM items map to their tagged part"
    # The three injected parts carry their own part index, in order.
    assert pm[2:] == [2, 3, 4]


def test_non_numbered_prompt_part_map_all_none():
    items = [{"subq": "a plain question about directors", "queries": ["q"], "scope": "", "part": None}]
    combined, meta = retrieve_mod._ensure_coverage("A single prose question?", items)
    assert meta["part_map"] == [None] * len(combined)


def _sa(subq, status="ok", answer="ans", scope=""):
    return {"subq": subq, "scope": scope, "status": status, "answer": answer,
            "cited": [], "ranked": [], "best_rerank": 0.5, "retried": False}


def test_part_slices_partition_docs_and_no_contamination():
    prompt = "1. Wrongful trading under s214?\n2. Unfair prejudice under s994?\n"
    sub_results = [_sa("director liability s214", answer="A director may be liable [1]."),
                   _sa("unfair prejudice s994", answer="A member may petition [1].")]
    part_map = [0, 1]
    # [D1] contributed by sub-Q0 (part 0), [D2] by sub-Q1 (part 1).
    doc_meta = [
        {"block": "[D1] 214 Wrongful trading operative text.", "subqs": {0}},
        {"block": "[D2] 994 Unfair prejudice operative text.", "subqs": {1}},
    ]
    slices = retrieve_mod._build_part_slices(prompt, sub_results, part_map, doc_meta)
    assert len(slices) == 2
    assert slices[0]["title"].startswith("Wrongful trading")
    # Part 0 sees only its own [D1] + its own sub-answer; NOT part 1's s994 answer/doc.
    assert "[D1]" in slices[0]["context"] and "[D2]" not in slices[0]["context"]
    assert "unfair prejudice" not in slices[0]["context"].lower()
    assert "petition" not in slices[0]["context"].lower(), "no cross-part sub-answer bleed"
    assert slices[0]["has_evidence"] is True
    # [D#] numbering stays turn-global (part 1 references [D2], not a renumbered [D1]).
    assert "[D2]" in slices[1]["context"] and "[D1]" not in slices[1]["context"]


def test_part_slice_has_evidence_false_when_failed_and_no_docs():
    prompt = "1. Question A about s10?\n2. Question B about s20?\n"
    sub_results = [_sa("A s10", status="failed", answer=""), _sa("B s20")]
    part_map = [0, 1]
    doc_meta = [{"block": "[D1] s20 text", "subqs": {1}}]  # only part 1 has a doc
    slices = retrieve_mod._build_part_slices(prompt, sub_results, part_map, doc_meta)
    by_title = {s["title"][:10]: s for s in slices}
    assert by_title["Question A"]["has_evidence"] is False
    assert by_title["Question B"]["has_evidence"] is True


def test_non_numbered_prompt_yields_no_slices():
    assert retrieve_mod._build_part_slices("plain question", [_sa("x")], [None], []) == []
