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

"""Edit-only global coherence pass (step 4): the
full draft fits the window by construction (budgets invariant). Fix
transitions, terminology and duplication; write the executive summary LAST
into its placeholder. Forbidden to add new claims — guarded deterministically:
a section that gains a citation ID or loses more than 40 % of its length
reverts to its pre-cohere text. Unparseable output keeps the draft."""

import logging
import re

from .. import llm
from .budgets import ResearchBudgets
from .templates import EXEC_SUMMARY_PLACEHOLDER

_log = logging.getLogger("pai.research.cohere")

_WID_RE = re.compile(r"\[[WD]\d+\]")
_H2_RE = re.compile(r"^## .+$", re.MULTILINE)


def split_sections(report: str) -> list[str]:
    """Split a draft into [preamble?, section, section, ...] on H2 headings,
    each part starting with its heading (preamble only if text precedes the
    first H2)."""
    matches = list(_H2_RE.finditer(report))
    if not matches:
        return [report]
    parts: list[str] = []
    if matches[0].start() > 0 and report[: matches[0].start()].strip():
        parts.append(report[: matches[0].start()].rstrip())
    for i, m in enumerate(matches):
        end = matches[i + 1].start() if i + 1 < len(matches) else len(report)
        parts.append(report[m.start() : end].rstrip())
    return parts


def _wids(text: str) -> set[str]:
    return set(_WID_RE.findall(text))


def guard_sections(draft: str, edited: str) -> str:
    """Per-section deterministic guard: any edited section that introduces a
    citation ID absent from its draft counterpart, or shrinks below 60 % of
    its draft length, reverts to the draft section. Sections are matched by
    heading; a structural mismatch (headings changed) keeps the whole draft."""
    d_parts, e_parts = split_sections(draft), split_sections(edited)
    d_by_head = {p.splitlines()[0]: p for p in d_parts if p.startswith("## ")}
    e_heads = [p.splitlines()[0] for p in e_parts if p.startswith("## ")]
    if set(d_by_head) != set(e_heads):
        return draft  # structure changed — cohere may not do that
    out: list[str] = []
    for part in e_parts:
        if not part.startswith("## "):
            out.append(part)
            continue
        head = part.splitlines()[0]
        d_part = d_by_head[head]
        if EXEC_SUMMARY_PLACEHOLDER in d_part:
            out.append(part)  # the placeholder section is MEANT to grow
            continue
        if not _wids(part) <= _wids(d_part):
            out.append(d_part)  # new citation appeared — revert
        elif len(part) < 0.6 * len(d_part):
            out.append(d_part)  # over-aggressive cut — revert
        else:
            out.append(part)
    return "\n\n".join(out)


async def run(draft: str, question: str, b: ResearchBudgets) -> str:
    """One edit-only pass over the whole draft. Returns the cohered report (or
    the draft unchanged on any failure)."""
    has_placeholder = EXEC_SUMMARY_PLACEHOLDER in draft
    system = (
        "You are EDITING a finished research report — an edit-only pass. Improve "
        "transitions between sections, unify terminology, remove duplicated points. "
        "Keep every heading exactly as it is, in the same order. Keep all citation "
        "markers ([W#] for web sources, [D#] for the user's documents) attached to "
        "their claims; NEVER add a citation or a new factual claim."
        + (
            f" Replace the literal placeholder {EXEC_SUMMARY_PLACEHOLDER} with a "
            "one-paragraph executive summary of the report's findings."
            if has_placeholder
            else ""
        )
        + " Return the FULL edited report in markdown."
    )
    try:
        llm.set_stage("research.cohere")
        out = await llm.complete(system, f"Question: {question}\n\n{draft}", max_tokens=b.cohere_max_tokens)
        edited = out.strip()
        result = guard_sections(draft, edited) if edited and "## " in edited else draft
    except Exception as e:  # noqa: BLE001 — keep the draft
        _log.warning("coherence pass failed (keeping draft): %s", e)
        result = draft
    # Never ship the literal placeholder — if cohere failed to write the
    # executive summary, drop the marker rather than print it.
    return result.replace(EXEC_SUMMARY_PLACEHOLDER, "").strip()
