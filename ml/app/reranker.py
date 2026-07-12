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

"""Reranker behind a swappable interface, using the standard `/v1/rerank` API.
Dev: llama.cpp server serving the Qwen3-Reranker GGUF (`--reranking`). Prod:
the Qwen3-Reranker service (Infinity) by base-URL — no re-embedding to swap
(08). Degrades gracefully to hybrid-fusion order if the reranker is down,
then self-heals: a failure starts a short cooldown, after which it retries."""

import asyncio
import logging
import time

import httpx

from . import http_client
from .config import settings

_log = logging.getLogger("pai.reranker")
_RETRY_AFTER = 30.0  # seconds to skip the reranker after a failure, then retry
_down_until = 0.0
_warned = False

# Dedicated concurrency gate for /v1/rerank, INDEPENDENT of
# retrieve_concurrency: a multi-part prompt fans ~6 sub-Q × 3 variants ≈ 18 rerank
# calls out at once and a rate-limited/free reranker 429s under that burst. Bound to
# the running loop — an asyncio primitive can't cross loops (same reasoning as the
# shared httpx client in http_client.py; the eval harness runs asyncio.run per call).
_sem: asyncio.Semaphore | None = None
_sem_loop: asyncio.AbstractEventLoop | None = None
_sem_size = 0


def _semaphore() -> asyncio.Semaphore:
    global _sem, _sem_loop, _sem_size
    from .rag_ctx import cfg

    loop = asyncio.get_running_loop()
    size = max(1, cfg("rerank_concurrency", settings.rerank_concurrency))
    if _sem is None or _sem_loop is not loop or _sem_size != size:
        _sem = asyncio.Semaphore(size)
        _sem_loop = loop
        _sem_size = size
    return _sem


def _retryable(e: Exception) -> bool:
    """A 429/5xx/timeout/transport hiccup is worth a retry; a 4xx (other than 429)
    or a malformed response is not (it won't fix itself)."""
    if isinstance(e, httpx.HTTPStatusError):
        code = e.response.status_code
        return code == 429 or 500 <= code < 600
    return isinstance(e, (httpx.TimeoutException, httpx.TransportError))


def degraded_now() -> bool:
    """True when the reranker is currently unavailable — disabled by config, or in the
    post-failure cooldown window. The retrieval pipeline reads this
    right after a `rerank` call to decide whether to keep the hybrid-fusion order instead
    of trusting a flat fallback score. Cheap and side-effect-free."""
    from .rag_ctx import cfg

    if not cfg("rerank_enabled", settings.rerank_enabled):
        return True
    return time.monotonic() < _down_until


async def rerank(query: str, docs: list[str]) -> list[float]:
    """Relevance score per doc (higher = better), aligned to `docs`. On failure returns
    equal (0.0) scores so the caller keeps the hybrid-fusion order, and trips a cooldown
    (surfaced via `degraded_now`) so a sustained outage doesn't hammer the endpoint or
    disable reranking for good. Retries a 429/5xx/timeout with short capped exponential
    backoff, throttled by a dedicated rerank semaphore so a
    multi-part prompt's ~18-call burst can't self-inflict the rate limit."""
    global _down_until, _warned
    from .rag_ctx import cfg

    if not cfg("rerank_enabled", settings.rerank_enabled) or not docs:
        return [0.0] * len(docs)
    if time.monotonic() < _down_until:
        return [0.0] * len(docs)  # cooling down after a recent failure

    url = f"{cfg('rerank_base_url', settings.rerank_base_url).rstrip('/')}/v1/rerank"
    payload = {"model": cfg("rerank_model", settings.rerank_model), "query": query, "documents": docs}
    headers = {"Authorization": f"Bearer {cfg('rerank_api_key', settings.rerank_api_key)}"}
    retries = max(1, cfg("rerank_max_retries", settings.rerank_max_retries))
    base = max(0.0, cfg("rerank_backoff_base", settings.rerank_backoff_base))
    last_err: Exception | None = None
    async with _semaphore():
        for attempt in range(retries):
            try:
                client = http_client.get_client()
                r = await client.post(url, json=payload, headers=headers, timeout=settings.rerank_timeout)
                r.raise_for_status()
                results = r.json()["results"]
                scores = [0.0] * len(docs)
                for item in results:
                    idx = item["index"]
                    scores[idx] = float(item.get("relevance_score", item.get("score", 0.0)))
                if _warned:
                    _log.info("reranker recovered")
                    _warned = False
                return scores
            except Exception as e:  # noqa: BLE001 — degrade, never break retrieval
                last_err = e
                if not _retryable(e) or attempt + 1 >= retries:
                    break
                # Exponential backoff, capped so the fan-out tail can't blow the ≤120s budget.
                await asyncio.sleep(min(base * (2 ** attempt), 4.0))
    _down_until = time.monotonic() + _RETRY_AFTER
    if not _warned:
        _log.warning(
            "reranker unavailable (%ss cooldown), falling back to hybrid order: %s",
            int(_RETRY_AFTER),
            last_err,
        )
        _warned = True
    return [0.0] * len(docs)
