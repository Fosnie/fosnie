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

"""Process-wide shared ``httpx.AsyncClient`` (optimisation audit, L1).

One pooled client is created at app startup and reused by every outbound
inference call (LLM, embeddings, reranker, model probe), so we pay TLS/TCP setup
once and reuse keep-alive connections instead of handshaking per call — the cost
otherwise multiplies across agentic-loop fan-out (decompose/grade/reformulate ×
rounds). Created and torn down by the FastAPI lifespan in ``main.py``.

The web fetcher keeps its own per-call client deliberately: connection reuse
there could bypass its per-hop SSRF re-validation, and it is low-volume.

Tests that exercise client code without the app can call ``set_client`` directly,
or rely on the lazy fallback in ``get_client``.
"""

import asyncio

import httpx

from .config import settings


def v1_url(base_url: str, path: str) -> str:
    """Join an OpenAI-style base to a ``/v1/<path>`` resource with exactly one
    ``/v1`` segment. Operators set the audio/STT base inconsistently — some with
    a trailing ``/v1`` (matching the chat/embeddings roles, e.g.
    ``https://api.openai.com/v1``), some without (local engines like kokoro). We
    normalise both so ``audio/speech`` never doubles to ``/v1/v1/...`` (a 404)."""
    base = base_url.rstrip("/")
    if base.endswith("/v1"):
        base = base[:-3].rstrip("/")
    return f"{base}/v1/{path.lstrip('/')}"

_client: httpx.AsyncClient | None = None
# Event loop the cached client belongs to. httpx pools keep-alive connections
# bound to the loop they were opened on; reusing a client across loops (e.g.
# the eval tests' asyncio.run-per-call style, or a post-shutdown straggler)
# raises "Event loop is closed". Re-audit R11.
_owner_loop: asyncio.AbstractEventLoop | None = None


def build_client() -> httpx.AsyncClient:
    """Construct the shared client with an explicit pool and split timeouts: a
    generous read for streaming generation, tighter connect/write/pool. Short,
    high-frequency calls (embeddings/rerank) override the read timeout per
    request."""
    return httpx.AsyncClient(
        limits=httpx.Limits(max_connections=200, max_keepalive_connections=50),
        timeout=httpx.Timeout(connect=5.0, read=settings.request_timeout, write=30.0, pool=5.0),
    )


def set_client(client: httpx.AsyncClient | None) -> None:
    global _client, _owner_loop
    _client = client
    _owner_loop = asyncio.get_running_loop() if client is not None else None


def get_client() -> httpx.AsyncClient:
    """The shared client. Lazily builds one if the lifespan has not run (e.g. a
    unit test importing a module directly), and rebuilds when the running event
    loop has changed — a client whose pooled connections belong to a dead loop
    is unusable (re-audit R11; the orphaned client is unclosed, acceptable in
    the test contexts that hit this path)."""
    global _client, _owner_loop
    loop = asyncio.get_running_loop()
    if _client is None or _owner_loop is not loop:
        _client = build_client()
        _owner_loop = loop
    return _client
