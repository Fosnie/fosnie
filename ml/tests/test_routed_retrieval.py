# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import asyncio

import pytest

from app import rag_ctx, retrieve
from app.inference_budget import BudgetExhausted
from test_inference_target import clean_context, anyio_backend


@pytest.mark.anyio
async def test_strict_decompose_does_not_salvage_failed_inference(monkeypatch):
    async def fail(*args, **kwargs):
        raise BudgetExhausted("denied")
    monkeypatch.setattr(retrieve.llm, "complete", fail)
    with rag_ctx.scoped_overrides({"routing": {}}):
        with pytest.raises(BudgetExhausted):
            await retrieve._decompose("Find the clause")
    with rag_ctx.scoped_overrides({}):
        items, _ = await retrieve._decompose("Find the clause")
        assert items[0]["subq"] == "Find the clause"


@pytest.mark.anyio
async def test_strict_subanswer_failure_is_not_missing_evidence(monkeypatch):
    async def fail(*args, **kwargs):
        raise BudgetExhausted("denied")
    monkeypatch.setattr(retrieve.llm, "complete", fail)
    with rag_ctx.scoped_overrides({"routing": {}}):
        with pytest.raises(BudgetExhausted):
            await retrieve._answer_over("clause?", "question", [{"chunk_text": "source"}], asyncio.Semaphore(1))


@pytest.mark.anyio
async def test_strict_fanout_raises_even_when_legacy_asks_for_exception_values():
    entered, stopped = asyncio.Event(), asyncio.Event()
    async def sibling():
        entered.set()
        try:
            await asyncio.Future()
        finally:
            stopped.set()
    async def fail():
        await entered.wait()
        raise BudgetExhausted("denied")
    with rag_ctx.scoped_overrides({"routing": {}}):
        with pytest.raises(BudgetExhausted):
            await retrieve._gather(sibling(), fail(), return_exceptions=True)
    assert stopped.is_set()


@pytest.mark.anyio
async def test_strict_fanout_parent_cancellation_awaits_children():
    entered, stopped = asyncio.Event(), asyncio.Event()
    async def child():
        entered.set()
        try:
            await asyncio.Future()
        finally:
            stopped.set()
    with rag_ctx.scoped_overrides({"routing": {}}):
        task = asyncio.create_task(retrieve._gather(child()))
        await entered.wait()
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
    assert stopped.is_set()
