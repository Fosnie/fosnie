# Voice (STT + TTS) — deployment

Voice is the cascade **STT → LLM → TTS**: dictation (speech →
text → send) and read-aloud (assistant text → speech). **Live / streaming voice**
(mode 3 — real-time, with barge-in) is built; see the section below. The platform is
the **HTTP client** of external audio engines via the **OpenAI audio contract** —
only the models/URLs differ per deployment.

Enable with `features.voice = true` (backend boot config) **and** `STT_*` / `TTS_*`
on the ML service (see `ml/.env.*.example`). If either engine is absent the voice
endpoints degrade to HTTP 503.

## Contract the platform targets

- **STT** — `POST {STT_BASE_URL}/v1/audio/transcriptions` (multipart `file`,
  `model`, `response_format=json`) → `{ "text": "…" }`.
- **TTS** — `POST {TTS_BASE_URL}/v1/audio/speech` (JSON `{model, input, voice,
  response_format}`) → audio bytes (`Content-Type` audio/*).

STT and TTS are **separate servers/ports** (different model architectures). The
backend never calls them directly — Rust → ML `/transcribe` and `/speech` → engine.

## Dev models / engines

```
# STT — Qwen3-ASR (llama.cpp, /v1/chat/completions + input_audio → STT_FORMAT=chat)
llama-server -hf foryoung365/Qwen3-ASR-1.7B-Q4_K_M-GGUF:Q4_K_M --port 8092

# TTS — kokoro-fastapi (OpenAI /v1/audio/speech, fully local)
docker run -d --name pai-kokoro-tts -p 8094:8880 ghcr.io/remsky/kokoro-fastapi-cpu:latest
#   → TTS_BASE_URL=http://127.0.0.1:8094  TTS_MODEL=kokoro  TTS_VOICE=af_sky
```

**STT input normalisation.** Browsers (MediaRecorder) capture Opus in a WebM/Ogg
container, which the llama.cpp ASR engine cannot decode (it reads WAV/MP3/FLAC).
`ml/app/stt.py` transcodes any incoming audio to 16 kHz mono WAV via **ffmpeg**
before the engine call, so every browser capture format works. ffmpeg must be on
the ML service's PATH; without it, only WAV/MP3/FLAC uploads transcribe.

**TTS engine choice.** OmniVoice (`Serveurperso/OmniVoice-GGUF`) is **not usable**
as a bare `llama-server`: the GGUF fails to load on current builds
(`general.file_type has wrong type str but expected type u32`), and codec-token TTS
LLMs expose only `["completion"]` (404 `/v1/audio/speech`) — they emit audio
*tokens* needing a vocoder. Read-aloud therefore uses **kokoro-fastapi**, which
serves the OpenAI `/v1/audio/speech` contract directly. Any server exposing that
endpoint is a drop-in (`TTS_*` only).

**Confirmed against the real build:** raw `llama-server` does **not** expose
`/v1/audio/*` (it 404s). Qwen3-ASR takes audio via `/v1/chat/completions`
(`input_audio`) and returns `language <l><asr_text>…`. The platform handles this
natively — set `STT_FORMAT=chat` (the default `openai` mode is for a wrapper that
exposes `/v1/audio/transcriptions`). TTS likely needs the same treatment; if the
OmniVoice build has no `/v1/audio/speech`, front it with an OpenAI-audio wrapper
(`omnivoice-server`) — platform contract unchanged.

**Run STT and TTS on different ports** — each `llama-server` defaults to `:8080`,
so start the second with `--port`.

**TTS needs an audio-out HTTP endpoint.** Confirmed: codec-token TTS LLMs
(VieNeu-TTS, NeuTTS, OuteTTS, etc.) loaded as a bare `-hf` model expose only
`["completion"]` and **404 `/v1/audio/speech`** — they emit audio *tokens* that
require a vocoder/decoder to become a waveform. The platform targets the
OpenAI `/v1/audio/speech` contract, so TTS must be served by something that
exposes it: an OpenAI-audio TTS server (e.g. `kokoro-fastapi`, `openai-edge-tts`,
`omnivoice-server`) or a llama.cpp build wired with a vocoder + the `/v1/audio/speech`
integration. (STT differs — Qwen3-ASR's `/v1/chat/completions` path is handled
natively via `STT_FORMAT=chat`.)

## Transports

- **WebSocket** (live path / SPA): `voice.transcribe` (audio_base64 → `voice.transcript`)
  and `voice.speak` (text → `voice.audio`).
- **REST** (non-WS / scripts / tests): `POST /api/voice/transcribe` (raw audio body,
  `?mime=`) → `{text}`; `POST /api/voice/speech` (`{text, voice?}`) → audio bytes.

`GET /api/whoami` reports `capabilities.voice` so the SPA shows the mic / read-aloud
affordances only when the host supports them.

## Live / streaming voice (mode 3)

Real-time cascade: **streaming STT → LLM → streaming TTS**, with **barge-in**. The
orchestrator lives in Rust (`backend/src/voice/`) — it owns the WebSocket transport,
reuses `chat::run_turn` for the LLM stage (so a live turn persists like any chat), and
calls the engines as external in-perimeter services. Enable with `features.voice = true`
**and** `features.voice_live = true`, plus the `[voice_live]` engine block.

**Wire frames** (multiplexed on the existing socket): client → `voice.stream.start` /
`voice.audio.chunk` (PCM16 LE 16 kHz mono, base64, 20–40 ms) / `voice.barge_in` /
`voice.stream.end`; server → `voice.state` / `voice.partial` / `voice.final` /
`voice.tts.chunk` / `voice.tts.end` / `voice.error` — **plus** the relayed `chat.*`
frames (the editable transcript, citations, persistence ride those). `whoami` adds
`capabilities.voice_live` + `voice_live_opts {ptt_default, aec_required, silence_threshold_ms}`.

**Engines** (`[voice_live]`, all in-perimeter, each swappable / optional):

| Role | Engine | Notes |
| --- | --- | --- |
| Streaming STT | sherpa-onnx `online-websocket-server` (Apache, CPU) **or** NVIDIA NeMo `nemotron-3.5-asr-streaming` (GPU) | `stt_stream_kind="websocket"`, `stt_stream_url=ws://…`. Wire: send PCM16 frames, receive `{type:"partial"\|"final","text":…}` JSON. |
| Turn detection | Silero VAD + Smart-Turn v3 sidecar | `turn_detector_url` → `deploy/voice/turn/`. **Rust-direct** (not via ML). |
| Streaming TTS | kokoro-fastapi chunked `/v1/audio/speech` | `tts_stream=true`, `tts_stream_url=http://…`. |

**Degradation matrix** — any engine may be absent; the loop still runs:

| Missing | Behaviour |
| --- | --- |
| Streaming STT (`stt_stream_kind="none"` / unreachable) | Per-utterance **batch** STT via ML `/transcribe` (no live partials). |
| Turn sidecar (`turn_detector_url` empty / down, or `voice.turn_detection` off) | The configurable **silence-threshold** gate decides (no mid-thought-pause holding). |
| Streaming TTS (`tts_stream=false` / unreachable) | **Per-clause batch** synthesis via ML `/speech` — the aggregator already chunks at clauses, so first-audio is still fast. |

So on a box with only the batch engines (Qwen3-ASR + kokoro), live voice works end to
end via the fallbacks; the streaming engines + sidecar are a per-profile upgrade.

**Runtime dials** (super-admin knobs, audited): `voice.silence_threshold_ms` (600;
the latency lever), `voice.ptt_default` (true), `voice.aec_required` (true),
`voice.turn_detection` (false). **Metrics:** `voice_turn_latency_ms` (final → first
audio chunk; target p50<500 / p95<800 ms) and `voice_barge_in_total`.

**NVIDIA vs CPU profile.** Nemotron streaming STT needs a CUDA GPU (Linux profile).
The macOS / no-CUDA profile uses the sherpa-onnx CPU streaming engine, or simply the
batch fallback. Live voice is **off by default** on the macOS example for this reason.

## Smoke

```
# with both llama-servers up and features.voice = true + STT_*/TTS_* set:
PAI_VOICE=1 cargo test --test voice_live
```

Round-trips: synthesise "hello" → audio; transcribe it back → text. Then a manual
chat: dictate a prompt, and read a reply aloud.
