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

"""Locate unit checks (groundedness §4.5): a decomposed/rephrased claim is bound
to the single best-matching SENTENCE of the source — bounded (no over-reach across
sentences) and exact (offsets index the returned text). An unlocatable claim binds
to nothing (so it is neither highlighted nor repaired). Run from ml/:
`uv run python -m pytest tests/test_locate.py`."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import locate  # noqa: E402

CV = (
    "The harbour office at Port Aldwick was rebuilt in 1947. "
    "The new building is made of local grey granite and has three floors. "
    "The harbourmaster keeps records on the top floor."
)


def test_binds_the_best_matching_sentence():
    r = locate.locate("The new building has three floors.", CV)
    assert r is not None
    assert "three floors" in r["text"]
    # Bounded to that one sentence — it must not bleed into its neighbours.
    assert "rebuilt" not in r["text"]
    assert "harbourmaster" not in r["text"]


def test_offsets_are_exact():
    r = locate.locate("The new building has three floors.", CV)
    assert r is not None
    assert CV[r["start"] : r["end"]] == r["text"]


def test_unlocatable_claim_returns_none():
    assert locate.locate("Dragons once guarded the harbour vault.", CV) is None


def test_empty_inputs_return_none():
    assert locate.locate("", CV) is None
    assert locate.locate("anything", "") is None


def test_single_sentence_no_tail_inflation():
    # A stray short match in the next sentence must not stitch two sentences
    # together (the greedy-tail bug): the span stays one sentence.
    text = (
        "The Pellman Bridge carries the county road across the gorge. "
        "It was completed in 1908 and spans 96 metres exactly."
    )
    r = locate.locate("It was completed in 1908 and spans 96 metres.", text)
    assert r is not None
    assert r["text"].count(".") <= 1
    assert "Pellman Bridge" not in r["text"]


def test_section_offsets_are_ordered_and_zero_based():
    text = "Alpha block one. Beta block two. Gamma block three."
    secs = ["Alpha block one.", " Beta block two.", " Gamma block three."]
    offs = locate.section_offsets(text, secs)
    assert offs[0] == 0
    assert offs == sorted(offs)


if __name__ == "__main__":  # allow `python tests/test_locate.py`
    import pytest

    raise SystemExit(pytest.main([__file__, "-q"]))
