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

"""NDJSON progress streaming: event sequence shape, terminal done/error, the
drop-on-full back-pressure policy, and the no-op default emitter."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import llm, reranker
from app.config import settings
from app.web import fetcher, loop, progress, provider, stream
from app.web.fetcher import FetchResult
from app.web.provider import SerpResult


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _page(body: str) -> str:
    return (
        "<html><head><title>T</title></head><body><article>"
        + f"<p>{body}</p>" * 6
        + "</article></body></html>"
    )


def _mock_stack(monkeypatch, grade: str = "yes"):
    plan = '[{"subq": "q", "queries": ["q"], "freshness": "any"}]'

    async def fake_llm(system, user, max_tokens=0):
        if "Decompose" in system:
            return plan
        if "yes, partial, or no" in system:
            return grade
        if "disagree" in system:
            return '{"conflict": false}'
        return "q narrowed"

    monkeypatch.setattr(llm, "complete", fake_llm)

    async def fake_serp(query, recency, limit):
        return [SerpResult("https://a.example/1", "A", "snippet", None, "e")]

    monkeypatch.setattr(loop, "_serp", fake_serp)

    async def fake_fetch(url):
        return FetchResult(final_url=url, status=200, body=_page("Body content here."), content_type="text/html")

    monkeypatch.setattr(fetcher, "fetch_page", fake_fetch)

    async def fake_rerank(query, docs):
        return [float(len(docs) - i) for i in range(len(docs))]

    monkeypatch.setattr(reranker, "rerank", fake_rerank)
    monkeypatch.setattr(settings, "web_host_rps", 1e6)
    monkeypatch.setattr(settings, "web_engine_rps", 1e6)
    monkeypatch.setattr(settings, "web_pacing_burst", 1e6)
    monkeypatch.setattr(settings, "web_domain_blocklist", "")
    monkeypatch.setattr(settings, "web_domain_allowlist", "")


async def _collect(query="q", depth="standard"):
    events = []
    async for e in stream.stream_events(query, "any", depth):
        events.append(e)
    return events


def test_stream_event_sequence(monkeypatch):
    _mock_stack(monkeypatch)
    events = _run(_collect())
    types = [e["type"] for e in events]
    assert types[-1] == "done", "terminal done event"
    assert types.count("done") == 1
    progress_stages = [e["stage"] for e in events if e["type"] == "progress"]
    assert "plan" in progress_stages
    assert "serp" in progress_stages
    assert "fetch" in progress_stages
    assert "assemble" in progress_stages
    done = events[-1]
    assert done["digest"].startswith("Web sources:")
    assert done["citations"]


def test_stream_error_terminal(monkeypatch):
    _mock_stack(monkeypatch)

    async def boom(query, recency="any", depth="standard"):
        raise RuntimeError("loop exploded")

    monkeypatch.setattr(loop, "run", boom)
    events = _run(_collect())
    assert events[-1]["type"] == "error"
    assert "loop exploded" in events[-1]["message"]


def test_queue_full_drops_progress_but_done_survives(monkeypatch):
    _mock_stack(monkeypatch)

    async def flooding_run(query, recency="any", depth="standard"):
        for i in range(1000):  # far past the queue bound, consumer not draining yet
            progress.emit("serp", f"flood {i}")
        return {"digest": "Web sources:\n[1] t — u\n\n[1] c", "citations": []}

    monkeypatch.setattr(loop, "run", flooding_run)
    events = _run(_collect())
    n_progress = sum(1 for e in events if e["type"] == "progress")
    assert n_progress < 1000, "excess progress events dropped, loop never blocked"
    assert events[-1]["type"] == "done", "the terminal event is never dropped"


def test_emit_noop_without_emitter():
    progress.set_emitter(None)
    progress.emit("serp", "nothing listens")  # must not raise


def test_non_stream_path_unchanged(monkeypatch):
    _mock_stack(monkeypatch)
    from app.web import pipeline

    out = _run(pipeline.web_search("q", recency="any", depth="quick"))
    assert set(out) == {"digest", "citations"}, "plain dict, no event wrapping"
