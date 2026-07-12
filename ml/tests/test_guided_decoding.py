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

"""The vLLM guided-decoding fragment set via
`llm.set_guided` is merged into the `/chat/completions` body IFF
`settings.llm_guided_decoding` is on — so the vLLM profile constrains the decode
while dev (Ollama) / Profile B (llama.cpp) post the byte-identical prompt-only
body. The fragment rides a ContextVar (like `set_stage`), so it is consumed once
and never leaks onto the next untagged call. No network: the shared http client
is replaced with a payload-capturing fake."""

import asyncio
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import pytest

from app import guided, llm
from app.config import settings


class _Resp:
    status_code = 200

    @staticmethod
    def raise_for_status() -> None:
        pass

    @staticmethod
    def json() -> dict:
        return {"choices": [{"message": {"content": "yes"}}], "usage": {}}


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


def test_guided_merged_when_flag_on(capture, monkeypatch):
    monkeypatch.setattr(settings, "llm_guided_decoding", True)

    async def scenario():
        llm.set_guided(guided.GRADE)
        await llm.complete("sys", "user", max_tokens=4)

    _run(scenario())
    body = capture.payloads[-1]
    assert body["guided_choice"] == ["yes", "partial", "no"]


def test_guided_absent_when_flag_off(capture, monkeypatch):
    monkeypatch.setattr(settings, "llm_guided_decoding", False)

    async def scenario():
        llm.set_guided(guided.GRADE)
        await llm.complete("sys", "user", max_tokens=4)

    _run(scenario())
    body = capture.payloads[-1]
    assert "guided_choice" not in body and "guided_json" not in body


def test_no_guided_set_leaves_body_clean(capture, monkeypatch):
    monkeypatch.setattr(settings, "llm_guided_decoding", True)
    _run(llm.complete("sys", "user"))  # nobody called set_guided
    body = capture.payloads[-1]
    assert "guided_choice" not in body and "guided_json" not in body


def test_guided_consumed_once(capture, monkeypatch):
    monkeypatch.setattr(settings, "llm_guided_decoding", True)

    async def scenario():
        llm.set_guided(guided.GRADE)
        await llm.complete("sys", "user")  # consumes the fragment
        await llm.complete("sys", "user")  # nothing set → no guided

    _run(scenario())
    assert "guided_choice" in capture.payloads[0]
    assert "guided_choice" not in capture.payloads[1]


def test_decompose_schema_shape():
    schema = guided.DECOMPOSE["guided_json"]
    assert schema["type"] == "array"
    item = schema["items"]
    assert set(item["required"]) == {"subq", "queries"}
    assert item["properties"]["queries"]["type"] == "array"


def test_grade_choice_is_the_three_labels():
    assert guided.GRADE == {"guided_choice": ["yes", "partial", "no"]}
