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

"""Blocked-engine fallback search (Tier 2). When
SearXNG returns nothing (engines blocked/down), search DuckDuckGo's HTML
endpoint directly through the same SSRF-guarded, paced fetcher and parse the
results with lxml. A Playwright-driven search exists behind web_render_enabled
as the last resort. Strictly a fallback — slower and more fragile than the
primary path."""

import logging
from urllib.parse import parse_qs, quote_plus, unquote, urlsplit

from ..config import settings
from . import fetcher
from .pacing import pacer
from .provider import SerpResult

_log = logging.getLogger("pai.web.fallback")

ENGINE_KEY = "engine:ddg-html"
_COOLDOWN_S = 120.0  # DDG blocks aggressively; back off harder than SearXNG


def _unwrap_ddg(href: str) -> str | None:
    """DDG HTML results wrap targets as //duckduckgo.com/l/?uddg=<enc>&rut=…"""
    if not href:
        return None
    if href.startswith("//"):
        href = "https:" + href
    parts = urlsplit(href)
    if parts.netloc.endswith("duckduckgo.com") and parts.path.startswith("/l/"):
        uddg = parse_qs(parts.query).get("uddg", [])
        return unquote(uddg[0]) if uddg else None
    if parts.scheme in ("http", "https"):
        return href
    return None


def parse_ddg_html(html: str, limit: int) -> list[SerpResult]:
    """Pure parse of the DDG HTML SERP (unit-testable from a fixture)."""
    from lxml import html as lxml_html

    out: list[SerpResult] = []
    tree = lxml_html.fromstring(html)
    # Word-boundary class match — a bare contains() would also catch the
    # wrapper <div class="results">.
    for result in tree.xpath(
        "//div[contains(concat(' ', normalize-space(@class), ' '), ' result ')]"
    ):
        links = result.xpath(".//a[contains(@class,'result__a')]")
        if not links:
            continue
        url = _unwrap_ddg(links[0].get("href", ""))
        if not url:
            continue
        title = " ".join(links[0].text_content().split())
        snips = result.xpath(".//*[contains(@class,'result__snippet')]")
        snippet = " ".join(snips[0].text_content().split()) if snips else ""
        out.append(SerpResult(url=url, title=title, snippet=snippet, published_date=None, engine="ddg-html"))
        if len(out) >= limit:
            break
    return out


async def search(query: str, recency: str, limit: int) -> list[SerpResult]:
    """DDG HTML fallback. `recency` is unsupported on this surface (ignored).
    Returns [] on any failure."""
    if pacer.cooling(ENGINE_KEY):
        return []
    await pacer.acquire(ENGINE_KEY, settings.web_engine_rps, settings.web_pacing_burst)
    url = f"https://html.duckduckgo.com/html/?q={quote_plus(query)}"
    try:
        page = await fetcher.fetch_page(url)
        results = parse_ddg_html(page.body, limit)
    except Exception as e:  # noqa: BLE001 — fallback of the fallback is the snippet pool
        pacer.set_cooldown(ENGINE_KEY, _COOLDOWN_S)
        _log.warning("ddg-html fallback failed (%ss cooldown): %s", int(_COOLDOWN_S), e)
        return []
    if not results:
        pacer.set_cooldown(ENGINE_KEY, _COOLDOWN_S)
    return results


async def search_rendered(query: str, limit: int) -> list[SerpResult]:
    """Last-resort Playwright-driven DDG search. Only when web_render_enabled;
    same pacing budget as any render. Returns [] on any failure."""
    if not settings.web_render_enabled:
        return []
    url = f"https://duckduckgo.com/?q={quote_plus(query)}"
    try:
        page = await fetcher.render_page(url)
        from lxml import html as lxml_html

        tree = lxml_html.fromstring(page.body)
        out: list[SerpResult] = []
        for a in tree.xpath("//a[@data-testid='result-title-a']"):
            href = a.get("href", "")
            if not href.startswith("http"):
                continue
            out.append(
                SerpResult(
                    url=href,
                    title=" ".join(a.text_content().split()),
                    snippet="",
                    published_date=None,
                    engine="ddg-rendered",
                )
            )
            if len(out) >= limit:
                break
        return out
    except Exception as e:  # noqa: BLE001
        _log.warning("rendered fallback search failed: %s", e)
        return []
