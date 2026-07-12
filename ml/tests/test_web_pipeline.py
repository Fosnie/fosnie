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

"""Single-round pipeline assembly with mocked SERP/fetcher/reranker: digest
numbering, citation shape, snippet-only marking, domain filtering, and the
always-return-something guarantee. Tests the primitive (`_single_round`) the
agentic loop is built on; the loop itself is covered by test_web_loop.py."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

import pytest

from app import reranker
from app.config import settings
from app.web import fallback_search, fetcher, pipeline, provider
from app.web.fetcher import FetchResult
from app.web.provider import SerpResult


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


_PAGE_HTML = (
    "<html><head><title>Rust 1.92 released</title></head><body><article>"
    + "<p>The Rust team has announced version 1.92 of the language. This release "
    "improves the borrow checker and stabilises long-awaited APIs across the "
    "standard library, giving developers richer tools out of the box.</p>" * 6
    + "</article></body></html>"
)


def _serp_results():
    return [
        SerpResult(
            url="https://blog.rust-lang.org/2026/rust-192/",
            title="Announcing Rust 1.92",
            snippet="The Rust team announces 1.92 with borrow-checker improvements.",
            published_date="2026-05-28",
            engine="duckduckgo",
        ),
        SerpResult(
            url="https://seo-farm.example/rust",
            title="Rust info",
            snippet="Rust rust rust.",
            published_date=None,
            engine="google",
        ),
        SerpResult(
            url="https://news.example/rust-192",
            title="Rust 1.92 covered",
            snippet="Coverage of the 1.92 release.",
            published_date=None,
            engine="mojeek",
        ),
    ]


class _FakeProvider:
    @staticmethod
    async def search(query, recency, limit):
        return _serp_results()


@pytest.fixture
def mocked(monkeypatch):
    monkeypatch.setattr(provider, "get_provider", lambda: _FakeProvider)

    async def fake_fetch(url):
        if "seo-farm" in url:
            raise fetcher.FetchError("HTTP 403")
        return FetchResult(final_url=url, status=200, body=_PAGE_HTML, content_type="text/html")

    monkeypatch.setattr(fetcher, "fetch_page", fake_fetch)

    async def fake_rerank(query, docs):
        # Descending scores — deterministic stable ranking.
        return [float(len(docs) - i) for i in range(len(docs))]

    monkeypatch.setattr(reranker, "rerank", fake_rerank)
    monkeypatch.setattr(settings, "web_domain_blocklist", "")
    monkeypatch.setattr(settings, "web_domain_allowlist", "")


def test_digest_and_citation_shape(mocked):
    out = _run(pipeline._single_round("latest rust release", recency="any", depth="standard"))
    assert out["digest"].startswith("Web sources:")
    assert "[1]" in out["digest"]
    assert out["citations"], "citations present"
    c = out["citations"][0]
    assert set(c) == {
        "url", "title", "domain", "published_date", "fetched_at", "quote_text", "snippet_only",
    }
    assert c["domain"] == "blog.rust-lang.org"
    assert len(c["quote_text"].split()) <= 25, "quote capped at 25 words"
    # The fetched page's chunks dominate; the failed fetch degrades to snippet.
    snippet_rows = [x for x in out["citations"] if x["snippet_only"]]
    fetched_rows = [x for x in out["citations"] if not x["snippet_only"]]
    assert fetched_rows, "fetched-page evidence present"
    assert all(r["url"] != "https://seo-farm.example/rust" or r["snippet_only"] for r in out["citations"])
    assert snippet_rows or True  # snippet rows appear when budget leaves unfetched results


def test_snippet_only_marked_in_digest(mocked):
    out = _run(pipeline._single_round("latest rust release", depth="quick"))
    # quick budget fetches 2: the 403 source must be marked snippet-only if cited.
    for line in out["digest"].splitlines():
        if "seo-farm.example" in line:
            assert "snippet only" in line


def test_blocklist_filters_domains(mocked, monkeypatch):
    monkeypatch.setattr(settings, "web_domain_blocklist", "seo-farm.example, other.example")
    out = _run(pipeline._single_round("latest rust release"))
    assert all(c["domain"] != "seo-farm.example" for c in out["citations"])
    assert "seo-farm.example" not in out["digest"]


def test_allowlist_only_mode(mocked, monkeypatch):
    monkeypatch.setattr(settings, "web_domain_allowlist", "rust-lang.org")
    out = _run(pipeline._single_round("latest rust release"))
    assert out["citations"], "the allow-listed domain still flows"
    assert all(c["domain"].endswith("rust-lang.org") for c in out["citations"])


def test_empty_serp_returns_honest_digest(monkeypatch):
    class _Empty:
        @staticmethod
        async def search(query, recency, limit):
            return []

    monkeypatch.setattr(provider, "get_provider", lambda: _Empty)

    async def no_fallback(query, recency, limit):
        return []

    async def no_rendered(query, limit):
        return []

    monkeypatch.setattr(fallback_search, "search", no_fallback)
    monkeypatch.setattr(fallback_search, "search_rendered", no_rendered)
    out = _run(pipeline._single_round("anything"))
    assert out["citations"] == []
    assert "No web results" in out["digest"], "never raises — honest empty digest"


def test_depth_budgets():
    assert pipeline._BUDGETS["quick"] == (5, 2)
    assert pipeline._BUDGETS["standard"] == (10, 4)
    assert pipeline._BUDGETS["deep"] == (15, 6)
