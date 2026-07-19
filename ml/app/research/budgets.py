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

"""Adaptive research budgets (a resolved
requirement, not an option). The platform never assumes a context size: it
learns `max_model_len` at runtime, and every lever here is a CONTINUOUS
function of it with floors and ceilings — 65k and 131k Dev boxes differ
smoothly, 32k stays correct, 1M sprawls. Same philosophy as the existing
STUFF-vs-EXHAUSTIVE token routing (`stuff_fraction`).

Pure module: no I/O, no LLM — fully unit-testable."""

from dataclasses import dataclass

from ..config import settings
from ..web.loop import Budget as WebBudget


def _clamp(v: int, lo: int, hi: int) -> int:
    return max(lo, min(hi, v))


@dataclass
class ResearchBudgets:
    max_model_len: int
    # Collection.
    subqs: int                 # research sub-questions planned
    max_sources: int           # memory-bank cap after top_sources
    serp_limit: int            # SERP results considered per query
    fetch_per_round: int       # pages fetched per sub-question round
    rounds: int                # web-loop rounds per sub-question (deep)
    max_serp_queries: int      # global across the whole run
    max_fetches: int           # global across the whole run
    collect_seconds: float     # wall-clock slice for collection
    # Notes.
    note_tokens: int           # max_tokens per notes call
    note_input_tokens: int     # source chunks visible per notes call
    # Outline.
    outline_input_tokens: int  # claim-bullet digest visible to the outline call
    sections_lo: int
    sections_hi: int
    # Writer.
    section_max_tokens: int    # output budget per section call
    section_words_lo: int
    section_words_hi: int
    writer_input_tokens: int   # notes + summary + register visible per section
    # Coherence.
    cohere_max_tokens: int
    # Corpus census (Phase 2). source ∈ {web, files, hybrid}; the time splits
    # below derive from it so corpus and web phases share one wall-clock budget.
    source: str = "web"
    census_seconds: float = 0.0    # wall-clock slice for the census/sampling sweep
    census_note_tokens: int = 0    # max_tokens per per-document note call
    census_input_tokens: int = 0   # document text visible per note call
    # Per-section deepening (an additive pre-write stage that judges each
    # section's evidence sufficiency and digs for the gaps). All levers are 0 on
    # a small context so the stage is a no-op there — the pipeline stays
    # byte-identical for weak deploys.
    deepen_rounds: int = 0              # judge→dig→re-judge iterations per section (0 disables)
    deepen_sections_hi: int = 0         # cap on how many hungry sections actually dig
    deepen_max_new_sources: int = 0     # cap on sources a single section may gain
    deepen_input_tokens: int = 0        # bound-notes digest visible to the judge call
    deepen_seconds: float = 0.0         # wall-clock slice for the whole deepening stage

    def per_subq_budget(self) -> WebBudget:
        """The web-loop Budget one research sub-question runs under. The caller
        shares one pool/seen/state across sub-questions, so the global caps are
        enforced by the shared `_State` the pipeline constructs — these per-call
        numbers bound a single sub-question's appetite."""
        return WebBudget(
            decompose=False,  # the research planner already decomposed
            subqs=1,
            rounds=self.rounds,
            serp_limit=self.serp_limit,
            fetch_per_round=self.fetch_per_round,
            max_serp_queries=max(6, self.max_serp_queries // max(1, self.subqs)),
            max_fetches=max(6, self.max_fetches // max(1, self.subqs)),
            wall_clock=self.collect_seconds / max(1, self.subqs),
        )

    def per_deepen_budget(self) -> WebBudget:
        """The web-loop Budget one deepening gap-query runs under — a tighter
        cousin of `per_subq_budget`: one shallow round, fewer fetches. The
        section shares one pool/seen/state across its gaps, so the section's
        appetite is bounded by that shared `_State` (sized from
        `deepen_max_new_sources` / `deepen_seconds`), not these per-call numbers."""
        return WebBudget(
            decompose=False,
            subqs=1,
            rounds=1,
            serp_limit=self.serp_limit,
            fetch_per_round=max(3, self.fetch_per_round // 2),
            max_serp_queries=max(3, self.max_serp_queries // 4),
            max_fetches=max(3, self.deepen_max_new_sources),
            wall_clock=self.deepen_seconds,
        )


def budgets(
    max_model_len: int,
    source: str = "web",
    max_minutes: float | None = None,
) -> ResearchBudgets:
    # Deep Research has a single, deep mode (the Standard/Deep choice was retired):
    # the deep levers below are unconditional.
    ctx = max(8_192, int(max_model_len))
    src = (source or "web").lower()
    run_minutes = settings.research_max_minutes if max_minutes is None else max_minutes

    subqs = _clamp(2 + ctx // 49_152, 3, 8) + 2
    subqs = min(subqs, 10)

    max_sources = _clamp(ctx // 4_096, 20, 80)
    max_sources = min(int(max_sources * 1.5), 120)

    sections_hi = _clamp(3 + ctx // 32_768, 5, 12)
    section_max_tokens = _clamp(ctx // 16, 2_000, 4_096)
    section_words_hi = _clamp(500 + ctx // 256, 1_000, 1_500)

    # The writer's prompt (outline + bound notes + rolling summary + register)
    # must leave room for the section output inside the stuff fraction.
    stuff_tokens = int(ctx * settings.stuff_fraction)
    writer_input = max(4_000, stuff_tokens - section_max_tokens)

    # The coherence pass reads the whole draft + emits the edited draft. Cap the
    # report's worst case (sections_hi × section_words_hi × ~1.4 tokens/word)
    # within the stuff fraction; budgets keep this invariant by construction —
    # asserted in tests rather than clamped at runtime.
    cohere_max_tokens = _clamp(int(sections_hi * section_words_hi * 1.5), 4_096, 32_768)

    # Collection wall-clock: a slice of the run budget, leaving the synthesis
    # phases the rest. Deep runs lean harder on collection. Corpus modes split
    # the budget between the census sweep and (for hybrid) the web gap-fill.
    total_seconds = run_minutes * 60.0
    if src == "files":
        census_seconds = total_seconds * 0.55
        collect_seconds = 0.0
    elif src == "hybrid":
        census_seconds = total_seconds * 0.40
        collect_seconds = max(90.0, total_seconds * 0.25)
    else:  # web
        census_seconds = 0.0
        collect_seconds = total_seconds * 0.6

    # Per-section deepening. Disabled below a 32k context (a small deploy keeps
    # the byte-identical single-pass path); above it, one or two rounds. The
    # stage is time-boxed to a slice of the run so a slow dig can never eat the
    # writer's budget.
    deepen_rounds = 0 if ctx < 32_768 else _clamp(1 + ctx // 131_072, 1, 2)
    deepen_seconds = min(total_seconds * 0.25, 240.0)

    return ResearchBudgets(
        source=src,
        census_seconds=census_seconds,
        census_note_tokens=_clamp(ctx // 200, 250, 800),
        census_input_tokens=_clamp(int(ctx * settings.stuff_fraction), 6_000, 200_000),
        deepen_rounds=deepen_rounds,
        deepen_sections_hi=_clamp(sections_hi // 2, 2, 6),
        deepen_max_new_sources=_clamp(ctx // 32_768, 3, 5),
        deepen_input_tokens=_clamp(int(ctx * 0.2), 3_000, 12_000),
        deepen_seconds=deepen_seconds,
        max_model_len=ctx,
        subqs=subqs,
        max_sources=max_sources,
        serp_limit=_clamp(ctx // 16_384, 12, 20),
        fetch_per_round=_clamp(ctx // 49_152, 6, 10),
        rounds=_clamp(4 + ctx // 131_072, 4, 6),
        max_serp_queries=_clamp(subqs * 6, 24, 80),
        max_fetches=_clamp(max_sources, 28, 80),
        collect_seconds=collect_seconds,
        note_tokens=_clamp(ctx // 200, 400, 900),
        note_input_tokens=_clamp(int(ctx * 0.35), 6_000, 24_000),
        outline_input_tokens=_clamp(int(ctx * 0.4), 6_000, 48_000),
        sections_lo=4,
        sections_hi=sections_hi,
        section_max_tokens=section_max_tokens,
        section_words_lo=500,
        section_words_hi=section_words_hi,
        writer_input_tokens=writer_input,
        cohere_max_tokens=cohere_max_tokens,
    )


def est_tokens(s: str) -> int:
    """Chars/4 — the platform-wide budgeting heuristic (matches Rust)."""
    return len(s) // 4
