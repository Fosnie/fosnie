# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json
from uuid import uuid4

import httpx
import pytest
from app import main, http_client, rag_ctx
from app.research import routed_triage, triage
from test_inference_target import clean_context, anyio_backend, target


def body():
    return dict(question="Compare the evidence", source="files", library_count=1, refinements=[],
                overrides={"routing": dict(schema_version=1, decision_id=str(uuid4()), purpose="research_plan",
                    assignments={"research_plan": target(name="planner", key="planner-key")},
                    budget=dict(id=str(uuid4()), timeout_ms=3000, allowances={"research_plan": dict(
                        max_calls=1, input_tokens=8192, output_tokens=1024)}))})


async def send(request):
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app), base_url="http://ml.test") as client:
        return await client.post("/research/routed-triage", json=request)


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["questions", "clear", "shape", "long", "duplicate", "control", "extra", "upstream", "truncated"])
async def test_one_pinned_call_and_strict_accounted_result(monkeypatch, kind):
    seen = []
    monkeypatch.setattr(main.settings, "shared_secret", "")
    def forbidden(*args, **kwargs):
        raise AssertionError("legacy triage must never run")
    monkeypatch.setattr(triage, "triage", forbidden)
    result = {"questions": [{"prompt": "Which timeframe?", "options": ["This year", "Last year"]}]}
    if kind == "clear": result["questions"] = []
    if kind == "shape": result["questions"] = "invalid"
    if kind == "long": result["questions"][0]["prompt"] = "é" * 121
    if kind == "duplicate": result["questions"][0]["options"] = ["A", "A"]
    if kind == "control": result["questions"][0]["prompt"] = "Hidden\nquestion"
    if kind == "extra": result["scope_indices"] = [99]
    def respond(request):
        data = json.loads(request.content); seen.append(data)
        assert data["model"] == "planner" and request.headers["authorization"] == "Bearer planner-key"
        assert json.loads(data["messages"][-1]["content"])["refinements"] == ["English report"]
        if kind == "upstream": return httpx.Response(503, text="SECRET /source/path/key")
        return httpx.Response(200, json={"choices": [{"message": {"content": json.dumps(result)},
            "finish_reason": "length" if kind == "truncated" else "stop"}],
            "usage": {"prompt_tokens": 20, "completion_tokens": 10}})
    request = body(); request["refinements"] = ["English report"]
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        response = await send(request)
    assert response.status_code == 200 and len(seen) == 1
    value = response.json()
    assert "SECRET" not in response.text and "planner-key" not in response.text
    assert value["routing_report"]["lease_id"] == request["overrides"]["routing"]["budget"]["id"]
    assert len(value["routing_report"]["calls"]) == 1
    assert value["routing_report"]["calls"][0]["purpose"] == "research_plan"
    if kind in ("questions", "clear"):
        assert value["questions"] == result["questions"] and value["routing_report"]["complete"]
    else:
        assert "questions" not in value and "error" in value


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["unrouted", "purpose", "budget", "calls", "scope", "refinements", "size", "extra"])
async def test_reject_before_inference(monkeypatch, kind):
    monkeypatch.setattr(main.settings, "shared_secret", "")
    request = body(); route = request["overrides"]["routing"]
    if kind == "unrouted": request["overrides"] = {}
    if kind == "purpose": route["purpose"] = "document"
    if kind == "budget": del route["budget"]
    if kind == "calls": route["budget"]["allowances"]["research_plan"]["max_calls"] = 2
    if kind == "scope": request["source"] = "web"
    if kind == "refinements": request["refinements"] = ["A"] * 6
    if kind == "size": request["question"] = "é" * 2049
    if kind == "extra": request["scope"] = [{"path": "/private"}]
    def forbidden(*args, **kwargs):
        raise AssertionError("validation must precede inference")
    monkeypatch.setattr(http_client, "get_client", forbidden)
    assert (await send(request)).status_code == 422


@pytest.mark.anyio
async def test_disconnect_stops_triage_without_fallback(monkeypatch):
    from fastapi import HTTPException
    request = body(); rag_ctx.set_overrides(request["overrides"])
    entered, stopped = asyncio.Event(), asyncio.Event()
    class Connection:
        async def is_disconnected(self): return entered.is_set()
    async def respond(_):
        entered.set()
        try: await asyncio.Future()
        finally: stopped.set()
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        with pytest.raises(HTTPException) as error:
            await main.routed_research_triage(routed_triage.Request.model_validate(request), Connection())
    assert error.value.status_code == 499 and stopped.is_set()
