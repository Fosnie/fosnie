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

"""Pool tier priority. Cross-referenced operative-text chunks
draw from a reserve ON TOP of the budget and are pooled BEFORE generic uncited, so a fetched
statutory section can never be evicted ("followed but not pooled"). Pure function, no network."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import retrieve as retrieve_mod
from app.config import settings


def _mk(text: str, sec: str | None = None, doc: str = "d") -> dict:
    return {"chunk_text": text, "doc_id": doc, "chunk_index": 0, "clause_section_ref": sec}


def _sa(i: int, status: str = "ok", n_cited: int = 3, n_uncited: int = 6) -> dict:
    cited = [_mk(f"s{i}-cited-{k}") for k in range(n_cited)]
    uncited = [_mk(f"s{i}-uncited-{k}") for k in range(n_uncited)]
    return {
        "status": status, "cited": cited, "ranked": cited + uncited,
        "subq": f"q{i}", "scope": "", "answer": "a", "best_rerank": 0.5, "retried": False,
    }


def test_every_subq_keeps_a_crossref_chunk_under_budget_pressure():
    # 6 sub-Qs whose cited+uncited alone (6×6=36) fill pool_budget — the exact condition that
    # used to squeeze crossref out. Each sub-Q also has 2 cross-ref chunks.
    n = 6
    sub_results = [_sa(i) for i in range(n)]
    crossref_lists = [[_mk(f"s{i}-xref-{k}", sec=f"{560 + i}{k}") for k in range(2)] for i in range(n)]
    pool, contrib, cross_used = retrieve_mod._assemble_pool(sub_results, crossref_lists, [], [])

    texts = {p["chunk_text"] for p in pool}
    # Non-eviction invariant: every sub-Q has ≥1 of its cross-ref chunks in the pool.
    for i in range(n):
        assert any(f"s{i}-xref-{k}" in texts for k in range(2)), f"sub-Q {i} lost its cross-ref chunks"
    # All 12 distinct cross-ref chunks pooled (reserve default = 12), on top of the budget.
    assert cross_used == 12
    # Budget items (cited+uncited) present too; total ≤ pool_budget + reserve.
    assert len(pool) <= 36 + settings.pool_crossref_reserve
    assert len(pool) > 36, "cross-ref reserve added chunks ON TOP of the full budget"


def test_deterministic_section_survives_full_budget():
    # §2b invariant: a deterministically-required section (crossref/TOC/anchor — all feed the
    # reserve tier) reaches the pool even when THIS sub-question's cited+uncited fill its whole
    # budget. It must not be evicted by generic uncited chunks.
    sa = _sa(0, n_cited=3, n_uncited=12)
    crossref_lists = [[_mk("s549 Exercise by directors of power to allot", sec="549")]]
    pool, contrib, cross_used = retrieve_mod._assemble_pool([sa], crossref_lists, [], [])
    texts = {p["chunk_text"] for p in pool}
    assert "s549 Exercise by directors of power to allot" in texts, "deterministic section evicted by budget"
    assert cross_used == 1


def test_crossref_stamps_subq_for_attribution():
    sub_results = [_sa(0), _sa(1)]
    crossref_lists = [[_mk("s0-xref", sec="570")], [_mk("s1-xref", sec="443A")]]
    pool, contrib, cross_used = retrieve_mod._assemble_pool(sub_results, crossref_lists, [], [])
    xref = {p["chunk_text"]: p for p in pool if "xref" in p["chunk_text"]}
    assert xref["s0-xref"]["_subq"] == 0 and xref["s1-xref"]["_subq"] == 1
    assert cross_used == 2


def test_failed_subq_ranked_capped_at_uncited_cap():
    # A FAILED sub-Q's ranked is treated as uncited but capped at uncited_cap (was per_budget).
    failed = _sa(0, status="failed", n_cited=0, n_uncited=10)
    failed["cited"] = []
    pool, contrib, cross_used = retrieve_mod._assemble_pool([failed], [[]], [], [])
    assert contrib[0] == settings.pool_uncited_per_subq  # 3, not the full per_budget (6)


def test_reserve_bounds_crossref():
    # More cross-ref chunks than the reserve → only `reserve` of them pool.
    over = settings.pool_crossref_reserve + 5
    sub_results = [_sa(0)]
    crossref_lists = [[_mk(f"x{k}", sec=str(600 + k)) for k in range(over)]]
    pool, contrib, cross_used = retrieve_mod._assemble_pool(sub_results, crossref_lists, [], [])
    assert cross_used == settings.pool_crossref_reserve
