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

"""Streaming tool-call accumulation for `/generate`: the model can request a tool
mid-answer. Exercises the pure helpers directly — no network."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import llm


def test_finalise_accumulates_two_index_keyed_calls():
    # Fragments have already been merged per index (id/name from an early fragment,
    # args concatenated); finalise parses each once and emits in index order.
    acc = {
        0: {"id": "a", "name": "search_library", "args": '{"query":"ratification"}'},
        1: {"id": "b", "name": "search_library", "args": '{"query":"s239","sections":["239"]}'},
    }
    evs = llm._finalise_stream_tool_calls(acc)
    assert [e["type"] for e in evs] == ["tool_call", "tool_call"]
    assert evs[0]["id"] == "a" and evs[0]["arguments"] == {"query": "ratification"}
    assert evs[1]["arguments"] == {"query": "s239", "sections": ["239"]}


def test_finalise_drops_malformed_and_nameless():
    acc = {
        0: {"id": "a", "name": "search_library", "args": "{not valid json"},
        1: {"id": "b", "name": None, "args": "{}"},
        2: {"id": "c", "name": "  ", "args": "{}"},
    }
    assert llm._finalise_stream_tool_calls(acc) == []


def test_finalise_empty_args_defaults_to_object():
    acc = {0: {"id": "a", "name": "search_library", "args": ""}}
    evs = llm._finalise_stream_tool_calls(acc)
    assert evs == [{"type": "tool_call", "id": "a", "name": "search_library", "arguments": {}}]


def test_responses_tools_flattens_chat_completions_schema():
    tools = [{
        "type": "function",
        "function": {"name": "search_library", "description": "d", "parameters": {"type": "object"}},
    }]
    out = llm._responses_tools(tools)
    assert out == [{
        "type": "function", "name": "search_library", "description": "d",
        "parameters": {"type": "object"},
    }]
    # A malformed entry (no name) is skipped.
    assert llm._responses_tools([{"type": "function", "function": {}}]) == []
    assert llm._responses_tools(None) == []
