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

"""A tool call emitted as TEXT in `content` (no native tool_calls)
is recovered into `tool_calls` so the loop EXECUTES it, and stripped from the visible
content — the `read_skill` UUID must never render. Plain prose is untouched. No
network: the shared http client is a canned-response fake (mirrors test_guided_decoding)."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import llm, rag_ctx

_SKILL_UUID = "5c111000-0000-0000-0000-000000000007"
_TEXT_CALL = '{"name": "read_skill", "arguments": {"skill_id": "' + _SKILL_UUID + '"}}'


class _Resp:
    status_code = 200

    def __init__(self, content: str) -> None:
        self._content = content

    def raise_for_status(self) -> None:
        pass

    def json(self) -> dict:
        return {"choices": [{"message": {"content": self._content}, "finish_reason": "stop"}], "usage": {}}


class _Client:
    def __init__(self, content: str) -> None:
        self._content = content

    async def post(self, url, json=None, headers=None):  # noqa: A002
        return _Resp(self._content)


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _chat_step(monkeypatch, content: str) -> dict:
    monkeypatch.setattr(llm.http_client, "get_client", lambda: _Client(content))
    rag_ctx.set_overrides({"llm_base_url": "http://localhost:11500/v1", "llm_model": "local-model"})
    try:
        return _run(llm.chat_step([{"role": "user", "content": "hi"}], None, {}, "local-model"))
    finally:
        rag_ctx.set_overrides({})


def test_text_emitted_tool_call_is_recovered_and_stripped(monkeypatch):
    out = _chat_step(monkeypatch, "I'll read it. " + _TEXT_CALL)
    assert out["tool_calls"] == [{"id": None, "name": "read_skill", "arguments": {"skill_id": _SKILL_UUID}}]
    # The UUID / JSON must be gone from the visible content.
    assert _SKILL_UUID not in out["content"]
    assert "arguments" not in out["content"]


def test_openai_function_wrapper_shape_is_recovered(monkeypatch):
    wrapped = '{"id": "call_1", "function": {"name": "read_skill", "arguments": {"skill_id": "' + _SKILL_UUID + '"}}}'
    out = _chat_step(monkeypatch, wrapped)
    assert out["tool_calls"][0]["name"] == "read_skill"
    assert out["tool_calls"][0]["arguments"] == {"skill_id": _SKILL_UUID}
    assert _SKILL_UUID not in out["content"]


def test_plain_prose_is_untouched(monkeypatch):
    out = _chat_step(monkeypatch, "The clause in section {a} applies to the company.")
    assert out["tool_calls"] == []
    assert out["content"] == "The clause in section {a} applies to the company."
