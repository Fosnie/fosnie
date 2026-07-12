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

"""The non-streaming /chat-step surfaces a read-timeout as a clean 504
(and an upstream error as 502) instead of a raw 500 after a long wait. No network: the
shared http client is a fake that raises."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import httpx
from fastapi.testclient import TestClient

import app.main as main
from app import llm

_BODY = {
    "messages": [{"role": "user", "content": "hi"}],
    "model": "local-model",
    "overrides": {"llm_base_url": "http://localhost:11500/v1", "llm_model": "local-model"},
}


def _post_with_client(monkeypatch, client) -> int:
    monkeypatch.setattr(llm.http_client, "get_client", lambda: client)
    with TestClient(main.app) as c:
        return c.post("/chat-step", json=_BODY).status_code


def test_read_timeout_becomes_504(monkeypatch):
    class _Timeout:
        async def post(self, *a, **k):
            raise httpx.ReadTimeout("timed out")

    assert _post_with_client(monkeypatch, _Timeout()) == 504


def test_upstream_error_becomes_502(monkeypatch):
    class _Broken:
        async def post(self, *a, **k):
            raise httpx.ConnectError("refused")

    assert _post_with_client(monkeypatch, _Broken()) == 502
