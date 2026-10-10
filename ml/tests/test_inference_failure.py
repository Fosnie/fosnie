# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json
from uuid import uuid4

import httpx
import pytest
from app import http_client, llm, rag_ctx
from app.inference_budget import BudgetExhausted
from app.inference_failure import failure_code
from test_inference_target import target, envelope, clean_context, anyio_backend


@pytest.mark.parametrize("status,expected", [(429, "rate_limited"), (500, "unavailable"),
    (502, "unavailable"), (503, "unavailable"), (504, "unavailable"), (400, None),
    (401, None), (403, None), (404, None), (501, None)])
def test_status_is_typed_and_output_fences_retry(status, expected):
    error = llm.LlmError("secret text with misleading 429", upstream_status=status)
    assert failure_code(error, emitted=False) == expected
    assert failure_code(error, emitted=True) is None


@pytest.mark.parametrize("error,expected", [(httpx.ConnectError("secret"), "connection"),
    (httpx.ConnectTimeout("secret"), "connection"), (httpx.ReadTimeout("secret"), "unavailable"),
    (TimeoutError("budget deadline"), None), (BudgetExhausted("429"), None),
    (asyncio.CancelledError(), None), (ValueError("503"), None), (llm.LlmError("429"), None)])
def test_exception_text_never_authorizes_retry(error, expected):
    assert failure_code(error, emitted=False) == expected


def routing(adapter):
    value = envelope(target(adapter=adapter, name="synthetic", key="synthetic-secret"), "final")
    value["budget"] = dict(id=str(uuid4()), timeout_ms=1000, allowances={
        "final": dict(max_calls=1, input_tokens=4096, output_tokens=128)})
    return value


@pytest.mark.anyio
@pytest.mark.parametrize("adapter", ["openai_chat", "openai_responses", "anthropic", "gemini"])
@pytest.mark.parametrize("status,expected", [(429, "rate_limited"), (503, "unavailable"), (401, None), (400, None)])
async def test_generate_failure_keeps_native_status_and_failed_lease(monkeypatch, adapter, status, expected):
    from app.main import generate, GenerateRequest
    seen = []
    def respond(request):
        seen.append(request)
        return httpx.Response(status, text="synthetic-secret echoed 429")
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        response = await generate(GenerateRequest(messages=[{"role": "user", "content": "Synthetic prompt"}],
            sampling={"max_tokens": 128}, overrides={"routing": routing(adapter)}))
        events = [json.loads(line) async for line in response.body_iterator]
    assert len(seen) == 1, "no hidden native adapter retry"
    assert len(events) == 1 and events[0]["type"] == "error"
    report = events[0]["routing_report"]
    assert report["complete"] and len(report["calls"]) == 1
    assert report["calls"][0]["status"] == "failed"
    assert report["calls"][0]["usage"]["total_tokens"] is None
    assert "synthetic-secret" not in json.dumps(events)
    if expected is None:
        assert events[0]["routing_failure"]["kind"] == ("unauthorized" if status==401 else "invalid_request")
        assert events[0]["routing_failure"]["before_output"] is True
    else:
        assert events[0]["routing_failure"] == {"schema_version": 1, "kind": expected, "before_output": True}


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["token", "reasoning", "tool_call"])
async def test_generate_never_marks_an_emitted_event_retryable(monkeypatch, kind):
    from app.main import generate, GenerateRequest
    async def stream(*args, **kwargs):
        yield {"type": kind, "delta": "partial"}
        raise llm.LlmError("secret", upstream_status=429)
    monkeypatch.setattr(llm, "stream_chat", stream)
    response = await generate(GenerateRequest(messages=[{"role": "user", "content": "Synthetic"}],
        overrides={"routing": routing("openai_chat")}))
    events = [json.loads(line) async for line in response.body_iterator]
    assert events[-1]["type"] == "error" and events[-1]["routing_failure"]["before_output"] is False
    assert "secret" not in json.dumps(events[-1])
    assert "secret" not in json.dumps(events)
