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


# The backend mirrors these four for the picker in
# backend/src/http/research_templates.rs (label, outline mode, section headings).
# If any of them change here, update that constant too (a matching pin test on the
# Rust side guards the other direction). This is a pin, not a proof: the two tests
# cannot see each other, so a synchronised edit of both would slip through.
EXPECTED_BUILTINS = {
    "exploration": {
        "label": "Exploration brief",
        "outline_mode": "constrained",
        "headings": [
            "Context & framing", "Landscape & options", "Key unknowns & risks", "Recommendations",
        ],
    },
    "formal": {
        "label": "Formal report",
        "outline_mode": "constrained",
        "headings": [
            "Executive summary", "Background", "Findings", "Analysis",
            "Conclusions & recommendations",
        ],
    },
    "freeform": {"label": "Free-form", "outline_mode": "free", "headings": []},
    "literature": {
        "label": "Literature review",
        "outline_mode": "constrained",
        "headings": [
            "Executive summary", "Introduction & scope", "Review method & corpus",
            "Themes in the literature", "Consensus, contradictions and gaps",
            "Conclusions & further research",
        ],
    },
}


def test_builtins_mirror_the_backend_picker():
    for tid, want in EXPECTED_BUILTINS.items():
        t = templates.get(tid)
        assert t.label == want["label"], tid
        assert t.outline_mode == want["outline_mode"], tid
        assert [s.heading for s in t.skeleton] == want["headings"], tid


def test_to_spec_wire_shape():
    # The wire shape `GET /research/templates` serves. Each section carries the two
    # derived per-section flags the editor round-trips.
    specs = [templates.to_spec(t) for t in templates.all_templates()]
    assert {s["id"] for s in specs} == {"exploration", "formal", "freeform", "literature"}
    formal = next(s for s in specs if s["id"] == "formal")
    assert set(formal) == {"id", "label", "skeleton", "writing_instructions", "outline_mode"}
    exec_row = formal["skeleton"][0]
    assert set(exec_row) == {"heading", "brief", "expandable", "exec_summary"}
    assert exec_row["exec_summary"] is True
    findings = next(s for s in formal["skeleton"] if s["heading"] == "Findings")
    assert findings["expandable"] is True
    assert findings["exec_summary"] is False


@pytest.mark.parametrize("tid", ["exploration", "formal", "freeform", "literature"])
def test_from_spec_inverts_to_spec(tid):
    # to_spec serves the picker; from_spec feeds the pipeline; between them the
    # backend just forwards JSON. They must be exact inverses for every built-in.
    original = templates.get(tid)
    rebuilt = templates.from_spec(templates.to_spec(original))
    assert rebuilt.id == original.id
    assert rebuilt.label == original.label
    assert rebuilt.writing_instructions == original.writing_instructions
    assert rebuilt.outline_mode == original.outline_mode
    assert rebuilt.expandable == original.expandable
    assert [(s.heading, s.brief, s.placeholder) for s in rebuilt.skeleton] == [
        (s.heading, s.brief, s.placeholder) for s in original.skeleton
    ]


def test_from_spec_exec_summary_and_expandable():
    spec = {
        "id": "x",
        "label": "Custom",
        "outline_mode": "constrained",
        "writing_instructions": "Be terse.",
        "skeleton": [
            {"heading": "Summary", "brief": "", "expandable": False, "exec_summary": True},
            {"heading": "Body", "brief": "the evidence", "expandable": True, "exec_summary": False},
        ],
    }
    t = templates.from_spec(spec)
    assert t.skeleton[0].placeholder == EXEC_SUMMARY_PLACEHOLDER
    assert t.skeleton[1].placeholder is None
    assert t.expandable == ("Body",)


def test_from_spec_free_mode_clears_flags():
    # In free mode the flags are inert downstream, so from_spec drops them.
    spec = {
        "id": "x",
        "label": "Custom",
        "outline_mode": "free",
        "writing_instructions": "",
        "skeleton": [
            {"heading": "A", "brief": "", "expandable": True, "exec_summary": True},
        ],
    }
    t = templates.from_spec(spec)
    assert t.expandable == ()
    assert t.skeleton[0].placeholder is None


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
