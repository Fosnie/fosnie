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

"""Voice turn-detection sidecar — **Silero VAD** + **Smart-Turn v3**, a small,
fully-offline service for the live-voice loop. Unlike
the groundedness verifier (which the Python ML service fronts), the Rust live-voice
orchestrator is this sidecar's **direct** HTTP client: turn detection sits on the
latency-critical path, so a Python hop is deliberately avoided.

Three signals over a recent audio window, kept distinct:
  * **is_speech** — Silero VAD speech probability over the window (the barge-in
    input while the assistant talks).
  * **endpoint** — speech was present but the trailing ~300 ms is quiet (an acoustic
    end-of-utterance; faster than a long silence timer).
  * **turn_complete** — Smart-Turn v3 reads the last ~8 s and predicts *completeness*
    from prosody, so it can fire before trailing silence — and HOLD on a mid-thought
    pause (the whole point of semantic turn detection).

Endpoints:
  POST /detect  {audio_base64, sample_rate}
        -> {is_speech, endpoint, turn_complete, prob}
  GET  /version

Models (CPU; downloaded once, then zero-egress):
  * Silero VAD (MIT) — via torch.hub `snakers4/silero-vad`.
  * Smart-Turn v3 (pipecat-ai/smart-turn-v3, open weights) — lazy-loaded.

Off by default. Enable via the backend's `[voice_live].turn_detector_url`. If the
models do not load on a given host (e.g. no weights fetched, or a deps mismatch),
DO NOT run this sidecar — the platform then degrades to its silence-threshold gate
(`should_fire_turn(detector_present=false, …)`), and the live loop still works.

Run (CPU):

    pip install -r requirements.txt
    VOICE_TURN_PORT=8096 python server.py
"""

import base64
import os

import numpy as np
import torch
import uvicorn
from fastapi import FastAPI
from pydantic import BaseModel

PORT = int(os.environ.get("VOICE_TURN_PORT", "8096"))
SMART_TURN_MODEL = os.environ.get("SMART_TURN_MODEL", "pipecat-ai/smart-turn-v3")
# Silero speech probability at/above which a frame counts as speech.
VAD_SPEECH = float(os.environ.get("VOICE_VAD_THRESHOLD", "0.5"))
# Smart-Turn completion probability at/above which the turn is judged complete.
TURN_COMPLETE = float(os.environ.get("VOICE_TURN_THRESHOLD", "0.5"))

app = FastAPI(title="PAI voice turn detector", version="1.0")

# --- Silero VAD (loaded at boot) ---------------------------------------------
_vad_model, _ = torch.hub.load("snakers4/silero-vad", "silero_vad", trust_repo=True)

# --- Smart-Turn v3 (lazy-loaded on first /detect) ----------------------------
_st_extractor = None
_st_model = None
_st_complete_idx = 1  # index of the "complete" class; resolved at load


def _ensure_smart_turn() -> None:
    global _st_extractor, _st_model, _st_complete_idx
    if _st_model is not None:
        return
    from transformers import AutoFeatureExtractor, AutoModelForAudioClassification

    _st_extractor = AutoFeatureExtractor.from_pretrained(SMART_TURN_MODEL)
    _st_model = AutoModelForAudioClassification.from_pretrained(SMART_TURN_MODEL)
    _st_model.eval()
    labels = {int(k): str(v).lower() for k, v in _st_model.config.id2label.items()}
    idx = next(
        (i for i, n in labels.items() if any(t in n for t in ("complete", "finish", "end", "true"))),
        None,
    )
    _st_complete_idx = idx if idx is not None else max(labels)


def _pcm16_to_float(audio_b64: str) -> np.ndarray:
    """Base64 PCM16 mono LE → float32 in [-1, 1]."""
    raw = base64.b64decode(audio_b64)
    if len(raw) < 2:
        return np.zeros(0, dtype=np.float32)
    return np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0


def _resample_to_16k(x: np.ndarray, sr: int) -> np.ndarray:
    """Cheap linear resample to 16 kHz (ample for VAD / turn detection)."""
    if sr == 16000 or x.size == 0:
        return x
    n = int(round(x.size * 16000 / sr))
    if n <= 0:
        return np.zeros(0, dtype=np.float32)
    idx = np.linspace(0, x.size - 1, n)
    return np.interp(idx, np.arange(x.size), x).astype(np.float32)


@torch.no_grad()
def _vad_prob(x16: np.ndarray) -> float:
    """Max Silero speech probability over 512-sample (32 ms @ 16 kHz) windows."""
    if x16.size < 512:
        return 0.0
    best = 0.0
    for i in range(0, x16.size - 512, 512):
        chunk = torch.from_numpy(x16[i : i + 512].copy())
        best = max(best, float(_vad_model(chunk, 16000).item()))
    return best


@torch.no_grad()
def _turn_complete_prob(x16: np.ndarray) -> float:
    """Smart-Turn v3 completion probability over the last ~8 s."""
    _ensure_smart_turn()
    tail = x16[-16000 * 8 :]
    enc = _st_extractor(tail, sampling_rate=16000, return_tensors="pt")
    logits = _st_model(**enc).logits
    return float(torch.softmax(logits, dim=-1)[0][_st_complete_idx].item())


class DetectRequest(BaseModel):
    audio_base64: str
    sample_rate: int = 16000


@app.get("/version")
def version() -> dict:
    return {"vad": "silero-vad", "smart_turn": SMART_TURN_MODEL, "version": "1.0"}


@app.post("/detect")
def detect(req: DetectRequest) -> dict:
    x16 = _resample_to_16k(_pcm16_to_float(req.audio_base64), req.sample_rate)
    speech = _vad_prob(x16)
    is_speech = speech >= VAD_SPEECH
    # Endpoint: speech in the window, but the trailing ~300 ms is quiet.
    tail = x16[-int(16000 * 0.3) :]
    tail_speech = _vad_prob(tail) if tail.size >= 512 else 0.0
    endpoint = is_speech and tail_speech < VAD_SPEECH
    # Semantic completeness (best-effort; on any model failure fall back to the
    # acoustic endpoint so the platform's gate still behaves sensibly).
    try:
        prob = _turn_complete_prob(x16)
    except Exception:  # noqa: BLE001
        prob = 1.0 if endpoint else 0.0
    return {
        "is_speech": bool(is_speech),
        "endpoint": bool(endpoint),
        "turn_complete": bool(prob >= TURN_COMPLETE),
        "prob": float(prob),
    }


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=PORT)
