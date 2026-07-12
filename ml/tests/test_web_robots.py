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

"""Robots policy: user_triggered (default) never fetches robots.txt; respect
honours per-host rules, caches per host, and fails open on fetch errors."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import rag_ctx
from app.web import fetcher, robots
from app.web.fetcher import FetchResult


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _reset():
    robots._robots_cache.clear()
    rag_ctx.set_overrides({})


_ROBOTS_BLOCK_PAI = "User-agent: PAIPlatform\nDisallow: /private/\n\nUser-agent: *\nAllow: /\n"
_ROBOTS_BLOCK_ALL = "User-agent: *\nDisallow: /\n"


def test_user_triggered_never_fetches_robots(monkeypatch):
    _reset()
    calls = {"n": 0}

    async def spy_fetch(url):
        calls["n"] += 1
        return FetchResult(final_url=url, status=200, body="", content_type="text/plain")

    monkeypatch.setattr(fetcher, "fetch_page", spy_fetch)
    assert _run(robots.allowed("https://example.com/anything"))
    assert calls["n"] == 0, "default policy makes zero robots fetches"


def test_respect_blocks_disallowed_path(monkeypatch):
    _reset()
    rag_ctx.set_overrides({"web_robots_policy": "respect"})

    async def fake_fetch(url):
        assert url.endswith("/robots.txt")
        return FetchResult(final_url=url, status=200, body=_ROBOTS_BLOCK_PAI, content_type="text/plain")

    monkeypatch.setattr(fetcher, "fetch_page", fake_fetch)
    assert not _run(robots.allowed("https://example.com/private/doc")), "agent-specific disallow honoured"
    assert _run(robots.allowed("https://example.com/public/doc")), "other paths allowed"
    _reset()


def test_respect_blocks_all_when_disallow_all(monkeypatch):
    _reset()
    rag_ctx.set_overrides({"web_robots_policy": "respect"})

    async def fake_fetch(url):
        return FetchResult(final_url=url, status=200, body=_ROBOTS_BLOCK_ALL, content_type="text/plain")

    monkeypatch.setattr(fetcher, "fetch_page", fake_fetch)
    assert not _run(robots.allowed("https://example.com/x"))
    _reset()


def test_respect_fails_open_on_fetch_error(monkeypatch):
    _reset()
    rag_ctx.set_overrides({"web_robots_policy": "respect"})

    async def broken_fetch(url):
        raise fetcher.FetchError("HTTP 500")

    monkeypatch.setattr(fetcher, "fetch_page", broken_fetch)
    assert _run(robots.allowed("https://example.com/x")), "robots is politeness, not security"
    _reset()


def test_robots_cached_per_host(monkeypatch):
    _reset()
    rag_ctx.set_overrides({"web_robots_policy": "respect"})
    calls = {"n": 0}

    async def counting_fetch(url):
        calls["n"] += 1
        return FetchResult(final_url=url, status=200, body=_ROBOTS_BLOCK_PAI, content_type="text/plain")

    monkeypatch.setattr(fetcher, "fetch_page", counting_fetch)
    _run(robots.allowed("https://example.com/a"))
    _run(robots.allowed("https://example.com/b"))
    _run(robots.allowed("https://example.com/c"))
    assert calls["n"] == 1, "one robots fetch per host within the TTL"
    _reset()


def test_agent_token():
    assert robots._agent_token() == "PAIPlatform"
