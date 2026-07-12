# Voice turn-detection sidecar (Silero VAD + Smart-Turn v3)

A small, fully-offline service that tells the live-voice loop **when the speaker has
finished their turn** — the single highest-leverage latency lever. It runs **Silero VAD** (frame-level speech/silence) and
**Smart-Turn v3** (semantic completeness from prosody) on CPU.

It is **off by default** and entirely optional: with no sidecar the platform falls
back to its configurable silence-threshold gate, and the live loop still works (no
mid-thought-pause holding, that's all you lose).

## Direct client (the one deliberate divergence)

Unlike the groundedness verifier — which the Python ML service fronts (Rust → ML →
sidecar) — the **Rust** live-voice orchestrator is this sidecar's **direct** HTTP
client (Rust → sidecar). Turn detection sits on the latency-critical path, so the
extra Python hop is deliberately avoided. It still obeys
the platform's zero-egress posture: in-perimeter only, no outbound calls after the
one-time model download.

## Contract

```
POST /detect   {audio_base64, sample_rate}  ->  {is_speech, endpoint, turn_complete, prob}
GET  /version
```

- `is_speech` — speech present in the window (the barge-in signal while the assistant talks).
- `endpoint` — speech was present but the trailing ~300 ms is quiet (acoustic end-of-utterance).
- `turn_complete` — Smart-Turn judged the thought finished (HOLDS on a mid-thought pause).
- `prob` — the completeness probability behind `turn_complete`.

The backend fires the turn on `(endpoint ∨ silence ≥ threshold) ∧ turn_complete` when
this sidecar is configured (`voice.turn_detection` knob on), and on the silence gate
alone otherwise — see `backend/src/voice/turn.rs::should_fire_turn`.

## Run (own venv — NOT the pip-less ml venv)

```sh
cd backend/deploy/voice/turn
python -m venv .venv && . .venv/bin/activate   # Windows: .venv\Scripts\activate
pip install -r requirements.txt
VOICE_TURN_PORT=8096 python server.py
```

Then point the backend at it and turn semantic detection on:

```toml
# config.toml
[voice_live]
turn_detector_url = "http://127.0.0.1:8096"
```
```
# super-admin runtime knob
voice.turn_detection = true
```

## Models & licences (CPU, downloaded once)

| Model | Licence | Role |
|---|---|---|
| Silero VAD (`snakers4/silero-vad`, torch.hub) | MIT | frame-level speech/silence |
| Smart-Turn v3 (`pipecat-ai/smart-turn-v3`) | open weights | semantic end-of-turn |

Env: `VOICE_TURN_PORT` (8096), `SMART_TURN_MODEL`, `VOICE_VAD_THRESHOLD` (0.5),
`VOICE_TURN_THRESHOLD` (0.5).

## Honest caveats

- The weights / `torch` / `transformers` stack may not install on every host (notably
  the dev box). That is fine: **do not run the sidecar there** and leave
  `voice.turn_detection` off — the platform degrades to the silence-threshold gate.
- Smart-Turn v3 is English-first. The CPU profile is the only profile that needs this
  sidecar at all (the NVIDIA streaming-STT engine carries its own endpointing).
- No systemd/launchd unit ships here (matching the verifier sidecar): start it as a
  documented per-profile process.
