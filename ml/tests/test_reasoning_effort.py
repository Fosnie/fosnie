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

"""Capability-aware reasoning translation in the OpenAI path."""

import app.llm as llm
from app import rag_ctx


def test_openai_effort_only_for_reasoning_models():
    # Non-reasoning chat model → never send (hard 400 otherwise).
    assert llm._openai_reasoning_effort("gpt-4o", {"enabled": True, "level": "high"}) is None
    # gpt-5.x reasoning model → level passes through.
    assert llm._openai_reasoning_effort("gpt-5.5", {"enabled": True, "level": "high"}) == "high"


def test_openai_effort_disable_per_family():
    # gpt-5.x can fully disable; o-series cannot → omit.
    assert llm._openai_reasoning_effort("gpt-5.5", {"enabled": False}) == "none"
    assert llm._openai_reasoning_effort("o3", {"enabled": False}) is None


def test_openai_effort_minimal_and_auto():
    # Utility fast-path: gpt-5 → none; o-series floor → low.
    assert llm._openai_reasoning_effort("gpt-5.5", {"enabled": True, "level": "minimal"}) == "none"
    assert llm._openai_reasoning_effort("o3", {"enabled": True, "level": "minimal"}) == "low"
    # auto → let the model default (omit).
    assert llm._openai_reasoning_effort("gpt-5.5", {"enabled": True, "level": "auto"}) is None


def test_reasoning_request_sampling_wins_over_overrides():
    # Explicit Sampling.reasoning_effort (utility calls) takes precedence.
    rag_ctx.set_overrides({"llm_reasoning_enabled": "true", "llm_reasoning_level": "high"})
    try:
        req = llm._reasoning_request({"reasoning_effort": "minimal"})
        assert req == {"enabled": True, "level": "minimal", "trace": True}
    finally:
        rag_ctx.set_overrides({})


def test_reasoning_request_reads_overrides_and_none_when_absent():
    assert llm._reasoning_request({}) is None
    rag_ctx.set_overrides({"llm_reasoning_enabled": "true", "llm_reasoning_level": "low", "llm_reasoning_trace": "false"})
    try:
        assert llm._reasoning_request({}) == {"enabled": True, "level": "low", "trace": False}
    finally:
        rag_ctx.set_overrides({})


def test_tool_step_forces_none_for_gpt5_when_tools_present():
    OA = "https://api.openai.com/v1"
    # gpt-5.x + tools → forced to none (else the tools are 400'd away and dropped),
    # regardless of the level the request asked for.
    assert llm._tool_effort("high", OA, "gpt-5.4-mini", True) == "none"
    assert llm._tool_effort("low", OA, "gpt-5", True) == "none"
    # No tools → the caller's effort is preserved (the final answer still reasons).
    assert llm._tool_effort("high", OA, "gpt-5.4-mini", False) == "high"
    # Non-gpt-5 OpenAI model (o-series) is unaffected — it never sends tools+level anyway.
    assert llm._tool_effort("low", OA, "o3", True) == "low"
    # Non-OpenAI base (local/vLLM) is untouched.
    assert llm._tool_effort("high", "http://localhost:11434/v1", "qwen3", True) == "high"


def test_openai_tool_name_sanitisation_and_remap():
    tools = [
        {"type": "function", "function": {"name": "desktop.fs_write", "parameters": {}}},
        {"type": "function", "function": {"name": "track_steps", "parameters": {}}},
    ]
    msgs = [
        {"role": "user", "content": "go"},
        {"role": "assistant", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "desktop.fs_write", "arguments": "{}"}}]},
        {"role": "tool", "tool_call_id": "c1", "content": "ok"},
    ]
    safe_tools, safe_msgs, remap = llm._openai_sanitise_names(tools, msgs)
    # dotted names rewritten for the wire; already-valid names untouched.
    assert safe_tools[0]["function"]["name"] == "desktop_fs_write"
    assert safe_tools[1]["function"]["name"] == "track_steps"
    # history tool_calls rewritten too (else OpenAI 400s on the follow-up).
    assert safe_msgs[1]["tool_calls"][0]["function"]["name"] == "desktop_fs_write"
    # the map restores the original on the way back.
    assert remap["desktop_fs_write"] == "desktop.fs_write"
    # inputs are not mutated.
    assert tools[0]["function"]["name"] == "desktop.fs_write"
    assert msgs[1]["tool_calls"][0]["function"]["name"] == "desktop.fs_write"
    # nothing dotted → no remap, no churn.
    _, _, rm = llm._openai_sanitise_names([{"function": {"name": "track_steps"}}], [{"role": "user", "content": "x"}])
    assert rm == {}


def test_normalise_reasoning_tokens_from_completion_details():
    u = llm._normalise_reasoning_tokens({"completion_tokens": 50, "completion_tokens_details": {"reasoning_tokens": 12}})
    assert u["reasoning_tokens"] == 12
    # No details → unchanged, no crash.
    assert llm._normalise_reasoning_tokens({"completion_tokens": 5}).get("reasoning_tokens") is None
    assert llm._normalise_reasoning_tokens(None) is None
