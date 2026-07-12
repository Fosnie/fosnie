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

"""Tiered fetcher policy: redirect-hop re-validation + cap, byte cap,
content-type screen. Mock transport — no network."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

import httpx
import pytest

from app.config import settings
from app.web import fetcher, ssrf
from app.web.fetcher import FetchError


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
    """Route the fetcher's AsyncClient through a MockTransport keyed on the
    Host header (the request URL itself carries the pinned IP)."""
    state = {"handler": None}

    real_client = httpx.AsyncClient

    def client_factory(**kwargs):
        kwargs["transport"] = httpx.MockTransport(state["handler"])
        return real_client(**kwargs)

    monkeypatch.setattr(fetcher.httpx, "AsyncClient", client_factory)
    return state


def test_pinned_request_carries_host_and_ip(fast_pacing, fake_dns, mock_transport):
    seen = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen["url"] = str(request.url)
        seen["host"] = request.headers.get("host")
        return httpx.Response(200, headers={"content-type": "text/html"}, content=b"<html>ok</html>")

    mock_transport["handler"] = handler
    out = _run(fetcher.fetch_page("http://example.com/page?q=1"))
    assert out.status == 200 and out.body == "<html>ok</html>"
    assert seen["host"] == "example.com", "Host header carries the real hostname"
    assert "93.184.216.34" in seen["url"], "connection pinned to the validated IP"
    assert out.final_url == "http://example.com/page?q=1", "reported URL stays hostname-shaped"


def test_redirect_cap(fast_pacing, fake_dns, mock_transport):
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(302, headers={"location": "http://example.com/loop"})

    mock_transport["handler"] = handler
    with pytest.raises(FetchError, match="redirect cap"):
        _run(fetcher.fetch_page("http://example.com/"))


def test_redirect_to_private_target_blocked(fast_pacing, fake_dns, mock_transport):
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(302, headers={"location": "http://169.254.169.254/latest/meta-data/"})

    mock_transport["handler"] = handler
    with pytest.raises(ssrf.SsrfBlocked):
        _run(fetcher.fetch_page("http://example.com/"))


def test_byte_cap_aborts(fast_pacing, fake_dns, mock_transport, monkeypatch):
    monkeypatch.setattr(settings, "web_fetch_max_bytes", 64)

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, headers={"content-type": "text/html"}, content=b"x" * 1024)

    mock_transport["handler"] = handler
    with pytest.raises(FetchError, match="over 64 bytes"):
        _run(fetcher.fetch_page("http://example.com/big"))


def test_non_text_content_type_refused(fast_pacing, fake_dns, mock_transport):
    # PDFs are accepted (the document-extraction branch); other
    # binary types are still refused.
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, headers={"content-type": "image/png"}, content=b"\x89PNG")

    mock_transport["handler"] = handler
    with pytest.raises(FetchError, match="unsupported content-type"):
        _run(fetcher.fetch_page("http://example.com/logo.png"))


def test_http_error_status_raises(fast_pacing, fake_dns, mock_transport):
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(403, headers={"content-type": "text/html"}, content=b"blocked")

    mock_transport["handler"] = handler
    with pytest.raises(FetchError, match="HTTP 403"):
        _run(fetcher.fetch_page("http://example.com/"))


def test_blocked_url_never_reaches_transport(fast_pacing, mock_transport):
    def handler(request: httpx.Request) -> httpx.Response:  # pragma: no cover
        raise AssertionError("transport must not be reached for a blocked URL")

    mock_transport["handler"] = handler
    with pytest.raises(ssrf.SsrfBlocked):
        _run(fetcher.fetch_page("http://127.0.0.1/admin"))
