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

"""Native Anthropic Messages API adapter.

`llm.py` speaks OpenAI `/chat/completions` to every provider. Anthropic's
OpenAI-compat shim rejects the OpenAI tool schema (so the agent makes no tool
calls) and 400s on `temperature`. When the LLM base_url is Anthropic we instead
translate to/from the native Messages API here, restoring full tool use.

The three async entry points (`a_chat_step`, `a_complete`, `a_stream_chat`) take
the SAME inputs and return the SAME shapes as their `llm.py` twins, so the
backend tool-loop and the `<think>` frontend render are unchanged.

Phase 1: chat + tools + streaming, NO extended thinking. Phase 2 (thinking +
signature round-trip cache) is deliberately out of scope — see the spec.
"""

import asyncio
import json
import logging
from collections.abc import AsyncIterator
from typing import Any
from urllib.parse import urlparse

from . import http_client, thinking_cache
from .config import settings
from .rag_ctx import cfg

logger = logging.getLogger("pai-ml.anthropic")

ANTHROPIC_VERSION = "2023-06-01"  # stable contract value — do NOT "update".
_MAX_RETRIES = 3  # for 429 / 529 overloaded


def is_anthropic(base_url: str | None) -> bool:
    """True when the configured LLM endpoint is the native Anthropic API (detect
    by host, so both `https://api.anthropic.com/v1` and future regional hosts
    route here)."""
    if not base_url:
        return False
    host = (urlparse(base_url).hostname or "").lower()
    return host == "anthropic.com" or host.endswith(".anthropic.com")


def _thinking_config() -> tuple[dict[str, Any], bool]:
    """Parse `llm_thinking` into a body fragment + on-flag (Phase 2). Model-dependent
    mode is the operator's responsibility — a wrong choice 400s and is logged."""
    spec = (cfg("llm_thinking", settings.llm_thinking) or "off").strip().lower()
    if not spec or spec == "off":
        return {}, False
    if spec.startswith("budget:"):
        try:
            n = int(spec.split(":", 1)[1])
        except ValueError:
            return {}, False
        return {"thinking": {"type": "enabled", "budget_tokens": max(1024, n)}}, True
    if spec == "adaptive" or spec.startswith("adaptive:"):
        # `display:"summarized"` shows a readable trace in the reasoning panel; when
        # the user hides the trace (`return_trace=false`) use "omitted" (Anthropic
        # still reasons + bills, we just don't stream the summary).
        display = "summarized" if _trace_on() else "omitted"
        frag: dict[str, Any] = {"thinking": {"type": "adaptive", "display": display}}
        if ":" in spec:
            effort = spec.split(":", 1)[1].strip()
            if effort:
                frag["output_config"] = {"effort": effort}
        return frag, True
    return {}, False


def _trace_on() -> bool:
    """Whether to stream the reasoning trace (the per-turn `llm_reasoning_trace`
    override; defaults on so a trace is never silently lost)."""
    return str(cfg("llm_reasoning_trace", "true")).strip().lower() != "false"


# --- request translation: OpenAI -> Anthropic --------------------------------


def _block_text(content: Any) -> str:
    """Flatten an OpenAI message `content` (str, or a list of `{type,text}`
    parts) to a plain string. Image parts are dropped — use `_translate_content`
    where images must be preserved (user turns)."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "".join(p.get("text", "") for p in content if isinstance(p, dict) and p.get("type") == "text")
    return "" if content is None else str(content)


def _data_url_to_image_block(url: str) -> dict[str, Any] | None:
    """`data:image/jpeg;base64,<data>` → Anthropic `{type:image, source:{…}}`.
    Remote (non-data) URLs are skipped — zero-egress: the adapter never fetches."""
    if not isinstance(url, str) or not url.startswith("data:"):
        return None
    try:
        head, data = url[len("data:"):].split(",", 1)
    except ValueError:
        return None
    media_type = head.split(";")[0] or "image/png"
    return {"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}}


def _translate_content(content: Any) -> Any:
    """OpenAI `content` (str | list of parts) → Anthropic content (str | block
    list). Text parts collapse to text; `image_url` data URLs become image blocks.
    Returns a plain string when there is no image, so the common path is unchanged."""
    if isinstance(content, str):
        return content
    if not isinstance(content, list):
        return "" if content is None else str(content)
    blocks: list[dict[str, Any]] = []
    for p in content:
        if not isinstance(p, dict):
            continue
        if p.get("type") == "text":
            blocks.append({"type": "text", "text": p.get("text", "")})
        elif p.get("type") == "image_url":
            img = _data_url_to_image_block((p.get("image_url") or {}).get("url", ""))
            if img:
                blocks.append(img)
    if all(b.get("type") == "text" for b in blocks):
        return "".join(b.get("text", "") for b in blocks)
    return blocks


def _hoist_system(messages: list[dict[str, Any]]) -> tuple[str, list[dict[str, Any]]]:
    """Pull `system`/`developer` messages out of the array and join their text
    into the top-level `system` string Anthropic expects."""
    sys_parts: list[str] = []
    rest: list[dict[str, Any]] = []
    for m in messages:
        if m.get("role") in ("system", "developer"):
            t = _block_text(m.get("content"))
            if t:
                sys_parts.append(t)
        else:
            rest.append(m)
    return "\n".join(sys_parts), rest


def _translate_messages(messages: list[dict[str, Any]], thinking_on: bool = False) -> list[dict[str, Any]]:
    """OpenAI user/assistant/tool messages -> Anthropic user/assistant messages
    with content blocks. Consecutive `tool` messages coalesce into ONE user
    message whose `tool_result` blocks come first.

    When `thinking_on`, an assistant turn that made tool calls is rehydrated from
    the round-trip cache (its raw thinking + tool_use blocks, signatures intact) so
    Anthropic doesn't 400 on missing thinking blocks. Cache miss / thinking off →
    reconstruct the tool_use blocks from the OpenAI shape (Phase-1 path, no thinking)."""
    out: list[dict[str, Any]] = []
    pending_tool_results: list[dict[str, Any]] = []

    def flush_tool_results() -> None:
        if pending_tool_results:
            out.append({"role": "user", "content": list(pending_tool_results)})
            pending_tool_results.clear()

    for m in messages:
        role = m.get("role")
        if role == "tool":
            pending_tool_results.append(
                {
                    "type": "tool_result",
                    "tool_use_id": m.get("tool_call_id"),
                    "content": _block_text(m.get("content")),
                }
            )
            continue
        flush_tool_results()

        if role == "assistant" and m.get("tool_calls"):
            cached = thinking_cache.get((m["tool_calls"][0] or {}).get("id")) if thinking_on else None
            if cached is not None:
                # Replay the original Anthropic assistant blocks verbatim (thinking +
                # redacted_thinking + tool_use, with signature) — required for the
                # thinking round-trip during tool use.
                out.append({"role": "assistant", "content": cached})
                continue
            blocks: list[dict[str, Any]] = []
            text = _block_text(m.get("content"))
            if text:
                blocks.append({"type": "text", "text": text})
            for tc in m["tool_calls"]:
                fn = tc.get("function") or {}
                args = fn.get("arguments")
                if isinstance(args, str):
                    try:
                        args = json.loads(args)
                    except json.JSONDecodeError:
                        args = {}
                blocks.append(
                    {"type": "tool_use", "id": tc.get("id"), "name": fn.get("name"), "input": args or {}}
                )
            out.append({"role": "assistant", "content": blocks})
        else:
            # User/assistant text turns: preserve image parts (vision) via
            # _translate_content; it returns a plain string when there is no image.
            out.append({"role": role, "content": _translate_content(m.get("content"))})

    flush_tool_results()
    return out


def _translate_tools(tools: list[dict[str, Any]] | None) -> list[dict[str, Any]] | None:
    """OpenAI `{type:function, function:{name,description,parameters}}` ->
    Anthropic `{name, description, input_schema}`."""
    if not tools:
        return None
    out = []
    for t in tools:
        fn = t.get("function") or t
        out.append(
            {
                "name": fn.get("name"),
                "description": fn.get("description", ""),
                "input_schema": fn.get("parameters") or {"type": "object", "properties": {}},
            }
        )
    return out


def _translate_tool_choice(tc: Any) -> dict[str, Any] | None:
    if tc is None:
        return None
    if tc == "auto":
        return {"type": "auto"}
    if tc == "required":
        return {"type": "any"}
    if tc == "none":
        return {"type": "none"}
    if isinstance(tc, dict) and tc.get("type") == "function":
        return {"type": "tool", "name": (tc.get("function") or {}).get("name")}
    return None


# --- response translation: Anthropic -> OpenAI -------------------------------

_STOP_MAP = {
    "end_turn": "stop",
    "stop_sequence": "stop",
    "max_tokens": "length",
    "tool_use": "tool_calls",
    "refusal": "content_filter",
}


def _map_stop_reason(sr: str | None) -> str | None:
    return _STOP_MAP.get(sr, sr)


def _map_usage(u: dict[str, Any] | None) -> dict[str, Any]:
    u = u or {}
    pt = u.get("input_tokens") or 0
    ct = u.get("output_tokens") or 0
    out = {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": pt + ct}
    # Normalise any thinking-token count Anthropic reports into the platform's
    # single `reasoning_tokens` field. Field naming has
    # varied across API versions, so probe the known spellings; absent ⇒ omit
    # (extended-thinking tokens are otherwise already inside `output_tokens`).
    tt = u.get("thinking_tokens") or u.get("reasoning_output_tokens")
    if tt:
        out["reasoning_tokens"] = tt
    return out


def _parse_message_content(blocks: list[dict[str, Any]] | None) -> tuple[str, list[dict[str, Any]]]:
    """Anthropic content blocks -> (answer text, tool_calls). tool_calls carry
    `arguments` as a dict (chat_step's normalised contract)."""
    text_parts: list[str] = []
    tool_calls: list[dict[str, Any]] = []
    for b in blocks or []:
        bt = b.get("type")
        if bt == "text":
            text_parts.append(b.get("text", ""))
        elif bt == "tool_use":
            tool_calls.append({"id": b.get("id"), "name": b.get("name"), "arguments": b.get("input") or {}})
    return "".join(text_parts), tool_calls


# --- request building / transport --------------------------------------------


def _headers() -> dict[str, str]:
    return {
        "x-api-key": cfg("llm_api_key", settings.llm_api_key),
        "anthropic-version": ANTHROPIC_VERSION,
        "content-type": "application/json",
    }


def _url() -> str:
    base = cfg("llm_base_url", settings.llm_base_url).rstrip("/")
    return f"{base}/messages"


def _build_body(
    messages: list[dict[str, Any]],
    sampling: dict[str, Any],
    model: str | None,
    *,
    stream: bool,
    tools: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    think_frag, thinking_on = _thinking_config()
    system, msgs = _hoist_system(messages)
    max_tokens = sampling.get("max_tokens") or settings.llm_default_max_tokens
    body: dict[str, Any] = {
        "model": model or cfg("llm_model", settings.llm_model),
        "messages": _translate_messages(msgs, thinking_on),
        "max_tokens": max_tokens,
        "stream": stream,
    }
    if system:
        body["system"] = system
    if thinking_on:
        body.update(think_frag)
        # budget_tokens must be < max_tokens — bump the cap if it isn't.
        budget = (think_frag.get("thinking") or {}).get("budget_tokens")
        if budget and budget >= body["max_tokens"]:
            body["max_tokens"] = budget + 1024
        # Sampling params are removed under thinking (adaptive models 400 on
        # temperature/top_p/top_k) — do not send them.
    else:
        # Native Messages API accepts temperature (0–1) and top_p — unlike the
        # OpenAI-compat shim that 400s on temperature (the reason `llm_omit_sampling`
        # exists). The adapter owns params directly, so that flag does not apply here.
        temp = sampling.get("temperature", 0)
        body["temperature"] = min(1.0, max(0.0, float(temp)))
        if "top_p" in sampling:
            body["top_p"] = sampling["top_p"]
        # frequency_penalty / presence_penalty are unsupported by Anthropic — never sent.
    a_tools = _translate_tools(tools)
    if a_tools:
        body["tools"] = a_tools
        tc = _translate_tool_choice(sampling.get("tool_choice"))
        if tc:
            # Forced tool_choice (any/tool) is forbidden with thinking — downgrade to auto.
            if thinking_on and tc.get("type") in ("any", "tool"):
                tc = {"type": "auto"}
            body["tool_choice"] = tc
    return body


async def _post(body: dict[str, Any]):
    """POST with bounded backoff on 429/529 (overloaded). Also drops `temperature`/
    `top_p` and retries once if a model rejects them — newer Anthropic flagships
    deprecate sampling params ("temperature is deprecated for this model"); model
    names churn, so we react to the 400 rather than hardcode a model list."""
    client = http_client.get_client()
    url, headers = _url(), _headers()
    dropped_sampling = False
    for attempt in range(_MAX_RETRIES):
        r = await client.post(url, json=body, headers=headers)
        if r.status_code in (429, 529) and attempt < _MAX_RETRIES - 1:
            retry_after = float(r.headers.get("retry-after") or 0) or (2**attempt)
            logger.warning(
                "Anthropic %s (req-id %s); backoff %.1fs", r.status_code, r.headers.get("request-id"), retry_after
            )
            await asyncio.sleep(retry_after)
            continue
        if (
            r.status_code == 400
            and not dropped_sampling
            and ("temperature" in body or "top_p" in body)
            and ("temperature" in r.text or "top_p" in r.text)
        ):
            logger.warning("Anthropic rejected sampling params; retrying without temperature/top_p")
            body.pop("temperature", None)
            body.pop("top_p", None)
            dropped_sampling = True
            continue
        return r
    return r


# --- async entry points (mirror llm.py) --------------------------------------


async def a_chat_step(
    messages: list[dict[str, Any]],
    tools: list[dict[str, Any]] | None = None,
    sampling: dict[str, Any] | None = None,
    model: str | None = None,
) -> dict[str, Any]:
    from . import llm  # lazy: stage/usage helpers live in llm

    stage = llm._consume_stage()
    sampling = sampling or {}
    body = _build_body(messages, sampling, model, stream=False, tools=tools)
    r = await _post(body)
    if r.status_code != 200:
        logger.warning("Anthropic chat_step %s: %.500r", r.status_code, r.text)
        r.raise_for_status()
    data = r.json()
    usage = _map_usage(data.get("usage"))
    llm._record_usage(stage, usage)
    raw_blocks = data.get("content") or []
    content, tool_calls = _parse_message_content(raw_blocks)
    # Thinking round-trip: when this turn made tool calls under extended thinking,
    # stash the raw assistant blocks (thinking + signature + tool_use) so the next
    # request can replay them verbatim (Anthropic 400s otherwise).
    if tool_calls and _thinking_config()[1]:
        thinking_cache.put([tc["id"] for tc in tool_calls if tc.get("id")], raw_blocks)
    return {
        "content": content,
        "tool_calls": tool_calls,
        "finish_reason": _map_stop_reason(data.get("stop_reason")),
        "usage": usage,
    }


async def a_complete(system: str, user: str, max_tokens: int = 512) -> str:
    from . import llm

    stage = llm._consume_stage()
    msgs: list[dict[str, Any]] = []
    if system:
        msgs.append({"role": "system", "content": system})
    msgs.append({"role": "user", "content": user})
    body = _build_body(msgs, {"max_tokens": max_tokens, "temperature": 0}, None, stream=False)
    r = await _post(body)
    if r.status_code != 200:
        logger.warning("Anthropic complete %s: %.500r", r.status_code, r.text)
        r.raise_for_status()
    data = r.json()
    llm._record_usage(stage, _map_usage(data.get("usage")))
    content, _ = _parse_message_content(data.get("content"))
    return content


def _handle_stream_event(obj: dict[str, Any], state: dict[str, Any]) -> list[dict[str, Any]]:
    """Pure SSE-event reducer (unit-tested). Mutates `state` (`usage`/`finish`)
    and returns zero or more `{"type":"token","delta":...}` events. Raises
    LlmError on a mid-stream `error` event."""
    from .llm import LlmError

    t = obj.get("type")
    if t == "message_start":
        u = (obj.get("message") or {}).get("usage")
        if u:
            state["usage"] = _map_usage(u)
    elif t == "content_block_delta":
        d = obj.get("delta") or {}
        dt = d.get("type")
        if dt == "thinking_delta":
            # Reasoning trace on the dedicated channel;
            # suppressed when the user hid the trace.
            if state.get("trace_on", True):
                return [{"type": "reasoning", "delta": d.get("thinking", "")}]
            return []
        if dt == "signature_delta":
            return []  # round-trip only (handled via the non-stream cache); not displayed
        if dt == "text_delta":
            return [{"type": "token", "delta": d.get("text", "")}]
        # input_json_delta (tool_use args) — accumulate; final-answer streams
        # rarely carry one, but keep it from leaking into the answer text.
        if dt == "input_json_delta":
            state.setdefault("tool_json", "")
            state["tool_json"] += d.get("partial_json", "")
    elif t == "message_delta":
        sr = (obj.get("delta") or {}).get("stop_reason")
        if sr:
            state["finish"] = _map_stop_reason(sr)
        u = obj.get("usage")
        if u:
            # message_delta carries cumulative output_tokens; merge with the
            # input_tokens captured at message_start.
            merged = dict(state.get("usage") or {})
            ct = u.get("output_tokens")
            if ct is not None:
                merged["completion_tokens"] = ct
                merged["total_tokens"] = (merged.get("prompt_tokens") or 0) + ct
            state["usage"] = merged
    elif t == "error":
        err = obj.get("error") or {}
        raise LlmError(f"Anthropic stream error: {err.get('type')}: {err.get('message')}")
    # ping / content_block_start / content_block_stop / message_stop -> no token
    return []


async def a_stream_chat(
    messages: list[dict[str, Any]],
    sampling: dict[str, Any],
    model: str | None = None,
) -> AsyncIterator[dict[str, Any]]:
    from . import llm
    from .llm import LlmError

    stage = llm._consume_stage()
    model = model or cfg("llm_model", settings.llm_model)
    body = _build_body(messages, sampling, model, stream=True)
    state: dict[str, Any] = {"usage": None, "finish": None, "trace_on": _trace_on()}

    client = http_client.get_client()
    # Drop temperature/top_p and retry once if a newer model rejects them (mirrors
    # `_post`); model names churn, so react to the 400 rather than hardcode.
    for attempt in range(2):
        async with client.stream("POST", _url(), json=body, headers=_headers()) as resp:
            if resp.status_code != 200:
                err_body = await resp.aread()
                btext = err_body.decode("utf-8", "replace") if isinstance(err_body, bytes) else str(err_body)
                if (
                    attempt == 0
                    and resp.status_code == 400
                    and ("temperature" in body or "top_p" in body)
                    and ("temperature" in btext or "top_p" in btext)
                ):
                    logger.warning("Anthropic stream rejected sampling params; retrying without temperature/top_p")
                    body.pop("temperature", None)
                    body.pop("top_p", None)
                    continue
                logger.warning("Anthropic stream %s: %.500r", resp.status_code, btext)
                raise LlmError(f"Anthropic upstream returned {resp.status_code}")
            async for line in resp.aiter_lines():
                line = line.strip()
                if not line.startswith("data:"):
                    continue  # skip `event:` lines — the `type` is in the JSON
                payload = line[len("data:") :].strip()
                try:
                    obj = json.loads(payload)
                except json.JSONDecodeError:
                    continue
                for ev in _handle_stream_event(obj, state):
                    yield ev
                if obj.get("type") == "message_stop":
                    break
        break  # streamed (or raised) — don't reconnect

    llm._record_usage(stage, state.get("usage"))
    yield {
        "type": "done",
        "finish_reason": state.get("finish"),
        "model": model,
        "usage": state.get("usage") or {},
    }
