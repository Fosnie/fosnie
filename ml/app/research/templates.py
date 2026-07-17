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

"""Report templates: section skeletons + writing
instructions, not prompts the user manages. v1 ships three, driven by the two
dominant company uses — *working on something new* (Exploration brief) and
*preparing a report* (Formal report) — plus Free-form. Literature review joins
in Phase 2 with corpus mode. The References section is always appended
deterministically by the pipeline, never written by the LLM."""

from dataclasses import dataclass, field

# The cohere pass replaces this placeholder with the executive summary —
# written LAST, after every findings section exists.
EXEC_SUMMARY_PLACEHOLDER = "[[EXECUTIVE-SUMMARY]]"


@dataclass
class SectionSpec:
    heading: str
    brief: str
    # A placeholder section is emitted verbatim and filled by cohere, not the writer.
    placeholder: str | None = None


@dataclass
class Template:
    id: str
    label: str
    # Empty skeleton = the outline is fully LLM-derived ("free" mode).
    skeleton: list[SectionSpec] = field(default_factory=list)
    writing_instructions: str = ""
    # "constrained": skeleton headings must survive outlining (extra subsections
    # allowed where noted). "free": the outline call invents the structure.
    outline_mode: str = "constrained"
    # Headings the outline may expand into several sections.
    expandable: tuple[str, ...] = ()


_EXPLORATION = Template(
    id="exploration",
    label="Exploration brief",
    skeleton=[
        SectionSpec("Context & framing", "What is being explored and why it matters now; the question behind the question."),
        SectionSpec("Landscape & options", "The main approaches/players/technologies found, compared honestly.", ),
        SectionSpec("Key unknowns & risks", "What the evidence does not settle; risks, open questions, conflicting claims."),
        SectionSpec("Recommendations", "What to do next given the evidence — concrete, hedged where the evidence is thin."),
    ],
    writing_instructions=(
        "You are writing an exploration brief for someone working on something new. "
        "Question-driven and options-oriented: lay out the landscape, compare options "
        "honestly, be explicit about unknowns. Prefer concrete facts, figures and "
        "named examples from the evidence over generalities."
    ),
    outline_mode="constrained",
    expandable=("Landscape & options",),
)

_FORMAL = Template(
    id="formal",
    label="Formal report",
    skeleton=[
        SectionSpec("Executive summary", "One-paragraph summary of the findings — written last.", placeholder=EXEC_SUMMARY_PLACEHOLDER),
        SectionSpec("Background", "The context and scope of this report."),
        SectionSpec("Findings", "The evidence, organised thematically.", ),
        SectionSpec("Analysis", "What the findings mean taken together; patterns, tensions, implications."),
        SectionSpec("Conclusions & recommendations", "Conclusions that follow from the analysis, and recommended actions."),
    ],
    writing_instructions=(
        "You are writing a formal report. Findings-structured, measured, third "
        "person. Every substantive claim cites its source. No rhetorical filler; "
        "the reader is a professional who wants the substance."
    ),
    outline_mode="constrained",
    expandable=("Findings",),
)

_FREEFORM = Template(
    id="freeform",
    label="Free-form",
    skeleton=[],
    writing_instructions=(
        "Shape the report to serve the user's question/instructions directly. "
        "Clear headings, evidence-led prose, no padding."
    ),
    outline_mode="free",
)

# Phase 2 — the university/research-corpus case. Its skeleton already contains a
# "Consensus, contradictions and gaps" section, so the pipeline does NOT inject
# its own (corpus/hybrid) analysis section for this template.
_LITERATURE = Template(
    id="literature",
    label="Literature review",
    skeleton=[
        SectionSpec("Executive summary", "One-paragraph synthesis of the review — written last.", placeholder=EXEC_SUMMARY_PLACEHOLDER),
        SectionSpec("Introduction & scope", "The body of work under review and why it matters; the review question."),
        SectionSpec("Review method & corpus", "How the review was conducted and what was read — the census over the corpus is the method."),
        SectionSpec("Themes in the literature", "Findings organised by theme, comparing and contrasting sources.", ),
        SectionSpec("Consensus, contradictions and gaps", "Where the sources agree, where they conflict, and what remains unstudied."),
        SectionSpec("Conclusions & further research", "What the literature establishes and the questions it leaves open."),
    ],
    writing_instructions=(
        "You are writing an academic literature review. Measured, third-person, "
        "British English. Every claim cites its source. Compare and contrast across "
        "sources — synthesise themes rather than summarising one source after "
        "another. Be explicit about where the evidence agrees, conflicts, and is "
        "silent."
    ),
    outline_mode="constrained",
    expandable=("Themes in the literature",),
)

_ALL = {t.id: t for t in (_EXPLORATION, _FORMAL, _FREEFORM, _LITERATURE)}


def get(template_id: str) -> Template:
    """Resolve a built-in template id. User-defined templates never reach this
    function: the backend resolves them from its own store and sends the whole
    specification inline (see `from_spec`), so anything arriving here is expected
    to be one of the four built-ins. An unknown id is therefore a bug, not a user
    choice — raise rather than silently downgrading to free-form (which would
    discard the requested structure without telling anyone). Free-form is reachable
    only by its explicit `freeform` id."""
    key = (template_id or "").strip().lower()
    try:
        return _ALL[key]
    except KeyError:
        raise ValueError(
            f"unknown report template '{template_id}'; expected one of {sorted(_ALL)}"
        ) from None


def all_templates() -> list[Template]:
    return list(_ALL.values())


def from_spec(spec: dict) -> Template:
    """Build a Template from a caller-supplied specification. The backend resolves
    user-defined templates from its own store and sends them inline; this service
    holds no database. Shape-mapping only: the backend has already enforced the
    limits (section count, heading uniqueness, lengths, at most one executive
    summary). The wire shape mirrors `to_spec`:

        {id, label, skeleton: [{heading, brief, expandable, exec_summary}],
         writing_instructions, outline_mode}

    In "free" mode the per-section flags do nothing downstream (they reach the
    outline only via the constrained skeleton-enforcement pass), so the executive
    summary placeholder is dropped and `expandable` collapses to empty — matching
    what the backend normalises and keeping this function honest on its own."""
    outline_mode = spec.get("outline_mode", "constrained")
    constrained = outline_mode == "constrained"
    sections = spec.get("skeleton") or []
    skeleton: list[SectionSpec] = []
    expandable: list[str] = []
    for s in sections:
        heading = s["heading"]
        placeholder = (
            EXEC_SUMMARY_PLACEHOLDER
            if constrained and s.get("exec_summary")
            else None
        )
        skeleton.append(SectionSpec(heading, s.get("brief", ""), placeholder=placeholder))
        if constrained and s.get("expandable"):
            expandable.append(heading)
    return Template(
        id=spec["id"],
        label=spec["label"],
        skeleton=skeleton,
        writing_instructions=spec.get("writing_instructions", ""),
        outline_mode=outline_mode,
        expandable=tuple(expandable),
    )


def to_spec(t: Template) -> dict:
    """The inverse of `from_spec`: the wire shape of a template. Serves the
    built-in definitions to callers that need a full copy — duplicating a built-in
    into a user-defined one. Inverse of `from_spec` for all four built-ins."""
    return {
        "id": t.id,
        "label": t.label,
        "skeleton": [
            {
                "heading": s.heading,
                "brief": s.brief,
                "expandable": s.heading in t.expandable,
                "exec_summary": s.placeholder == EXEC_SUMMARY_PLACEHOLDER,
            }
            for s in t.skeleton
        ],
        "writing_instructions": t.writing_instructions,
        "outline_mode": t.outline_mode,
    }
