# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import hashlib
import json
from uuid import uuid4

import httpx
import pytest

from app import main, paths, http_client, rag_ctx
from app.research import pipeline, census
from test_inference_target import clean_context, anyio_backend, target
from test_inference_fixed import fixed

KB, DOC = str(uuid4()), str(uuid4())


def routing(timeout=5000):
    value = fixed("ocr", calls=1, timeout_ms=timeout, input_bytes=8*1024*1024, output_bytes=2*1024*1024)
    stages = {"document": (1, 512), "research_plan": (2, 1536), "research_write": (9, 4096),
              "utility_summary": (8, 256), "utility_title": (1, 96)}
    value.update(purpose="research_plan", assignments={p: target(name=p, key=p+"-key") for p in stages},
                 budget=dict(id=str(uuid4()), timeout_ms=timeout, allowances={p: dict(max_calls=n,
                     input_tokens=64000, output_tokens=o) for p, (n, o) in stages.items()}))
    return value


@pytest.fixture
def research(tmp_path, monkeypatch):
    path = tmp_path / "source.txt"
    path.write_text("Evidence from the source.", encoding="utf-8")
    monkeypatch.setattr(paths, "safe_path", lambda p: p)
    monkeypatch.setattr(main.settings, "shared_secret", "")
    def forbidden(*args, **kwargs):
        raise AssertionError("legacy research/cache must never run")
    monkeypatch.setattr(pipeline, "run", forbidden)
    monkeypatch.setattr(census, "run_census", forbidden)
    return dict(question="What does the evidence say?", source="files", template="freeform",
                kb_ids=[KB], docs=[dict(doc_id=DOC, kb_id=KB, path=str(path), mime="text/plain",
                    sha256=hashlib.sha256(path.read_bytes()).hexdigest())], total_docs=1,
                research_limits=dict(schema_version=1), overrides={"routing": routing()})


def response(request, seen, kind="ok"):
    body = json.loads(request.content)
    model = body["model"]
    assert request.headers["authorization"] == f"Bearer {model}-key"
    seen.append(model)
    data = json.loads(body["messages"][-1]["content"])
    if model == "research_plan":
        text = '[{"heading":"Findings","brief":"Evidence"}]' if "skeleton" in data else "Source evidence."
        if kind == "outline" and "skeleton" in data:
            text = 'not JSON'
    elif model == "research_write":
        text = "Evidence [D1]." if body.get("stream") else "## Findings\n\nEvidence [D1]."
        if kind == "hybrid":
            text += " Web evidence [D2]."
        if kind == "citation":
            text = text.replace("D1", "D99")
        if kind == "cohere" and not body.get("stream"):
            text = "Evidence [D1]."
    elif model == "utility_summary":
        text = "Evidence already covered."
    elif model == "utility_title":
        text = "Evidence report"
    else:
        raise AssertionError(model)
    if kind == "upstream":
        return httpx.Response(503, text="SECRET source /path/token")
    usage = dict(prompt_tokens=20, completion_tokens=10)
    if body.get("stream"):
        chunks = [dict(choices=[dict(delta=dict(content=text))])]
        if kind != "truncated":
            chunks.append(dict(choices=[dict(delta={}, finish_reason="length" if kind=="length" else "stop")], usage=usage))
        content = "".join("data: "+json.dumps(c)+"\n\n" for c in chunks)
        return httpx.Response(200, text=content+"data: [DONE]\n\n", headers={"content-type":"text/event-stream"})
    return httpx.Response(200, json={"choices":[{"message":{"content":text},"finish_reason":"stop"}],"usage":usage})


async def collect(body):
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app), base_url="http://ml.test") as client:
        result = await client.post("/deep_research", json=body)
    return result, [json.loads(line) for line in result.text.splitlines()] if result.status_code==200 else []


@pytest.mark.anyio
async def test_research_uses_all_pinned_stages_in_one_ledger(research, monkeypatch):
    seen = []
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda r: response(r, seen))) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        result, events = await collect(research)
    assert result.status_code == 200, result.text
    done = events[-1]
    assert done["type"] == "done", events
    assert done["report_md"] == "## Findings\n\nEvidence [D1]."
    assert done["coverage"] == dict(selected=1, read=1, sampled=False)
    assert done["doc_citations"][0]["doc_id"] == DOC
    assert seen == ["research_plan", "research_plan", "research_write", "utility_summary", "research_write", "utility_title"]
    report = done["routing_report"]
    assert report["complete"] and len(report["calls"]) == 6
    assert {c["lease_id"] for c in report["calls"]} == {research["overrides"]["routing"]["budget"]["id"]}
    assert {c["status"] for c in report["calls"]} == {"completed"}
    assert done["fixed_routing_report"]["complete"] and done["fixed_routing_report"]["calls"] == []


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["web", "hybrid", "missing_limits", "missing_routing", "verify", "deepen", "legacy_knob",
                                 "sampling", "duplicate", "denied", "wrong_kb", "hash", "missing_stage", "calls", "ocr", "question"])
async def test_unsupported_requests_fail_before_inference(research, monkeypatch, kind):
    if kind in ("web", "hybrid"):
        research["source"] = kind
    elif kind == "missing_limits":
        research.pop("research_limits")
    elif kind == "missing_routing":
        research.pop("overrides")
    elif kind == "verify":
        research["verify"] = True
    elif kind == "deepen":
        research["research_deepen_enabled"] = True
    elif kind == "legacy_knob":
        research["research_max_minutes"] = 1
    elif kind == "sampling":
        research["total_docs"] = 2
    elif kind == "duplicate":
        research["docs"] *= 2
        research["total_docs"] = 2
    elif kind == "denied":
        research["deny_doc_ids"] = [DOC]
    elif kind == "wrong_kb":
        research["docs"][0]["kb_id"] = str(uuid4())
    elif kind == "hash":
        research["docs"][0].pop("sha256")
    elif kind == "question":
        research["question"] = "x"*4097
    elif kind == "missing_stage":
        research["overrides"]["routing"]["budget"]["allowances"].pop("utility_summary")
    elif kind == "calls":
        research["overrides"]["routing"]["budget"]["allowances"]["research_write"]["max_calls"] = 1
    elif kind == "ocr":
        research["overrides"]["routing"]["fixed_budget"] = None
    seen = []
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda r: response(r, seen))) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        result, _ = await collect(research)
    assert result.status_code == 422, result.text
    assert seen == []


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["upstream", "outline", "citation", "cohere", "length", "truncated", "source_changed", "changed_after_write", "oversize", "tokens"])
async def test_failure_never_delivers_a_successful_report(research, monkeypatch, kind):
    from pathlib import Path
    path = Path(research["docs"][0]["path"])
    if kind in ("source_changed", "oversize"):
        path.write_text("changed" if kind=="source_changed" else "x"*16001)
        if kind == "oversize":
            research["docs"][0]["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
    if kind == "tokens":
        research["overrides"]["routing"]["budget"]["allowances"]["research_plan"]["input_tokens"] = 1
    seen = []
    def reply(request):
        out = response(request, seen, kind)
        if kind == "changed_after_write" and seen[-1] == "utility_title":
            path.write_text("changed")
        return out
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        result, events = await collect(research)
    assert result.status_code == 200
    assert events[-1]["type"] == "error" and all(e["type"] != "done" for e in events), events
    assert "SECRET" not in result.text and str(path) not in result.text
    assert events[-1]["routing_report"]["complete"]


@pytest.mark.anyio
@pytest.mark.parametrize("kind", ["cancel", "deadline", "close"])
async def test_stream_cancellation_and_deadline_close_active_provider(research, monkeypatch, kind):
    entered, stopped = asyncio.Event(), asyncio.Event()
    seen = []
    async def reply(request):
        if json.loads(request.content).get("stream"):
            entered.set()
            try:
                await asyncio.Future()
            finally:
                stopped.set()
        return response(request, seen)
    if kind == "deadline":
        research["overrides"]["routing"] = routing(100)
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        response_obj = await main.deep_research(main.DeepResearchRequest(**research))
        if kind == "close":
            # Closing between events closes the generator; no child remains.
            iterator = response_obj.body_iterator
            await anext(iterator)
            await iterator.aclose()
            assert not entered.is_set()
        else:
            async def consume():
                return [json.loads(line) async for line in response_obj.body_iterator]
            task = asyncio.create_task(consume())
            await entered.wait()
            if kind == "cancel":
                task.cancel()
                with pytest.raises(asyncio.CancelledError):
                    await task
            else:
                events = await task
                assert events[-1]["type"] == "error"
            assert stopped.is_set()
    assert rag_ctx.cfg("_routing_budget", None).active == 0


def with_web(research, source="web"):
    research.update(source=source, max_fetches=4, domain_allowlist="public.test", allowlist_only=True,
                    robots_policy="respect")
    if source == "web":
        research.update(docs=[], kb_ids=[], total_docs=0)
    envelope = research["overrides"]["routing"]
    envelope["budget"]["allowances"]["research_plan"]["max_calls"] = len(research["docs"]) + 5
    envelope["fixed_budget"]["allowances"]["ocr"]["max_calls"] = len(research["docs"]) + 4
    service = fixed("rerank", calls=1)
    envelope["fixed_assignments"].update(service["fixed_assignments"])
    envelope["fixed_budget"]["allowances"].update(service["fixed_budget"]["allowances"])
    return research


def web_fixture(monkeypatch, mode="ok"):
    from types import SimpleNamespace
    from app.web import provider, pipeline as web
    calls = []
    async def search(query, recency, limit):
        calls.append(query)
        return [] if mode == "empty" else [provider.SerpResult("https://public.test/report", "Report", "Web evidence", None, "test")]
    async def fetch(result, sem):
        if mode == "fetch_failure":
            raise ValueError("SECRET upstream")
        return web._Source("https://public.test/report", "Report", "public.test", None, "now", mode == "snippet", ["Web evidence."])
    monkeypatch.setattr(provider, "get_provider", lambda: SimpleNamespace(search=search))
    monkeypatch.setattr(web, "_fetch_source", fetch)
    return calls


@pytest.mark.anyio
@pytest.mark.parametrize("source", ["web", "hybrid"])
async def test_web_and_hybrid_share_original_ledger_and_cited_fetched_sources(research, monkeypatch, source):
    with_web(research, source)
    queries = web_fixture(monkeypatch)
    seen = []
    def reply(request):
        if request.url.path.endswith("rerank"):
            return httpx.Response(200, json={"results": [{"index": 0, "relevance_score": 1.0}]})
        return response(request, seen, source)
    async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        result, events = await collect(research)
    assert result.status_code == 200, result.text
    done = events[-1]
    assert done["type"] == "done", events
    assert queries == [research["question"]]  # corpus content never leaves as a query
    assert done["citations"][0]["reference"] == ("D1" if source == "web" else "D2")
    assert done["citations"][0] == done["web_sources"][0]
    assert "text" not in done["web_sources"][0]
    assert len(done["doc_citations"]) == (source == "hybrid")
    assert len(done["routing_report"]["calls"]) == (6 if source == "web" else 7)
    fixed_report = done["fixed_routing_report"]
    assert fixed_report["complete"] and [c["role"] for c in fixed_report["calls"]] == ["rerank"]
    assert {c["lease_id"] for c in fixed_report["calls"]} == {research["overrides"]["routing"]["fixed_budget"]["id"]}


@pytest.mark.anyio
@pytest.mark.parametrize("mode", ["empty", "fetch_failure", "snippet"])
async def test_hybrid_never_publishes_file_only_success_after_web_failure(research, monkeypatch, mode):
    with_web(research, "hybrid")
    web_fixture(monkeypatch, mode)
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda r: httpx.Response(200, json={"results": [{"index": 0, "relevance_score": 1.0}]}))) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        result, events = await collect(research)
    assert events[-1]["type"] == "error" and not any(e["type"] == "done" for e in events)
    assert "SECRET" not in result.text


@pytest.mark.anyio
@pytest.mark.parametrize("mode", ["cancel", "deadline"])
async def test_web_collection_is_owned_by_research_deadline_and_cancellation(research, monkeypatch, mode):
    from types import SimpleNamespace
    from app.web import provider
    with_web(research)
    if mode == "deadline":
        research["overrides"]["routing"]["budget"]["timeout_ms"] = 100
    entered, stopped = asyncio.Event(), asyncio.Event()
    async def search(*a):
        entered.set()
        try:
            await asyncio.Future()
        finally:
            stopped.set()
    monkeypatch.setattr(provider, "get_provider", lambda: SimpleNamespace(search=search))
    task = asyncio.create_task(collect(research))
    await entered.wait()
    if mode == "cancel":
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
    else:
        _, events = await task
        assert events[-1]["type"] == "error"
    assert stopped.is_set()


def with_template(research, free=False):
    research["template"] = str(uuid4())
    research["template_spec"] = dict(id=research["template"], label="Custom", outline_mode="free" if free else "constrained",
        skeleton=[] if free else [dict(heading="Findings", brief="Evidence", expandable=False, exec_summary=False)],
        writing_instructions="Write an evidence-led report.")


@pytest.mark.anyio
@pytest.mark.parametrize("free", [False, True])
async def test_custom_template_is_used_with_original_stage_grants(research, monkeypatch, free):
    with_template(research, free)
    seen = []
    async with httpx.AsyncClient(transport=httpx.MockTransport(lambda r: response(r, seen))) as upstream:
        monkeypatch.setattr(http_client, "get_client", lambda: upstream)
        result, events = await collect(research)
    assert result.status_code == 200 and events[-1]["type"] == "done", result.text
    assert events[-1]["report_md"] == "## Findings\n\nEvidence [D1]."
    assert len(events[-1]["routing_report"]["calls"]) == 6


@pytest.mark.anyio
@pytest.mark.parametrize("mode", ["identity", "expand", "placeholder", "duplicate", "free_skeleton", "missing_brief", "instructions", "sections"])
async def test_custom_template_incompatibility_is_rejected_before_inference(research, monkeypatch, mode):
    with_template(research)
    spec = research["template_spec"]
    if mode == "identity": spec["id"] = str(uuid4())
    elif mode == "expand": spec["skeleton"][0]["expandable"] = True
    elif mode == "placeholder": spec["skeleton"][0]["exec_summary"] = True
    elif mode == "duplicate": spec["skeleton"] *= 2
    elif mode == "free_skeleton": spec["outline_mode"] = "free"
    elif mode == "missing_brief": spec["skeleton"][0]["brief"] = ""
    elif mode == "instructions": spec["writing_instructions"] = "x"*4097
    elif mode == "sections": spec["skeleton"] *= 9
    monkeypatch.setattr(http_client, "get_client", lambda: pytest.fail("invalid template reached inference"))
    result, _ = await collect(research)
    assert result.status_code == 422, result.text
