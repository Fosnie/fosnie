# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json
from types import SimpleNamespace
from uuid import uuid4
import httpx
import pytest
from app import main, rag_ctx, http_client, qdrant_store, sparse, decompose
from app.inference_target import with_report
from test_inference_target import clean_context, anyio_backend, target
from test_inference_fixed import fixed

KB, DOC, DENIED = [str(uuid4()) for _ in range(3)]


def routing(timeout=5000):
    value = fixed("ocr", calls=1, timeout_ms=timeout, input_bytes=8*1024*1024, output_bytes=2*1024*1024)
    for role in ("embed", "rerank", "verify_claims"):
        extra = fixed(role, calls=16, timeout_ms=timeout, input_bytes=2*1024*1024, output_bytes=2*1024*1024)
        value["fixed_assignments"].update(extra["fixed_assignments"])
        value["fixed_budget"]["allowances"].update(extra["fixed_budget"]["allowances"])
    value["fixed_assignments"]["embed"].update(active_index_revision=7, embedding_dimension=2, embedding_collection="pinned-index")
    value.update(purpose="document", assignments={"document":target(name="document-model")},
        budget=dict(id=str(uuid4()),timeout_ms=timeout,allowances={"document":dict(max_calls=16,input_tokens=64000,output_tokens=1024)}))
    return value


class Qdrant:
    def __init__(self, kind="ok"):
        self.kind = kind
        self.calls = 0
    async def collection_exists(self, name):
        assert name == "pinned-index"
        return self.kind != "missing_index"
    async def query_points(self, **kw):
        self.calls += 1
        assert kw["collection_name"] == "pinned-index"
        for prefetch in kw["prefetch"]:
            assert KB in prefetch.filter.model_dump_json()
            assert DENIED in prefetch.filter.model_dump_json()
        if self.kind == "empty_evidence":
            return SimpleNamespace(points=[])
        return SimpleNamespace(points=[SimpleNamespace(score=0.9,payload={
            "knowledge_base_id": KB, "doc_id": DENIED if self.kind == "denied" else DOC,"chunk_text":"Right."})])


def response(request, kind="ok", repair=False):
    body = json.loads(request.content)
    path = request.url.path
    if path.endswith("embeddings"):
        return httpx.Response(200,json={"data":[{"index":0,"embedding":[1.0,2.0]}]})
    if path.endswith("rerank"):
        return httpx.Response(200,json={"results":[{"index":0,"relevance_score":0.9}]})
    if path.endswith("verify-claims"):
        v = {"verdict":"supported" if repair else "contradicted", "score":0.9}
        if kind == "bad_label":
            v["verdict"] = "invented"
        if kind == "bad_score":
            v["score"] = "0.9"
        return httpx.Response(200,json={"model":"wrong" if kind == "wrong_model" else "fixed-model",
            "verdicts":[] if kind == "short_verdicts" else [v]})
    assert body["model"] == "document-model"
    raw = "Right." if repair else '["Wrong."]'
    if kind == "bad_decompose":
        raw = "failed to classify"
    if kind == "many_claims":
        raw = json.dumps([str(i) for i in range(17)])
    if kind == "http":
        return httpx.Response(503)
    return httpx.Response(200,json={"choices":[{"message":{"content":raw}}],"usage":{"prompt_tokens":10,"completion_tokens":5}})


@pytest.fixture
def draft(tmp_path, monkeypatch):
    path = tmp_path / "draft.txt"
    path.write_text("Wrong.",encoding="utf-8")
    monkeypatch.setattr(main, "safe_path", lambda p:p)
    monkeypatch.setattr(sparse, "sparse_one", lambda _: {"indices":[1],"values":[1.0]})
    return path


@pytest.mark.anyio
async def test_real_endpoints_preserve_shared_lanes_and_new_citation(draft, monkeypatch):
    phase = [False]
    store = Qdrant()
    monkeypatch.setattr(qdrant_store, "client", lambda:store)
    monkeypatch.setattr(main.settings, "shared_secret", "")
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda req:response(req,repair=phase[0]))) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda:upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app),base_url="http://ml.test") as client:
            checked = await client.post("/verify-draft",json=dict(path=str(draft),kb_ids=[KB],deny_doc_ids=[DENIED],overrides={"routing":routing()}))
            assert checked.status_code == 200, checked.text
            value = checked.json()
            assert value["score"] == 0 and value["total"] == 1 and value["document_text"] == "Wrong."
            assert value["claims"][0]["source_span"]["text"] == "Wrong."
            phase[0] = True
            repaired = await client.post("/repair-draft",json=dict(claims=[dict(text="Wrong.",source_text="Wrong.",verdict="contradicted")],
                kb_ids=[KB],deny_doc_ids=[DENIED],overrides={"routing":routing()}))
            assert repaired.status_code == 200, repaired.text
            value2 = repaired.json()
    assert value2["results"][0]["action"] == "regenerated" and value2["results"][0]["replacement"] == "Right."
    assert store.calls == 2
    for result in (value,value2):
        assert result["routing_report"]["complete"] and result["fixed_routing_report"]["complete"]
        assert len(result["routing_report"]["calls"]) == 1
        assert {c["role"] for c in result["fixed_routing_report"]["calls"]} == {"embed","rerank","verify_claims"}


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["bad_decompose","many_claims","bad_label","bad_score","short_verdicts","wrong_model","http","denied","missing_index","oversize"])
async def test_failures_never_become_successful_verification(draft, monkeypatch, kind):
    if kind == "oversize":
        draft.write_text("x"*16001)
    monkeypatch.setattr(qdrant_store, "client", lambda:Qdrant(kind))
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda req:response(req,kind))) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda:upstream)
        with pytest.raises((ValueError,httpx.HTTPStatusError)):
            await main.verify_draft(main.VerifyDraftRequest(path=str(draft),kb_ids=[KB],deny_doc_ids=[DENIED],overrides={"routing":routing()}))


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["http","short_verdicts","bad_score","denied"])
async def test_repair_failure_is_not_a_deletion(draft, monkeypatch, kind):
    monkeypatch.setattr(qdrant_store, "client", lambda:Qdrant(kind))
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda req:response(req,kind,True))) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda:upstream)
        with pytest.raises((ValueError,httpx.HTTPStatusError)):
            await main.repair_draft(main.RepairDraftRequest(claims=[main.RepairClaimIn(text="Wrong.",source_text="Wrong.",verdict="contradicted")],
                kb_ids=[KB],deny_doc_ids=[DENIED],overrides={"routing":routing()}))


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["cancel","disconnect","deadline"])
async def test_cancellation_awaits_active_inference(draft, monkeypatch, kind):
    entered, stopped = asyncio.Event(), asyncio.Event()
    async def reply(request):
        entered.set()
        try:
            await asyncio.Future()
        finally:
            stopped.set()
    class Request:
        async def is_disconnected(self):
            return entered.is_set()
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda:upstream)
        req = main.VerifyDraftRequest(path=str(draft),kb_ids=[KB],deny_doc_ids=[DENIED],overrides={"routing":routing(50 if kind=="deadline" else 5000)})
        if kind == "cancel":
            task = asyncio.create_task(main.verify_draft(req))
            await entered.wait()
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
        elif kind == "disconnect":
            from fastapi import HTTPException
            with pytest.raises(HTTPException) as error:
                await main.verify_draft(req,Request())
            assert error.value.status_code == 499
        else:
            with pytest.raises(TimeoutError):
                await main.verify_draft(req)
    assert stopped.is_set()
