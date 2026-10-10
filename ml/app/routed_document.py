# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Bounded document reads. The caller supplies capacities from the pinned plan."""
import asyncio
from pathlib import Path

from pydantic import BaseModel, ConfigDict, Field

from . import extract, map_reduce, rag_ctx
from .inference_budget import BudgetExhausted
from .inference_target import with_report


class Limits(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)
    max_file_bytes: int = Field(gt=0, le=4 * 1024 * 1024, strict=True)
    output_bytes: int = Field(gt=0, le=16000, strict=True)
    max_sections: int = Field(gt=0, le=32, strict=True)


def clip(text: str, size: int) -> str:
    return text.encode("utf-8")[:size].decode("utf-8", errors="ignore")


async def connected(request, work, operation="document"):
    """Non-streaming ASGI handlers otherwise keep working after disconnect."""
    stopping = False
    async def disconnected():
        while not stopping:
            if await request.is_disconnected():
                return
            # Starlette probes receive inside a cancelled AnyIO scope. A task
            # cancellation arriving in that scope can be consumed by the probe;
            # an explicit stop flag also terminates that cleanup race.
            if stopping:
                return
            await asyncio.sleep(0.05)
    task = asyncio.create_task(work)
    watcher = asyncio.create_task(disconnected())
    try:
        await asyncio.wait([task, watcher], return_when=asyncio.FIRST_COMPLETED)
        if watcher.done():
            from fastapi import HTTPException
            raise HTTPException(status_code=499, detail=f"{operation} caller disconnected")
        return await task
    finally:
        stopping = True
        for child in (task, watcher):
            if not child.done():
                child.cancel()
        await asyncio.gather(task, watcher, return_exceptions=True)


async def read(path: str, mime: str | None, prompt: str | None, limits: Limits, *, terminal=True) -> dict:
    budget = rag_ctx.cfg("_routing_budget", None)
    fixed = rag_ctx.cfg("_routing_fixed_budget", None)
    if (budget is None or fixed is None or "document" not in budget.grant.allowances
            or "ocr" not in fixed.grant.allowances):
        raise BudgetExhausted("document read requires both bounded inference lanes")
    async with asyncio.timeout(budget.remaining()):
        p = Path(path)
        size = await asyncio.to_thread(lambda: p.stat().st_size)
        if size <= 0 or size > limits.max_file_bytes:
            raise ValueError("document exceeds routed file limit or is empty")
        suffix = p.suffix.lower()
        if suffix in extract._IMAGE_SUFFIXES or (mime or "").startswith("image/"):
            pages = await extract.extract_pages_ocr(path, mime)
            text = "\n".join(t for _, t in pages)
        elif suffix == ".pdf" or mime == "application/pdf":
            pages = await asyncio.to_thread(extract._pdf_pages, p)
            if any(len(t.strip()) < extract._SCANNED_MIN_CHARS for _, t in pages):
                # A mixed PDF must not silently lose its scanned pages.
                from . import ocr
                raw = await asyncio.to_thread(p.read_bytes)
                text = await ocr.ocr_bytes(raw, "application/pdf")
            else:
                text = "\n".join(t for _, t in pages)
        else:
            text = await asyncio.to_thread(extract.extract, path, mime)
        out = clip(text, limits.output_bytes)
        if out == text or not (prompt and prompt.strip()):
            return with_report({"mode": "stuff", "text": out, "truncated": out != text}, terminal=terminal)

        allowance = budget.grant.allowances["document"]
        # JSON escaping can expand one character to six bytes. Leave space for
        # the system instruction, field names and the token-template margin.
        section_chars = min(3000, (allowance.input_tokens - len(prompt.encode("utf-8")) * 6 - 2048) // 6)
        if section_chars <= 0:
            raise BudgetExhausted("document query exceeds section input allowance")
        from .config import settings
        if settings.read_document_max_chars <= 0:
            raise ValueError("invalid document extraction limit")
        capped = text[:min(120000, settings.read_document_max_chars)]
        result = await map_reduce.map_reduce(capped, prompt.strip(),
            section_chars=section_chars, max_sections=min(limits.max_sections, allowance.max_calls),
            output_tokens=min(512, allowance.output_tokens))
        digest = clip(result["text"], limits.output_bytes)
        return with_report({"mode": "map_reduce", "text": digest, "sections": len(result["sections"]),
                            "truncated": len(capped) < len(text) or result["truncated"] or digest != result["text"]}, terminal=terminal)
