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


class PublicationRefused(ValueError):
    """A proven pre-swap refusal, distinct from an uncertain storage failure."""


async def corpus_snapshot(collection, source_kbs=None, source_docs=None):
    import hashlib, json
    from . import storage_writes
    from .inference_budget import BudgetExhausted
    rows = []
    size = 0
    # One coherent snapshot; the lock is released before any model call.
    async with storage_writes.fenced():
        async for batch in qdrant_store.scroll_all(collection):
            for point in batch:
                payload = dict(point.payload or {})
                if source_kbs is not None and (payload.get('knowledge_base_id') not in source_kbs or payload.get('doc_id') not in source_docs):
                    raise ValueError("rebuild source is outside the authorised corpus")
                row = {"id": str(point.id), "payload": payload}
                size += len(json.dumps(row, ensure_ascii=False).encode())
                if len(rows) >= 32768 or size > 64 * 1024 * 1024:
                    raise BudgetExhausted("rebuild corpus cap exceeded")
                rows.append(row)
        rows.sort(key=lambda row: row['id'])
        digest = hashlib.sha256(json.dumps(rows, sort_keys=True, ensure_ascii=False).encode()).hexdigest()
    return rows, digest


async def reindex_stream(new_dim: int, new_model: str, source_kbs=None, source_docs=None) -> AsyncIterator[dict[str, Any]]:
    from .inference_target import strict
    from .inference_budget import BudgetExhausted
    from . import storage_writes
    target = embeddings.desired_target() if strict() else None
    if target is not None and (new_dim != target.embedding_dimension or new_model != target.model):
        raise ValueError("reindex request differs from desired assignment")
    new = target.embedding_collection if target is not None else collection_name(new_dim, new_model)
    async with storage_writes.rebuild_guard(new, required=target is not None):
        old = await qdrant_store.alias_target(qdrant_store.COLLECTION, strict=target is not None)
        if target is not None:
            active = embeddings.active_target()
            if old != active.embedding_collection or old == new:
                raise ValueError("reindex source/target differs from pinned index provenance")
            if source_kbs is None or source_docs is None:
                raise ValueError("rebuild authorised corpus required")
        rows, digest = await corpus_snapshot(old, source_kbs if target is not None else None, source_docs)
        if target is not None:
            budget = _cfg('_routing_fixed_budget', None)
            allowance = budget.grant.allowances.get('embed_desired') if budget else None
            if allowance is None or (len(rows) + _EMBED_BATCH - 1) // _EMBED_BATCH > allowance.max_calls:
                raise BudgetExhausted("rebuild requires more calls than its grant")
            async with storage_writes.fenced():
                aliases = await qdrant_store.client().get_aliases()
                if any(alias.collection_name == new for alias in aliases.aliases):
                    raise ValueError("desired target is already serving an alias")
                if await qdrant_store.alias_target(qdrant_store.COLLECTION, strict=True) != old:
                    raise ValueError("rebuild source alias changed")
                if await qdrant_store.client().collection_exists(new):
                    await qdrant_store.client().delete_collection(new)
                await qdrant_store.create_kb_collection(new, new_dim)
        else:
            await qdrant_store.create_kb_collection(new, new_dim)
        total = len(rows)
        yield {"type": "start", "total": total, "new_collection": new, "old_collection": old}
        done = 0
        for start in range(0, total, _EMBED_BATCH):
            batch = rows[start:start + _EMBED_BATCH]
            texts = [contextual.augment(row['payload'].get('context', ''), row['payload'].get('chunk_text', '')) for row in batch]
            dense = await (embeddings.embed_desired(texts) if target is not None else embeddings.embed(texts))
            sp = sparse.sparse_embed(texts)
            await qdrant_store.upsert_named(new, batch, dense, sp)
            done += len(batch)
            yield {"type": "progress", "done": done, "total": total}
        async with storage_writes.fenced():
            _, after = await corpus_snapshot(old, source_kbs if target is not None else None, source_docs)
            if after != digest or await qdrant_store.alias_target(qdrant_store.COLLECTION, strict=True) != old:
                raise ValueError("rebuild source changed; publication stopped")
            if await qdrant_store.count(new) != total:
                raise ValueError("rebuild target count differs from snapshot")
        yield {"type": "built", "new_collection": new, "count": total, "old_collection": old, "source_sha256": digest}


async def swap(new_collection: str, old_collection: str | None, expected_old_collection: str | None = None,
               expected_source_sha256: str | None = None, publication_authorised: bool = True) -> dict[str, Any]:
    """Validate under the write fence and retain model-free lost-reply recovery."""
    from . import storage_writes
    async with storage_writes.rebuild_guard(new_collection, required=expected_source_sha256 is not None):
        async with storage_writes.fenced():
            current = await qdrant_store.alias_target(qdrant_store.COLLECTION, strict=True)
            if current == new_collection:
                # The switch already happened. Later writes/ACL changes must not
                # prevent SQL provenance recovery of that confirmed publication.
                return {"ok": True, "active_collection": new_collection, "old_collection_retained": True}
            expected = expected_old_collection or old_collection
            if expected is not None and current != expected:
                raise ValueError("embedding alias differs from captured publication source")
            if not publication_authorised:
                raise PublicationRefused("publication authority changed")
            if expected_source_sha256 is not None:
                if not await qdrant_store.client().collection_exists(new_collection):
                    raise PublicationRefused("rebuilt target disappeared before publication")
                if not re.fullmatch(r'[0-9a-f]{64}', expected_source_sha256):
                    raise PublicationRefused("invalid rebuild source receipt")
                _, source_digest = await corpus_snapshot(current)
                _, target_digest = await corpus_snapshot(new_collection)
                if source_digest != expected_source_sha256 or target_digest != expected_source_sha256:
                    raise PublicationRefused("rebuild corpus changed before publication")
            if not await qdrant_store.client().collection_exists(new_collection):
                raise ValueError("embedding publication target is unavailable")
            await qdrant_store.swap_alias(qdrant_store.COLLECTION, new_collection)
            await qdrant_store.drop_collection(qdrant_store.NOTES)
            return {"ok": True, "active_collection": new_collection, "old_collection_retained": True}


# Re-export so callers can set the embed overrides for the build context.
__all__ = ["collection_name", "reindex_stream", "swap", "_cfg"]


async def cleanup_retired(collection: str,expected_active: str):
    from . import storage_writes
    async with storage_writes.fenced():
        active=await qdrant_store.alias_target(qdrant_store.COLLECTION,strict=True)
        if active!=expected_active or collection in (active,qdrant_store.COLLECTION,qdrant_store.PARENTS,qdrant_store.NOTES,qdrant_store.TOC):
            raise ValueError("cleanup target is active or protected")
        aliases=await qdrant_store.client().get_aliases()
        if any(a.collection_name==collection for a in aliases.aliases):raise ValueError("cleanup target is referenced by an alias")
        if await qdrant_store.client().collection_exists(collection):await qdrant_store.client().delete_collection(collection)
        return {"deleted":True}
