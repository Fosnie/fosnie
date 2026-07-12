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

"""Provider probe — a MINIMAL real call per role so the
backend's "Test connection" can prove a base_url/model/key actually works rather
than guessing. One token / one vector / one doc: never burn quota.

Probes call the providers DIRECTLY (not the production helpers like
`reranker.rerank`, which swallow errors and degrade to equal scores) so that a
401/404/timeout is surfaced as a readable reason. The API key is read server-side
from the per-request override map and is NEVER echoed back: `_scrub` strips any
key value from the returned strings as a last line of defence."""

import base64
import time
from pathlib import Path
from urllib.parse import urlparse

import httpx

from . import anthropic_adapter, http_client
from .config import settings
from .llm import _gen_caps, _is_openai
from .rag_ctx import cfg

# Test image for the OCR vision probe. A real shipped PNG (ships with the package,
# so it is present in any container build) — a 1×1 or blank image is rejected by
# some vision APIs ("You uploaded an unsupported image"). Falls back to a small
# valid synthetic PNG if the asset is somehow missing, so the probe never crashes.
_FALLBACK_PNG_B64 = (
    "iVBORw0KGgoAAAANSUhEUgAAAQAAAAEACAIAAADTED8xAAAB+UlEQVR42u3TMQ0AAAzDsPIn3d7DMBtCpKTwWCTAAGAAMAAYAAwABgADgAHAAGAAMAAYAAwABgADgAHAAGAAMAAYAAwABgADgAHAAGAAMAAYAAwABgADgAHAAGAAMAAYAAwABgADgAHAAGAAMAAYAAwABgADgAHAAGAAMAAGAAOAAcAAYAAwABgADAAGAAOAAcAAYAAwABgADAAGAAOAAcAAYAAwABgADAAGAAOAAcAAYAAwABgADAAGAAOAAcAAYAAwABgADAAGAAOAAcAAYAAwABgADAAGAAOAAcAAGAAMAAYAA4ABwABgADAAGAAMAAYAA4ABwABgADAAGAAMAAYAA4ABwABgADAAGAAMAAYAA4ABwABgADAAGAAMAAYAA4ABwABgADAAGAAMAAYAA4ABwABgADAAGAAMAAbAAGAAMAAYAAwABgADgAHAAGAAMAAYAAwABgADgAHAAGAAMAAYAAwABgADgAHAAGAAMAAYAAwABgADgAHAAGAAMAAYAAwABgADgAHAAGAAMAAYAAwABgADgAHAAGAAMAAYAAOAAcAAYAAwABgADAAGAAOAAcAAYAAwABgADAAGAAOAAcAAYAAwABgADAAGAAOAAcAAYAAwABgADAAGAAOAAcAAYAAwABgADAAGAAOAAcAAYAAwABgADAAGgGvctyzUB/Dz3wAAAABJRU5ErkJggg=="
)


def _load_probe_data_uri() -> str:
    png = Path(__file__).parent / "assets" / "ocr_probe.png"
    try:
        b64 = base64.b64encode(png.read_bytes()).decode("ascii")
    except OSError:
        b64 = _FALLBACK_PNG_B64
    return "data:image/png;base64," + b64


_DATA_URI_PROBE = _load_probe_data_uri()

_PROBE_TIMEOUT = 20.0  # connect+read for a single tiny call

# role → (base_url settings key, default) — for the "cannot reach <host>" message.
_BASE_KEY = {
    "llm": ("llm_base_url", settings.llm_base_url),
    "embed": ("embed_base_url", settings.embed_base_url),
    "rerank": ("rerank_base_url", settings.rerank_base_url),
    "ocr": ("ocr_base_url", settings.ocr_base_url),
    "stt": ("stt_base_url", settings.stt_base_url),
    "tts": ("tts_base_url", settings.tts_base_url),
    "verify": ("verify_base_url", settings.verify_base_url),
}


def _err_from_status(code: int) -> str:
    """Map an HTTP status to a readable, non-leaking reason."""
    if code in (401, 403):
        return "invalid API key"
    if code == 404:
        return "wrong endpoint shape (404)"
    if code == 429:
        return "rate limited (429)"
    return f"HTTP {code}"


def _http_err(r) -> str:
    """Readable reason for a non-2xx response, enriched with the provider's own
    error message when present (OpenAI/LiteLLM `{error:{message}}`). The api_key
    is stripped later by `_scrub`."""
    base = _err_from_status(r.status_code)
    msg = None
    try:
        j = r.json()
        if isinstance(j, dict):
            err = j.get("error")
            msg = err.get("message") if isinstance(err, dict) else (err if isinstance(err, str) else None)
            msg = msg or j.get("message") or j.get("detail")
    except Exception:  # noqa: BLE001 — body not JSON; keep the status-only reason
        msg = None
    return f"{base}: {str(msg)[:160]}" if msg else base


def _host_for(role: str) -> str:
    key, default = _BASE_KEY.get(role, ("llm_base_url", settings.llm_base_url))
    base = cfg(key, default) or cfg("llm_base_url", settings.llm_base_url) or ""
    return urlparse(base).hostname or base or "endpoint"


def _scrub(res: dict, role: str) -> dict:
    """Strip the role's api_key from any returned string (defence in depth)."""
    key = cfg(f"{role}_api_key", getattr(settings, f"{role}_api_key", None))
    if isinstance(key, str) and len(key) >= 6:
        for f in ("error", "detail"):
            v = res.get(f)
            if isinstance(v, str) and key in v:
                res[f] = v.replace(key, "***")
    return res


async def _post(url: str, *, json=None, headers=None, timeout: float = _PROBE_TIMEOUT):
    return await http_client.get_client().post(url, json=json, headers=headers, timeout=timeout)


def _fail_if_error(r) -> dict | None:
    """Non-2xx → a readable error dict enriched with the provider's own message
    (e.g. Jina/OpenAI validation text on a 422), else None. Preferred over
    `raise_for_status()` in probes so the operator sees WHY, not just the status."""
    if r.status_code >= 400:
        return {"ok": False, "error": _http_err(r)}
    return None


async def _probe_llm() -> dict:
    base = cfg("llm_base_url", settings.llm_base_url)
    model = cfg("llm_model", settings.llm_model)
    if anthropic_adapter.is_anthropic(base):
        url = f"{base.rstrip('/')}/messages"
        headers = {
            "x-api-key": cfg("llm_api_key", settings.llm_api_key),
            "anthropic-version": anthropic_adapter.ANTHROPIC_VERSION,
            "content-type": "application/json",
        }
        payload = {"model": model, "max_tokens": 1, "messages": [{"role": "user", "content": "ping"}]}
        r = await _post(url, json=payload, headers=headers)
        r.raise_for_status()
        return {"model": model}

    # OpenAI-compatible path. `max_tokens` is the long-standing field, but OpenAI's
    # reasoning models (gpt-5.x / o-series) reject it and require
    # `max_completion_tokens` — so try the classic field first (vLLM/Ollama/
    # llama.cpp speak it) and, on a 400, retry with the newer field.
    url = f"{base.rstrip('/')}/chat/completions"
    headers = {"Authorization": f"Bearer {cfg('llm_api_key', settings.llm_api_key)}"}
    msgs = [{"role": "user", "content": "ping"}]
    r = await _post(url, json={"model": model, "messages": msgs, "max_tokens": 1, "stream": False}, headers=headers)
    if r.status_code == 400:
        # Reasoning models (gpt-5.x / o-series) reject `max_tokens` and spend tokens
        # on hidden reasoning, so a 1-token cap can't finish — give a little headroom.
        r = await _post(url, json={"model": model, "messages": msgs, "max_completion_tokens": 16, "stream": False}, headers=headers)
    if r.status_code >= 400:
        reason = _http_err(r)
        # A token/output-limit 400 still proves the endpoint + key + model are good
        # (the model answered, just couldn't fit the cap) — count it as reachable.
        if r.status_code == 400 and any(s in reason.lower() for s in ("max_tokens", "max_completion_tokens", "output limit", "finish the message", "length")):
            return {"model": model, "detail": "reachable (probe hit the token cap)"}
        return {"ok": False, "error": reason, "model": model}
    return {"model": model}


async def _probe_embed() -> dict:
    base = cfg("embed_base_url", settings.embed_base_url)
    model = cfg("embed_model", settings.embed_model)
    headers = {"Authorization": f"Bearer {cfg('embed_api_key', settings.embed_api_key)}"}
    r = await _post(f"{base.rstrip('/')}/embeddings", json={"model": model, "input": ["test"]}, headers=headers)
    if err := _fail_if_error(r):
        return {**err, "model": model}
    vec = r.json()["data"][0]["embedding"]
    if not vec:
        return {"ok": False, "error": "empty embedding vector"}
    return {"model": model, "detail": f"dim={len(vec)}"}


async def _probe_rerank() -> dict:
    base = cfg("rerank_base_url", settings.rerank_base_url)
    model = cfg("rerank_model", settings.rerank_model)
    headers = {"Authorization": f"Bearer {cfg('rerank_api_key', settings.rerank_api_key)}"}
    payload = {"model": model, "query": "test", "documents": ["a", "b"]}
    r = await _post(f"{base.rstrip('/')}/v1/rerank", json=payload, headers=headers)
    if err := _fail_if_error(r):
        return {**err, "model": model}
    if not r.json().get("results"):
        return {"ok": False, "error": "no rerank results"}
    return {"model": model}


async def _probe_ocr() -> dict:
    base = cfg("ocr_base_url", settings.ocr_base_url) or cfg("llm_base_url", settings.llm_base_url)
    model = cfg("ocr_model", settings.ocr_model)
    headers = {"Authorization": f"Bearer {cfg('ocr_api_key', settings.ocr_api_key)}"}
    # Match generation: OpenAI reasoning models need `max_completion_tokens` and
    # reject `temperature` (gpt-5.x / o-series); `_gen_caps` picks the right shape.
    token_key, sampling_ok = _gen_caps(base, model)
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "ok"},
            {"type": "image_url", "image_url": {"url": _DATA_URI_PROBE}},
        ]}],
        # Reasoning models spend the cap on hidden reasoning before any output, so
        # 1 can't finish — give a little headroom (we only probe reachability).
        token_key: 16,
    }
    if sampling_ok:
        payload["temperature"] = 0
    r = await _post(f"{base.rstrip('/')}/chat/completions", json=payload, headers=headers, timeout=settings.ocr_timeout)
    if err := _fail_if_error(r):
        return {**err, "model": model}
    return {"model": model}


async def _probe_verify() -> dict:
    base = cfg("verify_base_url", settings.verify_base_url)
    model = cfg("verify_model", settings.verify_model)
    headers = {"Authorization": f"Bearer {cfg('verify_api_key', settings.verify_api_key)}"}
    payload = {"context": ["The sky is blue."], "question": "What colour is the sky?", "answer": "Blue.", "model": model}
    r = await _post(f"{base.rstrip('/')}/v1/verify", json=payload, headers=headers, timeout=settings.verify_timeout)
    if err := _fail_if_error(r):
        return {**err, "model": model}
    return {"model": model, "detail": "local-only, optional"}


async def _probe_stt() -> dict:
    # Liveness only: real STT needs an audio file. Any HTTP response (even a
    # 400/422 from the empty body) proves the endpoint is up; only a connection
    # failure (→ "cannot reach") or an auth rejection is a real failure.
    base = cfg("stt_base_url", settings.stt_base_url).rstrip("/")
    model = cfg("stt_model", settings.stt_model)
    headers = {"Authorization": f"Bearer {cfg('stt_api_key', settings.stt_api_key)}"}
    r = await _post(http_client.v1_url(base, "audio/transcriptions"), headers=headers)
    if r.status_code in (401, 403):
        return {"ok": False, "error": "invalid API key"}
    return {"ok": True, "model": model, "detail": "reachable, not fully probed (needs audio)"}


async def _probe_tts() -> dict:
    base = cfg("tts_base_url", settings.tts_base_url).rstrip("/")
    model = cfg("tts_model", settings.tts_model)
    headers = {"Authorization": f"Bearer {cfg('tts_api_key', settings.tts_api_key)}"}
    # Voice sets differ per engine: OpenAI rejects kokoro's `af_sky` and vice-versa.
    # Use a known-valid voice for the OpenAI host; otherwise the configured one.
    voice = "alloy" if _is_openai(base) else cfg("tts_voice", settings.tts_voice)
    payload = {"model": model, "input": "ok", "voice": voice, "response_format": settings.tts_format}
    r = await _post(http_client.v1_url(base, "audio/speech"), json=payload, headers=headers)
    if err := _fail_if_error(r):
        # A voice-validation 400 still proves the endpoint + key + model are good.
        if r.status_code == 400 and "voice" in r.text.lower():
            return {"model": model, "detail": "reachable (probe voice rejected — set a valid voice)"}
        return {**err, "model": model}
    if not r.content:
        return {"ok": False, "error": "empty audio response"}
    return {"model": model, "detail": f"{len(r.content)} bytes"}


_PROBES = {
    "llm": _probe_llm,
    "embed": _probe_embed,
    "rerank": _probe_rerank,
    "ocr": _probe_ocr,
    "verify": _probe_verify,
    "stt": _probe_stt,
    "tts": _probe_tts,
}


async def probe(role: str) -> dict:
    """Run the minimal probe for `role`. Returns
    `{ok, latency_ms, error?, detail?, model?}` — never the api_key."""
    fn = _PROBES.get(role)
    if fn is None:
        return {"ok": False, "latency_ms": 0.0, "error": f"unknown role: {role}"}
    t0 = time.perf_counter()
    try:
        res = await fn()
    except httpx.HTTPStatusError as e:
        res = {"ok": False, "error": _err_from_status(e.response.status_code)}
    except (httpx.ConnectError, httpx.ConnectTimeout):
        res = {"ok": False, "error": f"cannot reach {_host_for(role)}"}
    except httpx.TimeoutException:
        res = {"ok": False, "error": "timed out"}
    except Exception as e:  # noqa: BLE001 — any other shape becomes a readable reason
        res = {"ok": False, "error": str(e) or e.__class__.__name__}
    res.setdefault("ok", True)
    res["latency_ms"] = round((time.perf_counter() - t0) * 1000, 1)
    return _scrub(res, role)
