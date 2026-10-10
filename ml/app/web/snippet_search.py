# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Explicit snippet-only quick search, with existing SERP/domain/egress guards."""
import asyncio
from datetime import datetime, timezone

from ..config import settings
from ..rag_ctx import cfg
from . import pipeline, progress, quick_evidence


async def run(query: str, recency: str) -> dict:
    progress.emit('search', 'searching public snippets')
    try:
        results = await asyncio.wait_for(pipeline._serp(query, recency, 5), timeout=settings.web_wall_clock_quick)
    except TimeoutError:
        results = []
    sources = []
    seen = set()
    for result in results:
        if result.url in seen or not result.snippet.strip() or not pipeline._domain_allowed(result.url):
            continue
        seen.add(result.url)
        sources.append(pipeline._Source(url=result.url, title=result.title, domain=pipeline._domain(result.url),
            published_date=result.published_date, fetched_at=datetime.now(timezone.utc).isoformat(),
            snippet_only=True, chunks=[result.snippet]))
    if not sources:
        return {'digest': 'Web search returned no usable snippets. Do not guess current facts.', 'citations': []}
    focus = quick_evidence.ranking_query(query, cfg('web_ranking_question', None))
    pairs, _ = quick_evidence.shortlist(focus, [(i, source.chunks[0]) for i, source in enumerate(sources)])
    return pipeline._assemble(query, sources, pairs[:quick_evidence.MAX_DIGEST_CHUNKS], notes=[
        'Snippet-only quick search: partial search-engine excerpts, no pages fetched. '
        'Check dates and distinguish current observations from forecasts; do not infer exhaustive coverage.'])
