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

"""Speech-to-text. Batch STT over an OpenAI-audio HTTP engine
(`POST /v1/audio/transcriptions`, multipart). Behind this swappable interface so
the engine — Qwen3-ASR / Whisper / a streaming engine later — is a config change,
not a code change. Degrades cleanly (HTTP 503 at the route) when absent."""

from __future__ import annotations

import asyncio
import base64
import logging
import os
import re
import shutil
import tempfile

from . import http_client
from .config import settings
from .llm import _is_openai
from .rag_ctx import cfg

_log = logging.getLogger("pai.stt")
_degraded = False


def available() -> bool:
    return settings.stt_enabled and not _degraded


def _audio_format(mime: str | None) -> str:
    m = (mime or "").lower()
    if "mpeg" in m or "mp3" in m:
        return "mp3"
    if "flac" in m:
        return "flac"
    if "ogg" in m or "opus" in m:
        return "ogg"
    return "wav"


def _suffix(mime: str | None) -> str:
    m = (mime or "").lower()
    if "webm" in m:
        return ".webm"
    if "ogg" in m or "opus" in m:
        return ".ogg"
    if "mp4" in m or "m4a" in m or "aac" in m:
        return ".m4a"
    if "mpeg" in m or "mp3" in m:
        return ".mp3"
    if "flac" in m:
        return ".flac"
    return ".wav"


async def _to_wav(audio: bytes, mime: str | None) -> tuple[bytes, str]:
    """Normalise any browser-captured audio to 16 kHz mono WAV via ffmpeg.

    Browsers (MediaRecorder) emit Opus in a WebM/Ogg container, which the
    llama.cpp ASR engine cannot decode — only WAV/MP3/FLAC. ffmpeg gives us one
    format the engine always accepts (16 kHz mono is also what Qwen3-ASR wants).
    Falls back to the original bytes if ffmpeg is absent or the decode fails, so
    a WAV upload still works without ffmpeg installed."""
    if shutil.which("ffmpeg") is None:
        return audio, _audio_format(mime)
    in_fd, in_path = tempfile.mkstemp(suffix=_suffix(mime))
    out_fd, out_path = tempfile.mkstemp(suffix=".wav")
    os.close(in_fd)
    os.close(out_fd)
    try:
        with open(in_path, "wb") as f:
            f.write(audio)
        proc = await asyncio.create_subprocess_exec(
            "ffmpeg", "-y", "-hide_banner", "-loglevel", "error",
            "-i", in_path, "-ar", "16000", "-ac", "1", out_path,
            stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE,
        )
        _, err = await proc.communicate()
        if proc.returncode != 0 or not os.path.getsize(out_path):
            _log.warning("ffmpeg transcode failed (rc=%s); sending original: %s",
                         proc.returncode, err.decode("utf-8", "replace")[:200])
            return audio, _audio_format(mime)
        with open(out_path, "rb") as f:
            return f.read(), "wav"
    except Exception as e:  # noqa: BLE001 — never let transcode crash STT
        _log.warning("ffmpeg transcode error (%s); sending original", e)
        return audio, _audio_format(mime)
    finally:
        for p in (in_path, out_path):
            try:
                os.remove(p)
            except OSError:
                pass


_LANG_NAMES = {
    "en": "English", "zh": "Chinese", "es": "Spanish", "fr": "French",
    "de": "German", "it": "Italian", "pt": "Portuguese", "ru": "Russian",
    "ja": "Japanese", "ko": "Korean", "ar": "Arabic", "hi": "Hindi",
}


def _lang_name(code: str) -> str:
    return _LANG_NAMES.get(code.lower(), code)


# A leading `language en` / `language zh-CN` prefix Qwen3-ASR emits without the marker.
_LANG_PREFIX_RE = re.compile(r"^\s*language\s+[A-Za-z][A-Za-z-]*\s*", re.IGNORECASE)
# Any residual angle-bracket control token: <asr_text>, </asr_text>, <|im_end|>, …
_ASR_CONTROL_RE = re.compile(r"<\|?[^<>]*\|?>")


def _clean_transcript(content: str) -> str:
    """Qwen3-ASR wraps output e.g. `language en<asr_text>the transcript</asr_text>`.
    Return just the transcript: keep what follows the `<asr_text>` marker (if present),
    drop a leading `language <code>` prefix, and strip any residual ASR control tags.
    Tolerates a missing close tag / plain content. Applied on EVERY wire path so raw
    control tags never leak downstream regardless of `stt_format`."""
    if not content:
        return ""
    marker = "<asr_text>"
    if marker in content:
        content = content.split(marker, 1)[1]
    content = _LANG_PREFIX_RE.sub("", content)
    content = _ASR_CONTROL_RE.sub("", content)
    return content.strip()


async def transcribe(audio: bytes, mime: str | None = None, language: str | None = None) -> str:
    """Transcribe audio to text. Two wire shapes, selected by `stt_format`:
    `openai` (POST /v1/audio/transcriptions, multipart) or `chat` (POST
    /v1/chat/completions with an input_audio part — raw llama.cpp multimodal
    ASR). Raises on failure (route → 503); latches `_degraded`."""
    global _degraded
    base = cfg("stt_base_url", settings.stt_base_url).rstrip("/")
    headers = {"Authorization": f"Bearer {cfg('stt_api_key', settings.stt_api_key)}"}
    # Caller hint wins; else the deployment default (English-first profile pins "en").
    lang = (language or settings.stt_language or "").strip()
    # Browser MediaRecorder emits Opus/WebM the engine can't decode — normalise
    # to 16 kHz mono WAV up front so any capture format works.
    audio, fmt = await _to_wav(audio, mime)
    send_mime = "audio/wav" if fmt == "wav" else (mime or "application/octet-stream")
    # Wire shape is runtime-overridable (`chat` = llama.cpp multimodal ASR, e.g.
    # local Qwen3-ASR; `openai` = /v1/audio/transcriptions). OpenAI has no chat-ASR,
    # so force the transcriptions shape for an OpenAI host regardless of the default.
    wire = "openai" if _is_openai(base) else cfg("stt_format", settings.stt_format)
    try:
        client = http_client.get_client()
        if wire == "chat":
            instruction = "Transcribe this audio."
            if lang:
                name = _lang_name(lang)
                instruction = (
                    f"Transcribe this audio. The spoken language is {name}; "
                    f"transcribe it verbatim in {name}."
                )
            payload = {
                "model": cfg("stt_model", settings.stt_model),
                "messages": [
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "input_audio",
                                "input_audio": {
                                    "data": base64.b64encode(audio).decode(),
                                    "format": fmt,
                                },
                            },
                            {"type": "text", "text": instruction},
                        ],
                    }
                ],
            }
            r = await client.post(
                http_client.v1_url(base, "chat/completions"),
                json=payload,
                headers=headers,
                timeout=settings.request_timeout,
            )
            r.raise_for_status()
            body = r.json()
            content = body["choices"][0]["message"]["content"]
            text = _clean_transcript(content if isinstance(content, str) else "")
        else:
            files = {"file": ("audio", audio, send_mime)}
            data = {"model": cfg("stt_model", settings.stt_model), "response_format": "json"}
            if lang:
                data["language"] = lang
            r = await client.post(
                http_client.v1_url(base, "audio/transcriptions"),
                files=files,
                data=data,
                headers=headers,
                timeout=settings.request_timeout,
            )
            r.raise_for_status()
            body = r.json()
            raw = body.get("text", "") if isinstance(body, dict) else ""
            text = _clean_transcript(raw if isinstance(raw, str) else "")
        _degraded = False
        return text
    except Exception as e:  # noqa: BLE001 — surface as unavailable, never 500-crash
        _degraded = True
        _log.warning("STT unavailable: %s", e)
        raise
