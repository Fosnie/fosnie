# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json
from uuid import uuid4

import httpx
import pytest
from app import embeddings, reranker, verify, ocr, stt, tts, rag_ctx, http_client, llm
from app.inference_target import with_report, InferenceEnvelope
from app.inference_budget import BudgetExhausted
from test_inference_target import clean_context, anyio_backend, envelope, target


def fixed(role="embed", calls=1, timeout_ms=1000, input_bytes=10000, output_bytes=10000):
    assignment = dict(schema_version=1, provider_id=str(uuid4()), provider_revision=3,
                      profile_revision=4, role=role, model="fixed-model", base_url="http://fixed.invalid",
                      api_key="")
    if role=="embed_desired":
        assignment.update(desired_index_revision=8,embedding_dimension=2,embedding_collection="new-collection")
    return dict(schema_version=1, decision_id=str(uuid4()), purpose={"embed_desired":"embed","verify_claims":"verify"}.get(role,role), assignments={},
                fixed_assignments={role:assignment}, fixed_budget=dict(id=str(uuid4()),timeout_ms=timeout_ms,
                allowances={role:dict(max_calls=calls,input_bytes=input_bytes,output_bytes=output_bytes)}))


def reply(request):
    path=request.url.path
    if path.endswith("embeddings"):
        return httpx.Response(200,json={"data":[{"index":0,"embedding":[1.0,2.0]}]})
    if path.endswith("rerank"):
        return httpx.Response(200,json={"results":[{"index":0,"relevance_score":0.9}]})
    if path.endswith("verify-claims"):
        return httpx.Response(200,json={"model":"fixed-model","verdicts":[{"verdict":"supported","score":1.0}]})
    if path.endswith("verify"):
        return httpx.Response(200,json={"model":"fixed-model","spans":[]})
    if path.endswith("transcriptions"):
        return httpx.Response(200,json={"text":"hello"})
    if path.endswith("speech"):
        return httpx.Response(200,content=b"audio",headers={"content-type":"audio/wav"})
    return httpx.Response(200,json={"choices":[{"message":{"content":"hello"}}]})


def mixed_grant(calls=2):
    from test_inference_budget import grant
    value=grant(calls=calls)
    service=fixed(calls=calls)
    value.update(fixed_assignments=service["fixed_assignments"],fixed_budget=service["fixed_budget"])
    return value


@pytest.mark.anyio
@pytest.mark.parametrize("operation",["search","upsert"])
async def test_routed_memory_cannot_use_unattested_legacy_collection(monkeypatch,operation):
    from app import memory
    monkeypatch.setattr(memory,"client",lambda:pytest.fail("unattested index reached Qdrant"))
    monkeypatch.setattr(http_client,"get_client",lambda:pytest.fail("unattested index reached model"))
    rag_ctx.set_overrides({"routing":fixed()})
    with pytest.raises(ValueError,match="attested memory index"):
        if operation=="search": await memory.search("user_test","query")
        else: await memory.upsert("user_test",str(uuid4()),"fact")
    assert with_report({})["fixed_routing_report"]["calls"]==[]


def test_legacy_memory_regression_with_isolated_local_qdrant(monkeypatch):
    from qdrant_client import AsyncQdrantClient
    from app import memory
    from test_memory import test_memory_upsert_and_search
    local=AsyncQdrantClient(location=":memory:")
    monkeypatch.setattr(memory,"client",lambda:local)
    rag_ctx.set_overrides({})
    try:
        # Run the existing deterministic embedding/ranking assertions against
        # SDK local storage; never touch the deployment's Qdrant collection.
        test_memory_upsert_and_search(monkeypatch)
    finally:
        asyncio.run(local.close())


@pytest.mark.anyio
async def test_mixed_parallel_pipeline_reports_and_seals_both_grants(monkeypatch):
    from test_inference_budget import response as llm_response
    from app.inference_target import report_events
    seen=[]
    async def respond(request):
        seen.append(request)
        await asyncio.sleep(0)
        return reply(request) if request.url.host=="fixed.invalid" else llm_response(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":mixed_grant()})
        async def pipeline():
            yield {"type":"progress"}
            await asyncio.gather(llm.complete("s","u"),embeddings.embed(["text"]))
            yield {"type":"done","answer":"ok"}
        events=[event async for event in report_events(pipeline())]
        assert "routing_report" not in events[0] and "fixed_routing_report" not in events[0]
        result=events[-1]
        for key in ("routing_report","fixed_routing_report"):
            assert result[key]["complete"] and len(result[key]["calls"])==1
            assert result[key]["calls"][0]["status"]=="completed"
        assert result["routing_report"]["calls"][0]["usage"]["total_tokens"]==7
        assert result["routing_report"]["calls"][0]["decision_id"]==result["fixed_routing_report"]["calls"][0]["decision_id"]
        assert result["routing_report"]["calls"][0]["call_id"]!=result["fixed_routing_report"]["calls"][0]["call_id"]
        with pytest.raises(BudgetExhausted): await llm.complete("s","late")
        with pytest.raises(BudgetExhausted): await embeddings.embed(["late"])
    assert len(seen)==2


@pytest.mark.anyio
@pytest.mark.parametrize("active_role",["llm","fixed"])
async def test_mixed_terminal_report_does_not_claim_active_other_lane_complete(monkeypatch,active_role):
    from test_inference_budget import response as llm_response
    started,release=asyncio.Event(),asyncio.Event()
    async def respond(request):
        is_fixed=request.url.host=="fixed.invalid"
        if is_fixed==(active_role=="fixed"):
            started.set()
            await release.wait()
        return reply(request) if is_fixed else llm_response(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":mixed_grant()})
        pending=asyncio.create_task(embeddings.embed(["a"]) if active_role=="fixed" else llm.complete("s","u"))
        try:
            await started.wait()
            if active_role=="fixed": await llm.complete("s","u")
            else: await embeddings.embed(["a"])
            result=with_report({})
            active_key="fixed_routing_report" if active_role=="fixed" else "routing_report"
            other_key="routing_report" if active_role=="fixed" else "fixed_routing_report"
            assert not result[active_key]["complete"] and result[other_key]["complete"]
            release.set()
            await pending
            assert result[active_key]["calls"]==[],"a sent partial report must not mutate when a child ends"
        finally:
            release.set()
            if not pending.done(): pending.cancel()
            await asyncio.gather(pending,return_exceptions=True)


@pytest.mark.anyio
async def test_mixed_readonly_index_reference_has_no_inference_allowance(monkeypatch):
    from test_inference_budget import response as llm_response
    value=mixed_grant()
    value["fixed_assignments"]["embed_desired"]=fixed("embed_desired")["fixed_assignments"]["embed_desired"]
    value["fixed_budget"]["allowances"]={"embed_desired":value["fixed_budget"]["allowances"]["embed"]}
    calls=[]
    def respond(request):
        calls.append(request)
        return llm_response(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":value})
        with pytest.raises(BudgetExhausted): await embeddings.embed(["source is read-only"])
        await llm.complete("s","u")
        result=with_report({})
    assert len(calls)==1 and result["fixed_routing_report"]["calls"]==[]


async def invoke(role):
    return await {"embed":lambda:embeddings.embed(["hello"]), "rerank":lambda:reranker.rerank("q",["d"]),
        "verify":lambda:verify.verify_live("context","q","answer"),"ocr":lambda:ocr.ocr_bytes(b"image","image/png"),
        "stt":lambda:stt.transcribe(b"audio","audio/wav"),"tts":lambda:tts.synthesize("hello")} [role]()


@pytest.mark.anyio
@pytest.mark.parametrize("role",["embed","rerank","verify","ocr","stt","tts"])
async def test_fixed_targets_override_all_defaults_and_report_each_http_attempt(monkeypatch, role):
    seen=[]
    async def to_wav(audio,mime): return audio,"wav"
    monkeypatch.setattr(stt,"_to_wav",to_wav)
    monkeypatch.setattr(reranker,"_down_until",float("inf"))
    monkeypatch.setattr(verify,"_down_until",float("inf"))
    async def respond(request):
        seen.append(request)
        assert request.url.host=="fixed.invalid"
        assert "authorization" not in request.headers
        assert b"fixed-model" in await request.aread()
        return reply(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond),auth=("wrong","default")) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":fixed(role),f"{role}_base_url":"http://wrong.invalid",
                              f"{role}_api_key":"wrong",f"{role}_model":"wrong",f"{role}_enabled":False})
        await invoke(role)
        report=with_report({})["fixed_routing_report"]
        assert report["complete"] and len(report["calls"])==1
        record=report["calls"][0]
        assert record["status"]=="completed" and record["role"]==role
        assert record["input_bytes"]>0 and record["output_bytes"]>0
        assert "fixed-model" not in json.dumps(report) and "api_key" not in json.dumps(report)
        with pytest.raises(BudgetExhausted): await invoke(role)
    assert len(seen)==1
    assert rag_ctx.cfg(f"{role}_model",None)=="wrong", "scope must restore even on error"


@pytest.mark.anyio
@pytest.mark.parametrize("role",["embed","rerank","verify","ocr","stt","tts"])
async def test_llm_envelope_cannot_fall_back_to_unassigned_fixed_service(monkeypatch,role):
    monkeypatch.setattr(http_client,"get_client",lambda:pytest.fail("unassigned inference reached HTTP"))
    rag_ctx.set_overrides({"routing":envelope(target(),"tool_loop")})
    with pytest.raises(ValueError,match="fixed assignment and grant"): await invoke(role)


@pytest.mark.anyio
async def test_fixed_grant_rejects_input_before_http_and_never_allows_llm_or_desired_index(monkeypatch):
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda r:pytest.fail("HTTP must not run"))) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":fixed(input_bytes=1)})
        with pytest.raises(BudgetExhausted): await embeddings.embed(["large"])
        with pytest.raises(ValueError,match="assignment missing"): await llm.complete("s","u")
        with pytest.raises(ValueError,match="desired embedding index"): await embeddings.embed_with(["a"],"http://other","other","")
        assert with_report({})["fixed_routing_report"]["calls"]==[]


@pytest.mark.anyio
async def test_fixed_parallel_calls_share_atomic_grant_and_seal_blocks_late_helpers(monkeypatch):
    async def respond(request):
        await asyncio.sleep(0)
        return reply(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":fixed(calls=2)})
        results=await asyncio.gather(*(embeddings.embed(["a"]) for _ in range(8)),return_exceptions=True)
        assert sum(isinstance(r,BudgetExhausted) for r in results)==6
        report=with_report({})["fixed_routing_report"]
        assert report["complete"] and len(report["calls"])==2
        assert len({c["call_id"] for c in report["calls"]})==2


@pytest.mark.anyio
async def test_verify_batches_consume_separate_slots_and_cannot_fail_open(monkeypatch):
    seen=[]
    monkeypatch.setattr(verify.settings,"verify_claims_batch",1)
    def respond(request): seen.append(request); return reply(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":fixed("verify_claims",calls=2)})
        with pytest.raises(BudgetExhausted):
            await verify.verify_claims([{"text":"c","evidence":"e"}]*3)
        report=with_report({})["fixed_routing_report"]
        assert len(seen)==len(report["calls"])==2


@pytest.mark.anyio
@pytest.mark.parametrize("role",["rerank","verify"])
async def test_strict_failures_do_not_retry_or_disappear(monkeypatch,role):
    seen=[]
    def respond(request): seen.append(request); return httpx.Response(429)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":fixed(role,calls=4)})
        with pytest.raises(httpx.HTTPStatusError): await invoke(role)
        assert len(seen)==1
        assert with_report({})["fixed_routing_report"]["calls"][0]["status"]=="failed"


@pytest.mark.anyio
async def test_fixed_timeout_consumes_attempt_and_closes_active_report(monkeypatch):
    started=asyncio.Event()
    async def respond(request): started.set(); await asyncio.sleep(10)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":fixed(timeout_ms=20)})
        with pytest.raises(TimeoutError): await embeddings.embed(["a"])
        report=with_report({})["fixed_routing_report"]
        assert started.is_set() and report["complete"]
        assert report["calls"][0]["status"]=="cancelled"
        assert report["calls"][0]["output_bytes"] is None


@pytest.mark.anyio
async def test_fixed_output_overrun_closes_stream_and_remaining_grant(monkeypatch):
    class Stream(httpx.AsyncByteStream):
        closed=False
        async def __aiter__(self): yield b"123456"; pytest.fail("must stop reading oversized response")
        async def aclose(self): self.closed=True
    stream=Stream()
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda r:httpx.Response(200,stream=stream))) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":fixed(calls=2,output_bytes=5)})
        with pytest.raises(BudgetExhausted): await embeddings.embed(["a"])
        with pytest.raises(BudgetExhausted): await embeddings.embed(["b"])
        report=with_report({})["fixed_routing_report"]
        assert stream.closed and report["calls"][0]["output_bytes"]==6


@pytest.mark.anyio
async def test_early_report_cannot_refund_inflight_child(monkeypatch):
    started,release=asyncio.Event(),asyncio.Event()
    async def respond(request): started.set(); await release.wait(); return reply(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":fixed(calls=2)})
        child=asyncio.create_task(embeddings.embed(["a"]))
        await started.wait()
        assert not with_report({})["fixed_routing_report"]["complete"]
        release.set(); await child
        with pytest.raises(BudgetExhausted): await embeddings.embed(["b"])


@pytest.mark.anyio
async def test_unacknowledged_verifier_model_and_wrong_index_dimension_fail(monkeypatch):
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda r:httpx.Response(200,json={"spans":[]}))) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":fixed("verify")})
        with pytest.raises(ValueError,match="acknowledge"): await invoke("verify")
    value=fixed(); value["fixed_assignments"]["embed"].update(active_index_revision=7,embedding_dimension=3,embedding_collection="active")
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":value})
        with pytest.raises(ValueError,match="dimension"): await embeddings.embed(["a"])


def test_fixed_schema_rejects_role_conflicts_and_partial_index_identity():
    value=fixed(); value["fixed_assignments"]["embed"]["role"]="tts"
    with pytest.raises(ValueError): InferenceEnvelope.model_validate(value)
    value=fixed(); value["fixed_assignments"]["embed"]["active_index_revision"]=7
    with pytest.raises(ValueError): InferenceEnvelope.model_validate(value)
    value=fixed(); value["fixed_budget"]["allowances"]["tts"]=value["fixed_budget"]["allowances"].pop("embed")
    with pytest.raises(ValueError): InferenceEnvelope.model_validate(value)


@pytest.mark.anyio
@pytest.mark.parametrize("role",["embed","stt","tts"])
async def test_real_fixed_endpoints_preserve_reports_in_json_or_audio_header(monkeypatch,role):
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings,"shared_secret","routing-test-secret")
    monkeypatch.setattr(settings,"stt_enabled",False)
    monkeypatch.setattr(settings,"tts_enabled",False)
    async def to_wav(audio,mime): return audio,"wav"
    monkeypatch.setattr(stt,"_to_wav",to_wav)
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as upstream:
        monkeypatch.setattr(http_client,"get_client",lambda:upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app),base_url="http://ml.test") as client:
            headers={"X-Fosnie-ML-Key":"routing-test-secret"}
            if role=="stt":
                headers["X-Fosnie-Overrides"]=json.dumps({"routing":fixed(role)})
                result=await client.post("/transcribe",headers=headers,content=b"audio")
            else:
                result=await client.post("/embed-dimension" if role=="embed" else "/speech",headers=headers,
                    json={"overrides":{"routing":fixed(role)},**({"text":"hello"} if role=="tts" else {})})
            assert result.status_code==200,result.text
            if role=="tts":
                assert result.content==b"audio"
                report=json.loads(result.headers["X-Fosnie-Fixed-Routing-Report"])
            else:
                report=result.json()["fixed_routing_report"]
            assert report["complete"] and report["calls"][0]["role"]==role
            if role=="embed": assert result.json()["model"]=="fixed-model"


@pytest.mark.anyio
async def test_invalid_audio_routing_header_cannot_silently_become_legacy(monkeypatch):
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings,"shared_secret","routing-test-secret")
    monkeypatch.setattr(http_client,"get_client",lambda:pytest.fail("invalid envelope reached inference"))
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app),base_url="http://ml.test") as client:
        for value in ('{"routing":{}}','{broken','[]'):
            result=await client.post("/transcribe",content=b"audio",headers={
                "X-Fosnie-ML-Key":"routing-test-secret","X-Fosnie-Overrides":value})
            assert result.status_code==400,result.text


def test_reference_verifier_checks_model_before_loading_or_running_inference(monkeypatch):
    """Import the real sidecar with fake local weights; no downloads or GPU."""
    import runpy
    import sys
    from pathlib import Path
    from types import SimpleNamespace
    from fastapi import HTTPException
    detector=SimpleNamespace(predict=lambda **kwargs:[])
    model=SimpleNamespace(eval=lambda:None,config=SimpleNamespace(id2label={0:"neutral"}))
    monkeypatch.setitem(sys.modules,"torch",SimpleNamespace(no_grad=lambda:lambda fn:fn))
    monkeypatch.setitem(sys.modules,"transformers",SimpleNamespace(
        AutoTokenizer=SimpleNamespace(from_pretrained=lambda _:object()),
        AutoModelForSequenceClassification=SimpleNamespace(from_pretrained=lambda _:model)))
    monkeypatch.setitem(sys.modules,"lettucedetect.models.inference",SimpleNamespace(HallucinationDetector=lambda **kwargs:detector))
    module=runpy.run_path(str(Path(__file__).resolve().parents[2]/"backend/deploy/verify/server.py"))
    for request_name,function_name,model_name,body in [
        ("VerifyRequest","verify","MODEL",dict(context=["c"],question="q",answer="a")),
        ("VerifyClaimsRequest","verify_claims","FACTCG_MODEL",dict(claims=[])),
    ]:
        request=module[request_name]; function=module[function_name]
        with pytest.raises(HTTPException) as exc:
            function(request(**body,routing_model="wrong-model"))
        assert exc.value.status_code==409
        assert function(request(**body,routing_model=module[model_name]))["model"]==module[model_name]
        assert function(request(**body))["model"]==module[model_name], "legacy requests remain supported"


@pytest.mark.anyio
async def test_legacy_reranker_still_retries_and_verifier_can_degrade(monkeypatch):
    seen=[]
    monkeypatch.setattr(reranker,"_down_until",0)
    monkeypatch.setattr(verify,"_down_until",0)
    rag_ctx.set_overrides({"rerank_enabled":True,"verify_enabled":True,"rerank_max_retries":2,"rerank_backoff_base":0})
    def respond(request): seen.append(request); return httpx.Response(503)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        assert await reranker.rerank("q",["d"])==[0.0]
        assert (await verify.verify_live("c","q","a"))["score"] is None
    assert len(seen)==3


@pytest.mark.anyio
async def test_cancelled_stt_transcode_kills_local_child(monkeypatch):
    from types import SimpleNamespace
    started=asyncio.Event()
    state={"killed":False,"waited":False}
    async def communicate(): started.set(); await asyncio.sleep(10)
    def kill(): state["killed"]=True
    async def wait(): state["waited"]=True
    async def spawn(*args,**kwargs): return SimpleNamespace(returncode=None,communicate=communicate,kill=kill,wait=wait)
    monkeypatch.setattr(stt.shutil,"which",lambda _:"ffmpeg")
    monkeypatch.setattr(stt.asyncio,"create_subprocess_exec",spawn)
    child=asyncio.create_task(stt._to_wav(b"audio","audio/wav"))
    await started.wait(); child.cancel()
    with pytest.raises(asyncio.CancelledError): await child
    assert state=={"killed":True,"waited":True}


@pytest.mark.anyio
async def test_stt_multipart_accounts_for_the_actual_encoded_body(monkeypatch):
    monkeypatch.setattr(stt.settings,"stt_format","openai")
    async def to_wav(audio,mime): return audio,"wav"
    monkeypatch.setattr(stt,"_to_wav",to_wav)
    seen=[]
    def respond(request): seen.append(request); return reply(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client,"get_client",lambda:client)
        rag_ctx.set_overrides({"routing":fixed("stt")})
        assert await stt.transcribe(b"audio","audio/wav")=="hello"
        record=with_report({})["fixed_routing_report"]["calls"][0]
        assert seen[0].headers["content-type"].startswith("multipart/form-data;")
        assert record["input_bytes"]==len(seen[0].content)>len(b"audio")


@pytest.mark.anyio
@pytest.mark.parametrize("input_bytes,timeout_ms,status",[(1,1000,429),(10000,20,504)])
async def test_audio_endpoint_distinguishes_budget_and_deadline(monkeypatch,input_bytes,timeout_ms,status):
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings,"shared_secret","routing-test-secret")
    seen=[]
    async def respond(request): seen.append(request); await asyncio.sleep(10)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as upstream:
        monkeypatch.setattr(http_client,"get_client",lambda:upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app),base_url="http://ml.test") as client:
            result=await client.post("/speech",headers={"X-Fosnie-ML-Key":"routing-test-secret"},json={
                "text":"hello","overrides":{"routing":fixed("tts",input_bytes=input_bytes,timeout_ms=timeout_ms)}})
    assert result.status_code==status,result.text
    assert len(seen)==(0 if status==429 else 1)


@pytest.mark.anyio
@pytest.mark.parametrize("explicit",[False,True])
async def test_speech_format_is_per_request_and_preserves_fixed_accounting(monkeypatch,explicit):
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings,"shared_secret","routing-test-secret")
    monkeypatch.setattr(settings,"tts_format","mp3")
    seen=[]
    def respond(request): seen.append(request); return reply(request)
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as upstream:
        monkeypatch.setattr(http_client,"get_client",lambda:upstream)
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app),base_url="http://ml.test") as client:
            body={"text":"hello","overrides":{"routing":fixed("tts")}}
            if explicit: body["response_format"]="wav"
            result=await client.post("/speech",headers={"X-Fosnie-ML-Key":"routing-test-secret"},json=body)
    assert result.status_code==200,result.text
    assert len(seen)==1 and seen[0].url.host=="fixed.invalid"
    payload=json.loads(seen[0].content)
    assert payload["model"]=="fixed-model"
    assert payload["response_format"]==("wav" if explicit else "mp3")
    assert settings.tts_format=="mp3"
    record=json.loads(result.headers["x-fosnie-fixed-routing-report"])["calls"][0]
    assert record["status"]=="completed"
    assert record["input_bytes"]==len(seen[0].content)
    assert record["output_bytes"]==len(result.content)


@pytest.mark.anyio
async def test_invalid_explicit_speech_format_never_contacts_provider(monkeypatch):
    from app import main
    from app.config import settings
    monkeypatch.setattr(settings,"shared_secret","routing-test-secret")
    monkeypatch.setattr(http_client,"get_client",lambda:pytest.fail("invalid format reached provider"))
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app),base_url="http://ml.test") as client:
        for value in ("pcm","mp3","",24000,{"rate":24000}):
            result=await client.post("/speech",headers={"X-Fosnie-ML-Key":"routing-test-secret"},json={
                "text":"hello","response_format":value,"overrides":{"routing":fixed("tts")}})
            assert result.status_code==422,result.text
