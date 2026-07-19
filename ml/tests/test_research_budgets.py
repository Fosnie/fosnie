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

"""Adaptive research budgets: continuous in max_model_len, floors at 32k,
ceilings at 1M, and the context invariants that keep every phase inside the
deployed window. Pure — no I/O."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app.config import settings
from app.research.budgets import ResearchBudgets, budgets, est_tokens

_CTXS = [32_768, 65_536, 131_072, 262_144, 1_048_576]


def test_monotonic_in_context():
    prev: ResearchBudgets | None = None
    for ctx in _CTXS:
        b = budgets(ctx)
        if prev:
            assert b.subqs >= prev.subqs
            assert b.max_sources >= prev.max_sources
            assert b.sections_hi >= prev.sections_hi
            assert b.section_max_tokens >= prev.section_max_tokens
            assert b.writer_input_tokens >= prev.writer_input_tokens
        prev = b


def test_floors_at_32k():
    b = budgets(32_768)
    assert b.subqs >= 3
    assert b.max_sources >= 12
    assert b.sections_lo == 4 and b.sections_hi >= 5
    assert b.section_max_tokens >= 2_000
    assert b.note_tokens >= 400
    assert b.writer_input_tokens >= 4_000


def test_32k_collection_is_deep():
    # The dev/Ollama box sits at 32k; collection must still be DEEP there, not on
    # the old shallow floor (~9 fetches / 2 rounds). Mirrors the retired deep tier.
    b = budgets(32_768)
    assert b.max_fetches >= 28
    assert b.rounds >= 4
    assert b.serp_limit >= 12
    assert b.fetch_per_round >= 6
    wb = b.per_subq_budget()
    assert wb.rounds == b.rounds
    assert wb.max_fetches >= 6


def test_ceilings_at_1m():
    b = budgets(1_048_576)
    assert b.subqs <= 10  # single deep mode ceiling
    assert b.max_sources <= 120
    assert b.sections_hi <= 12
    assert b.section_max_tokens <= 4_096
    assert b.note_tokens <= 900
    assert b.serp_limit <= 20


def test_dev_boxes_differ_smoothly():
    b65, b131 = budgets(65_536), budgets(131_072)
    assert b131.max_sources > b65.max_sources, "65k vs 131k must differ (continuous, not tiers)"
    assert b131.writer_input_tokens > b65.writer_input_tokens


def test_single_deep_mode_widens_but_respects_ceilings():
    # The Standard/Deep choice was retired: every run is deep. The deep levers
    # (subqs +2, max_sources ×1.5) apply unconditionally, still inside the ceilings.
    b = budgets(131_072)
    base_subqs = max(3, min(8, 2 + 131_072 // 49_152))
    assert b.subqs == base_subqs + 2
    assert budgets(1_048_576).subqs <= 10
    assert budgets(1_048_576).max_sources <= 120


def test_writer_fits_stuff_fraction():
    for ctx in _CTXS:
        b = budgets(ctx)
        stuff = int(ctx * settings.stuff_fraction)
        assert b.writer_input_tokens + b.section_max_tokens <= stuff + 1, (
            f"writer input + output must fit the stuff fraction at {ctx}"
        )


def test_cohere_invariant():
    # The whole draft (input) + the edited draft (output) must fit the window:
    # worst-case report tokens ≈ sections_hi × words_hi × 1.5.
    for ctx in _CTXS:
        b = budgets(ctx)
        worst_report = int(b.sections_hi * b.section_words_hi * 1.5)
        assert worst_report + b.cohere_max_tokens <= ctx, f"cohere I/O must fit at {ctx}"


def test_per_subq_budget_caps_sum_within_globals():
    for ctx in _CTXS:
        b = budgets(ctx)
        wb = b.per_subq_budget()
        assert not wb.decompose, "the research planner already decomposed"
        assert wb.max_serp_queries * b.subqs <= b.max_serp_queries + 2 * b.subqs
        assert wb.max_fetches * b.subqs <= b.max_fetches + 2 * b.subqs
        assert wb.wall_clock * b.subqs <= b.collect_seconds + 1


def test_est_tokens():
    assert est_tokens("abcd" * 100) == 100


# --- Phase 2: source-dependent time splits ----------------------------------


def test_source_default_is_web_and_back_compatible():
    # Old call sites pass no source — must default to web.
    assert budgets(131_072).source == "web"
    assert budgets(131_072).census_seconds == 0.0
    assert budgets(131_072).collect_seconds == budgets(131_072, "web").collect_seconds


def test_files_splits_budget_to_census_no_web():
    b = budgets(131_072, "files")
    assert b.census_seconds > 0
    assert b.collect_seconds == 0.0, "files-only does no web collection"
    assert b.census_note_tokens >= 250 and b.census_input_tokens >= 6_000


def test_hybrid_splits_between_census_and_web():
    b = budgets(131_072, "hybrid")
    total = settings.research_max_minutes * 60.0
    assert b.census_seconds > 0 and b.collect_seconds > 0
    assert b.census_seconds + b.collect_seconds <= total + 1, "phases share one wall-clock"
    assert b.collect_seconds >= 90.0, "web gap-fill keeps a floor"


def test_census_budgets_monotonic_and_bounded():
    prev = None
    for ctx in _CTXS:
        b = budgets(ctx, "files")
        assert 250 <= b.census_note_tokens <= 800
        if prev:
            assert b.census_note_tokens >= prev
        prev = b.census_note_tokens


# --- Per-section deepening budgets ------------------------------------------


def test_deepen_disabled_below_32k():
    # A small deploy (below the 32k floor) gets the no-op: zero rounds ⇒ the
    # deepening stage is skipped and the pipeline stays byte-identical.
    b = budgets(16_384)
    assert b.deepen_rounds == 0
    # At and above the floor it is enabled.
    assert budgets(32_768).deepen_rounds >= 1


def test_deepen_fields_floors_and_ceilings():
    for ctx in _CTXS:
        b = budgets(ctx)
        assert 1 <= b.deepen_rounds <= 2
        assert 2 <= b.deepen_sections_hi <= 6
        assert 3 <= b.deepen_max_new_sources <= 5
        assert b.deepen_input_tokens >= 3_000
        assert b.deepen_seconds > 0


def test_deepen_seconds_bounded_and_monotonic_in_minutes():
    # The stage is a slice of the run budget, capped so a slow dig cannot eat the
    # writer's time; more minutes ⇒ no less deepening time, up to the cap.
    short, long = budgets(131_072, max_minutes=4.0), budgets(131_072, max_minutes=30.0)
    assert short.deepen_seconds <= long.deepen_seconds
    assert long.deepen_seconds <= 240.0


def test_deepen_rounds_monotonic_in_context():
    prev = 0
    for ctx in _CTXS:
        r = budgets(ctx).deepen_rounds
        assert r >= prev
        prev = r


def test_per_deepen_budget_is_a_tighter_cousin():
    b = budgets(131_072)
    wb, primary = b.per_deepen_budget(), b.per_subq_budget()
    assert not wb.decompose and wb.subqs == 1
    assert wb.rounds == 1 and wb.rounds <= primary.rounds
    assert wb.fetch_per_round <= primary.fetch_per_round
    assert wb.max_fetches >= 3
