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

"""Blue-green embedding re-index. Re-embeds every chunk of
the live KB collection into a NEW collection using a NEW embedding model, then the
caller atomically swaps the `pai_kb` alias. Re-embedding reconstructs the embedded
text from the payload (`augment(context, chunk_text)`) — no source files needed.
The live collection serves search the whole time; only the final alias swap flips
queries to the new index."""

import re
from collections.abc import AsyncIterator
from typing import Any

from . import contextual, embeddings, qdrant_store, sparse
from .rag_ctx import cfg as _cfg

_EMBED_BATCH = 64


def collection_name(dim: int, model: str) -> str:
    """`pai_kb__<dim>__<model-slug>` — the alias-backed target name."""
    slug = re.sub(r"[^a-z0-9]+", "_", (model or "model").lower()).strip("_") or "model"
    return f"pai_kb__{dim}__{slug}"


async def reindex_stream(new_dim: int, new_model: str) -> AsyncIterator[dict[str, Any]]:
    """Build the new collection from the live one, yielding progress events:
    `{type:"start", total, new_collection, old_collection}`,
    `{type:"progress", done, total}`, then `{type:"built", new_collection, count}`.
    The embed overrides (new model/url/key) must already be set on the request
    context (`rag_ctx.set_overrides`) so `embeddings.embed` uses the NEW model.
    Does NOT swap the alias — the caller does that once the build is verified."""
    old = await qdrant_store.alias_target(qdrant_store.COLLECTION)
    new = collection_name(new_dim, new_model)
    await qdrant_store.create_kb_collection(new, new_dim)
    total = await qdrant_store.count(old)
    yield {"type": "start", "total": total, "new_collection": new, "old_collection": old}

    done = 0
    async for batch in qdrant_store.scroll_all(old):
        rows: list[dict] = []
        texts: list[str] = []
        for p in batch:
            payload = dict(p.payload or {})
            # The text originally embedded was the (optional) context blurb + the
            # verbatim chunk — reconstruct it so vectors are comparable across models.
            texts.append(contextual.augment(payload.get("context", ""), payload.get("chunk_text", "")))
            rows.append({"id": p.id, "payload": payload})
        if not rows:
            continue
        dense: list[list[float]] = []
        for i in range(0, len(texts), _EMBED_BATCH):
            dense.extend(await embeddings.embed(texts[i : i + _EMBED_BATCH]))
        sp = sparse.sparse_embed(texts)
        await qdrant_store.upsert_named(new, rows, dense, sp)
        done += len(rows)
        yield {"type": "progress", "done": done, "total": total}

    count = await qdrant_store.count(new)
    yield {"type": "built", "new_collection": new, "count": count, "old_collection": old}


async def swap(new_collection: str, old_collection: str | None) -> dict[str, Any]:
    """Atomically point the `pai_kb` alias at `new_collection`, drop the Deep
    Research notes cache (rebuilds), and drop the superseded old collection (unless
    it was the literal `pai_kb`, which `swap_alias` already removed in the one-time
    literal→alias transition). Parents (`pai_kb_parents`) are dim-agnostic — kept."""
    await qdrant_store.swap_alias(qdrant_store.COLLECTION, new_collection)
    await qdrant_store.drop_collection(qdrant_store.NOTES)
    if old_collection and old_collection != qdrant_store.COLLECTION and old_collection != new_collection:
        await qdrant_store.drop_collection(old_collection)
    return {"ok": True, "active_collection": new_collection}


# Re-export so callers can set the embed overrides for the build context.
__all__ = ["collection_name", "reindex_stream", "swap", "_cfg"]
