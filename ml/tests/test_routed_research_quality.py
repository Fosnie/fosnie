# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json
import httpx
import pytest
from app import http_client, main, rag_ctx
from test_inference_target import clean_context, anyio_backend
from test_inference_fixed import fixed
from test_routed_research import research, response, collect, with_web, web_fixture


def quality(request, verify=True, deepen=True):
    request["verify"] = verify
    request["research_limits"]["deepen"] = deepen
    envelope = request["overrides"]["routing"]
    if deepen:
        envelope["budget"]["allowances"]["research_plan"]["max_calls"] += 8
    if verify:
        service = fixed("verify", calls=1, input_bytes=512*1024, output_bytes=512*1024)
        envelope["fixed_assignments"].update(service["fixed_assignments"])
        envelope["fixed_budget"]["allowances"].update(service["fixed_budget"]["allowances"])


def reviewer(request, seen, kind, source="files"):
    data = json.loads(request.content)
    if request.url.path.endswith("/verify"):
        seen.append("verify")
        assert data["model"] == "fixed-model" and "[D1]" in data["context"][0]
        assert "Source evidence." not in data["context"][0]  # use original text, not generated notes
        if kind == "outage": return httpx.Response(503, text="SECRET verifier failure")
        if kind == "wrong_model": return httpx.Response(200, json={"model":"wrong", "spans":[]})
        if kind == "malformed": return httpx.Response(200, json={"model":"fixed-model", "spans":[{"start":0,"end":99999,"label":"not_mentioned","score":0.8}]})
        if kind == "flagged":
            text = data["answer"]; start = text.index("Evidence"); end = len(text)
            return httpx.Response(200, json={"model":"fixed-model", "spans":[{"start":start,"end":end,"text":text[start:end],"label":"not_mentioned","score":0.8}]})
        return httpx.Response(200, json={"model":"fixed-model", "spans":[]})
    if request.url.path.endswith("rerank"):
        return httpx.Response(200, json={"results":[{"index":0,"relevance_score":1.0}]})
    content = json.loads(data["messages"][-1]["content"])
    if "sources" in content:
        seen.append("deepen")
        assert data["model"] == "research_plan"
        assert "text" in content["sources"][0] and "note" not in content["sources"][0]
        result = {"sufficient":False,"findings":"Evidence [D1].","gaps":["No independent corroboration"]}
        if kind == "bad_review": result["sufficient"] = True
        if kind == "forged_source": result["findings"] = "Claim [D99]"
        return httpx.Response(200, json={"choices":[{"message":{"content":json.dumps(result)},
            "finish_reason":"length" if kind == "truncated_review" else "stop"}],"usage":{"prompt_tokens":20,"completion_tokens":10}})
    if "section" in content and data.get("stream"):
        assert content["evidence_review"]["gaps"] == ["No independent corroboration"]
    return response(request, seen, source)


@pytest.mark.anyio
@pytest.mark.parametrize("source", ["files", "web", "hybrid"])
async def test_quality_stages_share_ledger_and_original_evidence(research, monkeypatch, source):
    if source != "files": with_web(research, source); queries = web_fixture(monkeypatch)
    quality(research); seen=[]
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda r:reviewer(r,seen,"flagged",source))) as upstream:
        monkeypatch.setattr(http_client,"get_client",lambda:upstream)
        result,events=await collect(research)
    assert result.status_code==200 and events[-1]["type"]=="done",events
    done=events[-1]
    assert done["verification"]["flagged"]==1 and 0<=done["verification"]["score"]<1
    assert done["verification"]["spans"][0]["text"] in done["report_md"]  # no silent pruning
    assert done["deepening"]=={"method":"existing_sources_v1","sections":1,"sections_with_gaps":1}
    assert seen.count("deepen")==seen.count("verify")==1
    route=research["overrides"]["routing"]
    assert {c["lease_id"] for c in done["routing_report"]["calls"]}=={route["budget"]["id"]}
    assert [c["role"] for c in done["fixed_routing_report"]["calls"]]==(["verify"] if source=="files" else ["rerank","verify"])
    if source!="files": assert queries==[research["question"]]


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["outage","wrong_model","malformed","bad_review","truncated_review","forged_source"])
async def test_requested_quality_cannot_downgrade_to_success(research,monkeypatch,kind):
    quality(research);seen=[]
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda r:reviewer(r,seen,kind))) as upstream:
        monkeypatch.setattr(http_client,"get_client",lambda:upstream)
        result,events=await collect(research)
    assert result.status_code==200 and events[-1]["type"]=="error",events
    assert not any(e["type"]=="done" for e in events) and "SECRET" not in result.text
    assert events[-1]["routing_report"]["complete"] and events[-1]["fixed_routing_report"]["complete"]


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["missing_verifier","missing_review_calls"])
async def test_quality_grants_required_before_inference(research,monkeypatch,kind):
    quality(research);route=research["overrides"]["routing"]
    if kind=="missing_verifier": route["fixed_budget"]["allowances"].pop("verify")
    else: route["budget"]["allowances"]["research_plan"]["max_calls"]=2
    def forbidden(*args,**kwargs): raise AssertionError("no inference before complete quality grants")
    monkeypatch.setattr(http_client,"get_client",forbidden)
    result,_=await collect(research)
    assert result.status_code==422


@pytest.mark.anyio
@pytest.mark.parametrize("stage", ["deepen","verify"])
async def test_cancellation_closes_quality_service(research,monkeypatch,stage):
    quality(research);entered,stopped=asyncio.Event(),asyncio.Event();seen=[]
    async def reply(request):
        data=json.loads(request.content)
        is_verify=request.url.path.endswith("/verify")
        is_review=not is_verify and "sources" in json.loads(data["messages"][-1]["content"])
        if (stage=="verify" and is_verify) or (stage=="deepen" and is_review):
            entered.set()
            try: await asyncio.Future()
            finally: stopped.set()
        return reviewer(request,seen,"ok")
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as upstream:
        monkeypatch.setattr(http_client,"get_client",lambda:upstream)
        stream=await main.deep_research(main.DeepResearchRequest(**research))
        async def consume(): return [event async for event in stream.body_iterator]
        task=asyncio.create_task(consume());await asyncio.wait_for(entered.wait(),3)
        task.cancel()
        with pytest.raises(asyncio.CancelledError): await task
    assert stopped.is_set() and rag_ctx.cfg("_routing_budget",None).active==0
