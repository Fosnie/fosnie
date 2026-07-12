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

"""BM25 sparse vectors via fastembed (CPU, no GPU/service). Gives the sparse
half of Qdrant's native dense+BM25 hybrid. Model downloads once on first use."""

from functools import lru_cache

from fastembed import SparseTextEmbedding


@lru_cache(maxsize=1)
def _model() -> SparseTextEmbedding:
    return SparseTextEmbedding(model_name="Qdrant/bm25")


def sparse_embed(texts: list[str]) -> list[dict]:
    """Returns [{indices: [int], values: [float]}] aligned to `texts`."""
    out: list[dict] = []
    for emb in _model().embed(texts):
        out.append({"indices": emb.indices.tolist(), "values": emb.values.tolist()})
    return out


def sparse_one(text: str) -> dict:
    return sparse_embed([text])[0]
