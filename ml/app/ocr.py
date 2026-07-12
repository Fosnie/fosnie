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

"""OCR via GLM-OCR over an OpenAI-compatible vision endpoint. The OCR *service* handles the whole file — including rasterising
scanned PDFs — so the platform sends the document as-is (base64 data URI) and
never rasterises itself (PyMuPDF/Marker are struck on licence). GLM-OCR returns
text + structure (no bounding boxes).

A swappable interface: only the model id / base URL change per deployment. When
OCR is disabled or the call fails/returns nothing, `OcrUnavailable` is raised so
the ingest pipeline marks the document `error` rather than indexing empty text."""

import base64
import logging

from . import http_client
from .config import settings
from .llm import _gen_caps
from .rag_ctx import cfg

_log = logging.getLogger("ocr")

_INSTRUCTION = (
    "Transcribe ALL text from this document exactly, preserving reading order and "
    "table structure as plain text. Output only the transcribed text."
)


class OcrUnavailable(RuntimeError):
    """OCR is off, unreachable, or produced no text — the caller must not index
    empty content (it should surface an ingest error instead)."""


def _base_url() -> str:
    return (cfg("ocr_base_url", settings.ocr_base_url) or cfg("llm_base_url", settings.llm_base_url)).rstrip("/")


async def ocr_bytes(data: bytes, mime: str) -> str:
    """OCR one document (image or PDF). The OCR service rasterises/segments as
    needed; we just hand it the file. Returns the transcribed text."""
    if not cfg("ocr_enabled", settings.ocr_enabled):
        raise OcrUnavailable("OCR is disabled (set OCR_ENABLED=true to index scanned PDFs / images)")
    if not data:
        raise OcrUnavailable("empty file")

    b64 = base64.b64encode(data).decode("ascii")
    # OpenAI reasoning models (gpt-5.x / o-series) renamed `max_tokens` →
    # `max_completion_tokens` and reject a non-default `temperature`; `_gen_caps`
    # picks the right field + whether sampling is allowed (non-OpenAI unchanged).
    base = _base_url()
    model = cfg("ocr_model", settings.ocr_model)
    token_key, sampling_ok = _gen_caps(base, model)
    payload = {
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": _INSTRUCTION},
                    {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{b64}"}},
                ],
            }
        ],
        token_key: 8192,
    }
    if sampling_ok:
        payload["temperature"] = 0
    headers = {"Authorization": f"Bearer {cfg('ocr_api_key', settings.ocr_api_key)}"}
    url = f"{base}/chat/completions"
    try:
        client = http_client.get_client()
        r = await client.post(url, json=payload, headers=headers, timeout=settings.ocr_timeout)
        r.raise_for_status()
        text = r.json()["choices"][0]["message"]["content"] or ""
    except Exception as e:  # network / HTTP / shape — all become OcrUnavailable
        raise OcrUnavailable(f"OCR request failed: {e}") from e

    if not text.strip():
        raise OcrUnavailable("OCR returned no text")
    return text
