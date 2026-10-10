# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import json
import httpx
import pytest
from app import http_client, llm, rag_ctx
from app.inference_target import records
from app.inference_budget import BudgetExhausted
from app.output_schema import OutputSchemaError, validate_schema, validate_output, schema_overhead
from test_inference_target import target, envelope, clean_context, anyio_backend
from test_inference_budget import grant

SCHEMA = {"type": "object", "additionalProperties": False, "required": ["count", "ok"],
          "properties": {"count": {"type": "integer", "minimum": 1}, "ok": {"type": "boolean"}}}


def provider_response(adapter, content, finish="stop"):
    if adapter == "openai_chat":
        return {"choices": [{"message": {"content": content}, "finish_reason": finish}], "usage": {"prompt_tokens": 30, "completion_tokens": 9}}
    if adapter == "openai_responses":
        return {"status": "incomplete" if finish == "length" else "completed", "output": [{"type": "message", "content": [{"type": "output_text", "text": content}]}], "usage": {"input_tokens": 30, "output_tokens": 9}}
    return {"candidates": [{"content": {"parts": [{"text": content}]}, "finishReason": "MAX_TOKENS" if finish == "length" else "STOP"}], "usageMetadata": {"promptTokenCount": 30, "candidatesTokenCount": 9, "totalTokenCount": 39}}


@pytest.mark.anyio
@pytest.mark.parametrize("adapter", ["openai_chat", "openai_responses", "gemini"])
async def test_schema_native_wire_and_identity_accounted_through_real_endpoint(monkeypatch, adapter):
    from app.main import app, Sampling
    from app.config import settings
    monkeypatch.setattr(settings, "shared_secret", "routing-test-secret")
    seen = []
    def respond(request):
        seen.append(json.loads(request.content))
        return httpx.Response(200, json=provider_response(adapter, '{"count":2,"ok":true}'))
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://ml.test") as api:
            assignment = envelope(target(adapter), "primary")
            response = await api.post("/chat-step", headers={"x-fosnie-ml-key": settings.shared_secret}, json={
                "messages": [{"role": "user", "content": "Return the required object"}],
                "sampling": {"max_tokens": 128, "response_schema": SCHEMA}, "overrides": {"routing": assignment}})
    assert response.status_code == 200
    result = response.json()
    assert result["content"] == '{"count":2,"ok":true}'
    assert result["routing_usage"][0]["status"] == "completed"
    assert result["routing_usage"][0]["usage"]["total_tokens"] == 39
    assert result["routing_usage"][0]["purpose"] == "primary"
    assert len(seen) == 1
    if adapter == "openai_chat":
        assert seen[0]["response_format"] == {"type": "json_schema", "json_schema": {"name": "fosnie_output", "strict": True, "schema": SCHEMA}}
    elif adapter == "openai_responses":
        assert seen[0]["text"]["format"] == {"type": "json_schema", "name": "fosnie_output", "strict": True, "schema": SCHEMA}
    else:
        assert seen[0]["generationConfig"]["responseFormat"] == {"text": {"mimeType": "APPLICATION_JSON", "schema": SCHEMA}}
    assert Sampling().model_dump(exclude_none=True) == {}


@pytest.mark.anyio
@pytest.mark.parametrize("adapter", ["openai_chat", "openai_responses", "gemini"])
@pytest.mark.parametrize("content,finish", [('{"count":2,"ok":true}', "length"),
    ('```json\n{"count":2,"ok":true}\n```', "stop"), ('{"count":"2","ok":true}', "stop"), ("", "stop")])
async def test_bad_output_never_retries_and_preserves_actual_usage(monkeypatch, adapter, content, finish):
    seen = []
    def respond(request):
        seen.append(request)
        return httpx.Response(200, json=provider_response(adapter, content, finish))
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        rag_ctx.set_overrides({"routing": envelope(target(adapter), "router_classify")})
        with pytest.raises(OutputSchemaError, match="answer does not satisfy"):
            await llm.chat_step([{"role": "user", "content": "Synthetic"}], sampling={"max_tokens": 128, "response_schema": SCHEMA})
        assert len(seen) == 1
        assert records()[0]["status"] == "failed"
        assert records()[0]["usage"]["total_tokens"] == 39


@pytest.mark.anyio
async def test_schema_budget_adapter_tools_and_stream_deny_before_http(monkeypatch):
    monkeypatch.setattr(http_client, "get_client", lambda: pytest.fail("must deny before HTTP"))
    sampling = {"max_tokens": 128, "response_schema": SCHEMA}
    messages = [{"role": "user", "content": "hello"}]
    # Input fits without the schema, but not with the explicit schema allowance.
    rag_ctx.set_overrides({"routing": grant(input_tokens=600)})
    with pytest.raises(BudgetExhausted):
        await llm.chat_step(messages, sampling=sampling)
    assert records() == []
    rag_ctx.set_overrides({"routing": envelope(target("anthropic"), "primary")})
    with pytest.raises(OutputSchemaError):
        await llm.chat_step(messages, sampling=sampling)
    rag_ctx.set_overrides({"routing": envelope(target(), "primary")})
    with pytest.raises(OutputSchemaError):
        await llm.chat_step(messages, tools=[{"type": "function"}], sampling=sampling)
    with pytest.raises(OutputSchemaError):
        _ = [event async for event in llm.stream_chat(messages, sampling)]


@pytest.mark.parametrize("content", ['{"count":true,"ok":true}', '{"count":2,"count":3,"ok":true}', '{"count":2,"ok":true,"extra":0}', '{"count":NaN,"ok":true}', '{"count":0,"ok":true}', '{}'])
def test_strict_output_does_not_repair_or_ignore(content):
    with pytest.raises(OutputSchemaError):
        validate_output(SCHEMA, {"content": content, "tool_calls": [], "finish_reason": "stop"})


def test_unsupported_schema_features_fail_and_byte_overhead_is_conservative():
    assert schema_overhead(SCHEMA) == len(json.dumps(SCHEMA, ensure_ascii=False, separators=(",", ":")).encode()) + 256
    for bad in [{**SCHEMA, "$ref": "https://external.test"}, {**SCHEMA, "required": ["count"]},
                {**SCHEMA, "properties": {"count": {"type": "object"}, "ok": {"type": "boolean"}}},
                {**SCHEMA, "description": "ж" * 129}, {**SCHEMA, "additionalProperties": True}]:
        with pytest.raises(OutputSchemaError):
            validate_schema(bad)


@pytest.mark.parametrize("content,finish,reason", [
    ('{"count":2,"ok":true}', "length", "incomplete_response"),
    ('```json\n{"count":2,"ok":true}\n```', "stop", "markdown_fence"),
    ('{"count":2,"count":3,"ok":true}', "stop", "duplicate_keys"),
    ('{"count":2,"ok":true,"unexpected":"private-data"}', "stop", "fields_mismatch"),
    ('{"count":"private-data","ok":true}', "stop", "scalar_type_mismatch"),
    ('{"count":0,"ok":true}', "stop", "bounds_mismatch"),
    ('{"count":NaN,"ok":true}', "stop", "invalid_json"),
    ('private-data', "stop", "invalid_json"),
])
def test_output_contract_reason_is_allowlisted_and_contains_no_response(content, finish, reason):
    with pytest.raises(OutputSchemaError) as error:
        validate_output(SCHEMA, {"content": content, "tool_calls": [], "finish_reason": finish})
    assert error.value.reason == reason
    assert str(error.value) == "answer does not satisfy output schema"
    assert "private-data" not in str(error.value)
    assert OutputSchemaError("safe", "private-data").reason == "unsupported_contract"


@pytest.mark.anyio
async def test_contract_failure_http_category_keeps_safe_legacy_detail_and_one_call(monkeypatch):
    from app.main import app
    from app.config import settings
    monkeypatch.setattr(settings, "shared_secret", "routing-test-secret")
    seen, failed_usage = [], []
    original = llm.chat_step
    async def observed_step(*args, **kwargs):
        try:
            return await original(*args, **kwargs)
        except OutputSchemaError:
            # ASGI middleware isolates ContextVars from the caller's test task.
            # Inspect accounted inference inside the actual endpoint context.
            failed_usage.extend(records())
            raise
    monkeypatch.setattr(llm, "chat_step", observed_step)
    def respond(request):
        seen.append(request)
        return httpx.Response(200, json=provider_response("openai_chat", '```json\n{"count":2,"ok":true}\n```'))
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://ml.test") as api:
            response = await api.post("/chat-step", headers={"x-fosnie-ml-key": settings.shared_secret}, json={
                "messages": [{"role": "user", "content": "Synthetic private-data"}],
                "sampling": {"max_tokens": 128, "response_schema": SCHEMA},
                "overrides": {"routing": envelope(target("openai_chat"), "primary")}})
    assert response.status_code == 422
    assert response.json() == {"detail": "output schema contract rejected"}
    assert response.headers["x-fosnie-output-contract"] == "markdown_fence"
    assert "private-data" not in response.text
    assert len(seen) == 1
    assert len(failed_usage) == 1
    assert failed_usage[0]["usage"]["total_tokens"] == 39
