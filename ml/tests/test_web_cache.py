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

"""TTL caches: expiry, LRU cap, off-switch, and the SERP-cache wiring (repeat
query served from cache; empty result never cached)."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app.config import settings
from app.web import cache as cache_mod
from app.web import fallback_search, pipeline, provider
from app.web.cache import TTLCache
from app.web.provider import SerpResult


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def test_hit_miss_and_expiry(monkeypatch):
    c = TTLCache(ttl=10.0)
    now = [1000.0]
    monkeypatch.setattr(cache_mod.time, "monotonic", lambda: now[0])
    assert c.get("k") is None
    c.set("k", "v")
    assert c.get("k") == "v"
    now[0] += 9.9
    assert c.get("k") == "v"
    now[0] += 0.2  # past TTL
    assert c.get("k") is None


def test_lru_cap_evicts_oldest():
    c = TTLCache(ttl=100.0, max_entries=2)
    c.set("a", 1)
    c.set("b", 2)
    assert c.get("a") == 1  # touch a → b is now LRU
    c.set("c", 3)
    assert c.get("b") is None, "least-recently-used entry evicted"
    assert c.get("a") == 1 and c.get("c") == 3


def test_zero_ttl_disables():
    c = TTLCache(ttl=0.0)
    c.set("k", "v")
    assert c.get("k") is None


def test_serp_cache_serves_repeat_query(monkeypatch):
    calls = {"n": 0}

    class _P:
        @staticmethod
        async def search(query, recency, limit):
            calls["n"] += 1
            return [SerpResult("https://a.example/1", "A", "s", None, "e")]

    monkeypatch.setattr(provider, "get_provider", lambda: _P)
    pipeline._serp_cache.clear()
    monkeypatch.setattr(pipeline._serp_cache, "ttl", 600.0)
    r1 = _run(pipeline._serp("q", "any", 5))
    r2 = _run(pipeline._serp("q", "any", 5))
    assert calls["n"] == 1, "second identical query served from cache"
    assert [x.url for x in r1] == [x.url for x in r2]
    pipeline._serp_cache.clear()


def test_empty_serp_not_cached(monkeypatch):
    calls = {"n": 0}

    class _Empty:
        @staticmethod
        async def search(query, recency, limit):
            calls["n"] += 1
            return []

    async def no_fb(query, recency, limit):
        return []

    async def no_rend(query, limit):
        return []

    monkeypatch.setattr(provider, "get_provider", lambda: _Empty)
    monkeypatch.setattr(fallback_search, "search", no_fb)
    monkeypatch.setattr(fallback_search, "search_rendered", no_rend)
    pipeline._serp_cache.clear()
    monkeypatch.setattr(pipeline._serp_cache, "ttl", 600.0)
    _run(pipeline._serp("q2", "any", 5))
    _run(pipeline._serp("q2", "any", 5))
    assert calls["n"] == 2, "an outage (empty SERP) is never cached"
    pipeline._serp_cache.clear()
