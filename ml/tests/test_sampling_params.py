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

"""ts-sampling-params: every agent sampling param the backend carries
(`temperature, top_p, max_tokens, frequency_penalty, presence_penalty`) must reach
the OpenAI-shape `chat_step` payload when set — `top_p` was the one being dropped.
Unset params stay absent (provider default wins); OpenAI reasoning models omit all
sampling (they reject it). No network: the shared http client is a payload-capturing
fake (mirrors test_guided_decoding)."""

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
    """Captures every posted JSON body; returns a canned completion."""

    def __init__(self) -> None:
        self.payloads: list[dict] = []

    async def post(self, url, json=None, headers=None):  # noqa: A002 — httpx kw name
        self.payloads.append(json)
        return _Resp()


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


@pytest.fixture
def capture(monkeypatch):
    client = _Client()
    monkeypatch.setattr(llm.http_client, "get_client", lambda: client)
    return client


def _msgs():
    return [{"role": "user", "content": "hi"}]


def test_local_model_forwards_all_five(capture):
    # A local (non-OpenAI) endpoint accepts sampling → every set param is forwarded.
    rag_ctx.set_overrides({"llm_base_url": "http://localhost:11500/v1", "llm_model": "local-model"})
    try:
        sampling = {
            "temperature": 0.3,
            "top_p": 0.85,
            "max_tokens": 256,
            "frequency_penalty": 0.2,
            "presence_penalty": 0.1,
        }
        _run(llm.chat_step(_msgs(), None, sampling, "local-model"))
    finally:
        rag_ctx.set_overrides({})
    body = capture.payloads[-1]
    assert body["temperature"] == 0.3
    assert body["top_p"] == 0.85  # the param that used to be dropped
    assert body["max_tokens"] == 256
    assert body["frequency_penalty"] == 0.2
    assert body["presence_penalty"] == 0.1


def test_unset_top_p_is_omitted(capture):
    # No top_p on the Sampling → key absent, so the provider default is preserved.
    rag_ctx.set_overrides({"llm_base_url": "http://localhost:11500/v1", "llm_model": "local-model"})
    try:
        _run(llm.chat_step(_msgs(), None, {"temperature": 0.5}, "local-model"))
    finally:
        rag_ctx.set_overrides({})
    body = capture.payloads[-1]
    assert "top_p" not in body
    assert body["temperature"] == 0.5


def test_openai_reasoning_omits_all_sampling(capture):
    # gpt-5.x reasoning models reject sampling → the omit-sampling gate drops all of it.
    rag_ctx.set_overrides({"llm_base_url": "https://api.openai.com/v1", "llm_model": "gpt-5.5"})
    try:
        sampling = {"temperature": 0.3, "top_p": 0.85, "frequency_penalty": 0.2, "presence_penalty": 0.1}
        _run(llm.chat_step(_msgs(), None, sampling, "gpt-5.5"))
    finally:
        rag_ctx.set_overrides({})
    body = capture.payloads[-1]
    assert "temperature" not in body
    assert "top_p" not in body
    assert "frequency_penalty" not in body
    assert "presence_penalty" not in body
