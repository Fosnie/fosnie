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

"""The OpenAI Responses-API path streams SUMMARISED reasoning before
the first answer token, and falls back to chat-completions on rejection. No network."""

import asyncio
import json
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import pytest

from app import llm, rag_ctx


class _FakeStream:
    def __init__(self, status: int, lines: list[str]) -> None:
        self.status_code = status
        self._lines = lines

    async def __aenter__(self):
        return self

    async def __aexit__(self, *a):
        return False

    async def aiter_lines(self):
        for line in self._lines:
            yield line

    async def aread(self) -> bytes:
        return b'{"error":"unsupported"}'


class _FakeClient:
    """Routes /responses vs /chat/completions so a 4xx on /responses can be shown to
    fall back to the chat path."""

    def __init__(self, responses_status: int, responses_lines: list[str], chat_lines: list[str]) -> None:
        self._responses = (responses_status, responses_lines)
        self._chat = chat_lines
        self.urls: list[str] = []

    def stream(self, method, url, json=None, headers=None):  # noqa: A002
        self.urls.append(url)
        if url.endswith("/responses"):
            return _FakeStream(*self._responses)
        return _FakeStream(200, self._chat)


def _run_stream(sampling, model):
    async def _collect():
        return [ev async for ev in llm.stream_chat([{"role": "user", "content": "hi"}], sampling, model)]

    return asyncio.new_event_loop().run_until_complete(_collect())


@pytest.fixture(autouse=True)
def _openai(monkeypatch):
    rag_ctx.set_overrides({"llm_base_url": "https://api.openai.com/v1", "llm_api_key": "sk-x"})
    yield
    rag_ctx.set_overrides({})


def test_responses_streams_reasoning_then_tokens(monkeypatch):
    lines = [
        'data: {"type":"response.reasoning_summary_text.delta","delta":"weighing the clause"}',
        'data: {"type":"response.output_text.delta","delta":"Hello"}',
        'data: {"type":"response.output_text.delta","delta":" world"}',
        'data: {"type":"response.completed","response":{"usage":'
        '{"input_tokens":10,"output_tokens":5,"output_tokens_details":{"reasoning_tokens":3}}}}',
        "data: [DONE]",
    ]
    client = _FakeClient(200, lines, [])
    monkeypatch.setattr(llm.http_client, "get_client", lambda: client)

    events = _run_stream({"reasoning_effort": "medium"}, "gpt-5.4")

    assert any(e["url"].endswith("/responses") for e in [{"url": u} for u in client.urls])
    reasoning = [e["delta"] for e in events if e["type"] == "reasoning"]
    tokens = [e["delta"] for e in events if e["type"] == "token"]
    done = [e for e in events if e["type"] == "done"][0]
    assert reasoning == ["weighing the clause"], "summary reasoning streamed BEFORE the answer"
    assert "".join(tokens) == "Hello world"
    assert done["usage"]["prompt_tokens"] == 10
    assert done["usage"]["completion_tokens"] == 5
    assert done["usage"]["reasoning_tokens"] == 3


def test_falls_back_to_chat_completions_on_rejection(monkeypatch):
    chat_lines = [
        'data: {"choices":[{"delta":{"reasoning_content":"local think"}}]}',
        'data: {"choices":[{"delta":{"content":"Answer"}}]}',
        'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}',
        "data: [DONE]",
    ]
    client = _FakeClient(400, ['data: {"type":"error"}'], chat_lines)
    monkeypatch.setattr(llm.http_client, "get_client", lambda: client)

    events = _run_stream({"reasoning_effort": "medium"}, "gpt-5.4")

    # Both endpoints were hit: /responses (rejected) then /chat/completions (served).
    assert any(u.endswith("/responses") for u in client.urls)
    assert any(u.endswith("/chat/completions") for u in client.urls)
    tokens = "".join(e["delta"] for e in events if e["type"] == "token")
    assert tokens == "Answer", "fallback produced the answer via chat-completions"


def test_no_responses_when_reasoning_off(monkeypatch):
    # Reasoning not requested ⇒ never take the Responses path (nothing to summarise).
    chat_lines = [
        'data: {"choices":[{"delta":{"content":"plain"}}]}',
        'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}',
        "data: [DONE]",
    ]
    client = _FakeClient(200, [], chat_lines)
    monkeypatch.setattr(llm.http_client, "get_client", lambda: client)

    events = _run_stream({}, "gpt-5.4")  # no reasoning_effort, no per-turn enable
    assert all(not u.endswith("/responses") for u in client.urls)
    assert "".join(e["delta"] for e in events if e["type"] == "token") == "plain"
