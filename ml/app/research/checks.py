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

"""Deterministic structure checks: headings match
the approved outline, per-section word bands, every [W#] resolves, and a
citation-density floor per evidence section. Pure — no LLM. The pipeline
reacts (strip unresolved markers; one rewrite; deliver with a note); a report
is never failed for style."""

import re
from dataclasses import dataclass

from .bank import Bank
from .budgets import ResearchBudgets
from .cohere import split_sections
from .outline import Outline

_WID_RE = re.compile(r"\[([WD]\d+)\]")


@dataclass
class Violation:
    kind: str           # missing_heading | heading_order | word_band | unresolved_id | no_citations
    section: str | None
    detail: str


def strip_unresolved(report: str, bank: Bank) -> tuple[str, list[str]]:
    """Remove citation markers that don't resolve in the bank (the claim
    survives uncited rather than carrying a fabricated reference). Returns
    (cleaned report, stripped IDs)."""
    known = set(bank.sids())
    stripped: list[str] = []

    def repl(m: re.Match) -> str:
        sid = m.group(1)
        if sid in known:
            return m.group(0)
        stripped.append(sid)
        return ""

    return _WID_RE.sub(repl, report), stripped


def run_checks(report: str, outline: Outline, bank: Bank, b: ResearchBudgets) -> list[Violation]:
    violations: list[Violation] = []
    parts = split_sections(report)
    section_parts = [p for p in parts if p.startswith("## ")]
    headings = [p.splitlines()[0][3:].strip() for p in section_parts]
    # Outline headings as they appear in the report ("{n}. {heading}").
    expected = [f"{i + 1}. {s.heading}" for i, s in enumerate(outline.sections)]

    for e in expected:
        if e not in headings:
            violations.append(Violation("missing_heading", e, f"heading '{e}' absent"))
    present_expected = [h for h in headings if h in expected]
    if present_expected != [e for e in expected if e in present_expected]:
        violations.append(Violation("heading_order", None, "outline order not preserved"))

    known = set(bank.sids())
    placeholder_headings = {
        f"{i + 1}. {s.heading}" for i, s in enumerate(outline.sections) if s.placeholder is not None
    }
    lo = int(b.section_words_lo * 0.7)
    hi = int(b.section_words_hi * 1.3)
    for part in section_parts:
        head = part.splitlines()[0][3:].strip()
        body = "\n".join(part.splitlines()[1:]).strip()
        words = len(body.split())
        ids = set(_WID_RE.findall(body))
        if head in placeholder_headings:
            continue  # the executive summary is exempt from bands + density
        if head in ("References", "Coverage"):
            continue
        if words and not (lo <= words <= hi):
            violations.append(Violation("word_band", head, f"{words} words outside {lo}-{hi}"))
        for sid in ids - known:
            violations.append(Violation("unresolved_id", head, f"[{sid}] does not resolve"))
        if not ids and words > 50:
            violations.append(Violation("no_citations", head, "evidence section cites nothing"))
    return violations
