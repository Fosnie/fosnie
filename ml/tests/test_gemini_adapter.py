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

"""Native Gemini adapter translation."""

import app.gemini_adapter as ga
from app import rag_ctx


def test_is_gemini_native_excludes_openai_compat_path():
    assert ga.is_gemini_native("https://generativelanguage.googleapis.com/v1beta")
    # The OpenAI-compat shim lives under /openai and must be bypassed.
    assert not ga.is_gemini_native("https://generativelanguage.googleapis.com/v1beta/openai/")
    assert not ga.is_gemini_native("https://api.openai.com/v1")
    assert not ga.is_gemini_native("http://localhost:8000/v1")


def test_translate_roles_system_user_assistant_tool():
    msgs = [
        {"role": "system", "content": "be brief"},
        {"role": "user", "content": "weather?"},
        {"role": "assistant", "content": "", "tool_calls": [{"id": "c1", "function": {"name": "get_weather", "arguments": '{"loc":"Boston"}'}}]},
        {"role": "tool", "tool_call_id": "c1", "content": '{"temp": 20}'},
    ]
    system, contents = ga._translate(msgs)
    assert system == {"parts": [{"text": "be brief"}]}
    assert contents[0] == {"role": "user", "parts": [{"text": "weather?"}]}
    # Assistant tool call -> model role with a functionCall part.
    assert contents[1]["role"] == "model"
    assert contents[1]["parts"][0]["functionCall"] == {"name": "get_weather", "args": {"loc": "Boston"}}
    # Tool result -> user role with a functionResponse keyed by the call's name.
    fr = contents[2]
    assert fr["role"] == "user"
    assert fr["parts"][0]["functionResponse"] == {"name": "get_weather", "response": {"temp": 20}}


def test_translate_tools_to_function_declarations():
    tools = [{"type": "function", "function": {"name": "f", "description": "d", "parameters": {"type": "object"}}}]
    out = ga._translate_tools(tools)
    assert out == [{"functionDeclarations": [{"name": "f", "description": "d", "parameters": {"type": "object"}}]}]


def test_thinking_config_from_overrides():
    rag_ctx.set_overrides({"llm_reasoning_enabled": "true", "llm_reasoning_level": "high", "llm_reasoning_trace": "true"})
    try:
        assert ga._thinking_config() == {"thinkingBudget": ga._BUDGET["high"], "includeThoughts": True}
    finally:
        rag_ctx.set_overrides({})
    rag_ctx.set_overrides({"llm_reasoning_enabled": "true", "llm_reasoning_level": "auto", "llm_reasoning_trace": "false"})
    try:
        assert ga._thinking_config() == {"thinkingBudget": -1, "includeThoughts": False}
    finally:
        rag_ctx.set_overrides({})
    # Disabled → budget 0 (no-op on always-on Pro).
    rag_ctx.set_overrides({"llm_reasoning_enabled": "false"})
    try:
        assert ga._thinking_config() == {"thinkingBudget": 0}
    finally:
        rag_ctx.set_overrides({})


def test_parse_parts_excludes_thought_and_extracts_tool_calls():
    parts = [
        {"text": "thinking…", "thought": True},
        {"text": "the answer"},
        {"functionCall": {"name": "f", "args": {"x": 1}}},
    ]
    content, tool_calls = ga._parse_parts(parts)
    assert content == "the answer"
    assert tool_calls[0]["name"] == "f"
    assert tool_calls[0]["arguments"] == {"x": 1}


def test_map_usage_normalises_thoughts_tokens():
    u = ga._map_usage({"promptTokenCount": 10, "candidatesTokenCount": 20, "thoughtsTokenCount": 7, "totalTokenCount": 37})
    assert u["prompt_tokens"] == 10
    assert u["completion_tokens"] == 20
    assert u["reasoning_tokens"] == 7


def test_map_finish():
    assert ga._map_finish("STOP") == "stop"
    assert ga._map_finish("MAX_TOKENS") == "length"
    assert ga._map_finish(None) is None
