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
from .inference_fixed import routed_fixed, post
from .inference_target import strict


@routed_fixed("embed")
async def embed(texts: list[str]) -> list[list[float]]:
    return await _embed(texts, "embed")


async def _embed(texts, role):
    if not texts:
        return []
    base_url = cfg("embed_base_url", settings.embed_base_url)
    url = f"{base_url.rstrip('/')}/embeddings"
    headers = http_client.bearer_headers(cfg('embed_api_key', settings.embed_api_key))
    client = http_client.get_client()
    r = await post(
        role, client, url,
        json={"model": cfg("embed_model", settings.embed_model), "input": texts},
        headers=headers,
        timeout=settings.embed_timeout,
    )
    r.raise_for_status()
    data = r.json()["data"]
    target = cfg("_routing_fixed_target", None)
    if strict() and target.embedding_dimension is not None:
        _validate_vectors(data, len(texts), target.embedding_dimension)
    return [d["embedding"] for d in sorted(data, key=lambda x: x["index"])]


async def embed_with(texts: list[str], base_url: str | None, model: str | None, api_key: str | None) -> list[list[float]]:
    """Embed with an EXPLICIT config (not the request `cfg` override) — used by the
    dual-write path during a blue-green re-index to also embed with the NEW model."""
    if strict():
        desired_target()
        if base_url is None or model is None or api_key is None:
            raise ValueError("desired embedding index requires complete explicit fields")
        return await embed_desired(texts, explicit=(base_url,model,api_key))
    if not texts:
        return []
    base = (base_url or settings.embed_base_url).rstrip("/")
    headers = http_client.bearer_headers(api_key or settings.embed_api_key)
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


@routed_fixed("embed_desired")
async def embed_desired(texts, *, explicit=None):
    if not strict():
        raise ValueError("desired embedding assignment required")
    target = cfg("_routing_fixed_target", None)
    if explicit is not None and (explicit[0].rstrip("/") != target.base_url or explicit[1:] != (target.model,target.api_key)):
        raise ValueError("desired embedding fields conflict with assignment")
    return await _embed(texts,"embed_desired")


def desired_target():
    """Validate the pinned collection before any reindex/dual-write side effect."""
    from .inference_target import InferenceEnvelope
    envelope=InferenceEnvelope.model_validate(cfg("routing",None))
    target=envelope.fixed_assignments.get("embed_desired")
    if target is None or envelope.fixed_budget is None or "embed_desired" not in envelope.fixed_budget.allowances:
        raise ValueError("desired embedding index requires a separate routed assignment and grant")
    return target


def active_target(*, require_grant=False):
    from .inference_target import InferenceEnvelope
    envelope=InferenceEnvelope.model_validate(cfg("routing",None))
    target=envelope.fixed_assignments.get("embed")
    if target is None or target.active_index_revision is None:
        raise ValueError("active embedding index requires pinned provenance")
    if require_grant and (envelope.fixed_budget is None or "embed" not in envelope.fixed_budget.allowances):
        raise ValueError("active embedding grant required")
    return target


async def embed_one(text: str) -> list[float]:
    return (await embed([text]))[0]


async def dimension() -> int:
    return len(await embed_one("dimension probe"))


def _validate_vectors(data, count: int, dimension: int) -> None:
    """Reject reordered duplicates, missing inputs and unusable vectors before writes."""
    import math
    if not isinstance(data, list) or len(data) != count:
        raise ValueError("embedding output count differs from requested inputs")
    indexes = []
    for row in data:
        if not isinstance(row, dict) or type(row.get("index")) is not int:
            raise ValueError("embedding output requires an integer input index")
        indexes.append(row["index"])
        vector = row.get("embedding")
        if (not isinstance(vector, list) or len(vector) != dimension
                or any(type(v) not in (int, float) or not math.isfinite(v) for v in vector)):
            raise ValueError("embedding output differs from pinned index dimension or numeric values")
    if sorted(indexes) != list(range(count)):
        raise ValueError("embedding output indexes differ from requested inputs")
