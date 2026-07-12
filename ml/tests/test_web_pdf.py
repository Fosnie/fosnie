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

"""Web-PDF branch: fetcher accepts application/pdf (raw bytes, size-capped);
the pipeline routes the bytes through the document extraction path; extraction
failure degrades to snippet-only."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

import httpx
import pytest

from app import extract
from app.config import settings
from app.web import fetcher, pipeline, ssrf
from app.web.fetcher import FetchError, FetchResult
from app.web.provider import SerpResult


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


@pytest.fixture
def fast_pacing(monkeypatch):
    monkeypatch.setattr(settings, "web_host_rps", 1000.0)
    monkeypatch.setattr(settings, "web_pacing_burst", 1000.0)


@pytest.fixture
def fake_dns(monkeypatch):
    async def fake_resolve(host):
        return "93.184.216.34"

    monkeypatch.setattr(ssrf, "resolve_and_validate", fake_resolve)


@pytest.fixture
def mock_transport(monkeypatch):
    state = {"handler": None}
    real_client = httpx.AsyncClient

    def client_factory(**kwargs):
        kwargs["transport"] = httpx.MockTransport(state["handler"])
        return real_client(**kwargs)

    monkeypatch.setattr(fetcher.httpx, "AsyncClient", client_factory)
    return state


def test_fetcher_accepts_pdf_raw_bytes(fast_pacing, fake_dns, mock_transport):
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200, headers={"content-type": "application/pdf"}, content=b"%PDF-1.4 fake"
        )

    mock_transport["handler"] = handler
    out = _run(fetcher.fetch_page("https://example.com/doc.pdf"))
    assert out.raw == b"%PDF-1.4 fake"
    assert out.body == ""
    assert out.content_type.startswith("application/pdf")


def test_fetcher_pdf_size_cap(fast_pacing, fake_dns, mock_transport, monkeypatch):
    monkeypatch.setattr(settings, "web_fetch_max_bytes", 64)

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200, headers={"content-type": "application/pdf"}, content=b"%PDF" + b"x" * 1024
        )

    mock_transport["handler"] = handler
    with pytest.raises(FetchError, match="over 64 bytes"):
        _run(fetcher.fetch_page("https://example.com/big.pdf"))


def _pdf_serp():
    return SerpResult(
        url="https://example.com/paper.pdf",
        title="A paper",
        snippet="Abstract of the paper.",
        published_date=None,
        engine="e",
    )


def test_pipeline_pdf_extracts_and_chunks(monkeypatch):
    async def fake_fetch(url):
        return FetchResult(
            final_url=url, status=200, body="", content_type="application/pdf",
            raw=b"%PDF-1.4 fake",
        )

    async def fake_extract(path, mime):
        assert path.endswith(".pdf"), "extractor is suffix-driven"
        assert mime == "application/pdf"
        return [(1, "Page one text of the paper."), (2, "Page two with the conclusion.")]

    monkeypatch.setattr(fetcher, "fetch_page", fake_fetch)
    monkeypatch.setattr(extract, "extract_pages_ocr", fake_extract)
    src = _run(pipeline._fetch_source(_pdf_serp(), asyncio.Semaphore(1)))
    assert not src.snippet_only
    assert any("conclusion" in c for c in src.chunks)
    assert src.domain == "example.com"


def test_pipeline_pdf_extraction_failure_degrades_to_snippet(monkeypatch):
    async def fake_fetch(url):
        return FetchResult(
            final_url=url, status=200, body="", content_type="application/pdf",
            raw=b"%PDF-1.4 fake",
        )

    async def broken_extract(path, mime):
        from app.ocr import OcrUnavailable

        raise OcrUnavailable("scanned PDF, OCR down")

    monkeypatch.setattr(fetcher, "fetch_page", fake_fetch)
    monkeypatch.setattr(extract, "extract_pages_ocr", broken_extract)
    src = _run(pipeline._fetch_source(_pdf_serp(), asyncio.Semaphore(1)))
    assert src.snippet_only, "extraction failure degrades to snippet-only"
    assert src.chunks == ["Abstract of the paper."]
