# Copyright 2026 Private AI Ltd (SC881079)
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Tabular review cell generation + Excel export.

One engine, two presentations (matrix grid / N=1 prose) — identical per-cell
mechanism. Each cell is **per-document extraction over a known set**: the
document's text is pulled once and a single LLM call answers one column's prompt
for it. Completeness is guaranteed by iterating documents × columns, never by
retrieval ranking over a base (which could silently drop a document).

Bounded concurrency via an asyncio.Semaphore so we never fan out N×M unbounded
calls; vLLM continuous batching sits beneath. Cells are streamed as they
complete so the caller (Rust) can persist + broadcast incrementally."""

from __future__ import annotations

import asyncio
import json
import math
from collections.abc import AsyncIterator
from typing import Any

from . import chunker, embeddings, extract, llm
from .config import settings

# Per-cell document budget (chars). Over-budget docs are truncated; targeted
# single-document retrieval for very large docs is a Pass-2 refinement.
DOC_BUDGET = 12000

_SYSTEM = (
    "You are a precise legal document analyst. Extract exactly what is asked from "
    "the provided document and nothing else. Respond ONLY with a single JSON object: "
    '{"value": <answer>, "reasoning": "<one short sentence>", '
    '"quote": "<verbatim snippet (<=25 words) from the document supporting the '
    'answer, or empty string>"}. Do not wrap it in markdown.'
)


def format_suffix(fmt: str) -> str:
    """Format-specific instruction appended to a column prompt (SYNTHESIS B.3)."""
    return {
        "yes_no": "Answer strictly Yes or No.",
        "date": "Answer with a single date (ISO 8601 YYYY-MM-DD if possible).",
        "currency": "Answer with a currency amount including its symbol or code.",
        "monetary_amount": "Answer with a monetary amount including its currency.",
        "number": "Answer with a single number only.",
        "percentage": "Answer with a single percentage.",
        "tag": "Answer with a single short label.",
        "bulleted_list": "Answer as a list of short bullet points.",
        "text": "Answer concisely in prose.",
    }.get(fmt, "Answer concisely.")


def _parse_json(text: str) -> dict[str, Any]:
    """Tolerantly pull the first JSON object out of a model reply (the model may
    add prose or markdown fences around it)."""
    text = text.strip()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass
    start = text.find("{")
    end = text.rfind("}")
    if start >= 0 and end > start:
        try:
            return json.loads(text[start : end + 1])
        except json.JSONDecodeError:
            pass
    return {"value": text, "reasoning": "", "quote": ""}


def coerce_value(fmt: str, raw: Any) -> Any:
    """Type the raw answer per the column format, returning a JSON-serialisable value."""
    if raw is None:
        return None
    if fmt == "yes_no":
        s = str(raw).strip().lower()
        if s in ("yes", "true", "y"):
            return True
        if s in ("no", "false", "n"):
            return False
        return str(raw)
    if fmt in ("number", "percentage"):
        if isinstance(raw, (int, float)):
            return raw
        import re

        m = re.search(r"-?\d+(?:\.\d+)?", str(raw))
        return float(m.group()) if m else str(raw)
    if fmt == "bulleted_list":
        if isinstance(raw, list):
            return raw
        return [ln.strip(" -•\t") for ln in str(raw).splitlines() if ln.strip()]
    return raw if isinstance(raw, (str, int, float, bool, list)) else str(raw)


def _trim_quote(quote: str) -> str:
    words = quote.split()
    return " ".join(words[:25])


def _cosine(a: list[float], b: list[float]) -> float:
    dot = sum(x * y for x, y in zip(a, b))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(y * y for y in b))
    return dot / (na * nb) if na and nb else 0.0


async def _relevant_text(text: str, query: str, k: int) -> str:
    """Single-document retrieval: chunk THIS document, embed the query + chunks,
    return the top-k most relevant chunks joined. Scoped to one document — never
    base RAG. Falls back to the whole text when it already fits k chunks."""
    chunks = chunker.chunk_text(text)
    if len(chunks) <= k:
        return text
    vecs = await embeddings.embed([query] + chunks)
    qv, cvs = vecs[0], vecs[1:]
    ranked = sorted(range(len(cvs)), key=lambda i: _cosine(qv, cvs[i]), reverse=True)
    top = sorted(ranked[:k])  # keep original order for readability
    return "\n\n".join(chunks[i] for i in top)


async def _select_text(doc_text: str, prompt: str, mechanism: str) -> str:
    """Pick the document text fed to the model, per the column mechanism.
    `stuff` uses the whole document but falls back to per-document retrieval when
    it is over budget (never silently truncate, for completeness)."""
    k = settings.tabular_topk
    if mechanism == "per_document_rag":
        return await _relevant_text(doc_text, prompt, k)
    if mechanism == "map_section":
        return await _relevant_text(doc_text, prompt, k * 3)
    # stuff (default)
    if len(doc_text) > DOC_BUDGET:
        return await _relevant_text(doc_text, prompt, k)
    return doc_text


async def _generate_cell(
    sem: asyncio.Semaphore, doc_id: str, doc_text: str, column: dict[str, Any]
) -> dict[str, Any]:
    fmt = column.get("format", "text")
    prompt = column.get("prompt", "")
    mechanism = column.get("mechanism", "stuff")
    async with sem:
        try:
            # Inside the semaphore AND the try (re-audit R3/R18): the per-cell
            # embed call is bounded by the review's concurrency budget (not an
            # unbounded all-cells fan-out against the shared HTTP pool), and an
            # embed failure/timeout degrades this one cell instead of raising
            # out of the task and killing the whole review stream.
            selected = await _select_text(doc_text, prompt, mechanism)
            user = f"{prompt}\n\n{format_suffix(fmt)}\n\n--- DOCUMENT ---\n{selected[:DOC_BUDGET]}"
            raw = await llm.complete(_SYSTEM, user, max_tokens=512)
            parsed = _parse_json(raw)
            value = coerce_value(fmt, parsed.get("value"))
            reasoning = str(parsed.get("reasoning") or "")
            quote = _trim_quote(str(parsed.get("quote") or ""))
            citations = (
                [{"document_id": doc_id, "quote_text": quote, "page_number": None, "clause_section_ref": None}]
                if quote
                else []
            )
            return {
                "type": "cell",
                "document_id": doc_id,
                "column_key": column["key"],
                "status": "done",
                "value": value,
                "reasoning": reasoning,
                "citations": citations,
            }
        except Exception as e:  # failure-isolation: one bad cell never poisons the review
            return {
                "type": "cell",
                "document_id": doc_id,
                "column_key": column["key"],
                "status": "error",
                "error": str(e)[:500],
            }


async def _extract_doc(path: str, mime: str | None) -> str:
    """Extract a review document to text. Images and PDFs go through the OCR-aware
    path (`extract.extract_pages_ocr`) so photos and scanned/text-less PDFs become
    transcribed text instead of binary garbage fed to the embedder (which the text
    embeddings endpoint rejects with a 400). Office/text formats stay on the native,
    off-event-loop path so large extractions don't block this generator."""
    is_image = (mime or "").startswith("image/") or path.lower().endswith(extract._IMAGE_SUFFIXES)
    if is_image or path.lower().endswith(".pdf"):
        pages = await extract.extract_pages_ocr(path, mime)
        return "\n".join(t for _, t in pages)
    return await asyncio.to_thread(extract.extract, path, mime)


async def generate_review(
    documents: list[dict[str, Any]],
    columns: list[dict[str, Any]],
    concurrency: int | None = None,
) -> AsyncIterator[dict[str, Any]]:
    """Stream `{type:'cell',...}` per (document × column), then `{type:'done'}`.
    `documents`: [{document_id, path, mime}]; `columns`: [{key, format, prompt}]."""
    n = concurrency or settings.tabular_concurrency
    sem = asyncio.Semaphore(max(1, n))

    # Extract each document's text once (shared across that doc's columns). Images
    # and scanned PDFs are OCR'd; a document with no readable text (or a failed
    # extraction/OCR) is reported as a single whole-doc `"*"` error and its cells are
    # skipped — we never embed empty/garbage text.
    texts: dict[str, str] = {}
    for d in documents:
        try:
            text = await _extract_doc(d["path"], d.get("mime"))
        except Exception as e:
            yield {
                "type": "cell",
                "document_id": d["document_id"],
                "column_key": "*",
                "status": "error",
                "error": f"extract failed: {e}",
            }
            continue
        if not text.strip():
            yield {
                "type": "cell",
                "document_id": d["document_id"],
                "column_key": "*",
                "status": "error",
                "error": "No extractable text — the file looks like an image or scan with no readable text.",
            }
            continue
        texts[d["document_id"]] = text

    tasks = [
        asyncio.create_task(_generate_cell(sem, d["document_id"], texts[d["document_id"]], col))
        for d in documents
        if d["document_id"] in texts
        for col in columns
    ]
    for fut in asyncio.as_completed(tasks):
        yield await fut
    yield {"type": "done"}


def export_xlsx(name: str, columns: list[dict[str, Any]], rows: list[dict[str, Any]], out_path: str) -> str:
    """Render the matrix to an .xlsx grid (header = column names; one row per
    document). `rows`: [{document, cells:{column_key: value}}]."""
    from openpyxl import Workbook

    wb = Workbook()
    ws = wb.active
    ws.title = (name or "Review")[:31]  # Excel sheet-name limit
    ws.append(["Document"] + [c.get("name", c["key"]) for c in columns])
    for row in rows:
        cells = row.get("cells", {})
        line = [row.get("document", "")]
        for c in columns:
            v = cells.get(c["key"])
            line.append(", ".join(map(str, v)) if isinstance(v, list) else ("" if v is None else str(v)))
        ws.append(line)
    wb.save(out_path)
    return out_path
