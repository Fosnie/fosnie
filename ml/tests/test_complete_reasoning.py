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

"""The RAG scaffolding calls (`complete`) minimise reasoning so a
gpt-5.x model runs with reasoning OFF (`reasoning_effort="none"`) — the latency win —
while local/vLLM bodies stay byte-identical (field never sent). `utility_*` overrides
select a fast/cheap endpoint with `llm_*` fallback. No network: payload-capturing fake."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import pytest

from app import llm, rag_ctx


class _Resp:
    status_code = 200

    @staticmethod
    def raise_for_status() -> None:
        pass

    @staticmethod
    def json() -> dict:
        return {"choices": [{"message": {"content": "ok"}}], "usage": {}}


class _Client:
    def __init__(self) -> None:
        self.payloads: list[dict] = []

    async def post(self, url, json=None, headers=None):  # noqa: A002
        self.payloads.append(json)
        return _Resp()


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


@pytest.fixture
def capture(monkeypatch):
    client = _Client()
    monkeypatch.setattr(llm.http_client, "get_client", lambda: client)
    return client


def _complete(overrides: dict, **kw):
    rag_ctx.set_overrides(overrides)
    try:
        _run(llm.complete("sys", "user", max_tokens=4, **kw))
    finally:
        rag_ctx.set_overrides({})


def test_gpt5_reasoning_disabled(capture):
    _complete({"llm_base_url": "https://api.openai.com/v1", "llm_model": "gpt-5.4"})
    assert capture.payloads[-1]["reasoning_effort"] == "none"


def test_o_series_floor_is_low(capture):
    _complete({"llm_base_url": "https://api.openai.com/v1", "llm_model": "o1"})
    assert capture.payloads[-1]["reasoning_effort"] == "low"


def test_local_body_has_no_reasoning_field(capture):
    # vLLM/Ollama/local: the field must never be sent (byte-identical body).
    _complete({"llm_base_url": "http://localhost:11500/v1", "llm_model": "local-model"})
    assert "reasoning_effort" not in capture.payloads[-1]


def test_explicit_none_effort_omits_field(capture):
    _complete({"llm_base_url": "https://api.openai.com/v1", "llm_model": "gpt-5.4"}, reasoning_effort=None)
    assert "reasoning_effort" not in capture.payloads[-1]


def test_utility_model_override_wins_with_llm_fallback(capture):
    # utility_model selects the scaffolding model; llm_model is the fallback.
    _complete({
        "llm_base_url": "https://api.openai.com/v1",
        "llm_model": "gpt-5.4",
        "utility_model": "gpt-5.4-mini",
    })
    assert capture.payloads[-1]["model"] == "gpt-5.4-mini"

    _complete({"llm_base_url": "https://api.openai.com/v1", "llm_model": "gpt-5.4"})
    assert capture.payloads[-1]["model"] == "gpt-5.4"
