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

"""Padding detection: the anti-verbosity
guard. LLM judges reward length, so the report is checked for two padding
signatures — near-duplicate sections (cross-section embedding similarity) and
thin sections (citation-density floor). Emits `checks.Violation`s consumed by
the structural note + the eval harness.

Kept OUT of the pure `checks.py` because it needs `embeddings.embed` (network
I/O). Async, deadline-aware, fail-open: any failure / disabled / past-deadline
⇒ `[]` (no signal, report unaffected)."""

import logging
import re
import time

from .. import embeddings
from . import cohere as cohere_mod
from .bank import Bank
from .budgets import ResearchBudgets
from .checks import Violation
from .outline import Outline

_log = logging.getLogger("pai.research.padding")

_WID_RE = re.compile(r"\[[WD]\d+\]")
_SIM_THRESHOLD = 0.86       # cosine above this ⇒ two sections say the same thing
_DENSITY_FLOOR = 1.0 / 250  # < one citation per 250 words ⇒ thin/padded section


def _exempt(outline: Outline) -> set[str]:
    ex = {"References", "Coverage"}
    for i, s in enumerate(outline.sections):
        if s.placeholder is not None:
            ex.add(f"{i + 1}. {s.heading}")
    return ex


def _cosine(a: list[float], b: list[float]) -> float:
    num = sum(x * y for x, y in zip(a, b))
    da = sum(x * x for x in a) ** 0.5
    db = sum(y * y for y in b) ** 0.5
    return num / (da * db) if da and db else 0.0


async def detect_padding(
    report: str, outline: Outline, bank: Bank, b: ResearchBudgets, deadline: float
) -> list[Violation]:
    """Returns padding violations, or [] on disabled / past-deadline / failure."""
    if time.monotonic() >= deadline:
        return []
    exempt = _exempt(outline)
    headings: list[str] = []
    bodies: list[str] = []
    for part in cohere_mod.split_sections(report):
        if not part.startswith("## "):
            continue
        head = part.splitlines()[0][3:].strip()
        if head in exempt:
            continue
        body = "\n".join(part.splitlines()[1:]).strip()
        if body:
            headings.append(head)
            bodies.append(body)
    if len(bodies) < 1:
        return []

    violations: list[Violation] = []

    # Citation-density floor (cheap, no I/O).
    for head, body in zip(headings, bodies):
        words = len(body.split())
        cites = len(_WID_RE.findall(body))
        if words > 50 and (cites / words) < _DENSITY_FLOOR:
            violations.append(
                Violation("citation_density", head, f"{cites} citations across {words} words (thin)")
            )

    # Cross-section similarity (needs embeddings; fail-open).
    if len(bodies) >= 2:
        try:
            vecs = await embeddings.embed(bodies)
        except Exception as e:  # noqa: BLE001 — padding is best-effort
            _log.debug("padding embed failed (skipped): %s", e)
            vecs = []
        for i in range(len(vecs)):
            for j in range(i + 1, len(vecs)):
                if _cosine(vecs[i], vecs[j]) > _SIM_THRESHOLD:
                    violations.append(
                        Violation("padding_similarity", headings[j],
                                  f"near-duplicate of '{headings[i]}'")
                    )
    return violations
