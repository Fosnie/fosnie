# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json

import httpx
import pytest
from app import verify, rag_ctx, http_client, main
from app.inference_target import with_report
from test_inference_target import clean_context, anyio_backend
from test_inference_fixed import fixed


ANSWER = "🙂 Wrong."
SPAN = dict(start=2, end=7, text="Wrong", label="contradicted", confidence=0.9)


@pytest.mark.anyio
async def test_real_endpoint_preserves_pinned_model_unicode_and_accounting(monkeypatch):
    seen = []
    def reply(request):
        seen.append(request)
        body = json.loads(request.content)
        assert body["routing_model"] == "fixed-model" and body["hhem_filter"] is False
        assert "authorization" not in request.headers
        return httpx.Response(200, json=dict(model="fixed-model", spans=[SPAN]))
    monkeypatch.setattr(main.settings, "shared_secret", "")
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app), base_url="http://ml.test") as client:
            response = await client.post("/verify", json=dict(context="evidence", question="question", answer=ANSWER,
                overrides={"routing": fixed("verify")}))
    assert response.status_code == 200, response.text
    result = response.json()
    assert result["score"] == 0.375 and result["spans"][0]["text"] == "Wrong"
    assert result["fixed_routing_report"]["complete"] and len(result["fixed_routing_report"]["calls"]) == 1
    assert result["fixed_routing_report"]["calls"][0]["role"] == "verify" and len(seen) == 1


@pytest.mark.anyio
@pytest.mark.parametrize("data", [{}, {"spans": None}, {"spans": [None]}, {"spans": [SPAN, SPAN]},
    *[{"spans": [{**SPAN, **bad}]} for bad in [dict(start=-1), dict(start=True), dict(end=99), dict(end=2),
        dict(text="forged"), dict(label="supported"), dict(confidence="0.9"), dict(confidence=2)]],
    {"spans": [SPAN] * 1025}])
async def test_malformed_success_never_becomes_fully_grounded(monkeypatch, data):
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, json={"model":"fixed-model", **data}))) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        rag_ctx.set_overrides({"routing": fixed("verify")})
        with pytest.raises(ValueError):
            await verify.verify_live("evidence", "question", ANSWER)


@pytest.mark.anyio
@pytest.mark.parametrize("kwargs", [dict(context=""), dict(answer=" " * 2), dict(question="x" * 65537),
    dict(answer="🙂" * 65537), dict(context="x" * (1024*1024+1)), dict(strictness="unknown"),
    dict(threshold=float("nan")), dict(threshold=2), dict(hhem_filter=True), dict(model="wrong")])
async def test_invalid_request_denied_before_http(monkeypatch, kwargs):
    monkeypatch.setattr(http_client, "get_client", lambda: pytest.fail("invalid request reached HTTP"))
    rag_ctx.set_overrides({"routing": fixed("verify")})
    with pytest.raises(ValueError):
        await verify.verify_live(**{**dict(context="evidence", question="question", answer=ANSWER), **kwargs})


@pytest.mark.anyio
async def test_filters_validate_before_excluding_spans(monkeypatch):
    raw = [{**SPAN, "label": "not_mentioned"}]
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(200, json=dict(model="fixed-model", spans=raw)))) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        for opts in [dict(strictness="lenient"), dict(threshold=1)]:
            rag_ctx.set_overrides({"routing": fixed("verify")})
            result = await verify.verify_live("evidence", "question", ANSWER, **opts)
            assert result["spans"] == [] and result["score"] == 1
        raw[0]["end"] = 999
        rag_ctx.set_overrides({"routing": fixed("verify")})
        with pytest.raises(ValueError):
            await verify.verify_live("evidence", "question", ANSWER, strictness="lenient")


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["cancel", "disconnect", "deadline"])
async def test_waiting_upstream_is_cancelled_and_awaited(monkeypatch, kind):
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
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        request = main.VerifyRequest(context="evidence", question="question", answer=ANSWER,
            overrides={"routing": fixed("verify", timeout_ms=50 if kind == "deadline" else 1000)})
        if kind == "disconnect":
            from fastapi import HTTPException
            with pytest.raises(HTTPException) as error:
                await main.verify(request, Request())
            assert error.value.status_code == 499
        elif kind == "deadline":
            with pytest.raises(TimeoutError):
                await main.verify(request)
        else:
            task = asyncio.create_task(main.verify(request))
            await entered.wait()
            task.cancel()
            with pytest.raises(asyncio.CancelledError):
                await task
    assert stopped.is_set()


@pytest.mark.anyio
async def test_legacy_failure_still_skips_verification(monkeypatch):
    monkeypatch.setattr(verify.settings, "verify_enabled", True)
    monkeypatch.setattr(verify, "_down_until", 0)
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda _: httpx.Response(503))) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        rag_ctx.set_overrides({})
        assert (await verify.verify_live("evidence", "question", ANSWER))["score"] is None
