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

"""Templates: well-formed skeletons; Formal carries the exec-summary
placeholder; an unknown id raises (Rust validates the four ids upstream — a
mismatch is a bug, never a silent free-form downgrade)."""

import pathlib
import sys

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app.research import templates
from app.research.templates import EXEC_SUMMARY_PLACEHOLDER


def test_all_templates():
    ids = {t.id for t in templates.all_templates()}
    assert ids == {"exploration", "formal", "freeform", "literature"}


def test_literature_skeleton_has_analysis_and_exec_summary():
    lit = templates.get("literature")
    headings = [s.heading for s in lit.skeleton]
    assert headings[0] == "Executive summary"
    assert lit.skeleton[0].placeholder == EXEC_SUMMARY_PLACEHOLDER
    # Its own contradictions/gaps section (so the pipeline does not inject one).
    assert "Consensus, contradictions and gaps" in headings
    assert lit.outline_mode == "constrained"
    assert "Themes in the literature" in lit.expandable


def test_formal_exec_summary_placeholder():
    formal = templates.get("formal")
    first = formal.skeleton[0]
    assert first.heading == "Executive summary"
    assert first.placeholder == EXEC_SUMMARY_PLACEHOLDER
    assert formal.outline_mode == "constrained"
    assert "Findings" in formal.expandable


def test_exploration_constrained():
    t = templates.get("exploration")
    assert [s.heading for s in t.skeleton] == [
        "Context & framing", "Landscape & options", "Key unknowns & risks", "Recommendations",
    ]
    assert t.outline_mode == "constrained"


def test_freeform_explicit_only():
    assert templates.get("freeform").skeleton == []
    assert templates.get("freeform").outline_mode == "free"


def test_unknown_id_raises():
    # Fail closed: an id outside the validated four is a bug, not a free-form choice.
    with pytest.raises(ValueError):
        templates.get("nonsense")
    with pytest.raises(ValueError):
        templates.get("")
