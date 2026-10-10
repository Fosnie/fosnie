# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio
from types import SimpleNamespace

import pytest
from app import rag_ctx
from app.inference_budget import BudgetExhausted
from app.web import loop, pipeline, stream, robots, progress
from app.web.provider import SerpResult
from test_inference_target import clean_context, anyio_backend


@pytest.mark.anyio
@pytest.mark.parametrize("name,args", [
    ("_plan", ("question", "any", "2026-09-18", 3)),
    ("_grade", ("question", ["evidence"], "2026-09-18")),
    ("_reformulate", ("question",)),
    ("_detect_conflict", ("question", ["one", "two"])),
])
async def test_model_failure_is_not_successful_partial_evidence(monkeypatch, name, args):
    async def fail(*a, **kw):
        raise BudgetExhausted("denied")
    monkeypatch.setattr(loop.llm, "complete", fail)
    with rag_ctx.scoped_overrides({"routing": {}}):
        with pytest.raises(BudgetExhausted):
            await getattr(loop, name)(*args)
    with rag_ctx.scoped_overrides({}):
        await getattr(loop, name)(*args)  # legacy salvage remains supported


@pytest.mark.anyio
async def test_strict_pdf_ignores_unattested_page_cache_and_propagates_ocr(monkeypatch):
    result = SerpResult(url="https://public.test/doc.pdf", title="Document", snippet="snippet", published_date=None, engine="test")
    pipeline._page_cache.set(result.url, pipeline._Source(result.url, "old", "public.test", None, "now", False, ["old OCR"]))
    calls = []
    async def allowed(url):
        return True
    async def fetch(url):
        calls.append(url)
        return SimpleNamespace(raw=b"pdf", content_type="application/pdf")
    async def ocr(raw):
        raise BudgetExhausted("OCR denied")
    monkeypatch.setattr(robots, "allowed", allowed)
    monkeypatch.setattr(pipeline.fetcher, "fetch_page", fetch)
    monkeypatch.setattr(pipeline, "_extract_pdf", ocr)
    try:
        with rag_ctx.scoped_overrides({"routing": {}}):
            with pytest.raises(BudgetExhausted):
                await pipeline._fetch_source(result, asyncio.Semaphore(1))
        assert calls == [result.url]
    finally:
        pipeline._page_cache.clear()


@pytest.mark.anyio
@pytest.mark.parametrize("cancel_parent", [False, True])
async def test_strict_fanout_owns_children(cancel_parent):
    entered, stopped = asyncio.Event(), asyncio.Event()
    async def child():
        entered.set()
        try:
            await asyncio.Future()
        finally:
            stopped.set()
    async def fail():
        await entered.wait()
        raise BudgetExhausted("denied")
    with rag_ctx.scoped_overrides({"routing": {}}):
        task = asyncio.create_task(pipeline._gather(child(), *([] if cancel_parent else [fail()])))
        await entered.wait()
        if cancel_parent:
            task.cancel()
        with pytest.raises(asyncio.CancelledError if cancel_parent else BudgetExhausted):
            await task
    assert stopped.is_set()


@pytest.mark.anyio
async def test_disconnect_with_full_progress_queue_does_not_deadlock(monkeypatch):
    entered, stopped = asyncio.Event(), asyncio.Event()
    async def run(*a, **kw):
        try:
            progress.emit("first")
            await entered.wait()
            progress.emit("queued")
            await progress.emit_token("blocked")
        finally:
            stopped.set()
    monkeypatch.setattr(stream, "_QUEUE_MAX", 1)
    monkeypatch.setattr(loop, "run", run)
    events = stream.stream_events("query")
    await anext(events)
    entered.set()
    await asyncio.sleep(0)  # let the producer fill the queue and block on the token
    await asyncio.wait_for(events.aclose(), 1)
    assert stopped.is_set()
