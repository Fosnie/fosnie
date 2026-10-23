# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json
from uuid import uuid4

import httpx
import pytest
from app import llm, rag_ctx, http_client
from app.inference_target import InferenceTarget, for_call, normalise_usage, records


def target(adapter="openai_chat", name="utility", key="utility-key"):
    return dict(schema_version=1, provider_id=str(uuid4()), provider_revision=1,
                profile_revision=1, adapter=adapter, base_url="https://utility.test/v1",
                model=name, api_key=key, effort=None)


def envelope(assignment, purpose="retrieval_grade"):
    return dict(schema_version=1, decision_id=str(uuid4()), purpose=purpose,
                assignments={purpose: assignment})


@pytest.fixture(autouse=True)
def clean_context():
    rag_ctx.set_overrides({})
    llm.set_stage(None)
    yield
    rag_ctx.set_overrides({})
    llm.set_stage(None)


@pytest.fixture
def anyio_backend():
    return "asyncio"


@pytest.mark.anyio
@pytest.mark.parametrize("adapter", ["openai_chat", "openai_responses", "anthropic", "gemini"])
@pytest.mark.parametrize("key", ["utility-key", ""])
async def test_r10_native_utility_receives_its_own_atomic_target(monkeypatch, adapter, key):
    seen = []
    def respond(request):
        body = json.loads(request.content)
        seen.append((str(request.url), body, dict(request.headers)))
        if adapter == "openai_responses":
            return httpx.Response(200, json={"status": "completed", "output": [{"type": "message", "content": [{"type": "output_text", "text": "ok"}]}]})
        if adapter == "anthropic":
            return httpx.Response(200, json={"content": [{"type": "text", "text": "ok"}], "usage": {"input_tokens": 4, "output_tokens": 2}})
        if adapter == "gemini":
            return httpx.Response(200, json={"candidates": [{"content": {"parts": [{"text": "ok"}]}}]})
        return httpx.Response(200, json={"choices": [{"message": {"content": "ok"}}], "usage": {"prompt_tokens": 4, "completion_tokens": 2}})
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond),
                                auth=("wrong", "default"), headers={"Authorization": "Bearer wrong-default"},
                                cookies={"wrong": "default"}) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        rag_ctx.set_overrides({"llm_base_url": "https://primary.test/v1", "llm_model": "primary",
                               "llm_api_key": "primary-key", "routing": envelope(target(adapter, key=key))})
        llm.set_stage("rag.grade")
        assert await llm.complete("system", "user") == "ok"
    assert len(seen) == 1
    url, body, headers = seen[0]
    assert url.startswith("https://utility.test/v1/")
    assert "primary" not in json.dumps(seen)
    assert "utility" in json.dumps(body) or "utility:" in url
    if adapter in ("openai_chat", "openai_responses"):
        assert headers.get("authorization") == (f"Bearer {key}" if key else None)
    else:
        assert "authorization" not in headers
        assert headers["x-api-key" if adapter == "anthropic" else "x-goog-api-key"] == key
    assert "cookie" not in headers
    assert rag_ctx.cfg("llm_model", None) == "primary"
    assert rag_ctx.cfg("llm_api_key", None) == "primary-key"
    assert len(records()) == 1
    assert records()[0]["purpose"] == "retrieval_grade"
    assert records()[0]["status"] == "completed"
    assert "utility-key" not in json.dumps(records())


@pytest.mark.anyio
async def test_r10_r24_concurrent_targets_do_not_share_credentials(monkeypatch):
    seen = []
    async def respond(request):
        await asyncio.sleep(0)
        seen.append((json.loads(request.content)["model"], request.headers["authorization"]))
        return httpx.Response(200, json={"choices": [{"message": {"content": "ok"}}]})
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        async def run(name):
            rag_ctx.set_overrides({"routing": envelope(target(name=name, key=name + "-key"))})
            await llm.complete("system", "user")
        await asyncio.gather(run("alice"), run("bob"))
    assert set(seen) == {("alice", "Bearer alice-key"), ("bob", "Bearer bob-key")}
    assert rag_ctx.cfg("routing", None) is None


@pytest.mark.anyio
async def test_r24_cancellation_restores_primary_context(monkeypatch):
    async def respond(request):
        raise asyncio.CancelledError()
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        rag_ctx.set_overrides({"llm_model": "primary", "routing": envelope(target())})
        with pytest.raises(asyncio.CancelledError):
            await llm.complete("system", "user")
    assert rag_ctx.cfg("llm_model", None) == "primary"
    assert records()[0]["status"] == "cancelled"


@pytest.mark.anyio
@pytest.mark.parametrize("adapter", ["openai_chat", "gemini"])
async def test_r14_tools_rejection_does_not_retry_without_tools(monkeypatch, adapter):
    seen = []
    def respond(request):
        seen.append(request)
        return httpx.Response(400, text="tools unsupported")
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        rag_ctx.set_overrides({"routing": envelope(target(adapter), "tool_loop")})
        with pytest.raises(httpx.HTTPStatusError):
            await llm.chat_step([{"role": "user", "content": "weather"}],
                                tools=[{"type": "function", "function": {"name": "web_search", "parameters": {"type": "object"}}}])
    assert len(seen) == 1


def test_r24_partial_target_and_missing_assignment_fail_before_inference():
    incomplete = target()
    del incomplete["api_key"]
    with pytest.raises(ValueError):
        InferenceTarget.model_validate(incomplete)
    rag_ctx.set_overrides({"routing": envelope(target())})
    with pytest.raises(ValueError, match="assignment missing"):
        with for_call("web.plan"):
            pytest.fail("must not dispatch")


def test_r25_usage_does_not_double_count_reasoning_or_cache():
    usage = normalise_usage({"input_tokens": 100, "output_tokens": 60,
                             "output_tokens_details": {"reasoning_tokens": 40},
                             "input_tokens_details": {"cached_tokens": 80}})
    assert usage["total_tokens"] == 160
    assert usage["reasoning_tokens"] == 40
    assert usage["cached_input_tokens"] == 80
    assert normalise_usage(None)["total_tokens"] is None


def test_target_repr_excludes_endpoint_and_credential():
    model = InferenceTarget.model_validate(target(key="secret-value"))
    assert "secret-value" not in repr(model)
    assert "utility.test" not in repr(model)


@pytest.mark.anyio
async def test_r14_required_effort_is_sent_exactly_and_rejection_is_not_downgraded(monkeypatch):
    seen = []
    def respond(request):
        seen.append(json.loads(request.content))
        return httpx.Response(400, text="reasoning_effort unsupported")
    assignment = target(name="unfamiliar-model")
    assignment.update(base_url="https://api.openai.com/v1", effort="xhigh")
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        rag_ctx.set_overrides({"routing": envelope(assignment)})
        with pytest.raises(httpx.HTTPStatusError):
            await llm.complete("system", "user")
    assert len(seen) == 1
    assert seen[0]["reasoning_effort"] == "xhigh"


@pytest.mark.anyio
async def test_r10_explicit_model_cannot_override_target():
    rag_ctx.set_overrides({"routing": envelope(target(), "tool_loop")})
    with pytest.raises(ValueError, match="conflicts"):
        await llm.chat_step([], model="another-model")


@pytest.mark.anyio
@pytest.mark.parametrize("adapter",["anthropic","gemini"])
async def test_r14_native_adapter_cannot_silently_drop_unsupported_image(adapter):
    rag_ctx.set_overrides({"routing":envelope(target(adapter),"tool_loop")})
    with pytest.raises(ValueError,match="inline image"):
        await llm.chat_step([{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://image.test/photo.png"}}]}])


@pytest.mark.anyio
async def test_r14_unknown_gemini_effort_does_not_become_dynamic_reasoning():
    assignment=target("gemini"); assignment["effort"]="unsupported-level"
    rag_ctx.set_overrides({"routing":envelope(assignment,"tool_loop")})
    with pytest.raises(ValueError,match="parameter mapping"):
        await llm.chat_step([{"role":"user","content":"hello"}])


def test_r25_native_usage_is_normalised_once_with_raw_evidence():
    from app import anthropic_adapter, gemini_adapter
    rag_ctx.set_overrides({"routing":envelope(target())})
    claude = {"input_tokens":10,"cache_read_input_tokens":70,"cache_creation_input_tokens":20,"output_tokens":50,"thinking_tokens":30}
    usage = normalise_usage(anthropic_adapter._map_usage(claude))
    assert usage["input_tokens"] == 100
    assert usage["output_tokens"] == 50
    assert usage["total_tokens"] == 150
    assert usage["raw_usage"] == claude
    gemini = {"promptTokenCount":100,"candidatesTokenCount":20,"thoughtsTokenCount":30,"totalTokenCount":150,"cachedContentTokenCount":70}
    usage = normalise_usage(gemini_adapter._map_usage(gemini))
    assert usage["total_tokens"] == 150
    assert usage["output_tokens"] == 50
    assert usage["raw_usage"] == gemini
    assert normalise_usage(anthropic_adapter._map_usage(None))["total_tokens"] is None
    assert normalise_usage(gemini_adapter._map_usage(None))["total_tokens"] is None


@pytest.mark.anyio
async def test_r24_stream_close_restores_context_and_closes_upstream(monkeypatch):
    response = httpx.Response(200, content='data: {"choices":[{"delta":{"content":"hello"}}]}\n\ndata: [DONE]\n\n')
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda request: response)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"llm_model":"primary","routing":envelope(target(),"final")})
        stream = llm.stream_chat([{"role":"user","content":"hello"}],{"max_tokens":64})
        assert (await anext(stream))["type"] == "token"
        await stream.aclose()
        assert response.is_closed
        assert rag_ctx.cfg("llm_model",None)=="primary"
        assert records()[0]["status"]=="cancelled"


@pytest.mark.anyio
@pytest.mark.parametrize("key", ["stream-key", ""])
async def test_r07_stream_uses_only_assignment_auth_and_never_follows_redirects(monkeypatch, key):
    seen = []
    def respond(request):
        seen.append(request)
        assert request.headers.get("authorization") == (f"Bearer {key}" if key else None)
        assert "cookie" not in request.headers
        return httpx.Response(302, headers={"location": "https://forbidden.test/v1/chat/completions"})
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond), follow_redirects=True,
                                auth=("wrong", "default"), headers={"Authorization": "Bearer wrong-default"},
                                cookies={"wrong": "default"}) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        rag_ctx.set_overrides({"routing": envelope(target(key=key), "final")})
        with pytest.raises(llm.LlmError):
            _ = [event async for event in llm.stream_chat([], {"max_tokens": 64})]
    assert len(seen) == 1
    assert records()[0]["status"] == "failed"


@pytest.mark.anyio
@pytest.mark.parametrize("adapter,status",[("anthropic",429),("openai_responses",400)])
async def test_r22_one_attempt_and_no_adapter_fallback_or_secret_echo(monkeypatch,caplog,adapter,status):
    calls=[]
    def respond(request):
        calls.append(request)
        return httpx.Response(status,text="echoed-secret-content")
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":envelope(target(adapter),"final")})
        if adapter=="openai_responses":
            with pytest.raises(llm.LlmError):
                _ = [event async for event in llm.stream_chat([], {"max_tokens":64})]
        else:
            with pytest.raises(httpx.HTTPStatusError):
                await llm.complete("system","user")
    assert len(calls)==1
    assert "echoed-secret-content" not in caplog.text
    assert records()[0]["status"]=="failed"


@pytest.mark.anyio
@pytest.mark.parametrize("model,effort,expected", [
    ("gemini-3.1-flash-lite", "minimal", {"thinkingLevel": "MINIMAL"}),
    ("gemini-3-flash-preview", "low", {"thinkingLevel": "LOW"}),
    ("gemini-3.5-flash-lite", "high", {"thinkingLevel": "HIGH"}),
    ("models/gemini-3.1-flash-lite", "medium", {"thinkingLevel": "MEDIUM"}),
    ("gemini-2.5-flash", "low", {"thinkingBudget": 4096}),
    ("gemini-2.5-flash", "none", {"thinkingBudget": 0}),
    ("gemini-3.1-flash-lite", None, None),
    ("gemma-4-26b-a4b-it", "minimal", {"thinkingLevel": "MINIMAL"}),
    ("gemma-4-31b-it", "high", {"thinkingLevel": "HIGH"}),
    ("models/gemma-4-31b-it", "minimal", {"thinkingLevel": "MINIMAL"}),
    ("gemma-4-31b-it", None, None),
])
async def test_model_specific_gemini_effort_wire_contract(monkeypatch,model,effort,expected):
    seen=[]
    def respond(request):
        seen.append(json.loads(request.content))
        return httpx.Response(200,json={"candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2,"totalTokenCount":6}})
    assignment=target("gemini",name=model);assignment["effort"]=effort
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":envelope(assignment,"router_classify")})
        result=await llm.chat_step([{"role":"user","content":"Classify"}],sampling={"max_tokens":128})
    assert result["content"]=="ok";assert len(seen)==1
    think=seen[0]["generationConfig"].get("thinkingConfig")
    if expected is None: assert think is None
    else:
        for key,value in expected.items(): assert think[key]==value
        assert not ("thinkingLevel" in think and "thinkingBudget" in think)


@pytest.mark.anyio
@pytest.mark.parametrize("effort",["none","max","unsupported"])
async def test_gemini_level_family_never_downgrades_an_unsupported_effort(monkeypatch,effort):
    def forbidden(): raise AssertionError("unsupported mapping must fail before HTTP client lookup")
    monkeypatch.setattr(http_client,"get_client",forbidden)
    assignment=target("gemini",name="gemini-3.1-flash-lite");assignment["effort"]=effort
    rag_ctx.set_overrides({"routing":envelope(assignment,"router_classify")})
    with pytest.raises(ValueError,match="parameter mapping"):
        await llm.chat_step([{"role":"user","content":"Classify"}])


@pytest.mark.anyio
@pytest.mark.parametrize("model",["gemma-4-26b-a4b-it","models/gemma-4-31b-it"])
@pytest.mark.parametrize("effort",["none","low","medium","max"])
async def test_gemma_level_family_rejects_unsupported_effort_before_HTTP(monkeypatch,model,effort):
    def forbidden():raise AssertionError("unsupported Gemma effort must not dispatch")
    monkeypatch.setattr(http_client,"get_client",forbidden)
    assignment=target("gemini",name=model);assignment["effort"]=effort
    rag_ctx.set_overrides({"routing":envelope(assignment,"router_classify")})
    with pytest.raises(ValueError,match="parameter mapping"):
        await llm.chat_step([{"role":"user","content":"Classify"}])


@pytest.mark.anyio
@pytest.mark.parametrize("adapter,effort",[("openai_chat","none"),("openai_responses","high")])
async def test_pinned_openai_effort_survives_helper_call(monkeypatch,adapter,effort):
    seen=[]
    def respond(request):
        seen.append(json.loads(request.content))
        if adapter=="openai_responses":
            return httpx.Response(200,json={"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}]})
        return httpx.Response(200,json={"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]})
    assignment=target(adapter,name="opaque-provider-id");assignment["effort"]=effort
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":envelope(assignment,"web_plan")})
        assert await llm.complete("system","user")=="ok"
    assert len(seen)==1
    if adapter=="openai_chat": assert seen[0]["reasoning_effort"]==effort
    else: assert seen[0]["reasoning"]=={"effort":effort}
