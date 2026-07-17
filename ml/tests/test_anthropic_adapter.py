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

"""Native Anthropic adapter: pure OpenAI<->Anthropic
translation. No network — exercises the helpers directly on fixtures."""

import pytest

from app import anthropic_adapter as aa
from app import rag_ctx, thinking_cache


def teardown_function() -> None:
    rag_ctx.set_overrides({})
    thinking_cache.clear()


# --- detection ---------------------------------------------------------------


@pytest.mark.parametrize(
    "url,expected",
    [
        ("https://api.anthropic.com/v1", True),
        ("https://anthropic.com/v1", True),
        ("https://eu.api.anthropic.com/v1", True),
        ("http://localhost:11500/v1", False),
        ("https://api.openai.com/v1", False),
        ("https://api.anthropic.com.evil.test/v1", False),
        ("", False),
        (None, False),
    ],
)
def test_is_anthropic(url, expected):
    assert aa.is_anthropic(url) is expected


# --- request translation -----------------------------------------------------


def test_hoist_system_joins_and_strips():
    msgs = [
        {"role": "system", "content": "A"},
        {"role": "developer", "content": "B"},
        {"role": "user", "content": "hi"},
    ]
    system, rest = aa._hoist_system(msgs)
    assert system == "A\nB"
    assert rest == [{"role": "user", "content": "hi"}]


def test_translate_tools_renames_schema():
    tools = [
        {
            "type": "function",
            "function": {
                "name": "search",
                "description": "find",
                "parameters": {"type": "object", "properties": {"q": {"type": "string"}}},
            },
        }
    ]
    out = aa._translate_tools(tools)
    assert out == [
        {
            "name": "search",
            "description": "find",
            "input_schema": {"type": "object", "properties": {"q": {"type": "string"}}},
        }
    ]


def test_translate_tool_choice():
    assert aa._translate_tool_choice("auto") == {"type": "auto"}
    assert aa._translate_tool_choice("required") == {"type": "any"}
    assert aa._translate_tool_choice("none") == {"type": "none"}
    assert aa._translate_tool_choice({"type": "function", "function": {"name": "x"}}) == {"type": "tool", "name": "x"}
    assert aa._translate_tool_choice(None) is None


def test_assistant_tool_calls_to_tool_use_with_dict_input():
    msgs = [
        {
            "role": "assistant",
            "content": "let me check",
            "tool_calls": [
                {"id": "call_1", "function": {"name": "search", "arguments": '{"q": "dxy"}'}},
            ],
        }
    ]
    out = aa._translate_messages(msgs)
    assert out == [
        {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "let me check"},
                {"type": "tool_use", "id": "call_1", "name": "search", "input": {"q": "dxy"}},
            ],
        }
    ]


def test_consecutive_tool_messages_coalesce_into_one_user_message():
    msgs = [
        {"role": "tool", "tool_call_id": "call_1", "content": "r1"},
        {"role": "tool", "tool_call_id": "call_2", "content": "r2"},
        {"role": "user", "content": "thanks"},
    ]
    out = aa._translate_messages(msgs)
    assert out[0] == {
        "role": "user",
        "content": [
            {"type": "tool_result", "tool_use_id": "call_1", "content": "r1"},
            {"type": "tool_result", "tool_use_id": "call_2", "content": "r2"},
        ],
    }
    assert out[1] == {"role": "user", "content": "thanks"}


def test_build_body_required_fields_and_param_handling():
    body = aa._build_body(
        [{"role": "system", "content": "sys"}, {"role": "user", "content": "hi"}],
        {"temperature": 0.5, "frequency_penalty": 1, "top_p": 0.9},
        "claude-opus-4-8",
        stream=False,
    )
    assert body["model"] == "claude-opus-4-8"
    assert body["system"] == "sys"
    assert body["max_tokens"] > 0  # required by Anthropic
    assert body["temperature"] == 0.5
    assert body["top_p"] == 0.9
    assert "frequency_penalty" not in body  # unsupported -> dropped
    assert "thinking" not in body  # Phase 1


def test_build_body_clamps_temperature():
    body = aa._build_body([{"role": "user", "content": "x"}], {"temperature": 2.0}, "m", stream=True)
    assert body["temperature"] == 1.0
    assert body["stream"] is True


# --- response translation ----------------------------------------------------


def test_map_stop_reason():
    assert aa._map_stop_reason("end_turn") == "stop"
    assert aa._map_stop_reason("stop_sequence") == "stop"
    assert aa._map_stop_reason("max_tokens") == "length"
    assert aa._map_stop_reason("tool_use") == "tool_calls"
    assert aa._map_stop_reason("refusal") == "content_filter"


def test_map_usage():
    assert aa._map_usage({"input_tokens": 10, "output_tokens": 4}) == {
        "prompt_tokens": 10,
        "completion_tokens": 4,
        "total_tokens": 14,
    }


def test_parse_message_content_text_and_tool_use():
    blocks = [
        {"type": "text", "text": "hello "},
        {"type": "text", "text": "world"},
        {"type": "tool_use", "id": "tu_1", "name": "search", "input": {"q": "x"}},
    ]
    text, calls = aa._parse_message_content(blocks)
    assert text == "hello world"
    assert calls == [{"id": "tu_1", "name": "search", "arguments": {"q": "x"}}]


# --- streaming reassembly ----------------------------------------------------


def test_handle_stream_event_reassembly():
    state: dict = {"usage": None, "finish": None}
    tokens: list[str] = []
    events = [
        {"type": "message_start", "message": {"usage": {"input_tokens": 12}}},
        {"type": "content_block_start", "content_block": {"type": "text"}},
        {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "Hel"}},
        {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "lo"}},
        {"type": "ping"},
        {"type": "content_block_stop"},
        {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}},
        {"type": "message_stop"},
    ]
    for obj in events:
        for ev in aa._handle_stream_event(obj, state):
            assert ev["type"] == "token"
            tokens.append(ev["delta"])
    assert "".join(tokens) == "Hello"
    assert state["finish"] == "stop"
    assert state["usage"] == {"prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17}


def test_handle_stream_event_raises_on_error():
    from app.llm import LlmError

    with pytest.raises(LlmError):
        aa._handle_stream_event({"type": "error", "error": {"type": "overloaded_error", "message": "busy"}}, {})


def test_handle_stream_event_ignores_orphan_tool_json_delta():
    # An input_json_delta with no preceding tool_use block-start is dropped entirely —
    # it never leaks into the answer token stream, and there is no block to accumulate into.
    state: dict = {}
    out = aa._handle_stream_event(
        {"type": "content_block_delta", "index": 0,
         "delta": {"type": "input_json_delta", "partial_json": '{"q":'}}, state
    )
    assert out == []


def test_handle_stream_event_emits_tool_call_on_block_stop():
    # A tool_use block: start (id+name) → argument fragments → stop ⇒ one tool_call event,
    # never framed as answer tokens.
    state: dict = {"trace_on": True}
    assert aa._handle_stream_event(
        {"type": "content_block_start", "index": 0,
         "content_block": {"type": "tool_use", "id": "tu_1", "name": "search_library"}}, state
    ) == []
    assert aa._handle_stream_event(
        {"type": "content_block_delta", "index": 0,
         "delta": {"type": "input_json_delta", "partial_json": '{"query":'}}, state
    ) == []
    assert aa._handle_stream_event(
        {"type": "content_block_delta", "index": 0,
         "delta": {"type": "input_json_delta", "partial_json": '"ratification"}'}}, state
    ) == []
    out = aa._handle_stream_event({"type": "content_block_stop", "index": 0}, state)
    assert out == [{
        "type": "tool_call", "id": "tu_1", "name": "search_library",
        "arguments": {"query": "ratification"},
    }]


def test_handle_stream_event_drops_malformed_tool_args():
    state: dict = {}
    aa._handle_stream_event(
        {"type": "content_block_start", "index": 0,
         "content_block": {"type": "tool_use", "id": "tu_1", "name": "search_library"}}, state
    )
    aa._handle_stream_event(
        {"type": "content_block_delta", "index": 0,
         "delta": {"type": "input_json_delta", "partial_json": "{not json"}}, state
    )
    assert aa._handle_stream_event({"type": "content_block_stop", "index": 0}, state) == []


# --- Phase 2: extended thinking ----------------------------------------------


def test_thinking_config_off():
    rag_ctx.set_overrides({"llm_thinking": "off"})
    assert aa._thinking_config() == ({}, False)
    rag_ctx.set_overrides({})  # default settings value is "off"
    assert aa._thinking_config() == ({}, False)


def test_thinking_config_budget():
    rag_ctx.set_overrides({"llm_thinking": "budget:2048"})
    frag, on = aa._thinking_config()
    assert on is True
    assert frag == {"thinking": {"type": "enabled", "budget_tokens": 2048}}


def test_thinking_config_budget_floor():
    rag_ctx.set_overrides({"llm_thinking": "budget:100"})
    frag, _ = aa._thinking_config()
    assert frag["thinking"]["budget_tokens"] == 1024  # floored to the minimum


def test_thinking_config_adaptive_and_effort():
    rag_ctx.set_overrides({"llm_thinking": "adaptive"})
    frag, on = aa._thinking_config()
    assert on is True
    assert frag == {"thinking": {"type": "adaptive", "display": "summarized"}}

    rag_ctx.set_overrides({"llm_thinking": "adaptive:high"})
    frag, _ = aa._thinking_config()
    assert frag["thinking"] == {"type": "adaptive", "display": "summarized"}
    assert frag["output_config"] == {"effort": "high"}


def test_build_body_thinking_drops_sampling_and_downgrades_tool_choice():
    rag_ctx.set_overrides({"llm_thinking": "adaptive"})
    tools = [{"type": "function", "function": {"name": "s", "parameters": {"type": "object"}}}]
    body = aa._build_body(
        [{"role": "user", "content": "hi"}],
        {"temperature": 0.7, "top_p": 0.9, "tool_choice": "required"},
        "claude-opus-4-8",
        stream=False,
        tools=tools,
    )
    assert body["thinking"] == {"type": "adaptive", "display": "summarized"}
    assert "temperature" not in body  # removed under thinking
    assert "top_p" not in body
    assert body["tool_choice"] == {"type": "auto"}  # forced -> auto


def test_build_body_thinking_bumps_max_tokens_above_budget():
    rag_ctx.set_overrides({"llm_thinking": "budget:10000"})
    body = aa._build_body([{"role": "user", "content": "x"}], {"max_tokens": 4096}, "m", stream=False)
    assert body["max_tokens"] == 10000 + 1024  # bumped above budget_tokens


def test_round_trip_rehydrates_cached_blocks_verbatim():
    raw_blocks = [
        {"type": "thinking", "thinking": "reasoning…", "signature": "sig-abc"},
        {"type": "tool_use", "id": "tu_1", "name": "search", "input": {"q": "x"}},
    ]
    thinking_cache.put(["tu_1"], raw_blocks)
    msgs = [
        {"role": "assistant", "content": "", "tool_calls": [{"id": "tu_1", "function": {"name": "search", "arguments": "{}"}}]},
    ]
    out = aa._translate_messages(msgs, thinking_on=True)
    # Cached blocks replayed verbatim (thinking + signature preserved).
    assert out == [{"role": "assistant", "content": raw_blocks}]


def test_round_trip_reconstructs_without_thinking_on_or_cache_miss():
    msgs = [
        {"role": "assistant", "content": "", "tool_calls": [{"id": "tu_9", "function": {"name": "search", "arguments": '{"q":"x"}'}}]},
    ]
    # thinking off -> Phase-1 reconstruction, no thinking blocks.
    out = aa._translate_messages(msgs, thinking_on=False)
    assert out == [{"role": "assistant", "content": [{"type": "tool_use", "id": "tu_9", "name": "search", "input": {"q": "x"}}]}]
    # thinking on but cache miss -> also reconstructs (no 400-causing fabricated blocks).
    out2 = aa._translate_messages(msgs, thinking_on=True)
    assert out2[0]["content"][0]["type"] == "tool_use"


def test_handle_stream_event_splits_reasoning_and_answer_channels():
    # Thinking deltas go on the dedicated `reasoning` channel; answer text on
    # `token`. Signature deltas are round-trip-only.
    state: dict = {"trace_on": True}
    reasoning: list[str] = []
    answer: list[str] = []
    events = [
        {"type": "content_block_delta", "delta": {"type": "thinking_delta", "thinking": "let me "}},
        {"type": "content_block_delta", "delta": {"type": "thinking_delta", "thinking": "think"}},
        {"type": "content_block_delta", "delta": {"type": "signature_delta", "signature": "sig"}},
        {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "answer"}},
    ]
    for obj in events:
        for ev in aa._handle_stream_event(obj, state):
            (reasoning if ev["type"] == "reasoning" else answer).append(ev["delta"])
    assert "".join(reasoning) == "let me think"
    assert "".join(answer) == "answer"


def test_handle_stream_event_suppresses_reasoning_when_trace_off():
    state: dict = {"trace_on": False}
    out = []
    for obj in [
        {"type": "content_block_delta", "delta": {"type": "thinking_delta", "thinking": "secret"}},
        {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "answer"}},
    ]:
        out.extend(aa._handle_stream_event(obj, state))
    assert out == [{"type": "token", "delta": "answer"}]


def test_thinking_cache_ttl_eviction(monkeypatch):
    import app.thinking_cache as tc

    monkeypatch.setattr(tc, "_TTL_SECONDS", -1.0)  # already expired on insert
    tc.put(["tu_x"], [{"type": "tool_use", "id": "tu_x"}])
    assert tc.get("tu_x") is None
