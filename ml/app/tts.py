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

"""Text-to-speech. Read-aloud over an OpenAI-audio HTTP engine
(`POST /v1/audio/speech`, JSON → audio bytes). Behind this swappable interface so
the engine — OmniVoice / Kokoro / other — is a config change. Degrades cleanly
(HTTP 503 at the route) when absent."""

from __future__ import annotations

import logging

from . import http_client
from .config import settings
from .llm import _is_openai
from .rag_ctx import cfg

_log = logging.getLogger("pai.tts")
_degraded = False

# OpenAI's /v1/audio/speech voices (from its own rejection message). When the TTS
# base is OpenAI, an unset/local voice (e.g. kokoro's `af_sky`) is invalid — fall
# back to a valid one rather than 400 → 503.
_OPENAI_TTS_VOICES = {
    "alloy", "echo", "fable", "onyx", "nova", "shimmer",
    "coral", "verse", "ballad", "ash", "sage", "marin", "cedar",
}

_FORMAT_MIME = {
    "wav": "audio/wav",
    "mp3": "audio/mpeg",
    "opus": "audio/opus",
    "flac": "audio/flac",
    "pcm": "audio/pcm",
}


def available() -> bool:
    return settings.tts_enabled and not _degraded


async def synthesize(text: str, voice: str | None = None) -> tuple[bytes, str]:
    """Synthesise speech for `text` via the OpenAI `/v1/audio/speech` contract.
    Returns (audio_bytes, mime). Raises on failure (route maps to 503)."""
    global _degraded
    base = cfg("tts_base_url", settings.tts_base_url)
    url = http_client.v1_url(base, "audio/speech")
    headers = {"Authorization": f"Bearer {cfg('tts_api_key', settings.tts_api_key)}"}
    fmt = settings.tts_format
    resolved_voice = voice or cfg("tts_voice", settings.tts_voice)
    # OpenAI rejects the local engines' voices (e.g. `af_sky`); default to `alloy`.
    if _is_openai(base) and resolved_voice not in _OPENAI_TTS_VOICES:
        resolved_voice = "alloy"
    payload = {
        "model": cfg("tts_model", settings.tts_model),
        "input": text,
        "voice": resolved_voice,
        "response_format": fmt,
    }
    try:
        client = http_client.get_client()
        r = await client.post(url, json=payload, headers=headers, timeout=settings.request_timeout)
        r.raise_for_status()
        audio = r.content
        _degraded = False
        mime = r.headers.get("content-type") or _FORMAT_MIME.get(fmt, "application/octet-stream")
        return audio, mime
    except Exception as e:  # noqa: BLE001
        _degraded = True
        _log.warning("TTS unavailable: %s", e)
        raise
