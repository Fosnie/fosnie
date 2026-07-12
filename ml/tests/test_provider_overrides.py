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

"""Provider overrides: a per-request `overrides` dict set
via rag_ctx is read back through cfg(), falling back to settings when absent."""

from app import rag_ctx
from app.config import settings


def teardown_function() -> None:
    rag_ctx.set_overrides({})


def test_cfg_falls_back_to_settings_without_override() -> None:
    rag_ctx.set_overrides({})
    assert rag_ctx.cfg("llm_base_url", settings.llm_base_url) == settings.llm_base_url
    assert rag_ctx.cfg("embed_model", settings.embed_model) == settings.embed_model


def test_cfg_returns_override_when_present() -> None:
    rag_ctx.set_overrides({
        "llm_base_url": "https://api.anthropic.example/v1",
        "llm_model": "claude-test",
        "llm_api_key": "sk-secret",
    })
    assert rag_ctx.cfg("llm_base_url", settings.llm_base_url) == "https://api.anthropic.example/v1"
    assert rag_ctx.cfg("llm_model", settings.llm_model) == "claude-test"
    assert rag_ctx.cfg("llm_api_key", settings.llm_api_key) == "sk-secret"
    # A role with no override still falls back to settings.
    assert rag_ctx.cfg("embed_base_url", settings.embed_base_url) == settings.embed_base_url


def test_voice_overrides_apply() -> None:
    # Voice (stt/tts) ride the same channel: header for transcribe,
    # body for /speech, both into set_overrides.
    rag_ctx.set_overrides({"stt_base_url": "http://stt.x", "tts_model": "kokoro-x"})
    assert rag_ctx.cfg("stt_base_url", settings.stt_base_url) == "http://stt.x"
    assert rag_ctx.cfg("tts_model", settings.tts_model) == "kokoro-x"
    assert rag_ctx.cfg("tts_base_url", settings.tts_base_url) == settings.tts_base_url


def test_none_values_are_dropped() -> None:
    # set_overrides drops None values, so cfg falls back for them.
    rag_ctx.set_overrides({"llm_model": None, "llm_base_url": "http://x"})
    assert rag_ctx.cfg("llm_model", settings.llm_model) == settings.llm_model
    assert rag_ctx.cfg("llm_base_url", settings.llm_base_url) == "http://x"
