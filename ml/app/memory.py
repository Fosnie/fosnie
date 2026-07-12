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

"""Memory vector store. One dense
collection per scope — `pai_mem_user_<uid>` / `pai_mem_proj_<pid>` — so large
memory can be relevance-ranked on recall instead of injected whole. Explicit-only
writes still go through Postgres (source of truth); this is the recall index."""

from __future__ import annotations

from qdrant_client import models

from . import embeddings
from .qdrant_store import client


def _name(scope_key: str) -> str:
    return f"pai_mem_{scope_key}"


async def ensure(scope_key: str) -> str:
    c = client()
    name = _name(scope_key)
    if not await c.collection_exists(name):
        dim = await embeddings.dimension()
        await c.create_collection(
            collection_name=name,
            vectors_config=models.VectorParams(size=dim, distance=models.Distance.COSINE),
        )
    return name


async def upsert(scope_key: str, fact_id: str, content: str) -> None:
    name = await ensure(scope_key)
    vec = (await embeddings.embed([content]))[0]
    await client().upsert(
        collection_name=name,
        points=[models.PointStruct(id=fact_id, vector=vec, payload={"fact_id": fact_id})],
    )


async def search(scope_key: str, query: str, limit: int = 10) -> list[str]:
    c = client()
    name = _name(scope_key)
    if not await c.collection_exists(name):
        return []
    qv = (await embeddings.embed([query]))[0]
    res = await c.query_points(collection_name=name, query=qv, limit=limit, with_payload=True)
    return [p.payload.get("fact_id") for p in res.points if p.payload]


async def delete(scope_key: str, fact_id: str) -> None:
    c = client()
    name = _name(scope_key)
    if await c.collection_exists(name):
        await c.delete(name, points_selector=models.PointIdsList(points=[fact_id]))
