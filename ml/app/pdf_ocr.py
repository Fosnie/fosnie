# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Explicit image-only OCR adapter for bounded scanned/mixed PDF pages."""
import asyncio
import io
import math
import threading
from contextlib import closing
from . import ocr,rag_ctx
from .config import settings
from .inference_budget import BudgetExhausted
from .inference_target import InferenceEnvelope,strict

MAX_SCANNED_PAGES=8
MAX_PAGE_PIXELS=8_000_000
MAX_TOTAL_PIXELS=32_000_000
MAX_PAGE_BYTES=2*1024*1024
MAX_TOTAL_BYTES=8*1024*1024
_pdfium_lock=threading.Lock()  # PDFium forbids concurrent calls across threads.


def mode():
    if strict():
        target=InferenceEnvelope.model_validate(rag_ctx.cfg('routing',None)).fixed_assignments.get('ocr')
        if target is None:return 'document'  # native text needs no OCR assignment
        return target.ocr_pdf_mode or 'document'
    value=rag_ctx.cfg('ocr_pdf_mode',settings.ocr_pdf_mode)
    if value not in ('document','page_images'):raise ValueError('unsupported PDF OCR mode')
    return value


def _capacity(count):
    if not 0<count<=MAX_SCANNED_PAGES:raise BudgetExhausted('PDF scanned page cap exceeded')
    if strict():
        budget=rag_ctx.cfg('_routing_fixed_budget',None)
        if budget is None:raise BudgetExhausted('PDF OCR requires a fixed grant')
        with budget.lock:
            budget.remaining()
            allowance=budget.grant.allowances.get('ocr')
            if budget.closed or allowance is None or count>allowance.max_calls-budget.counts.get('ocr',0):
                raise BudgetExhausted('PDF scanned pages exceed remaining OCR grant')


def _render_pages(path,indexes):
    import pypdfium2 as pdfium
    images=[];total_pixels=0;total_bytes=0
    # Offloaded to a thread, but every PDFium call/close shares this lock.
    # Cancelling the await prevents subsequent OCR calls; the bounded render
    # finishes and closes its handles even if its owning request was cancelled.
    with _pdfium_lock, pdfium.PdfDocument(path) as document:
        geometry=[]
        for index in indexes:
            with closing(document[index]) as page:
                width,height=page.get_size()
            if not all(math.isfinite(v) and v>0 for v in (width,height)):
                raise ValueError('invalid PDF page geometry')
            pixels=math.ceil(width*2)*math.ceil(height*2)
            total_pixels+=pixels
            if pixels>MAX_PAGE_PIXELS or total_pixels>MAX_TOTAL_PIXELS:
                raise BudgetExhausted('PDF raster pixel cap exceeded')
            geometry.append(index)
        for index in geometry:
            with closing(document[index]) as page:
                bitmap=page.render(scale=2)
                try:
                    original=bitmap.to_pil()
                    try:image=original.convert('RGB')
                    finally:original.close()
                    try:
                        buffer=io.BytesIO();image.save(buffer,format='PNG');data=buffer.getvalue()
                    finally:image.close()
                finally:bitmap.close()
            total_bytes+=len(data)
            if len(data)>MAX_PAGE_BYTES or total_bytes>MAX_TOTAL_BYTES:
                raise BudgetExhausted('PDF raster byte cap exceeded')
            images.append((index,data))
    return images


async def extract_scanned_pages(path,pages,minimum_chars):
    indexes=[i for i,(_,text) in enumerate(pages) if len(text.strip())<minimum_chars]
    if not indexes:return pages
    _capacity(len(indexes))
    images=await asyncio.to_thread(_render_pages,path,indexes)
    _capacity(len(indexes))  # deadline/remaining grant may change during rendering
    result=list(pages)
    for index,image in images:
        text=await ocr.ocr_bytes(image,'image/png')
        result[index]=(pages[index][0],text)
    return result
