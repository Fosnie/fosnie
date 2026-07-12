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

"""decompose coverage-guardrail. A numbered
N-part prompt must never lose a whole question. Coverage is read from the model's own
1-based `part` tag (NOT token overlap); any UNTAGGED part is injected verbatim. No
network — pure functions and a mocked decompose LLM."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import llm
from app import retrieve as retrieve_mod

FIVE_PART = (
    "1. Can a director be held liable for wrongful trading under section 214?\n"
    "2. What are the statutory duties owed by a company secretary?\n"
    "3. How is a special resolution validly passed by the members?\n"
    "4. What remedies exist for a shareholder facing unfair prejudice under section 994?\n"
    "5. When must a company deliver its annual confirmation statement to Companies House?\n"
)


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def test_numbered_parts_detects_all_five():
    parts = retrieve_mod._numbered_parts(FIVE_PART)
    assert len(parts) == 5
    assert parts[0].startswith("Can a director")
    assert parts[4].startswith("When must a company")


def test_single_marker_is_not_multipart():
    # One numbered item isn't a multi-part prompt — no guardrail work.
    assert retrieve_mod._numbered_parts("1. Only one question here about directors?") == []
    assert retrieve_mod._numbered_parts("A plain prose question with no enumeration.") == []


def test_date_leading_line_is_not_a_part_marker():
    # A commencement-date line must not be mistaken for an enumerator.
    txt = "1.10.2007 the Act came into force.\n2.11.2008 amendments applied.\n"
    assert retrieve_mod._numbered_parts(txt) == []


# --- inline single-paragraph enumeration -------------------
# A real-world prompt is ONE line with inline "1. … 2. …"; line-start detection missed it
# (parts=0/0) so per_part + the coverage guarantee never engaged. The inline fallback must
# recover exactly 5 parts despite embedded "Companies Act 2006", "s.549"-style refs, ECCTA.
INLINE_FIVE_PART = (
    "1. Under the Companies Act 2006, what documents must be delivered to the registrar to "
    "incorporate a private company limited by shares after the Economic Crime and Corporate "
    "Transparency Act amendments? "
    "2. A private company's articles include an entrenched provision; if it later passes only "
    "a special resolution to amend that entrenched article, is the amendment effective? "
    "3. A director causes the company to enter a transaction in which he has a personal "
    "interest under sections s.172 and s.175 — how does ratification affect a derivative claim? "
    "4. A private company with only one class of shares wants to allot equity securities for "
    "cash under s.549 to s.571 without first offering them to existing shareholders — which "
    "pre-emption provisions apply? "
    "5. For a company that may qualify as a micro-entity, how do the filing obligations differ "
    "for annual accounts and directors' reports?"
)


def test_inline_five_part_prompt_detected():
    parts = retrieve_mod._numbered_parts(INLINE_FIVE_PART)
    assert len(parts) == 5
    assert parts[0].startswith("Under the Companies Act 2006")
    assert parts[3].startswith("A private company with only one class")
    assert parts[4].startswith("For a company that may qualify")
    # Embedded numerals inside a part (2006, s.549, s.571) must NOT split it.
    assert "s.549 to s.571" in parts[3]


def test_inline_two_part_minimal():
    assert retrieve_mod._numbered_parts("1. Alpha question here. 2. Beta question there.") == \
        ["Alpha question here.", "Beta question there."]


def test_inline_prose_numerals_without_series_are_not_parts():
    # Years/sentence numerals that don't form a 1,2,3 run must not trigger the fallback.
    txt = "In 2006. The company was formed. In 2008. It changed its name later."
    assert retrieve_mod._numbered_parts(txt) == []


def test_line_start_wins_over_inline_when_both_present():
    # A newline-enumerated prompt with inline numerals in the bodies keeps the line-start
    # partition (2 parts), not an inline re-split.
    txt = "1. First part mentions step 2. of a process.\n2. Second part is separate.\n"
    parts = retrieve_mod._numbered_parts(txt)
    assert len(parts) == 2
    assert parts[0].startswith("First part")
    assert parts[1].startswith("Second part")


def test_guardrail_injects_untagged_parts():
    # The LLM tagged only parts 1-3; the guardrail must inject 4 and 5 verbatim on the
    # ABSENCE of a tag — regardless of any token overlap the old heuristic would have found.
    items = [
        {"subq": "director liability wrongful trading section 214", "queries": ["q"], "scope": "", "part": 1},
        {"subq": "statutory duties owed by a company secretary", "queries": ["q"], "scope": "", "part": 2},
        {"subq": "how is a special resolution validly passed by members", "queries": ["q"], "scope": "", "part": 3},
    ]
    combined, meta = retrieve_mod._ensure_coverage(FIVE_PART, items)
    assert meta["parts_detected"] == 5
    assert meta["subqs_injected"] == 2
    assert meta["parts_covered"] == 5, "all parts covered once the guardrail injects the gaps"
    # part_map covers every part index (invariant: no empty slice possible).
    assert set(meta["part_map"]) == {0, 1, 2, 3, 4}
    subqs = [c["subq"] for c in combined]
    assert any("unfair prejudice" in s for s in subqs), "part 4 recovered verbatim"
    assert any("confirmation statement" in s for s in subqs), "part 5 recovered verbatim"


def test_single_untagged_part_is_injected_and_mapped():
    # §5.1: the model forgets to tag part 4 (index 3) — it must be injected and its slice
    # index must appear in part_map so _build_part_slices can never skip it.
    parts = retrieve_mod._numbered_parts(FIVE_PART)
    items = [
        {"subq": p, "queries": [p], "scope": "", "part": n}
        for n, p in enumerate(parts, 1)
        if n != 4  # part 4 left untagged
    ]
    combined, meta = retrieve_mod._ensure_coverage(FIVE_PART, items)
    assert meta["subqs_injected"] >= 1
    assert 3 in meta["part_map"], "part 4 (index 3) is mapped via the injected sub-question"
    injected = combined[meta["part_map"].index(3)]
    assert "unfair prejudice" in injected["subq"] and injected["part"] == 4


def test_guardrail_no_injection_when_all_tagged():
    items = [
        {"subq": p, "queries": [p], "scope": "", "part": n}
        for n, p in enumerate(retrieve_mod._numbered_parts(FIVE_PART), 1)
    ]
    combined, meta = retrieve_mod._ensure_coverage(FIVE_PART, items)
    assert meta["subqs_injected"] == 0
    assert meta["parts_covered"] == 5
    assert len(combined) == 5
    assert meta["part_map"] == [0, 1, 2, 3, 4]


def test_budget_trims_only_over_covered_parts(monkeypatch):
    # §3: the model dumps 20 sub-questions all on part 1 and tags nothing else. Injects for
    # parts 2-5 are guaranteed; the LLM overflow is trimmed ONLY from part 1 (which has >1),
    # never a part's last representative — and every part is still mapped.
    from app.config import settings

    monkeypatch.setattr(settings, "max_subqueries_ceiling", 12)
    items = [{"subq": f"aspect {i} of wrongful trading section 214", "queries": ["q"],
              "scope": "", "part": 1} for i in range(20)]
    combined, meta = retrieve_mod._ensure_coverage(FIVE_PART, items)
    assert meta["subqs_injected"] == 4, "parts 2-5 injected"
    assert len(combined) == 12, "trimmed to the ceiling"
    assert set(meta["part_map"]) == {0, 1, 2, 3, 4}, "every part still mapped"
    # part 1 (index 0) kept several LLM sub-Qs but was trimmed from 20; injects untouched.
    assert meta["part_map"].count(0) == 8


def test_non_numbered_prompt_unchanged():
    # A prose prompt with no numbered parts: no coverage work, flat cap, part_map all None
    # → the backend falls back to unified synthesis (per_part disabled).
    prose = "Explain the fiduciary duties a company director owes and how they are enforced."
    items = [{"subq": prose, "queries": [prose], "scope": "", "part": None}]
    combined, meta = retrieve_mod._ensure_coverage(prose, items)
    assert meta["parts_detected"] == 0
    assert meta["parts_covered"] == 0
    assert meta["subqs_injected"] == 0
    assert meta["part_map"] == [None]


def test_decompose_end_to_end_recovers_untagged_questions(monkeypatch):
    # The decompose LLM tags only 3 of the 5 parts; _decompose must parse the `part` tags
    # and the guardrail must inject the two untagged parts.
    async def fake_complete(system, user, max_tokens=2048):
        return (
            '[{"subq": "director liability wrongful trading section 214", "queries": ["a"], "part": 1},'
            ' {"subq": "statutory duties of a company secretary", "queries": ["b"], "part": 2},'
            ' {"subq": "how a special resolution is passed", "queries": ["c"], "part": 3}]'
        )

    monkeypatch.setattr(llm, "complete", fake_complete)
    monkeypatch.setattr(llm, "set_stage", lambda *a, **k: None)
    monkeypatch.setattr(llm, "set_guided", lambda *a, **k: None)
    items, meta = _run(retrieve_mod._decompose(FIVE_PART))
    assert meta["parts_detected"] == 5
    assert meta["subqs_injected"] == 2
    assert meta["parts_covered"] == 5
    assert len(items) == 5
    # The three model sub-questions carry their parsed tags; the two injects carry theirs.
    assert sorted(i["part"] for i in items) == [1, 2, 3, 4, 5]
