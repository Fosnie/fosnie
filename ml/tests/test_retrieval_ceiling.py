# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import json
from uuid import uuid4
import httpx
import pytest
from app import llm,rag_ctx,http_client
from app.inference_budget import BudgetExhausted
from app.inference_target import records,with_report
from test_inference_target import target,envelope,clean_context,anyio_backend


@pytest.mark.anyio
async def test_browser_retrieval_default_is_bounded_before_native_dispatch(monkeypatch):
    seen=[]
    def respond(request):
        seen.append(json.loads(request.content))
        return httpx.Response(200,json={"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}})
    routed=envelope(target(),"retrieval_decompose")
    routed["budget"]={"id":str(uuid4()),"timeout_ms":1000,"allowances":{"retrieval_decompose":{"max_calls":1,"input_tokens":4096,"output_tokens":1024}}}
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":routed})
        assert await llm.complete("system","user",max_tokens=2048)=="ok"
        assert seen[0]["max_tokens"]==1024
        assert with_report({})["routing_report"]["complete"]
        assert len(records())==1


@pytest.mark.anyio
async def test_explicit_generation_cap_still_denied_before_http(monkeypatch):
    monkeypatch.setattr(http_client,"get_client",lambda:pytest.fail("explicit cap must be denied before HTTP"))
    routed=envelope(target(),"tool_loop")
    routed["budget"]={"id":str(uuid4()),"timeout_ms":1000,"allowances":{"tool_loop":{"max_calls":1,"input_tokens":4096,"output_tokens":1024}}}
    rag_ctx.set_overrides({"routing":routed})
    with pytest.raises(BudgetExhausted):
        await llm.chat_step([{"role":"user","content":"public fixture"}],None,{"max_tokens":1025})
    assert records()==[]


@pytest.mark.anyio
@pytest.mark.parametrize("adapter",["openai_responses","gemini"])
async def test_native_reasoning_grade_uses_generated_token_room_in_grant(monkeypatch,adapter):
    seen=[]
    def respond(request):
        seen.append(json.loads(request.content))
        if adapter=="openai_responses":
            return httpx.Response(200,json={"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"YES"}]}],"usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}})
        return httpx.Response(200,json={"candidates":[{"content":{"parts":[{"text":"YES"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":2,"totalTokenCount":7}})
    assigned=target(adapter=adapter,name="gemini-3.1-flash-lite" if adapter=="gemini" else "opaque-model");assigned["effort"]="low"
    routed=envelope(assigned,"retrieval_grade")
    routed["budget"]={"id":str(uuid4()),"timeout_ms":1000,"allowances":{"retrieval_grade":{"max_calls":1,"input_tokens":4096,"output_tokens":1024}}}
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":routed})
        assert await llm.complete("binary grade","public evidence",max_tokens=4)=="YES"
        cap=seen[0].get("max_output_tokens",seen[0].get("generationConfig",{}).get("maxOutputTokens"))
        assert cap==1024
        assert with_report({})["routing_report"]["complete"]
