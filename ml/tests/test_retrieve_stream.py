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

"""`/retrieve?stream=true` emits NDJSON progress
events while the agentic loop runs, then exactly one terminal `done` carrying the
same context + citations the non-streaming `retrieve()` returns. Back-pressure
drops excess progress but never the terminal event; the emitter is a no-op when
none is installed (the non-stream path costs nothing). Backends are stubbed — no
network."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import embeddings, llm, qdrant_store, reranker
from app import retrieve as retrieve_mod
from app import retrieve_stream


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _hits(_kb, _qd, _qs, _k):
    return [
        {
            "payload": {
                "doc_id": "d1", "chunk_index": 0, "page_number": 1,
                "clause_section_ref": "1", "chunk_text": "the term is 12 months",
            }
        }
    ]


def _mock_backends(monkeypatch, grade: str = "yes"):
    async def fake_llm(system, user, max_tokens=256):
        return '[{"subq": "q", "queries": ["q"]}]' if "Decompose" in system else grade

    async def fake_embed(texts):
        return [[0.1, 0.2] for _ in texts]

    async def fake_hybrid(kb_ids, qd, qs, k):
        return _hits(kb_ids, qd, qs, k)

    async def fake_rerank(query, texts):
        return [1.0 for _ in texts]

    monkeypatch.setattr(llm, "complete", fake_llm)
    monkeypatch.setattr(embeddings, "embed", fake_embed)
    monkeypatch.setattr(retrieve_mod.sparse, "sparse_one", lambda t: {"indices": [], "values": []})
    monkeypatch.setattr(qdrant_store, "hybrid_search", fake_hybrid)
    monkeypatch.setattr(reranker, "rerank", fake_rerank)


async def _collect(prompt="q", kb_ids=("kb1",)):
    events = []
    async for e in retrieve_stream.stream_events(prompt, list(kb_ids)):
        events.append(e)
    return events


def test_stream_event_sequence(monkeypatch):
    _mock_backends(monkeypatch)
    events = _run(_collect())
    types = [e["type"] for e in events]
    assert types[-1] == "done", "terminal done event"
    assert types.count("done") == 1
    stages = [e["stage"] for e in events if e["type"] == "progress"]
    assert "decompose" in stages
    assert "search" in stages
    assert "assemble" in stages
    done = events[-1]
    assert "12 months" in done["context"]
    assert done["citations"] and done["citations"][0]["doc_id"] == "d1"


def test_done_matches_non_stream(monkeypatch):
    _mock_backends(monkeypatch)
    streamed = _run(_collect())[-1]
    direct = _run(retrieve_mod.retrieve("q", ["kb1"]))  # no emitter installed → plain
    assert streamed["context"] == direct["context"]
    assert streamed["citations"] == direct["citations"]


def test_stream_error_terminal(monkeypatch):
    _mock_backends(monkeypatch)

    async def boom(prompt, kb_ids, deny_doc_ids=None):
        raise RuntimeError("retrieve exploded")

    monkeypatch.setattr(retrieve_mod, "retrieve", boom)
    events = _run(_collect())
    assert events[-1]["type"] == "error"
    assert "retrieve exploded" in events[-1]["message"]


def test_queue_full_drops_progress_but_done_survives(monkeypatch):
    _mock_backends(monkeypatch)

    async def flooding(prompt, kb_ids, deny_doc_ids=None):
        for i in range(1000):  # far past the queue bound, consumer not draining yet
            retrieve_mod.emit("search", f"flood {i}")
        return {"context": "ctx", "citations": []}

    monkeypatch.setattr(retrieve_mod, "retrieve", flooding)
    events = _run(_collect())
    n_progress = sum(1 for e in events if e["type"] == "progress")
    assert n_progress < 1000, "excess progress dropped, retrieval never blocked"
    assert events[-1]["type"] == "done", "the terminal event is never dropped"


def test_emit_noop_without_emitter():
    retrieve_mod.set_emitter(None)
    retrieve_mod.emit("search", "nothing listens")  # must not raise


def test_fail_closed_empty_allowlist_streams_done(monkeypatch):
    _mock_backends(monkeypatch)
    events = _run(_collect(kb_ids=[]))  # empty allow-list ⇒ zero retrieval
    assert events[-1] == {"type": "done", "context": "", "citations": []}
