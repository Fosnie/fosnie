# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json
from uuid import uuid4

import httpx
import pytest
from app import llm, rag_ctx, http_client
from app.inference_target import with_report, report_events, records
from app.inference_budget import BudgetExhausted
from test_inference_target import target, envelope, clean_context, anyio_backend


def grant(calls=1, timeout_ms=1000, input_tokens=2048, output_tokens=512):
    value = envelope(target(), "tool_loop")
    value["budget"] = dict(id=str(uuid4()), timeout_ms=timeout_ms, allowances={
        "tool_loop": dict(max_calls=calls, input_tokens=input_tokens, output_tokens=output_tokens)})
    return value


def response(request):
    return httpx.Response(200, json={"choices": [{"message": {"content": "ok"}}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2}})


@pytest.mark.anyio
async def test_r26_parallel_helpers_share_one_grant_and_unused_slots_cannot_reopen(monkeypatch):
    seen = []
    async def respond(request):
        seen.append(request)
        await asyncio.sleep(0)
        return response(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client, "get_client", lambda:client)
        rag_ctx.set_overrides({"routing":grant(calls=2)})
        results = await asyncio.gather(*(llm.complete("system", "user") for _ in range(8)), return_exceptions=True)
        assert results.count("ok") == 2
        assert sum(isinstance(r, BudgetExhausted) for r in results) == 6
        report = with_report({"ok":True})["routing_report"]
        assert report["complete"] and len(report["calls"]) == 2
        assert len({c["call_id"] for c in report["calls"]}) == 2
        assert all(c["usage"]["total_tokens"] == 7 for c in report["calls"])
        with pytest.raises(BudgetExhausted):
            await llm.complete("system", "late background helper")
    assert len(seen) == 2


@pytest.mark.anyio
@pytest.mark.parametrize("input_tokens,output_tokens,text,max_tokens", [
    (100,512,"hello",512), (2048,16,"hello",17), (600,512,"ж"*100,512),
])
async def test_r26_input_and_output_bounds_deny_before_http(monkeypatch, input_tokens,output_tokens,text,max_tokens):
    def forbidden():
        pytest.fail("HTTP client must not be reached")
    monkeypatch.setattr(http_client,"get_client",forbidden)
    rag_ctx.set_overrides({"routing":grant(input_tokens=input_tokens,output_tokens=output_tokens)})
    with pytest.raises(BudgetExhausted):
        await llm.complete("system",text,max_tokens=max_tokens)
    assert records() == []


@pytest.mark.anyio
async def test_r21_timeout_cancels_http_and_consumes_attempt(monkeypatch):
    cancelled = asyncio.Event()
    async def respond(request):
        try:
            await asyncio.sleep(10)
        finally:
            cancelled.set()
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":grant(timeout_ms=20)})
        with pytest.raises(TimeoutError):
            await llm.complete("system","user")
        assert cancelled.is_set()
        assert records()[0]["status"] == "cancelled"
        assert records()[0]["usage"]["total_tokens"] is None
        with pytest.raises(BudgetExhausted):
            await llm.complete("system","retry")


@pytest.mark.anyio
async def test_r23_done_report_contains_finalized_stream_usage(monkeypatch):
    class Stream(httpx.AsyncByteStream):
        closed = False
        async def __aiter__(self):
            yield b'data: {"choices":[{"delta":{"content":"ok"}}]}\n\n'
            yield b'data: {"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}\n\n'
            yield b'data: [DONE]\n\n'
        async def aclose(self):
            self.closed = True
    stream = Stream()
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda r:httpx.Response(200,stream=stream))) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        value=grant(); value["assignments"]["final"]=value["assignments"].pop("tool_loop")
        value["budget"]["allowances"]["final"]=value["budget"]["allowances"].pop("tool_loop")
        value["purpose"]="final"
        rag_ctx.set_overrides({"routing":value})
        events=[e async for e in report_events(llm.stream_chat([{"role":"user","content":"hello"}],{"max_tokens":32}))]
    report=events[-1]["routing_report"]
    assert report["complete"] and stream.closed
    assert report["calls"][0]["status"] == "completed"
    assert report["calls"][0]["usage"]["total_tokens"] == 7
    assert "utility-key" not in json.dumps(report)


@pytest.mark.anyio
async def test_r24_pipeline_reports_child_task_usage_and_seals_unused_slots(monkeypatch):
    async with httpx.AsyncClient(transport=httpx.MockTransport(response)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":grant(calls=4)})
        async def pipeline():
            yield {"type":"progress"}
            await asyncio.gather(llm.complete("s","a"),llm.complete("s","b"))
            yield {"type":"done","answer":"ok"}
        events=[e async for e in report_events(pipeline())]
        assert "routing_report" not in events[0]
        assert len(events[1]["routing_report"]["calls"]) == 2
        with pytest.raises(BudgetExhausted):
            await llm.complete("s","unused slot after report")


@pytest.mark.anyio
async def test_r26_report_with_active_child_cannot_refund(monkeypatch):
    started, release = asyncio.Event(), asyncio.Event()
    async def respond(request):
        started.set()
        await release.wait()
        return response(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":grant(calls=2)})
        child=asyncio.create_task(llm.complete("s","u"))
        await started.wait()
        assert with_report({})["routing_report"]["complete"] is False
        release.set()
        await child
        with pytest.raises(BudgetExhausted):
            await llm.complete("s","late")


def test_r26_invalid_grant_never_falls_back_to_legacy():
    value=grant(calls=1024)
    value["budget"]["allowances"]["unplanned"] = dict(max_calls=1,input_tokens=1,output_tokens=1)
    with pytest.raises(ValueError):
        rag_ctx.set_overrides({"routing":value})


@pytest.mark.anyio
async def test_r26_provider_overrun_closes_remaining_helper_slots(monkeypatch):
    seen=[]
    def respond(request):
        seen.append(request)
        return httpx.Response(200,json={"choices":[{"message":{"content":"ok"}}],
            "usage":{"prompt_tokens":3000,"completion_tokens":2}})
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":grant(calls=3)})
        with pytest.raises(BudgetExhausted,match="provider exceeded"):
            await llm.complete("s","u")
        with pytest.raises(BudgetExhausted):
            await llm.complete("s","next helper")
        assert records()[0]["usage"]["total_tokens"] == 3002
    assert len(seen)==1


@pytest.mark.anyio
@pytest.mark.parametrize("purpose", ["tool_loop", "primary", "final", "router_classify", "document"])
async def test_r24_real_chat_endpoint_returns_lease_report(monkeypatch, purpose):
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings,"shared_secret","routing-test-secret")
    value = grant()
    value["purpose"] = purpose
    value["assignments"][purpose] = value["assignments"].pop("tool_loop")
    value["budget"]["allowances"][purpose] = value["budget"]["allowances"].pop("tool_loop")
    expected_provider = value["assignments"][purpose]["provider_id"]
    if purpose != "tool_loop":
        # A second target must not steal a primitive call's explicit purpose.
        value["assignments"]["tool_loop"] = target(name="wrong-secondary")
    async with httpx.AsyncClient(transport=httpx.MockTransport(response)) as upstream:
        monkeypatch.setattr(http_client,"get_client",lambda:upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app),base_url="http://ml.test") as client:
            result=await client.post("/chat-step",headers={"X-Fosnie-ML-Key":"routing-test-secret"},json={
                "messages":[{"role":"user","content":"hello"}],"sampling":{"max_tokens":128},
                "overrides":{"routing":value}})
    assert result.status_code==200,result.text
    report=result.json()["routing_report"]
    assert report["complete"] and len(report["calls"])==1
    assert report["calls"][0]["usage"]["total_tokens"]==7
    assert report["calls"][0]["purpose"] == purpose
    assert report["calls"][0]["provider_id"] == expected_provider


@pytest.mark.anyio
@pytest.mark.parametrize("timeout_ms,input_tokens,expected",[(1000,1,429),(20,2048,504)])
async def test_r26_real_chat_endpoint_reports_budget_and_timeout(monkeypatch,timeout_ms,input_tokens,expected):
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings,"shared_secret","routing-test-secret")
    async def slow(request):
        await asyncio.sleep(1)
        return response(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(slow)) as upstream:
        monkeypatch.setattr(http_client,"get_client",lambda:upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app),base_url="http://ml.test") as client:
            result=await client.post("/chat-step",headers={"X-Fosnie-ML-Key":"routing-test-secret"},json={
                "messages":[{"role":"user","content":"hello"}],"sampling":{"max_tokens":128},
                "overrides":{"routing":grant(timeout_ms=timeout_ms,input_tokens=input_tokens)}})
    assert result.status_code==expected,result.text


@pytest.mark.anyio
@pytest.mark.parametrize("endpoint,body,pipeline_module,pipeline_fn,purpose",[
    ("/retrieve",{"prompt":"hello","kb_ids":[]},"app.retrieve","retrieve","retrieval_grade"),
    ("/web_search",{"query":"hello"},"app.web.pipeline","web_search","web_plan"),
])
async def test_r24_real_pipeline_endpoint_preserves_all_helper_usage(monkeypatch,endpoint,body,pipeline_module,pipeline_fn,purpose):
    import importlib
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings,"shared_secret","routing-test-secret")
    async def pipeline(*args,**kwargs):
        await asyncio.gather(llm.complete("s","one"),llm.complete("s","two"))
        return {"context":"ok","digest":"ok","citations":[]}
    monkeypatch.setattr(importlib.import_module(pipeline_module),pipeline_fn,pipeline)
    value=grant(calls=3)
    value["assignments"][purpose]=value["assignments"].pop("tool_loop")
    value["budget"]["allowances"][purpose]=value["budget"]["allowances"].pop("tool_loop")
    value["purpose"]=purpose
    body={**body,"overrides":{"routing":value}}
    async with httpx.AsyncClient(transport=httpx.MockTransport(response)) as upstream:
        monkeypatch.setattr(http_client,"get_client",lambda:upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app),base_url="http://ml.test") as client:
            result=await client.post(endpoint,headers={"X-Fosnie-ML-Key":"routing-test-secret"},json=body)
    assert result.status_code==200,result.text
    report=result.json()["routing_report"]
    assert report["complete"] and len(report["calls"])==2
    assert {call["purpose"] for call in report["calls"]}=={purpose}
