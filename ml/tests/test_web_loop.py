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

"""Agentic loop: planning, rounds, grading/reformulation, diminishing-returns
and budget stops, conflict-triggered verification, beast-mode assembly. SERP /
fetch / LLM / reranker are all mocked — no network, deterministic."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

import pytest

from app import llm, reranker
from app.config import settings
from app.web import fetcher, loop, pipeline, provider
from app.web.fetcher import FetchResult
from app.web.provider import SerpResult


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _page(title: str, body: str) -> str:
    para = f"<p>{body}</p>" * 6
    return f"<html><head><title>{title}</title></head><body><article>{para}</article></body></html>"


class _LLM:
    """Records prompts and answers plan/grade/reformulate/conflict by keyword."""

    def __init__(self, plan_json: str, grade: str = "yes", conflict: str = '{"conflict": false}'):
        self.plan_json = plan_json
        self.grade = grade
        self.conflict = conflict
        self.calls: list[tuple[str, str]] = []

    async def complete(self, system: str, user: str, max_tokens: int = 0) -> str:
        self.calls.append((system, user))
        if "Decompose" in system:
            return self.plan_json
        if "yes, partial, or no" in system:
            return self.grade
        if "disagree" in system:
            return self.conflict
        if "NARROWER" in system:
            return user.replace("Sub-question: ", "") + " narrowed"
        return ""

    def kinds(self) -> list[str]:
        out = []
        for system, _ in self.calls:
            if "Decompose" in system:
                out.append("plan")
            elif "yes, partial, or no" in system:
                out.append("grade")
            elif "disagree" in system:
                out.append("conflict")
            elif "NARROWER" in system:
                out.append("reformulate")
        return out


@pytest.fixture
def fast(monkeypatch):
    # Make pacing instant and reranker deterministic-descending.
    monkeypatch.setattr(settings, "web_host_rps", 1e6)
    monkeypatch.setattr(settings, "web_engine_rps", 1e6)
    monkeypatch.setattr(settings, "web_pacing_burst", 1e6)

    async def fake_rerank(query, docs):
        return [float(len(docs) - i) for i in range(len(docs))]

    monkeypatch.setattr(reranker, "rerank", fake_rerank)
    monkeypatch.setattr(settings, "web_domain_blocklist", "")
    monkeypatch.setattr(settings, "web_domain_allowlist", "")


def _mock_serp(monkeypatch, by_query):
    """by_query: callable(query)->list[SerpResult]."""
    async def fake_serp(query, recency, limit):
        return by_query(query)[:limit]

    monkeypatch.setattr(loop, "_serp", fake_serp)


def _mock_fetch(monkeypatch, body_for):
    async def fake_fetch(url):
        body = body_for(url)
        if body is None:
            raise fetcher.FetchError("HTTP 403")
        return FetchResult(final_url=url, status=200, body=body, content_type="text/html")

    monkeypatch.setattr(fetcher, "fetch_page", fake_fetch)


def test_quick_makes_zero_llm_calls(fast, monkeypatch):
    spy = _LLM("[]")
    monkeypatch.setattr(llm, "complete", spy.complete)
    _mock_serp(monkeypatch, lambda q: [
        SerpResult("https://a.example/1", "A", "snippet about rust", None, "duckduckgo"),
    ])
    _mock_fetch(monkeypatch, lambda u: _page("A", "Rust 1.92 is the latest stable release this month."))
    out = _run(loop.run("latest rust release", "any", "quick"))
    assert spy.kinds() == [], "quick path makes no plan/grade/reformulate calls"
    assert out["digest"].startswith("Web sources:")
    assert out["citations"]


def test_standard_decomposes_and_grades(fast, monkeypatch):
    plan = '[{"subq": "rust release", "queries": ["rust release", "rust latest"], "freshness": "month"}]'
    spy = _LLM(plan, grade="yes")
    monkeypatch.setattr(llm, "complete", spy.complete)
    _mock_serp(monkeypatch, lambda q: [
        SerpResult("https://a.example/1", "A", "rust release notes", "2026-05-28", "ddg"),
        SerpResult("https://b.example/2", "B", "rust blog", None, "mojeek"),
    ])
    _mock_fetch(monkeypatch, lambda u: _page("T", "Rust 1.92 shipped with borrow-checker work and new APIs."))
    out = _run(loop.run("what is the latest rust release", "any", "standard"))
    kinds = spy.kinds()
    assert "plan" in kinds and "grade" in kinds
    assert out["citations"]


def test_reformulate_on_partial_then_stop(fast, monkeypatch):
    plan = '[{"subq": "q", "queries": ["q"], "freshness": "any"}]'
    spy = _LLM(plan, grade="partial")
    monkeypatch.setattr(llm, "complete", spy.complete)
    # Round 1 and round 2 both return fresh URLs so the loop runs both rounds.
    pages = {
        "q": [SerpResult("https://a.example/1", "A", "s1", None, "e")],
        "q narrowed": [SerpResult("https://b.example/2", "B", "s2", None, "e")],
    }
    _mock_serp(monkeypatch, lambda query: pages.get(query, []))
    _mock_fetch(monkeypatch, lambda u: _page("T", "Content body that is long enough to chunk well here."))
    out = _run(loop.run("q", "any", "standard"))
    assert "reformulate" in spy.kinds(), "partial grade triggers a reformulation"
    assert out["citations"]


def test_diminishing_returns_stops_early(fast, monkeypatch):
    plan = '[{"subq": "q", "queries": ["q"], "freshness": "any"}]'
    spy = _LLM(plan, grade="no")  # never satisfied → only the stop logic ends it
    monkeypatch.setattr(llm, "complete", spy.complete)
    # Every round returns the SAME single URL → 0% unseen after round 1.
    _mock_serp(monkeypatch, lambda q: [SerpResult("https://a.example/1", "A", "s", None, "e")])
    _mock_fetch(monkeypatch, lambda u: _page("T", "Body text long enough to make a chunk."))
    out = _run(loop.run("q", "any", "standard"))
    # Only one fetch happened (the single URL); round 2 saw no new URLs and stopped.
    assert sum(1 for c in out["citations"] if not c["snippet_only"]) <= 1


def test_budget_exhaustion_beast_mode(fast, monkeypatch):
    plan = '[{"subq": "q", "queries": ["q"], "freshness": "any"}]'
    spy = _LLM(plan, grade="no")
    monkeypatch.setattr(llm, "complete", spy.complete)
    monkeypatch.setattr(settings, "web_wall_clock_standard", 0.0)  # immediate deadline
    _mock_serp(monkeypatch, lambda q: [SerpResult("https://a.example/1", "A", "snippet", None, "e")])
    _mock_fetch(monkeypatch, lambda u: _page("T", "Body."))
    out = _run(loop.run("q", "any", "standard"))
    assert "best-effort" in out["digest"], "beast-mode note present on budget exhaustion"


def test_conflict_triggers_verification_round(fast, monkeypatch):
    plan = '[{"subq": "q", "queries": ["q"], "freshness": "any"}]'
    spy = _LLM(plan, grade="yes", conflict='{"conflict": true, "topic": "the value"}')
    monkeypatch.setattr(llm, "complete", spy.complete)
    serp_queries: list[str] = []

    def by_query(query):
        serp_queries.append(query)
        return [
            SerpResult("https://a.example/1", "A", "s1", None, "e"),
            SerpResult("https://b.example/2", "B", "s2", None, "e"),
        ]

    _mock_serp(monkeypatch, by_query)
    # Distinct bodies so syndication dedup keeps both → ≥2 evidence sources, which
    # is what arms the conflict check.
    bodies = {
        "https://a.example/1": _page("A", "The value is widely reported to be ten according to source A."),
        "https://b.example/2": _page("B", "Source B insists the value is actually twenty, contradicting others."),
    }
    _mock_fetch(monkeypatch, lambda u: bodies[u])
    out = _run(loop.run("q", "any", "standard"))
    assert "conflict" in spy.kinds()
    assert "⚠ Sources disagree" in out["digest"]
    assert any("the value" in q for q in serp_queries), "a verification SERP used the topic"


def test_grade_prompt_carries_dates(fast, monkeypatch):
    plan = '[{"subq": "q", "queries": ["q"], "freshness": "any"}]'
    spy = _LLM(plan, grade="yes")
    monkeypatch.setattr(llm, "complete", spy.complete)
    _mock_serp(monkeypatch, lambda q: [SerpResult("https://a.example/1", "A", "s", "2026-05-01", "e")])
    _mock_fetch(monkeypatch, lambda u: _page("T", "Body content with enough words to chunk."))
    _run(loop.run("q", "any", "standard"))
    grade_user = [u for s, u in spy.calls if "yes, partial, or no" in s]
    assert grade_user and "[a.example, 2026-05-01]" in grade_user[0], "evidence carries domain + date"


def test_plan_parse_fallback_to_single_subq(fast, monkeypatch):
    spy = _LLM("not json at all")  # planner returns garbage
    monkeypatch.setattr(llm, "complete", spy.complete)
    _mock_serp(monkeypatch, lambda q: [SerpResult("https://a.example/1", "A", "s", None, "e")])
    _mock_fetch(monkeypatch, lambda u: _page("T", "Body content to chunk for the digest."))
    out = _run(loop.run("only question", "any", "standard"))
    assert out["citations"], "garbage plan degrades to a single-subq run, still answers"


def test_collect_merges_into_shared_pool(fast, monkeypatch):
    spy = _LLM("[]")
    monkeypatch.setattr(llm, "complete", spy.complete)
    serp_map = {
        "q1": [SerpResult("https://a.example/1", "A", "s1", None, "e")],
        "q2": [
            SerpResult("https://a.example/1", "A", "s1", None, "e"),  # already seen
            SerpResult("https://b.example/2", "B", "s2", None, "e"),
        ],
    }
    _mock_serp(monkeypatch, lambda q: serp_map.get(q, []))
    bodies = {
        "https://a.example/1": _page("A", "Alpha body content with plenty of words to chunk."),
        "https://b.example/2": _page("B", "Beta body, distinct content entirely about other things."),
    }
    _mock_fetch(monkeypatch, lambda u: bodies[u])

    async def scenario():
        pool = loop._Pool()
        seen: set[str] = set()
        b = loop._budget("quick")
        r1 = await loop.collect("q1", "any", b, pool=pool, seen=seen)
        r2 = await loop.collect("q2", "any", b, pool=pool, seen=seen)
        assert r1.pool is pool and r2.pool is pool
        urls = [s.url for s in pool.sources]
        assert urls.count("https://a.example/1") == 1, "shared pool merges by URL"
        assert "https://b.example/2" in urls
        fetched = [s for s in pool.sources if not s.snippet_only]
        assert len(fetched) == 2, "the seen set prevented a repeat fetch of a.example"

    _run(scenario())


def test_budget_table():
    assert loop._budget("quick").rounds == 1 and not loop._budget("quick").decompose
    assert loop._budget("standard").decompose and loop._budget("standard").rounds == 2
    assert loop._budget("deep").rounds == settings.web_deep_rounds
    assert loop._budget("deep").max_fetches == 28
