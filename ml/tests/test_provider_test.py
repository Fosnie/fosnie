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

"""Provider probe: minimal per-role health call. We mock
the shared httpx client so no real provider is hit, and assert the readable
error mapping plus the invariant that the api_key never appears in the output."""

import asyncio

import httpx

from app import provider_test, rag_ctx


def teardown_function() -> None:
    rag_ctx.set_overrides({})


class _FakeClient:
    """Stands in for the shared httpx client: each call returns a pre-built
    response or raises a pre-set exception."""

    def __init__(self, *, response=None, raises=None):
        self._response = response
        self._raises = raises

    async def post(self, url, json=None, headers=None, timeout=None):
        if self._raises is not None:
            raise self._raises
        # Attach a request so .raise_for_status() can build an HTTPStatusError.
        self._response.request = httpx.Request("POST", url)
        return self._response


def _patch_client(monkeypatch, **kw):
    monkeypatch.setattr(provider_test.http_client, "get_client", lambda: _FakeClient(**kw))


def test_embed_ok_reports_dimension(monkeypatch) -> None:
    resp = httpx.Response(200, json={"data": [{"embedding": [0.1, 0.2, 0.3], "index": 0}]})
    _patch_client(monkeypatch, response=resp)
    rag_ctx.set_overrides({"embed_base_url": "http://embed.x", "embed_model": "m", "embed_api_key": "sk-embed"})
    out = asyncio.run(provider_test.probe("embed"))
    assert out["ok"] is True
    assert out["detail"] == "dim=3"
    assert out["model"] == "m"
    assert isinstance(out["latency_ms"], float)


def test_bad_key_maps_to_invalid_and_does_not_leak(monkeypatch) -> None:
    resp = httpx.Response(401, json={"detail": "unauthorised"})
    _patch_client(monkeypatch, response=resp)
    rag_ctx.set_overrides({"llm_base_url": "http://llm.x", "llm_model": "m", "llm_api_key": "sk-supersecret"})
    out = asyncio.run(provider_test.probe("llm"))
    assert out["ok"] is False
    # The status reason is enriched with the provider's own message; the key must
    # never appear regardless.
    assert out["error"].startswith("invalid API key")
    assert "sk-supersecret" not in repr(out)


def test_unreachable_maps_to_cannot_reach(monkeypatch) -> None:
    _patch_client(monkeypatch, raises=httpx.ConnectError("connection refused"))
    rag_ctx.set_overrides({"rerank_base_url": "http://reranker.local:8091"})
    out = asyncio.run(provider_test.probe("rerank"))
    assert out["ok"] is False
    assert out["error"] == "cannot reach reranker.local"


def test_404_maps_to_wrong_endpoint(monkeypatch) -> None:
    resp = httpx.Response(404, json={"detail": "not found"})
    _patch_client(monkeypatch, response=resp)
    rag_ctx.set_overrides({"llm_base_url": "http://llm.x"})
    out = asyncio.run(provider_test.probe("llm"))
    assert out["ok"] is False
    assert out["error"].startswith("wrong endpoint shape (404)")


def test_key_scrubbed_from_arbitrary_error(monkeypatch) -> None:
    _patch_client(monkeypatch, raises=ValueError("boom sk-leakme123 in message"))
    rag_ctx.set_overrides({"llm_base_url": "http://llm.x", "llm_api_key": "sk-leakme123"})
    out = asyncio.run(provider_test.probe("llm"))
    assert out["ok"] is False
    assert "sk-leakme123" not in out["error"]
    assert "***" in out["error"]


def test_unknown_role() -> None:
    out = asyncio.run(provider_test.probe("nope"))
    assert out["ok"] is False
    assert "unknown role" in out["error"]
