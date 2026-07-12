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

"""Outline with evidence bindings (step 2,
WebWeaver-style): every outline node carries the note IDs its section may
cite. One LLM call over the template skeleton + a claim-bullet digest; a
deterministic post-pass clamps the section count, drops unknown IDs, and
rerank-assigns unbound notes so no section is evidence-empty. Unparseable
output degrades to the template skeleton — never raises."""

import json
import logging
from dataclasses import dataclass

from .. import guided, llm, reranker
from .bank import Bank
from .budgets import ResearchBudgets
from .templates import Template

_log = logging.getLogger("pai.research.outline")


@dataclass
class OutlineSection:
    heading: str
    brief: str
    note_ids: list[str]
    placeholder: str | None = None  # emitted verbatim; filled by cohere


@dataclass
class Outline:
    sections: list[OutlineSection]


def _digest(bank: Bank, budget_tokens: int) -> str:
    """Claim bullets per source, sliced to the outline input budget."""
    lines: list[str] = []
    for rec in bank.records:
        claims = rec.note.claims if rec.note else []
        lines.append(rec.meta_line())
        lines.extend(f"  - {c}" for c in claims[:6])
    text = "\n".join(lines)
    return text[: budget_tokens * 4]


def _skeleton_outline(template: Template, bank: Bank) -> Outline:
    """Deterministic fallback: the template skeleton with notes distributed
    round-robin (rerank assignment happens in the post-pass)."""
    sections = [
        OutlineSection(heading=s.heading, brief=s.brief, note_ids=[], placeholder=s.placeholder)
        for s in template.skeleton
    ] or [
        OutlineSection(heading="Findings", brief="What the evidence shows.", note_ids=[]),
        OutlineSection(heading="Analysis", brief="What it means.", note_ids=[]),
        OutlineSection(heading="Conclusions", brief="Where this lands.", note_ids=[]),
        OutlineSection(heading="Recommendations", brief="What to do next.", note_ids=[]),
    ]
    return Outline(sections=sections)


async def _assign_unbound(outline: Outline, bank: Bank) -> None:
    """Every source should be citable somewhere: rerank each unbound note's
    text against the section headings and bind it to its best section. Then
    guarantee no evidence section is empty — a section the outline LLM left
    unbound (e.g. a re-inserted skeleton heading) borrows the most relevant note
    from the corpus, so the writer always has evidence (placeholders exempt)."""
    bound = {sid for s in outline.sections for sid in s.note_ids}
    unbound = [rec for rec in bank.records if rec.sid not in bound]
    evidence_sections = [s for s in outline.sections if s.placeholder is None]
    if not evidence_sections:
        return
    headings = [f"{s.heading}. {s.brief}" for s in evidence_sections]
    for rec in unbound:
        text = rec.note.text() if rec.note else (rec.source.title or rec.source.url)
        scores = await reranker.rerank(text[:1200], headings)
        best = max(range(len(headings)), key=lambda i: scores[i]) if scores else 0
        evidence_sections[best].note_ids.append(rec.sid)

    # Rebalance: any evidence section still empty borrows the best-matching note
    # from the corpus (sharing — a source may be cited in several sections; donors
    # are not depleted). Evidence-empty only when the bank itself is empty, which
    # the pipeline already short-circuits before outlining.
    records = bank.records
    if not records:
        return
    candidate_texts = [
        (rec.note.text() if rec.note else (rec.source.title or rec.source.url))[:1200]
        for rec in records
    ]
    for section in evidence_sections:
        if section.note_ids:
            continue
        scores = await reranker.rerank(f"{section.heading}. {section.brief}", candidate_texts)
        best = max(range(len(records)), key=lambda i: scores[i]) if scores else 0
        section.note_ids.append(records[best].sid)


async def build(question: str, template: Template, bank: Bank, b: ResearchBudgets) -> Outline:
    known = set(bank.sids())
    skeleton_desc = (
        "\n".join(f"- {s.heading}: {s.brief}" for s in template.skeleton)
        if template.skeleton
        else "(no fixed skeleton — derive the structure from the question and evidence)"
    )
    expandable = ", ".join(template.expandable) or "none"
    system = (
        "You are planning a research report outline. Given the evidence notes, return "
        'ONLY a JSON array of sections: [{"heading": "...", "brief": "one sentence", '
        '"note_ids": ["W3", "D2", ...]}]. Bind each section to the evidence it will '
        "cite (use the [W#]/[D#] IDs exactly as shown in the notes). "
        f"Between {b.sections_lo} and {b.sections_hi} sections. "
        + (
            f"The skeleton below is REQUIRED — keep its headings in order (only these may "
            f"be split into several sections: {expandable}):\n{skeleton_desc}"
            if template.outline_mode == "constrained"
            else f"Suggested starting point (you may restructure freely):\n{skeleton_desc}"
        )
    )
    try:
        llm.set_stage("research.outline")
        llm.set_guided(guided.RESEARCH_OUTLINE)
        out = await llm.complete(
            system,
            f"Research question: {question}\n\nEvidence notes:\n{_digest(bank, b.outline_input_tokens)}",
            max_tokens=1024,
        )
        start, end = out.find("["), out.rfind("]")
        arr = json.loads(out[start : end + 1]) if start >= 0 else []
        sections: list[OutlineSection] = []
        for obj in arr:
            if not isinstance(obj, dict):
                continue
            heading = str(obj.get("heading", "")).strip()
            if not heading:
                continue
            ids = [i for i in obj.get("note_ids", []) if isinstance(i, str) and i in known]
            sections.append(
                OutlineSection(heading=heading, brief=str(obj.get("brief", "")).strip(), note_ids=ids)
            )
        if not sections:
            raise ValueError("no sections parsed")
        outline = Outline(sections=sections[: b.sections_hi])
    except Exception as e:  # noqa: BLE001 — skeleton fallback, never raise
        _log.warning("outline call failed, using template skeleton: %s", e)
        outline = _skeleton_outline(template, bank)

    # Constrained templates must keep their skeleton headings present, in order.
    if template.outline_mode == "constrained" and template.skeleton:
        outline = _enforce_skeleton(outline, template)

    await _assign_unbound(outline, bank)
    return outline


def _enforce_skeleton(outline: Outline, template: Template) -> Outline:
    """Keep the LLM's evidence bindings where headings match the skeleton; any
    missing skeleton heading is re-inserted at its position. Extra sections
    survive only as expansions of an expandable heading (kept after it)."""
    by_heading = {s.heading.strip().lower(): s for s in outline.sections}
    result: list[OutlineSection] = []
    for spec in template.skeleton:
        got = by_heading.get(spec.heading.strip().lower())
        if got is not None:
            got.placeholder = spec.placeholder
            result.append(got)
        else:
            result.append(
                OutlineSection(heading=spec.heading, brief=spec.brief, note_ids=[], placeholder=spec.placeholder)
            )
        if spec.heading in template.expandable:
            skeleton_headings = {x.heading.strip().lower() for x in template.skeleton}
            for s in outline.sections:
                if s.heading.strip().lower() not in skeleton_headings and s not in result:
                    result.append(s)
    return Outline(sections=result)
