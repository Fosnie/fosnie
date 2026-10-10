# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
import json
from uuid import uuid4

import httpx
import pytest
from app import main, rag_ctx, http_client, extract, map_reduce
from app.routed_document import Limits, read, connected
from app.inference_budget import BudgetExhausted
from test_inference_target import clean_context, anyio_backend, target
from test_inference_fixed import fixed


def routing(calls=32, tokens=22000):
    value = fixed("ocr", input_bytes=8*1024*1024, output_bytes=8*1024*1024)
    value.update(purpose="document", assignments={"document": target(name="document-model")},
                 budget=dict(id=str(uuid4()), timeout_ms=5000, allowances={
                     "document": dict(max_calls=calls, input_tokens=tokens, output_tokens=512)}))
    return value


def limits(**kw):
    return Limits(max_file_bytes=kw.get("max_file_bytes", 4096),
                  output_bytes=kw.get("output_bytes", 100), max_sections=kw.get("max_sections", 32))


@pytest.mark.anyio
async def test_native_stuff_uses_no_model_discovery_or_default_endpoint(tmp_path, monkeypatch):
    path = tmp_path / "text.txt"
    path.write_text("short evidence", encoding="utf-8")
    monkeypatch.setattr(main, "safe_path", lambda p: p)
    async def forbidden():
        pytest.fail("legacy model discovery")
    monkeypatch.setattr(main, "_resolve_model", forbidden)
    result = await main.read_document(main.ReadDocumentRequest(path=str(path), overrides={"routing": routing()}, document_limits=limits().model_dump()))
    assert result["text"] == "short evidence"
    assert result["routing_report"]["calls"] == result["fixed_routing_report"]["calls"] == []
    assert result["routing_report"]["complete"] and result["fixed_routing_report"]["complete"]


@pytest.mark.anyio
async def test_unicode_stuff_is_byte_bounded_and_marks_truncation(tmp_path):
    path = tmp_path / "text.txt"
    path.write_text("界" * 50, encoding="utf-8")
    rag_ctx.set_overrides({"routing": routing()})
    result = await read(str(path), None, None, limits())
    assert result["text"] == "界" * 33
    assert result["truncated"]


@pytest.mark.anyio
@pytest.mark.parametrize("mode", ["map", "image", "mixed_pdf", "ocr_error"])
async def test_real_adapters_use_pinned_document_and_ocr_grants(tmp_path, monkeypatch, mode):
    path = tmp_path / ("doc.png" if mode in ("image", "ocr_error") else "doc.pdf" if mode == "mixed_pdf" else "doc.txt")
    path.write_text("evidence " * 1000, encoding="utf-8")
    if mode == "mixed_pdf":
        monkeypatch.setattr(extract, "_pdf_pages", lambda p: [(1, "native page " * 5), (2, "")])
    seen = []
    def respond(request):
        body = json.loads(request.content)
        seen.append(body["model"])
        if body["model"] == "fixed-model" and mode == "ocr_error":
            return httpx.Response(503)
        return httpx.Response(200, json={"choices": [{"message": {"content": "extracted evidence"}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}})
    rag_ctx.set_overrides({"routing": routing()})
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        if mode == "ocr_error":
            with pytest.raises(httpx.HTTPStatusError):
                await read(str(path), None, "task", limits(max_file_bytes=16000))
            assert seen == ["fixed-model"]
            return
        result = await read(str(path), None, "task", limits(max_file_bytes=16000))
    if mode == "map":
        assert seen == ["document-model"] * 4
        assert all(c["purpose"] == "document" for c in result["routing_report"]["calls"])
        assert len(result["routing_report"]["calls"]) == 4
        assert result["truncated"]  # digest is also byte bounded
    else:
        assert seen == ["fixed-model"]
        assert len(result["fixed_routing_report"]["calls"]) == 1
        assert result["mode"] == "stuff"


@pytest.mark.anyio
async def test_map_section_ceiling_marks_partial_result(tmp_path, monkeypatch):
    path = tmp_path / "doc.txt"
    path.write_text("x" * 9000)
    rag_ctx.set_overrides({"routing": routing(calls=2)})
    calls = []
    def respond(request):
        calls.append(request)
        return httpx.Response(200, json={"choices": [{"message": {"content": "finding"}}], "usage": {"prompt_tokens": 10, "completion_tokens": 5}})
    async with httpx.AsyncClient(transport=httpx.MockTransport(respond)) as client:
        monkeypatch.setattr(http_client, "get_client", lambda: client)
        result = await read(str(path), None, "task", limits(max_file_bytes=16000))
    assert len(calls) == 2 and result["truncated"]


@pytest.mark.anyio
@pytest.mark.parametrize("case", ["file", "query", "missing_limits", "missing_fixed"])
async def test_invalid_bounds_fail_before_inference(tmp_path, monkeypatch, case):
    path = tmp_path / "doc.txt"
    path.write_text("x" * 500)
    value = routing(tokens=2100)
    if case == "missing_fixed":
        value.pop("fixed_budget")
    monkeypatch.setattr(main, "safe_path", lambda p: p)
    with pytest.raises((ValueError, BudgetExhausted)):
        await main.read_document(main.ReadDocumentRequest(path=str(path), prompt="query" * 100,
            overrides={"routing": value}, document_limits=None if case == "missing_limits" else limits(max_file_bytes=10 if case == "file" else 4096).model_dump()))


@pytest.mark.anyio
@pytest.mark.parametrize("cancel", [False, True])
async def test_map_failure_or_parent_cancel_awaits_siblings(monkeypatch, cancel):
    entered, stopped = asyncio.Event(), asyncio.Event()
    async def complete(system, user, **kw):
        if "aaa" in user:
            entered.set()
            try:
                await asyncio.Future()
            finally:
                stopped.set()
        await entered.wait()
        if cancel:
            await asyncio.Future()
        raise BudgetExhausted("denied")
    monkeypatch.setattr(map_reduce.llm, "complete", complete)
    with rag_ctx.scoped_overrides({"routing": {}}):
        task = asyncio.create_task(map_reduce.map_reduce("aaa\n\nbbb", "task", section_chars=3))
        await entered.wait()
        if cancel:
            task.cancel()
        with pytest.raises(asyncio.CancelledError if cancel else BudgetExhausted):
            await task
    assert stopped.is_set()


@pytest.mark.anyio
async def test_disconnect_cancels_and_awaits_document_work():
    from fastapi import HTTPException
    entered, stopped = asyncio.Event(), asyncio.Event()
    class Request:
        async def is_disconnected(self):
            return entered.is_set()
    async def work():
        entered.set()
        try:
            await asyncio.Future()
        finally:
            stopped.set()
    with pytest.raises(HTTPException) as error:
        await connected(Request(), work())
    assert error.value.status_code == 499 and stopped.is_set()


@pytest.mark.anyio
async def test_http_endpoint_keeps_complete_reports(tmp_path, monkeypatch):
    path = tmp_path / "text.txt"
    path.write_text("evidence", encoding="utf-8")
    monkeypatch.setattr(main, "safe_path", lambda p: p)
    monkeypatch.setattr(main.settings, "shared_secret", "")
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=main.app), base_url="http://ml.test") as client:
        response = await client.post("/read-document", json={"path": str(path), "overrides": {"routing": routing()},
            "document_limits": limits().model_dump()})
    assert response.status_code == 200
    assert response.json()["routing_report"]["complete"]
    assert response.json()["fixed_routing_report"]["complete"]


def test_strict_pdf_parser_error_cannot_become_partial_success(monkeypatch):
    from contextlib import contextmanager
    from types import SimpleNamespace
    import pdfplumber
    class Page:
        def extract_text(self):
            raise ValueError("bad page")
    @contextmanager
    def opened(path):
        yield SimpleNamespace(pages=[Page()])
    monkeypatch.setattr(pdfplumber, "open", opened)
    with rag_ctx.scoped_overrides({"routing": {}}):
        with pytest.raises(ValueError, match="bad page"):
            extract._pdf_pages(__import__("pathlib").Path("doc.pdf"))
    with rag_ctx.scoped_overrides({"routing": None}):
        assert extract._pdf_pages(__import__("pathlib").Path("doc.pdf")) == [(1, "")]
