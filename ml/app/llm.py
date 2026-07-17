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

"""OpenAI-shape streaming chat client. Forwards deltas with no buffering so
TTFT is preserved end to end (chat-turn)."""

import contextvars
import json
import logging
import re
from collections.abc import AsyncIterator
from typing import Any
from urllib.parse import urlparse

from prometheus_client import Counter

from . import anthropic_adapter, gemini_adapter, http_client
from .config import settings
from .rag_ctx import cfg

logger = logging.getLogger("pai-ml.llm")

# OpenAI's Chat Completions API renamed `max_tokens` → `max_completion_tokens`, and
# its reasoning models (gpt-5.x, o-series) reject a non-default `temperature`/`top_p`.
# Other OpenAI-compatible servers (vLLM, Ollama, llama.cpp) keep the classic
# `max_tokens` + sampling, so gate this on the OpenAI host only — every non-OpenAI
# path stays byte-identical.
_OPENAI_REASONING_RE = re.compile(r"^(gpt-5|o[1-9])", re.IGNORECASE)


def _is_openai(base_url: str | None) -> bool:
    host = (urlparse(base_url or "").hostname or "").lower()
    return host == "api.openai.com" or host.endswith(".openai.com")


def _gen_caps(base_url: str | None, model: str | None) -> tuple[str, bool]:
    """(token-cap field name, sampling-allowed) for the configured endpoint+model."""
    if _is_openai(base_url):
        reasoning = bool(_OPENAI_REASONING_RE.match((model or "").strip()))
        return "max_completion_tokens", not reasoning
    return "max_tokens", True


def _reasoning_request(sampling: dict[str, Any]) -> dict[str, Any] | None:
    """Resolve the effective reasoning intent for this call.

    An explicit `reasoning_effort` on the Sampling (utility calls, e.g. chat-title
    naming) takes precedence; otherwise read the per-turn override keys the backend
    set from the user's control (`llm_reasoning_enabled/_level/_trace`). Returns
    `None` when nothing was requested ⇒ provider defaults, wire byte-unchanged."""
    explicit = sampling.get("reasoning_effort")
    if explicit:
        return {"enabled": True, "level": str(explicit), "trace": True}
    enabled = cfg("llm_reasoning_enabled", None)
    if enabled is None:
        return None
    return {
        "enabled": str(enabled).lower() == "true",
        "level": cfg("llm_reasoning_level", None),
        "trace": str(cfg("llm_reasoning_trace", "true")).lower() == "true",
    }


def _openai_reasoning_effort(model: str | None, req: dict[str, Any]) -> str | None:
    """Map the unified reasoning request to OpenAI's `reasoning_effort`, or `None`
    to omit. Gate the caller on `_is_openai` first — only reasoning models accept
    the field; on a plain chat model (gpt-4o/*-chat) it is a HARD error, not a
    no-op. Note (gpt-5.x): Chat Completions accepts function tools ONLY when
    reasoning is disabled (`reasoning_effort:"none"`); a call that keeps reasoning
    on while passing tools must use the Responses API instead."""
    m = (model or "").strip().lower()
    if not _OPENAI_REASONING_RE.match(m):
        return None  # non-reasoning OpenAI model — never send (else 400)
    if not req.get("enabled", False):
        # gpt-5.x documents `none` (fully off); o-series cannot disable → omit.
        return "none" if m.startswith("gpt-5") else None
    level = (req.get("level") or "").strip().lower()
    if level in ("", "auto"):
        return None  # let the model pick its default effort
    if level == "minimal":
        # Utility fast-path: gpt-5.x → `none` (instant; `minimal` isn't universal,
        # e.g. gpt-5.5 rejects it); o-series floor is `low`.
        return "none" if m.startswith("gpt-5") else "low"
    if level == "max":
        return "high"
    if level in ("low", "medium", "high", "xhigh"):
        return level
    return None


def _normalise_reasoning_tokens(usage: dict[str, Any] | None) -> dict[str, Any] | None:
    """Surface a normalised `reasoning_tokens` on an OpenAI-shape usage dict
    (OpenAI nests it under `completion_tokens_details.reasoning_tokens`). No-op if
    absent. Anthropic/Gemini are normalised in their own adapters."""
    if not usage:
        return usage
    if usage.get("reasoning_tokens") is None:
        rt = (usage.get("completion_tokens_details") or {}).get("reasoning_tokens")
        if rt:
            usage["reasoning_tokens"] = rt
    return usage

# Per-stage token spend. A caller tags the next call with
# `set_stage("decompose"/"grade"/"generate"/…)` and the real client attributes the
# response `usage` to it — the loops were spending tokens blind. The label rides a
# ContextVar (not a call kwarg) so it is async-safe across the concurrent fan-out
# AND invisible to test doubles that stub `complete`/`stream_chat`. `kind` is
# prompt|completion.
_LLM_TOKENS = Counter("llm_tokens_total", "LLM tokens by stage and kind", ["stage", "kind"])
_stage_var: contextvars.ContextVar[str | None] = contextvars.ContextVar("llm_stage", default=None)

# vLLM guided-decoding fragment for the next call.
# Rides a ContextVar for the SAME reasons as `_stage_var`: async-safe across the
# concurrent fan-out, and INVISIBLE to the ~30 test doubles that stub `complete`
# (a `guided=` kwarg would break every one of their signatures). Consumed at call
# entry and merged into the body only when `settings.llm_guided_decoding` is on.
_guided_var: contextvars.ContextVar[dict[str, Any] | None] = contextvars.ContextVar(
    "llm_guided", default=None
)


def set_stage(stage: str | None) -> None:
    """Tag the next LLM call in this task with a pipeline stage (consumed once)."""
    _stage_var.set(stage)


def set_guided(guided: dict[str, Any] | None) -> None:
    """Tag the next complete()/chat_step() in this task with a vLLM guided-decoding
    fragment (`guided_json` / `guided_choice`, see app/guided.py). Consumed once;
    inert unless `settings.llm_guided_decoding` is on (the vLLM profile)."""
    _guided_var.set(guided)


def _consume_stage() -> str | None:
    """Take the pending stage at call ENTRY (get + clear). Consuming before the
    HTTP request means a raised call can never leak its stage onto the next
    untagged call in the same task — the failure mode would otherwise corrupt
    the per-stage spend metric exactly when a pipeline degrades."""
    stage = _stage_var.get()
    if stage is not None:
        _stage_var.set(None)
    return stage


def _consume_guided() -> dict[str, Any] | None:
    """Take the pending guided fragment at call ENTRY (get + clear) — same
    leak-proofing as `_consume_stage`."""
    g = _guided_var.get()
    if g is not None:
        _guided_var.set(None)
    return g


def _record_usage(stage: str | None, usage: dict[str, Any] | None) -> None:
    """Account a call's token usage to its consumed stage. No-op without a stage
    (single-shot calls that skip attribution)."""
    if not stage or not usage:
        return
    pt = usage.get("prompt_tokens")
    ct = usage.get("completion_tokens")
    if pt:
        _LLM_TOKENS.labels(stage=stage, kind="prompt").inc(pt)
    if ct:
        _LLM_TOKENS.labels(stage=stage, kind="completion").inc(ct)
    logger.info("llm spend stage=%s prompt=%s completion=%s", stage, pt, ct)


class LlmError(Exception):
    pass


def _tool_call_shape(obj: Any) -> tuple[str | None, dict]:
    """(name, arguments) when `obj` looks like a tool call — top-level `name`+`arguments`
    or an OpenAI `{"function": {...}}` wrapper — else (None, {}). A plausible tool name
    is a non-empty, space-free string (so a normal `{...}` in prose never matches)."""
    if not isinstance(obj, dict):
        return None, {}
    fn = obj.get("function")
    has_args = "arguments" in obj or (isinstance(fn, dict) and "arguments" in fn)
    if isinstance(fn, dict):
        name, args = fn.get("name"), fn.get("arguments")
    else:
        name, args = obj.get("name"), obj.get("arguments")
    if not has_args or not isinstance(name, str) or not name.strip() or " " in name.strip():
        return None, {}
    if isinstance(args, str):
        try:
            args = json.loads(args)
        except (json.JSONDecodeError, ValueError):
            args = {}
    return name.strip(), args if isinstance(args, dict) else {}


def _extract_text_tool_call(content: str) -> tuple[list[dict], str]:
    """Recover a tool call a model emitted as TEXT in `content` instead of the native
    `tool_calls` array — e.g. a `read_skill` UUID leaked into an answer.
    Brace-balanced scan (string-aware) for the first tool-call-shaped JSON object; on a
    match return `([call], content_without_that_span)` so the loop EXECUTES the call and
    the JSON never renders. No match → `([], content)` unchanged."""
    if "{" not in content or "arguments" not in content:
        return [], content
    n = len(content)
    i = 0
    while i < n:
        if content[i] != "{":
            i += 1
            continue
        depth, j, in_str, esc = 0, i, False, False
        while j < n:
            c = content[j]
            if in_str:
                if esc:
                    esc = False
                elif c == "\\":
                    esc = True
                elif c == '"':
                    in_str = False
            elif c == '"':
                in_str = True
            elif c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    break
            j += 1
        if depth != 0:
            break  # unbalanced from here on — give up
        try:
            obj = json.loads(content[i : j + 1])
        except (json.JSONDecodeError, ValueError):
            i += 1
            continue
        name, args = _tool_call_shape(obj)
        if name:
            stripped = (content[:i] + content[j + 1 :]).strip()
            return [{"id": None, "name": name, "arguments": args}], stripped
        i = j + 1
    return [], content


def _finalise_stream_tool_calls(acc: dict[int, dict[str, Any]]) -> list[dict[str, Any]]:
    """Turn accumulated streamed tool-call fragments into `tool_call` events, in call
    order. Each call's arguments are parsed exactly once; a call with no name, or whose
    arguments never became valid JSON, is dropped with a warning rather than executed
    half-formed. `arguments` is emitted as a parsed object (the shape the tool loop and
    the non-streaming step both use)."""
    events: list[dict[str, Any]] = []
    for idx in sorted(acc):
        slot = acc[idx]
        name = (slot.get("name") or "").strip()
        if not name:
            continue
        raw = slot.get("args") or "{}"
        try:
            parsed = json.loads(raw)
        except (json.JSONDecodeError, ValueError):
            logger.warning("dropping malformed streamed tool_call args for %s: %.200r", name, raw)
            continue
        events.append({
            "type": "tool_call",
            "id": slot.get("id"),
            "name": name,
            "arguments": parsed if isinstance(parsed, dict) else {},
        })
    return events


async def chat_step(
    messages: list[dict[str, Any]],
    tools: list[dict[str, Any]] | None = None,
    sampling: dict[str, Any] | None = None,
    model: str | None = None,
) -> dict[str, Any]:
    """Non-streaming step for the tool-call loop. Returns content + normalised
    tool_calls (arguments parsed to a dict). Non-streaming sidesteps flaky
    streamed tool_calls; the final answer streams via `stream_chat`."""
    sampling = sampling or {}
    base = cfg("llm_base_url", settings.llm_base_url)
    if anthropic_adapter.is_anthropic(base):
        return await anthropic_adapter.a_chat_step(messages, tools, sampling, model)
    if gemini_adapter.is_gemini_native(base):
        return await gemini_adapter.a_chat_step(messages, tools, sampling, model)
    stage = _consume_stage()
    guided = _consume_guided()
    model = model or cfg("llm_model", settings.llm_model)
    token_key, sampling_ok = _gen_caps(cfg("llm_base_url", settings.llm_base_url), model)
    omit_sampling = cfg("llm_omit_sampling", settings.llm_omit_sampling) or not sampling_ok
    payload: dict[str, Any] = {
        "model": model,
        "messages": messages,
        "stream": False,
        token_key: sampling.get("max_tokens") or settings.llm_default_max_tokens,
    }
    if not omit_sampling:
        payload["temperature"] = sampling.get("temperature", 0)
        if "top_p" in sampling and sampling["top_p"] is not None:
            payload["top_p"] = sampling["top_p"]
        if "frequency_penalty" in sampling:
            payload["frequency_penalty"] = sampling["frequency_penalty"]
        if "presence_penalty" in sampling:
            payload["presence_penalty"] = sampling["presence_penalty"]
    req = _reasoning_request(sampling)
    effort = _openai_reasoning_effort(model, req) if (req and _is_openai(base)) else None
    if effort:
        payload["reasoning_effort"] = effort
    # Local/vLLM qwen models: steer `enable_thinking` from the reasoning toggle (they
    # ignore `reasoning_effort`). Off ⇒ no <think> on the tool-deciding steps too.
    if req is not None and not _is_openai(base):
        _lvl = (req.get("level") or "").strip().lower()
        payload.setdefault("chat_template_kwargs", {})["enable_thinking"] = (
            bool(req.get("enabled")) and _lvl != "minimal"
        )
    if tools:
        payload["tools"] = tools
    if guided and settings.llm_guided_decoding:
        payload.update(guided)
    url = f"{cfg('llm_base_url', settings.llm_base_url).rstrip('/')}/chat/completions"
    headers = {"Authorization": f"Bearer {cfg('llm_api_key', settings.llm_api_key)}"}
    client = http_client.get_client()
    r = await client.post(url, json=payload, headers=headers)
    # Graceful degradation: models without a tool template (e.g. small GGUFs
    # via Ollama) reject the `tools` param with a 4xx. Retry once without it so
    # chat still works — the turn just won't make tool calls this round.
    if tools and r.status_code >= 400:
        logger.warning("LLM rejected tools (%s); retrying without tools", r.status_code)
        payload.pop("tools", None)
        r = await client.post(url, json=payload, headers=headers)
    # Graceful degradation: a model may reject our `reasoning_effort` value (the
    # accepted set differs across gpt-5/gpt-5.x/o-series/Gemini). Drop it and retry
    # once — the call still succeeds (reasoning just isn't minimised this turn),
    # so a utility call like chat-title naming never hard-fails on this param.
    if effort and r.status_code == 400 and "reasoning_effort" in r.text:
        logger.warning("LLM rejected reasoning_effort=%s; retrying without it", effort)
        payload.pop("reasoning_effort", None)
        r = await client.post(url, json=payload, headers=headers)
    if r.status_code != 200:
        # Surface the upstream error body server-side — raise_for_status() hides
        # it, which makes provider misconfig (wrong endpoint shape, rejected
        # params) impossible to diagnose from the logs.
        logger.warning("LLM chat_step upstream %s: %.500r", r.status_code, r.text)
        r.raise_for_status()
    data = r.json()
    _record_usage(stage, data.get("usage"))
    choice = data["choices"][0]
    msg = choice.get("message") or {}
    tool_calls = []
    for t in msg.get("tool_calls") or []:
        fn = t.get("function") or {}
        args = fn.get("arguments")
        if isinstance(args, str):
            try:
                args = json.loads(args)
            except json.JSONDecodeError:
                args = {}
        tool_calls.append({"id": t.get("id"), "name": fn.get("name"), "arguments": args or {}})
    content = msg.get("content") or ""
    # a model may emit a tool call as TEXT in `content` instead of
    # the native array (this leaked a `read_skill` UUID into an answer). When no native
    # call came back, recover it so the loop EXECUTES it and the JSON never renders.
    if not tool_calls:
        recovered, content = _extract_text_tool_call(content)
        tool_calls = recovered
    return {
        "content": content,
        "tool_calls": tool_calls,
        "finish_reason": choice.get("finish_reason"),
        "usage": _normalise_reasoning_tokens(data.get("usage")) or {},
    }


async def complete(system: str, user: str, max_tokens: int = 512, *, reasoning_effort: str | None = "minimal") -> str:
    """Non-streaming completion — used by the agentic loop (decompose, grade,
    reformulate). Off the chat hot prefix. Token spend is attributed to the stage
    set via `set_stage` before the call.

: these scaffolding calls only need a JSON array / one-word
    grade / one-line query, so they default to `reasoning_effort="minimal"` — on an
    OpenAI reasoning model (gpt-5.x) that maps to `"none"` (reasoning OFF), which is
    the main latency win (the heavy answer path keeps full reasoning). The effort is
    OpenAI-gated, so vLLM/Ollama/llama.cpp bodies stay byte-identical. Endpoint/model
    resolve `utility_*` first (a future fast/cheap provider role) then fall back to the
    `llm_*` values, so an un-configured deploy behaves exactly as before.

    A guided-decoding fragment set via `set_guided` before the call is merged into the
    body ONLY when `settings.llm_guided_decoding` is on (the vLLM profile); otherwise
    the body is byte-identical to the prompt-only path."""
    base = cfg("utility_base_url", cfg("llm_base_url", settings.llm_base_url))
    model = cfg("utility_model", cfg("llm_model", settings.llm_model))
    api_key = cfg("utility_api_key", cfg("llm_api_key", settings.llm_api_key))
    if anthropic_adapter.is_anthropic(base):
        return await anthropic_adapter.a_complete(system, user, max_tokens)
    if gemini_adapter.is_gemini_native(base):
        return await gemini_adapter.a_complete(system, user, max_tokens)
    stage = _consume_stage()
    guided = _consume_guided()
    token_key, sampling_ok = _gen_caps(base, model)
    payload = {
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "stream": False,
        token_key: max_tokens,
    }
    if not cfg("llm_omit_sampling", settings.llm_omit_sampling) and sampling_ok:
        payload["temperature"] = 0
    # Minimise reasoning on the scaffolding calls (OpenAI reasoning models only; the
    # field is never sent to vLLM/local, so those bodies are unchanged).
    req = _reasoning_request({"reasoning_effort": reasoning_effort}) if reasoning_effort else None
    effort = _openai_reasoning_effort(model, req) if (req and _is_openai(base)) else None
    if effort:
        payload["reasoning_effort"] = effort
    if guided and settings.llm_guided_decoding:
        payload.update(guided)
    url = f"{base.rstrip('/')}/chat/completions"
    headers = {"Authorization": f"Bearer {api_key}"}
    client = http_client.get_client()
    r = await client.post(url, json=payload, headers=headers)
    # Graceful degradation: a model may reject our reasoning_effort value; drop it and
    # retry once so the scaffolding call still succeeds (mirrors chat_step).
    if effort and r.status_code == 400 and "reasoning_effort" in r.text:
        logger.warning("LLM rejected reasoning_effort=%s; retrying without it", effort)
        payload.pop("reasoning_effort", None)
        r = await client.post(url, json=payload, headers=headers)
    if r.status_code != 200:
        logger.warning("LLM complete upstream %s: %.500r", r.status_code, r.text)
        r.raise_for_status()
    data = r.json()
    _record_usage(stage, data.get("usage"))
    return data["choices"][0]["message"]["content"] or ""


class _ResponsesUnsupported(Exception):
    """Raised (before any event is yielded) when the OpenAI Responses API rejects the
    call, so `stream_chat` can transparently fall back to chat-completions."""


def _normalise_responses_usage(usage: dict[str, Any] | None) -> dict[str, Any] | None:
    """Map Responses-API usage (`input_tokens`/`output_tokens`/`output_tokens_details.
    reasoning_tokens`) onto the chat-shape (`prompt_tokens`/`completion_tokens`/
    `reasoning_tokens`) the backend + metrics expect."""
    if not usage:
        return usage
    out: dict[str, Any] = {
        "prompt_tokens": usage.get("input_tokens"),
        "completion_tokens": usage.get("output_tokens"),
        "total_tokens": usage.get("total_tokens"),
    }
    rt = (usage.get("output_tokens_details") or {}).get("reasoning_tokens")
    if rt:
        out["reasoning_tokens"] = rt
    return out


def _responses_effort(effort: str | None) -> str:
    """Clamp our unified effort to a value the Responses API `reasoning.effort` accepts
    (low/medium/high; `minimal` for gpt-5, but we only reach here with reasoning ON)."""
    return effort if effort in ("minimal", "low", "medium", "high") else "high"


def _responses_tools(tools: list[dict[str, Any]] | None) -> list[dict[str, Any]]:
    """Translate chat-completions tool schemas (`{type:function, function:{...}}`) to the
    flatter Responses-API function shape (`{type:function, name, description, parameters}`)."""
    out: list[dict[str, Any]] = []
    for t in tools or []:
        fn = t.get("function") or {}
        if fn.get("name"):
            out.append({
                "type": "function",
                "name": fn["name"],
                "description": fn.get("description", ""),
                "parameters": fn.get("parameters") or {"type": "object", "properties": {}},
            })
    return out


async def _stream_responses(
    base: str, model: str | None, messages: list[dict[str, Any]], sampling: dict[str, Any],
    effort: str | None, trace_on: bool, stage: str | None,
    tools: list[dict[str, Any]] | None = None,
) -> AsyncIterator[dict[str, Any]]:
    """OpenAI Responses API stream: summarised reasoning streamed on the
    dedicated `reasoning` channel BEFORE the first answer token, then the answer tokens,
    then a terminal `done`. Raises `_ResponsesUnsupported` (before any yield) on a non-200
    so the caller falls back to chat-completions; a mid-stream failure surfaces as LlmError.

    With `tools`, a function call the model makes is accumulated per output item and
    emitted as a `{"type":"tool_call",...}` event before `done` (finish `tool_calls`)."""
    url = f"{base.rstrip('/')}/responses"
    payload: dict[str, Any] = {
        "model": model,
        "input": messages,
        "reasoning": {"effort": _responses_effort(effort), "summary": "auto"},
        "max_output_tokens": sampling.get("max_tokens") or settings.llm_default_max_tokens,
        "stream": True,
    }
    if tools:
        payload["tools"] = _responses_tools(tools)
    headers = {"Authorization": f"Bearer {cfg('llm_api_key', settings.llm_api_key)}"}
    client = http_client.get_client()
    usage: dict[str, Any] | None = None
    finish: str | None = None
    # Function calls stream as their own output items: `output_item.added` carries the
    # id/name, `function_call_arguments.delta` the JSON fragments, `.done` the full string.
    tool_acc: dict[str, dict[str, Any]] = {}
    async with client.stream("POST", url, json=payload, headers=headers) as resp:
        if resp.status_code != 200:
            body = await resp.aread()
            btext = body.decode("utf-8", "replace") if isinstance(body, bytes) else str(body)
            logger.warning(
                "Responses API %s: %.400r — falling back to chat-completions", resp.status_code, btext
            )
            raise _ResponsesUnsupported(f"responses {resp.status_code}")
        async for line in resp.aiter_lines():
            if not line or not line.startswith("data:"):
                continue
            data = line[len("data:") :].strip()
            if not data or data == "[DONE]":
                continue
            try:
                ev = json.loads(data)
            except json.JSONDecodeError:
                continue
            etype = ev.get("type")
            if etype == "response.reasoning_summary_text.delta":
                d = ev.get("delta")
                if d and trace_on:
                    yield {"type": "reasoning", "delta": d}
            elif etype == "response.output_text.delta":
                d = ev.get("delta")
                if d:
                    yield {"type": "token", "delta": d}
            elif etype == "response.output_item.added":
                item = ev.get("item") or {}
                if item.get("type") == "function_call":
                    iid = item.get("id") or ev.get("item_id") or str(len(tool_acc))
                    tool_acc[iid] = {
                        "call_id": item.get("call_id") or item.get("id"),
                        "name": item.get("name"),
                        "args": item.get("arguments") or "",
                    }
            elif etype == "response.function_call_arguments.delta":
                iid = ev.get("item_id") or ""
                slot = tool_acc.setdefault(iid, {"call_id": None, "name": None, "args": ""})
                slot["args"] += ev.get("delta") or ""
            elif etype == "response.function_call_arguments.done":
                iid = ev.get("item_id") or ""
                slot = tool_acc.setdefault(iid, {"call_id": None, "name": None, "args": ""})
                if ev.get("arguments"):
                    slot["args"] = ev["arguments"]  # authoritative full string
            elif etype in ("response.completed", "response.incomplete"):
                r = ev.get("response") or {}
                usage = r.get("usage")
                finish = "stop" if etype == "response.completed" else (
                    (r.get("incomplete_details") or {}).get("reason") or "length"
                )
            elif etype in ("error", "response.failed"):
                raise LlmError(f"Responses API stream error: {str(ev)[:300]}")
    for slot in tool_acc.values():
        name = (slot.get("name") or "").strip()
        if not name:
            continue
        try:
            parsed = json.loads(slot.get("args") or "{}")
        except (json.JSONDecodeError, ValueError):
            logger.warning("dropping malformed Responses tool_call args for %s", name)
            continue
        finish = "tool_calls"
        yield {
            "type": "tool_call",
            "id": slot.get("call_id"),
            "name": name,
            "arguments": parsed if isinstance(parsed, dict) else {},
        }
    usage = _normalise_responses_usage(usage)
    _record_usage(stage, usage)
    yield {"type": "done", "finish_reason": finish, "model": model, "usage": usage or {}}


async def stream_chat(
    messages: list[dict[str, Any]],
    sampling: dict[str, Any],
    model: str | None = None,
    tools: list[dict[str, Any]] | None = None,
) -> AsyncIterator[dict[str, Any]]:
    """Stream an OpenAI chat completion. Yields `{"type":"token","delta":...}`
    answer events and (on the dedicated channel) `{"type":"reasoning","delta":...}`
    trace events, then a final `{"type":"done", finish_reason, model, usage}`.

    When `tools` is given the model may request a tool mid-stream: once a call's
    arguments have fully accumulated and parsed, a `{"type":"tool_call","id","name",
    "arguments":{...}}` event is emitted (never partial args) and the terminal
    `done` carries `finish_reason:"tool_calls"`. The caller executes the tool and
    continues the answer in a follow-up request. `tools=None` ⇒ unchanged."""
    base = cfg("llm_base_url", settings.llm_base_url)
    if anthropic_adapter.is_anthropic(base):
        async for ev in anthropic_adapter.a_stream_chat(messages, sampling, model, tools=tools):
            yield ev
        return
    if gemini_adapter.is_gemini_native(base):
        async for ev in gemini_adapter.a_stream_chat(messages, sampling, model, tools=tools):
            yield ev
        return
    stage = _consume_stage()
    model = model or cfg("llm_model", settings.llm_model)
    token_key, sampling_ok = _gen_caps(base, model)
    payload: dict[str, Any] = {
        "model": model,
        "messages": messages,
        "stream": True,
    }
    if not cfg("llm_omit_sampling", settings.llm_omit_sampling) and sampling_ok:
        if "temperature" in sampling:
            payload["temperature"] = sampling["temperature"]
        if "top_p" in sampling:
            payload["top_p"] = sampling["top_p"]
        if "frequency_penalty" in sampling:
            payload["frequency_penalty"] = sampling["frequency_penalty"]
        if "presence_penalty" in sampling:
            payload["presence_penalty"] = sampling["presence_penalty"]
    # Capability-aware reasoning effort on the streamed answer — only for
    # reasoning-capable OpenAI models; the field is omitted (and
    # never sent to vLLM/Ollama/local) when not applicable, so it can't 400.
    req = _reasoning_request(sampling)
    effort = _openai_reasoning_effort(model, req) if (req and _is_openai(base)) else None
    if effort:
        payload["reasoning_effort"] = effort
    # Whether to forward the reasoning trace to the client (paid for either way).
    trace_on = req.get("trace", True) if req else True
    # Local/vLLM qwen-family models don't honour OpenAI's `reasoning_effort`; steer
    # their chat template's `enable_thinking` instead so the composer's Reasoning
    # toggle actually works (off ⇒ no <think>, fast; on ⇒ think + stream the trace).
    # Leave the template default when nothing was requested (`req is None`).
    if req is not None and not _is_openai(base):
        _lvl = (req.get("level") or "").strip().lower()
        payload.setdefault("chat_template_kwargs", {})["enable_thinking"] = (
            bool(req.get("enabled")) and _lvl != "minimal"
        )
    # Always cap generation so a looping/long turn still completes (and records
    # usage) — agent max_tokens wins, else the configured default.
    payload[token_key] = sampling.get("max_tokens") or settings.llm_default_max_tokens
    if settings.llm_stream_usage:
        payload["stream_options"] = {"include_usage": True}

    # on an OpenAI reasoning model with reasoning enabled + trace on,
    # stream SUMMARISED thoughts before the first answer token via the Responses API
    # (chat-completions does not surface them). Falls back to chat-completions on any
    # endpoint/model rejection (image parts, unsupported model, …), so nothing regresses.
    want_summaries = (
        _is_openai(base)
        and bool(_OPENAI_REASONING_RE.match((model or "").strip()))
        and bool(req and req.get("enabled"))
        and effort not in (None, "none")
        and trace_on
        and cfg("openai_reasoning_summaries", settings.openai_reasoning_summaries)
    )
    # Tools + a reasoning OpenAI model: chat-completions rejects function tools unless
    # reasoning is disabled, so a model told to keep reasoning must go through the
    # Responses API (which carries tools AND reasoning). vLLM always stays on
    # chat-completions (its Responses endpoint mis-emits XML tool calls as text).
    want_tools_responses = bool(tools) and _is_openai(base) and effort not in (None, "none")
    if want_summaries or want_tools_responses:
        try:
            async for ev in _stream_responses(
                base, model, messages, sampling, effort, trace_on, stage, tools=tools
            ):
                yield ev
            return
        except _ResponsesUnsupported:
            pass  # nothing yielded yet — fall through to the chat-completions path below

    if tools:
        payload["tools"] = tools
        # chat-completions accepts function tools only with reasoning disabled
        # (a reasoning model 400s otherwise); the reasoning path went to Responses above.
        if _is_openai(base):
            payload["reasoning_effort"] = "none"

    headers = {"Authorization": f"Bearer {cfg('llm_api_key', settings.llm_api_key)}"}
    url = f"{cfg('llm_base_url', settings.llm_base_url).rstrip('/')}/chat/completions"

    usage: dict[str, Any] | None = None
    finish: str | None = None
    # Streamed tool calls arrive as index-keyed fragments: the id/name land in an
    # early fragment, the JSON arguments accrue across many. Accumulate per index and
    # parse once at the end — the caller never sees a partial call. `content_acc`
    # backs the speculative recovery of a call a model wrote as answer text.
    tool_acc: dict[int, dict[str, Any]] = {}
    content_acc = ""
    client = http_client.get_client()

    # vLLM with `--reasoning-parser qwen3` splits the model's `<think>...</think>`
    # block out of `delta.content` into `delta.reasoning_content`; OpenAI reasoning
    # summaries and the native adapters likewise carry reasoning separately. We emit
    # those on the dedicated `reasoning` channel (the client renders them in its own
    # panel — addendum), keeping the answer stream clean. A model that instead
    # emits literal `<think>` tags inside `content` still works: those tokens flow
    # through as answer text and the frontend's `splitThink` fallback separates them.
    for attempt in range(2):
        async with client.stream("POST", url, json=payload, headers=headers) as resp:
            if resp.status_code != 200:
                body = await resp.aread()
                btext = body.decode("utf-8", "replace") if isinstance(body, bytes) else str(body)
                # Graceful degradation: a reasoning model may reject our effort value
                # (the accepted set differs across gpt-5/gpt-5.x/o-series). Drop it and
                # retry once so the answer still streams (reasoning just isn't tuned).
                if attempt == 0 and effort and resp.status_code == 400 and "reasoning_effort" in btext:
                    logger.warning("LLM rejected reasoning_effort=%s; retrying without it", effort)
                    payload.pop("reasoning_effort", None)
                    continue
                # Log the upstream body server-side; surface only the status to
                # the caller (the body can carry infra detail / echoed headers).
                logger.warning("LLM upstream %s: %.500r", resp.status_code, btext)
                raise LlmError(f"LLM upstream returned {resp.status_code}")

            async for line in resp.aiter_lines():
                if not line or not line.startswith("data:"):
                    continue
                data = line[len("data:") :].strip()
                if data == "[DONE]":
                    break
                try:
                    chunk = json.loads(data)
                except json.JSONDecodeError:
                    continue

                if chunk.get("usage"):
                    usage = chunk["usage"]
                for choice in chunk.get("choices", []):
                    delta = choice.get("delta") or {}
                    # vLLM's reasoning parser streams the trace as `reasoning` (0.24+);
                    # older builds / OpenAI use `reasoning_content`. Accept either.
                    reasoning = delta.get("reasoning_content") or delta.get("reasoning")
                    if reasoning and trace_on:
                        yield {"type": "reasoning", "delta": reasoning}
                    content = delta.get("content")
                    if content:
                        content_acc += content
                        yield {"type": "token", "delta": content}
                    for tc in delta.get("tool_calls") or []:
                        idx = tc.get("index", 0)
                        slot = tool_acc.setdefault(idx, {"id": None, "name": None, "args": ""})
                        if tc.get("id"):
                            slot["id"] = tc["id"]
                        fn = tc.get("function") or {}
                        if fn.get("name"):
                            slot["name"] = fn["name"]
                        if fn.get("arguments"):
                            slot["args"] += fn["arguments"]
                    if choice.get("finish_reason"):
                        finish = choice["finish_reason"]
        break  # streamed (or raised) — don't loop again

    # Emit each fully-accumulated tool call. A call whose arguments never became valid
    # JSON is dropped (logged) rather than half-executed — the stream still completes.
    tool_events = _finalise_stream_tool_calls(tool_acc)
    if not tool_events and tools and finish != "tool_calls":
        # Second chance: a model that emitted the call as answer TEXT instead of the
        # native array (the same leak `chat_step` recovers from).
        recovered, _ = _extract_text_tool_call(content_acc)
        tool_events = [
            {"type": "tool_call", "id": c["id"], "name": c["name"], "arguments": c["arguments"]}
            for c in recovered
        ]
    if tool_events:
        finish = "tool_calls"
        for ev in tool_events:
            yield ev

    usage = _normalise_reasoning_tokens(usage)
    _record_usage(stage, usage)
    yield {
        "type": "done",
        "finish_reason": finish,
        "model": model,
        "usage": usage or {},
    }
