# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""One bounded search, fetched evidence only, inside the Research request grant."""
import asyncio
from datetime import datetime, timezone
from urllib.parse import urlsplit

from .. import rag_ctx, reranker
from ..web import pipeline, provider, ssrf

MAX_PAGES = 4


async def collect(question):
    # Neither corpus text nor model-generated instructions become search queries.
    budget = rag_ctx.cfg("_routing_budget", None)
    async with asyncio.timeout(budget.remaining()):
        results = await provider.get_provider().search(question, "any", 8)
        if len(results) > 8:
            raise ValueError("research search exceeded its result limit")
        candidates, seen = [], set()
        for result in results:
            if not pipeline._domain_allowed(result.url) or result.url in seen:
                continue
            ssrf.validate_url(result.url)
            if any(len(s.encode()) > limit for s, limit in ((result.url, 4096), (result.title, 512), (result.snippet, 4096))):
                raise ValueError("research search metadata exceeds limit")
            seen.add(result.url)
            candidates.append(result)
        if not candidates:
            raise ValueError("no allowed research search results")
        scores = await reranker.rerank(question, [f"{r.title}\n{r.snippet}" for r in candidates])
        if len(scores) != len(candidates):
            raise ValueError("incomplete research ranking")
        chosen = [r for r, _ in sorted(zip(candidates, scores), key=lambda x: x[1], reverse=True)][:MAX_PAGES]
        # Sequential collection bounds concurrent extraction and owns cancellation.
        sources = []
        for result in chosen:
            with rag_ctx.scoped_overrides({"bounded_research_web": True}):
                source = await pipeline._fetch_source(result, asyncio.Semaphore(1))
            if source.snippet_only or not pipeline._domain_allowed(source.url):
                raise ValueError("research requires fetched allowed evidence")
            ssrf.validate_url(source.url)
            text = "\n\n".join(source.chunks)
            if not text.strip() or len(text.encode()) > 16000:
                raise ValueError("research web source exceeds text limit")
            if source.url in {s["url"] for s in sources}:
                raise ValueError("research pages redirected to duplicate evidence")
            if len(source.url.encode()) > 4096 or len((source.title or "").encode()) > 512:
                raise ValueError("research source metadata exceeds limit")
            sources.append(dict(url=source.url, title=source.title or None,
                domain=urlsplit(source.url).hostname.lower(), published_date=None,
                fetched_at=datetime.now(timezone.utc).isoformat(), snippet_only=False,
                quote_text=" ".join(text.split()[:25]), text=text))
        return sources
