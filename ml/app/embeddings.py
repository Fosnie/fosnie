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

"""Dense embeddings over an OpenAI-shape /v1/embeddings endpoint. Dev: Ollama
bge-m3 (1024-dim). Prod: Qwen3-Embedding service. Order is preserved by index."""

from . import http_client
from .config import settings
from .rag_ctx import cfg


async def embed(texts: list[str]) -> list[list[float]]:
    if not texts:
        return []
    base_url = cfg("embed_base_url", settings.embed_base_url)
    url = f"{base_url.rstrip('/')}/embeddings"
    headers = {"Authorization": f"Bearer {cfg('embed_api_key', settings.embed_api_key)}"}
    client = http_client.get_client()
    r = await client.post(
        url,
        json={"model": cfg("embed_model", settings.embed_model), "input": texts},
        headers=headers,
        timeout=settings.embed_timeout,
    )
    r.raise_for_status()
    data = r.json()["data"]
    return [d["embedding"] for d in sorted(data, key=lambda x: x["index"])]


async def embed_with(texts: list[str], base_url: str | None, model: str | None, api_key: str | None) -> list[list[float]]:
    """Embed with an EXPLICIT config (not the request `cfg` override) — used by the
    dual-write path during a blue-green re-index to also embed with the NEW model."""
    if not texts:
        return []
    base = (base_url or settings.embed_base_url).rstrip("/")
    headers = {"Authorization": f"Bearer {api_key or settings.embed_api_key}"}
    client = http_client.get_client()
    r = await client.post(
        f"{base}/embeddings",
        json={"model": model or settings.embed_model, "input": texts},
        headers=headers,
        timeout=settings.embed_timeout,
    )
    r.raise_for_status()
    data = r.json()["data"]
    return [d["embedding"] for d in sorted(data, key=lambda x: x["index"])]


async def embed_one(text: str) -> list[float]:
    return (await embed([text]))[0]


async def dimension() -> int:
    return len(await embed_one("dimension probe"))
