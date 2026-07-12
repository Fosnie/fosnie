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

"""SearXNG SERP client (the v1 primary provider). Talks to the self-hosted
instance's JSON API — `GET {base}/search?q=…&format=json` — which requires
`search.formats: [json]` in the instance settings (backend/deploy/searxng/).
Paced via the `engine:searxng` bucket; an HTTP 429 or an empty result set puts
the engine on cooldown so the fallback path takes over."""

import logging

from .. import http_client
from ..config import settings
from .pacing import pacer
from .provider import SerpResult

_log = logging.getLogger("pai.web.searxng")

ENGINE_KEY = "engine:searxng"
_COOLDOWN_S = 60.0

# Tool-surface recency -> SearXNG time_range. "any" (or unknown) omits the param.
_TIME_RANGE = {"day": "day", "week": "week", "month": "month", "year": "year"}


def parse_results(data: dict, limit: int) -> list[SerpResult]:
    """Pure parse of the SearXNG JSON body (unit-tested without a network)."""
    out: list[SerpResult] = []
    for item in data.get("results", [])[: max(limit, 0)]:
        url = (item.get("url") or "").strip()
        if not url:
            continue
        published = item.get("publishedDate") or None
        if isinstance(published, str):
            published = published.strip()[:10] or None  # ISO date part only
        else:
            published = None
        out.append(
            SerpResult(
                url=url,
                title=(item.get("title") or "").strip(),
                snippet=(item.get("content") or "").strip(),
                published_date=published,
                engine=str(item.get("engine") or "searxng"),
            )
        )
    return out


async def search(query: str, recency: str, limit: int) -> list[SerpResult]:
    """One SERP call. Returns [] on any failure (the pipeline falls back)."""
    if pacer.cooling(ENGINE_KEY):
        return []
    await pacer.acquire(ENGINE_KEY, settings.web_engine_rps, settings.web_pacing_burst)

    params: dict[str, str] = {"q": query, "format": "json", "pageno": "1"}
    tr = _TIME_RANGE.get((recency or "any").lower())
    if tr:
        params["time_range"] = tr

    url = f"{settings.searxng_base_url.rstrip('/')}/search"
    try:
        client = http_client.get_client()
        r = await client.get(url, params=params, timeout=settings.web_fetch_timeout)
        if r.status_code == 429:
            pacer.set_cooldown(ENGINE_KEY, _COOLDOWN_S)
            _log.warning("searxng rate-limited us (429); cooling down")
            return []
        r.raise_for_status()
        results = parse_results(r.json(), limit)
    except Exception as e:  # noqa: BLE001 — degrade to the fallback, never break the turn
        pacer.set_cooldown(ENGINE_KEY, _COOLDOWN_S)
        _log.warning("searxng unavailable (%ss cooldown): %s", int(_COOLDOWN_S), e)
        return []

    if not results:
        # Blank SERP usually means the upstream engines are blocking the
        # instance — back off so we don't burn its reputation further.
        pacer.set_cooldown(ENGINE_KEY, _COOLDOWN_S)
    return results
