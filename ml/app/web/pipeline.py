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

"""Single-round web-search pipeline:

    query (+recency, depth budget) → SERP (provider, then fallback chain)
    → domain filter → candidate ranking (reranker on title+snippet)
    → fetch top-N (paced, SSRF-guarded, render escalation) → extract
    → chunk → rerank chunks vs query → assemble digest + citations.

The Phase-2 agentic loop (decompose/grade/reformulate rounds) lands later; the
shape here already matches it. The pipeline NEVER raises on empty/blocked
results — it degrades to snippet-only evidence ("beast mode" philosophy) so the
model always gets an honest digest back."""

import asyncio
import dataclasses
import logging
from dataclasses import dataclass
from datetime import datetime, timezone
from urllib.parse import urlsplit

from .. import chunker, reranker
from ..config import settings
from . import extractor, fallback_search, fetcher, provider
from .cache import TTLCache

_log = logging.getLogger("pai.web.pipeline")

# Phase-3 caches: repeated SERP queries / page fetches within the TTL are
# served from memory — faster, and kinder to the pacing budget. SERP entries
# are only stored when non-empty (an engine outage must not be cached).
_serp_cache = TTLCache(settings.web_serp_cache_ttl)
_page_cache = TTLCache(settings.web_page_cache_ttl)

# Chunks fed into the digest (mirrors retrieve.py's _MAX_CONTEXT_CHUNKS).
_MAX_DIGEST_CHUNKS = 10

# depth -> (SERP results considered, pages fetched).
_BUDGETS = {"quick": (5, 2), "standard": (10, 4), "deep": (15, 6)}


@dataclass
class _Source:
    """One web source feeding the digest (a fetched page or a bare snippet)."""

    url: str
    title: str
    domain: str
    published_date: str | None
    fetched_at: str
    snippet_only: bool
    chunks: list[str]


def _quote(text: str, max_words: int = 25) -> str:
    return " ".join(text.split()[:max_words])


def _domain(url: str) -> str:
    return (urlsplit(url).hostname or "").lower()


def _domain_allowed(url: str) -> bool:
    """Suffix-match against the allow/block lists. Per-request runtime overrides
    (the admin's `web_search.*` config rows, sent by the Rust backend) take
    precedence over the env defaults — a present-but-empty override means "list
    off", letting an admin clear an env-baked list. Fail-closed: allowlist-only
    mode with an empty allowlist blocks every domain."""
    from ..rag_ctx import cfg

    host = _domain(url)
    if not host:
        return False

    def _matches(csv: str) -> bool:
        for d in (x.strip().lower().lstrip(".") for x in csv.split(",")):
            if d and (host == d or host.endswith("." + d)):
                return True
        return False

    allowlist = cfg("web_domain_allowlist", settings.web_domain_allowlist)
    blocklist = cfg("web_domain_blocklist", settings.web_domain_blocklist)
    allowlist_only = cfg("web_allowlist_only", settings.web_allowlist_only)

    if blocklist and _matches(blocklist):
        return False
    if allowlist_only:
        return bool(allowlist) and _matches(allowlist)
    if allowlist:
        return _matches(allowlist)
    return True


async def _extract_pdf(raw: bytes) -> str:
    """Route fetched PDF bytes through the existing document extraction path
    (pypdf native text; OCR service for scanned). The
    extractor is suffix-driven, so the temp file must end `.pdf`."""
    import os
    import tempfile

    from .. import extract

    tmp = tempfile.NamedTemporaryFile(suffix=".pdf", delete=False)
    try:
        tmp.write(raw)
        tmp.close()
        pages = await extract.extract_pages_ocr(tmp.name, "application/pdf")
        return "\n\n".join(t for _, t in pages if t and t.strip())
    finally:
        try:
            os.unlink(tmp.name)
        except OSError:
            pass


async def _serp(query: str, recency: str, limit: int) -> list[provider.SerpResult]:
    """Primary provider, then the blocked-engine fallback chain. Non-empty
    results are TTL-cached (an outage is never cached)."""
    key = (settings.web_search_provider, query, recency, limit)
    cached = _serp_cache.get(key)
    if cached is not None:
        return list(cached)
    results = await provider.get_provider().search(query, recency, limit)
    if not results:
        results = await fallback_search.search(query, recency, limit)
    if not results:
        results = await fallback_search.search_rendered(query, limit)
    if results:
        _serp_cache.set(key, list(results))
    return results


async def _fetch_source(r: provider.SerpResult, sem: asyncio.Semaphore) -> _Source:
    """Fetch + extract one candidate; degrade to snippet-only on any failure.
    Successful extractions are TTL-cached by requested URL (the pool mutates
    sources, so cache hits return a copy)."""
    cached = _page_cache.get(r.url)
    if cached is not None:
        return dataclasses.replace(cached, chunks=list(cached.chunks))
    fetched_at = datetime.now(timezone.utc).isoformat(timespec="seconds")
    async with sem:
        try:
            from . import robots

            if not await robots.allowed(r.url):
                raise fetcher.FetchError(f"disallowed by robots.txt: {r.url}")
            page = await fetcher.fetch_page(r.url)
            if page.raw is not None and page.content_type.startswith("application/pdf"):
                text = await _extract_pdf(page.raw)
                if text:
                    chunks = chunker.chunk_text(text, settings.chunk_size, settings.chunk_overlap)
                    src = _Source(
                        url=page.final_url,
                        title=r.title or _domain(page.final_url),
                        domain=_domain(page.final_url),
                        published_date=r.published_date,
                        fetched_at=fetched_at,
                        snippet_only=False,
                        chunks=chunks,
                    )
                    _page_cache.set(r.url, dataclasses.replace(src, chunks=list(chunks)))
                    return src
                raise fetcher.FetchError(f"no text extracted from PDF {r.url}")
            ex = extractor.extract(page.body, page.final_url)
            if len(ex.text) < extractor.NEAR_EMPTY_CHARS and settings.web_render_enabled:
                try:
                    from . import progress

                    progress.emit("render", _domain(r.url))
                    rendered = await fetcher.render_page(r.url)
                    ex2 = extractor.extract(rendered.body, rendered.final_url)
                    if len(ex2.text) > len(ex.text):
                        ex, page = ex2, rendered
                except fetcher.FetchError as e:
                    _log.debug("render escalation failed for %s: %s", r.url, e)
            if ex.text:
                chunks = chunker.chunk_text(ex.text, settings.chunk_size, settings.chunk_overlap)
                src = _Source(
                    url=page.final_url,
                    title=ex.title or r.title or _domain(page.final_url),
                    domain=_domain(page.final_url),
                    published_date=ex.published_date or r.published_date,
                    fetched_at=fetched_at,
                    snippet_only=False,
                    chunks=chunks,
                )
                _page_cache.set(r.url, dataclasses.replace(src, chunks=list(chunks)))
                return src
        except Exception as e:  # noqa: BLE001 — snippet-only beats nothing
            _log.info("fetch failed for %s (snippet-only): %s", r.url, e)
    return _Source(
        url=r.url,
        title=r.title or _domain(r.url),
        domain=_domain(r.url),
        published_date=r.published_date,
        fetched_at=fetched_at,
        snippet_only=True,
        chunks=[r.snippet] if r.snippet else [],
    )


def _assemble(
    query: str,
    sources: list[_Source],
    picked: list[tuple[int, str]],
    notes: list[str] | None = None,
) -> dict:
    """Build the digest text + citation list from the picked (source_idx, chunk)
    pairs. Numbering is per source; the digest embeds the source list so the
    model can refer to [n] naturally. `notes` (conflict warnings, a beast-mode
    budget note) are surfaced above the sources when present."""
    used_idx: list[int] = []
    for idx, _ in picked:
        if idx not in used_idx:
            used_idx.append(idx)
    number = {src_idx: n + 1 for n, src_idx in enumerate(used_idx)}

    header_lines = []
    for src_idx in used_idx:
        s = sources[src_idx]
        date = f", published {s.published_date}" if s.published_date else ""
        note = " (search-result snippet only — page not fetched)" if s.snippet_only else ""
        header_lines.append(f"[{number[src_idx]}] {s.title} — {s.url}{date}{note}")

    blocks = [f"[{number[idx]}] {chunk}" for idx, chunk in picked]
    preamble = ("\n".join(notes) + "\n\n") if notes else ""
    digest = preamble + "Web sources:\n" + "\n".join(header_lines) + "\n\n" + "\n\n".join(blocks)

    citations = [
        {
            "url": sources[idx].url,
            "title": sources[idx].title or None,
            "domain": sources[idx].domain,
            "published_date": sources[idx].published_date,
            "fetched_at": sources[idx].fetched_at,
            "quote_text": _quote(chunk),
            "snippet_only": sources[idx].snippet_only,
        }
        for idx, chunk in picked
    ]
    return {"digest": digest, "citations": citations}


async def web_search(query: str, recency: str = "any", depth: str = "standard") -> dict:
    """Entry point (unchanged signature; main.py imports this). Dispatches every
    depth into the agentic loop: quick = single round,
    standard = decompose + bounded rounds, deep = wider/deeper budget. Local
    import breaks the loop↔pipeline cycle (loop reuses these primitives)."""
    from . import loop

    return await loop.run(query, recency or "any", depth or "standard")


async def _single_round(query: str, recency: str = "any", depth: str = "standard") -> dict:
    """Phase-1 single-round pipeline, retained as the primitive the loop's quick
    path and tests exercise directly."""
    top_results, fetch_top_n = _BUDGETS.get((depth or "standard").lower(), _BUDGETS["standard"])
    top_results = min(top_results, settings.web_top_results)
    fetch_top_n = min(fetch_top_n, settings.web_fetch_top_n)

    serp = await _serp(query, recency or "any", top_results)
    serp = [r for r in serp if _domain_allowed(r.url)]
    # De-dup by URL, keep SERP order.
    seen: set[str] = set()
    serp = [r for r in serp if not (r.url in seen or seen.add(r.url))]
    if not serp:
        return {
            "digest": "No web results were found for this query (search engines may be "
            "unavailable). Tell the user web search returned nothing rather than guessing.",
            "citations": [],
        }

    # Rank candidates by relevance of title+snippet to the query; reranker
    # degradation (all-equal scores) preserves SERP order via stable sort.
    cand_texts = [f"{r.title} — {r.snippet}" for r in serp]
    scores = await reranker.rerank(query, cand_texts)
    ranked = [r for r, _ in sorted(zip(serp, scores), key=lambda x: x[1], reverse=True)]

    # Fetch the top-N concurrently (paced per host inside the fetcher); the
    # remaining candidates contribute snippet-only evidence.
    sem = asyncio.Semaphore(max(1, settings.web_fetch_concurrency))
    fetched = await asyncio.gather(*[_fetch_source(r, sem) for r in ranked[:fetch_top_n]])
    snippet_sources = [
        _Source(
            url=r.url,
            title=r.title or _domain(r.url),
            domain=_domain(r.url),
            published_date=r.published_date,
            fetched_at=datetime.now(timezone.utc).isoformat(timespec="seconds"),
            snippet_only=True,
            chunks=[r.snippet] if r.snippet else [],
        )
        for r in ranked[fetch_top_n:]
    ]
    sources = list(fetched) + snippet_sources

    # Rerank every chunk (fetched + snippets) against the query; stable sort
    # keeps fetched-first order when the reranker is degraded.
    indexed: list[tuple[int, str]] = [
        (idx, chunk) for idx, s in enumerate(sources) for chunk in s.chunks if chunk.strip()
    ]
    if not indexed:
        return {
            "digest": "Web results were found but no usable content could be read from them.",
            "citations": [],
        }
    chunk_scores = await reranker.rerank(query, [c for _, c in indexed])
    picked = [
        pair
        for pair, _ in sorted(zip(indexed, chunk_scores), key=lambda x: x[1], reverse=True)
    ][:_MAX_DIGEST_CHUNKS]

    return _assemble(query, sources, picked)
