"""Voice STT/TTS availability gating (no audio engine needed). The real
round-trip is the gated live check (PAI_VOICE=1). Run: `uv run pytest
tests/test_voice.py` from ml/."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import stt, tts  # noqa: E402
from app.config import settings  # noqa: E402


def test_stt_available_reflects_enabled(monkeypatch):
    monkeypatch.setattr(stt, "_degraded", False)
    monkeypatch.setattr(settings, "stt_enabled", False)
    assert stt.available() is False
    monkeypatch.setattr(settings, "stt_enabled", True)
    assert stt.available() is True


def test_tts_available_reflects_enabled(monkeypatch):
    monkeypatch.setattr(tts, "_degraded", False)
    monkeypatch.setattr(settings, "tts_enabled", False)
    assert tts.available() is False
    monkeypatch.setattr(settings, "tts_enabled", True)
    assert tts.available() is True


def test_degraded_latch_forces_unavailable(monkeypatch):
    monkeypatch.setattr(settings, "tts_enabled", True)
    monkeypatch.setattr(tts, "_degraded", True)  # a prior failure latched it
    assert tts.available() is False


def test_clean_transcript():
    # Qwen3-ASR (chat-completions mode) wraps output as `language <l><asr_text>…`.
    assert stt._clean_transcript("language en<asr_text>hello world</asr_text>") == "hello world"
    assert stt._clean_transcript("language None<asr_text>") == ""  # silence → empty
    assert stt._clean_transcript("plain transcript") == "plain transcript"
    # The literal marker / control tags must never leak through (any wire path).
    assert stt._clean_transcript("<asr_text>") == ""
    assert stt._clean_transcript("<asr_text></asr_text>") == ""
    assert stt._clean_transcript("<|im_end|>") == ""
    # A bare `language xx` prefix without the marker is dropped; the rest survives.
    assert stt._clean_transcript("language ru привет") == "привет"
    assert stt._clean_transcript("") == ""


def test_audio_format_from_mime():
    assert stt._audio_format("audio/wav") == "wav"
    assert stt._audio_format("audio/mpeg") == "mp3"
    assert stt._audio_format(None) == "wav"
