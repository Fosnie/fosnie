"""Source-ACL retrieval deny-list (connector-kb-rag §2): a `deny_doc_ids` set on
the request adds a Qdrant `must_not doc_id` to every query filter, and drops a
denied document's parent block from the by-id parent fetch. Empty ⇒ no clause
(byte-identical). Fully monkeypatched — no real Qdrant."""

import asyncio

from app import qdrant_store


class _FakeClient:
    def __init__(self):
        self.last = None

    async def collection_exists(self, name):
        return True

    async def query_points(self, **kw):
        self.last = kw

        class _R:
            points = []

        return _R()


def _run(coro):
    return asyncio.run(coro)


def test_hybrid_search_no_must_not_when_deny_empty(monkeypatch):
    fake = _FakeClient()
    monkeypatch.setattr(qdrant_store, "client", lambda: fake)
    _run(qdrant_store.hybrid_search(["kb1"], [0.1, 0.2], {"indices": [], "values": []}, 5))
    # Both prefetch legs carry the KB `must` but no deny `must_not`.
    for leg in fake.last["prefetch"]:
        assert leg.filter.must_not is None


def test_hybrid_search_adds_must_not_when_deny_set(monkeypatch):
    fake = _FakeClient()
    monkeypatch.setattr(qdrant_store, "client", lambda: fake)
    token = qdrant_store.set_deny_docs(["dX", "dY"])
    try:
        _run(qdrant_store.hybrid_search(["kb1"], [0.1, 0.2], {"indices": [], "values": []}, 5))
    finally:
        qdrant_store.reset_deny_docs(token)
    for leg in fake.last["prefetch"]:
        mn = leg.filter.must_not
        assert mn is not None, "deny set ⇒ must_not present on every leg"
        assert mn[0].key == "doc_id"
        assert set(mn[0].match.any) == {"dX", "dY"}


def test_retrieve_parents_drops_denied_docs(monkeypatch):
    class _P:
        def __init__(self, pid, doc):
            self.payload = {"parent_id": pid, "doc_id": doc, "text": f"TEXT-{pid}"}

    class _FC:
        async def collection_exists(self, name):
            return True

        async def retrieve(self, collection_name, ids, with_payload):
            return [_P("P1", "dOK"), _P("P2", "dDENY")]

    monkeypatch.setattr(qdrant_store, "client", lambda: _FC())
    token = qdrant_store.set_deny_docs(["dDENY"])
    try:
        out = _run(qdrant_store.retrieve_parents(["P1", "P2"]))
    finally:
        qdrant_store.reset_deny_docs(token)
    assert "P1" in out, "an entitled doc's parent survives"
    assert "P2" not in out, "a denied doc's parent is filtered out of the by-id fetch"
