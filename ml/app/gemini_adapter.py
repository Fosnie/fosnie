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

"""Native Google Gemini adapter.

The OpenAI-compat shim Google ships is beta and lossy for reasoning control
(`thinking_level`/`include_thoughts` only reachable via a nested `extra_body`,
and `none`-style disable errors on Pro). For full reasoning + tools we talk the
native `:generateContent` / `:streamGenerateContent` REST API directly — the same
pattern as `anthropic_adapter`: translate OpenAI-shape messages/tools in, map
`generationConfig.thinkingConfig` (thinkingBudget + includeThoughts), and map the
candidate parts (incl. `thought` summaries) back out, emitting reasoning on the
dedicated channel. The Rust backend and the rest of the ML service are unchanged —
they still see the OpenAI shape.

A deployment that *wants* the compat shim points the base_url at
`…/v1beta/openai/`; `is_gemini_native` returns False for that path, so this
adapter is bypassed and `llm.py` uses the OpenAI-compat code path.
"""

from __future__ import annotations

import json
import logging
from collections.abc import AsyncIterator
from typing import Any
from urllib.parse import urlparse

from . import http_client
from .config import settings
from .rag_ctx import cfg
from .inference_target import strict

logger = logging.getLogger("fosnie-ml.gemini")


def is_gemini_native(base_url: str | None) -> bool:
    """True when the endpoint is the native Gemini REST API (not the OpenAI-compat
    shim, which lives under a `/openai` path and is handled by the OpenAI code)."""
    parsed = urlparse(base_url or "")
    host = (parsed.hostname or "").lower()
    is_g = host == "generativelanguage.googleapis.com" or host.endswith(".googleapis.com")
    return is_g and "/openai" not in (parsed.path or "")


def _base() -> str:
    return cfg("llm_base_url", settings.llm_base_url).rstrip("/")


def _api_key() -> str:
    return cfg("llm_api_key", settings.llm_api_key) or ""


def _headers() -> dict[str, str]:
    return {"Content-Type": "application/json", "x-goog-api-key": _api_key()}


def _url(model: str, method: str, sse: bool = False) -> str:
    suffix = "?alt=sse" if sse else ""
    return f"{_base()}/models/{model}:{method}{suffix}"


# --- reasoning config --------------------------------------------------------

# Map the unified effort levels to a Gemini `thinkingBudget` (tokens). `auto`/dynamic
# is -1; disable is 0. Values sit inside Flash's range and above Pro's floor, so they
# are valid across models; a wrong choice degrades (we never hard-require thinking).
_BUDGET = {"low": 4096, "medium": 8192, "high": 16384, "max": 24576}

# Exact hosted IDs documented by Google. A different Gemma/alias is not assumed
# to expose the same API, and no quality rank is inferred from its name.
_GEMMA_LEVEL_MODELS = frozenset({"gemma-4-26b-a4b-it", "gemma-4-31b-it"})


def _uses_thinking_level(model: str) -> bool:
    if model.removeprefix("models/") in _GEMMA_LEVEL_MODELS:return True
    family = model.removeprefix("models/").split("-", 2)
    major = family[1].split(".", 1)[0] if len(family) > 1 and family[0] == "gemini" else ""
    return major.isdigit() and int(major) >= 3


def _level_config(model: str, level: str, trace: bool) -> dict[str, Any]:
    supported = {"minimal", "low", "medium", "high"}
    # Only exact documented model IDs narrow the API enum. No quality inference,
    # broad alias guessing or silently replacing an unsupported effort.
    model = model.removeprefix("models/")
    if model == "gemini-3-pro-preview":supported = {"low", "high"}
    elif model == "gemini-3.1-pro-preview":supported = {"low", "medium", "high"}
    elif model in _GEMMA_LEVEL_MODELS:supported = {"minimal", "high"}
    if level not in supported:
        raise ValueError("selected Gemini effort has no supported parameter mapping")
    # Hosted Gemma accepts the level, but requesting Gemini thought summaries
    # caused live INTERNAL500/timeouts. Do not add that unrelated trace field.
    if model in _GEMMA_LEVEL_MODELS:return {"thinkingLevel": level.upper()}
    return {"thinkingLevel": level.upper(), "includeThoughts": trace}


def _thinking_config(model: str | None = None) -> dict[str, Any] | None:
    """Translate reasoning for the actual request model, preserving its default."""
    model = str(model or cfg("llm_model", settings.llm_model))
    trace = str(cfg("llm_reasoning_trace", "true")).strip().lower() != "false"
    if strict():
        level = cfg("routing_effort", None)
        if level is None:return None
        if _uses_thinking_level(model):return _level_config(model, level, trace)
        if level == "none":return {"thinkingBudget": 0}
        if level not in _BUDGET:
            raise ValueError("selected Gemini effort has no supported parameter mapping")
        return {"thinkingBudget": _BUDGET[level], "includeThoughts": trace}
    enabled = cfg("llm_reasoning_enabled", None)
    spec = (cfg("llm_thinking", settings.llm_thinking) or "off").strip().lower()
    if _uses_thinking_level(model):
        if enabled is None:
            if not spec.startswith("adaptive"):return None
            level = spec.partition(":")[2] or "auto"
        elif str(enabled).lower() != "true":
            if model.removeprefix("models/") in _GEMMA_LEVEL_MODELS:return _level_config(model, "minimal", trace)
            raise ValueError("Gemini 3 cannot guarantee thinking off; select minimal where supported")
        else:level = (cfg("llm_reasoning_level", None) or "auto").strip().lower()
        if level in ("", "auto"):return None if model.removeprefix("models/") in _GEMMA_LEVEL_MODELS else {"includeThoughts": trace}
        return _level_config(model, level, trace)
    # Earlier/unknown families retain the existing budget translation.
    if enabled is None:
        if spec in ("", "off"):return None
        if spec.startswith("adaptive"):return {"thinkingBudget": -1, "includeThoughts": trace}
        return None
    if str(enabled).lower() != "true":return {"thinkingBudget": 0}
    level = (cfg("llm_reasoning_level", None) or "auto").strip().lower()
    budget = -1 if level in ("", "auto") else _BUDGET.get(level, -1)
    return {"thinkingBudget": budget, "includeThoughts": trace}


# --- request translation: OpenAI -> Gemini -----------------------------------


def _part_text(content: Any) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "".join(p.get("text", "") for p in content if isinstance(p, dict) and p.get("type") == "text")
    return "" if content is None else str(content)


def _data_url_to_inline(url: str) -> dict[str, Any] | None:
    """`data:image/jpeg;base64,<data>` → Gemini `{inlineData:{mimeType,data}}`.
    Remote (non-data) URLs are skipped — zero-egress: the adapter never fetches."""
    if not isinstance(url, str) or not url.startswith("data:"):
        return None
    try:
        head, data = url[len("data:"):].split(",", 1)
    except ValueError:
        return None
    media_type = head.split(";")[0] or "image/png"
    return {"inlineData": {"mimeType": media_type, "data": data}}


def _user_parts(content: Any) -> list[dict[str, Any]]:
    """OpenAI user `content` (str | list of parts) → Gemini parts: text parts plus
    inline image data for any `image_url` data URL."""
    if not isinstance(content, list):
        return [{"text": _part_text(content)}]
    parts: list[dict[str, Any]] = []
    for p in content:
        if not isinstance(p, dict):
            continue
        if p.get("type") == "text":
            parts.append({"text": p.get("text", "")})
        elif p.get("type") == "image_url":
            inl = _data_url_to_inline((p.get("image_url") or {}).get("url", ""))
            if inl:
                parts.append(inl)
    return parts or [{"text": ""}]


def _translate(messages: list[dict[str, Any]]) -> tuple[dict[str, Any] | None, list[dict[str, Any]]]:
    """OpenAI messages -> (systemInstruction, contents). Tool results are matched
    to their call's function name via the preceding assistant `tool_calls`."""
    sys_parts: list[str] = []
    contents: list[dict[str, Any]] = []
    id_to_name: dict[str, str] = {}

    for m in messages:
        role = m.get("role")
        if role in ("system", "developer"):
            t = _part_text(m.get("content"))
            if t:
                sys_parts.append(t)
            continue
        if role == "user":
            contents.append({"role": "user", "parts": _user_parts(m.get("content"))})
            continue
        if role == "assistant":
            parts: list[dict[str, Any]] = []
            text = _part_text(m.get("content"))
            if text:
                parts.append({"text": text})
            for tc in m.get("tool_calls") or []:
                fn = tc.get("function") or {}
                name = fn.get("name") or ""
                if tc.get("id"):
                    id_to_name[tc["id"]] = name
                args = fn.get("arguments")
                if isinstance(args, str):
                    try:
                        args = json.loads(args)
                    except json.JSONDecodeError:
                        args = {}
                from .native_replay import get
                replay = get(tc, "gemini")
                part = {"functionCall": {"name": name, "args": args or {}}}
                if replay:
                    native = replay.get("part")
                    if (not isinstance(native,dict) or set(native)-{"functionCall","thoughtSignature"}
                        or native.get("functionCall",{}).get("name")!=name
                        or native.get("functionCall",{}).get("args",{})!=(args or {})):
                        raise ValueError("Gemini replay call mismatch")
                    part = native
                parts.append(part)
            contents.append({"role": "model", "parts": parts or [{"text": ""}]})
            continue
        if role == "tool":
            name = id_to_name.get(m.get("tool_call_id", ""), m.get("name", "tool"))
            raw = _part_text(m.get("content"))
            try:
                resp_obj = json.loads(raw)
                if not isinstance(resp_obj, dict):
                    resp_obj = {"result": resp_obj}
            except (json.JSONDecodeError, TypeError):
                resp_obj = {"result": raw}
            # Function results are sent back in a user-role content (Gemini accepts
            # only `user`/`model` roles; the `functionResponse` part marks it).
            response_part = {"functionResponse": {"name": name, "response": resp_obj}}
            for previous in contents:
                for part in previous.get("parts", []):
                    fc = part.get("functionCall", {})
                    if fc.get("id")==m.get("tool_call_id"):response_part["functionResponse"]["id"] = fc["id"]
            contents.append({"role": "user", "parts": [response_part]})
            continue
    system = {"parts": [{"text": "\n".join(sys_parts)}]} if sys_parts else None
    return system, contents


def _translate_tools(tools: list[dict[str, Any]] | None) -> list[dict[str, Any]] | None:
    """OpenAI tool defs -> a single Gemini `functionDeclarations` tool."""
    if not tools:
        return None
    decls: list[dict[str, Any]] = []
    for t in tools:
        fn = t.get("function") or t
        decl: dict[str, Any] = {"name": fn.get("name")}
        if fn.get("description"):
            decl["description"] = fn["description"]
        params = fn.get("parameters")
        if params:
            decl["parameters"] = params
        decls.append(decl)
    return [{"functionDeclarations": decls}]


def _build_body(
    messages: list[dict[str, Any]],
    sampling: dict[str, Any],
    tools: list[dict[str, Any]] | None,
    model: str | None = None,
) -> dict[str, Any]:
    system, contents = _translate(messages)
    gen: dict[str, Any] = {
        "maxOutputTokens": sampling.get("max_tokens") or settings.llm_default_max_tokens,
    }
    think = _thinking_config(model)
    if think is not None:
        gen["thinkingConfig"] = think
    # Sampling params are dropped while thinking is on (mirrors the Anthropic
    # discipline — adaptive reasoning is sensitive to temperature/top_p).
    if not think or think.get("thinkingBudget") == 0:
        if "temperature" in sampling:
            gen["temperature"] = sampling["temperature"]
        if "top_p" in sampling:
            gen["topP"] = sampling["top_p"]
    if sampling.get("response_schema") is not None:
        gen["responseFormat"] = {"text": {"mimeType": "APPLICATION_JSON", "schema": sampling["response_schema"]}}
    body: dict[str, Any] = {"contents": contents, "generationConfig": gen}
    if system:
        body["systemInstruction"] = system
    from .gemini_wire_compat import system_layout
    body=system_layout(body,model or cfg("llm_model",settings.llm_model))
    gtools = _translate_tools(tools)
    if gtools:
        body["tools"] = gtools
    return body


# --- response translation: Gemini -> OpenAI ----------------------------------

_FINISH = {"STOP": "stop", "MAX_TOKENS": "length", "SAFETY": "content_filter", "RECITATION": "content_filter"}


def _map_finish(fr: str | None) -> str | None:
    if not fr:
        return None
    return _FINISH.get(fr, fr.lower())


def _map_usage(u: dict[str, Any] | None) -> dict[str, Any]:
    u = u or {}
    if strict():
        if not isinstance(u, dict):u = {}
        def count(name: str, default: Any = None) -> int | None:
            value = u.get(name, default)
            return value if type(value) is int and value >= 0 else None
        pt = count("promptTokenCount")
        ct = count("candidatesTokenCount")
        tt = count("thoughtsTokenCount", 0)
        total = count("totalTokenCount")
        cache = count("cachedContentTokenCount", 0)
        # Implicit-presence proto3 zero defaults apply only to a complete,
        # conserved native report. Keep raw absence, malformed and missing usage.
        complete = (all(v is not None for v in (pt, ct, tt, total, cache))
                    and total == pt + ct + tt and cache <= pt
                    and not u.get("toolUsePromptTokenCount", 0))
        return {"prompt_tokens": pt, "completion_tokens": ct + tt if ct is not None and tt is not None else None,
                "total_tokens": total, "reasoning_tokens": tt if complete else count("thoughtsTokenCount"),
                "prompt_tokens_details": {"cached_tokens": cache if complete else count("cachedContentTokenCount")},
                "raw_provider_usage": dict(u)}
    pt = u.get("promptTokenCount") or 0
    ct = u.get("candidatesTokenCount") or 0
    out = {"prompt_tokens": pt, "completion_tokens": ct, "total_tokens": u.get("totalTokenCount") or (pt + ct)}
    tt = u.get("thoughtsTokenCount")
    if tt:
        out["reasoning_tokens"] = tt
    return out


def _parse_parts(parts: list[dict[str, Any]] | None) -> tuple[str, list[dict[str, Any]]]:
    """Candidate parts -> (answer text, tool_calls). Thought parts are excluded
    from the answer (handled on the reasoning channel during streaming)."""
    text_parts: list[str] = []
    tool_calls: list[dict[str, Any]] = []
    for i, p in enumerate(parts or []):
        if p.get("thought"):
            continue
        if "text" in p:
            text_parts.append(p.get("text") or "")
        elif "functionCall" in p:
            fc = p["functionCall"]
            from .native_replay import bind
            call = {"id": fc.get("id") or f"call_{i}", "name": fc.get("name"), "arguments": fc.get("args") or {}}
            part = {"functionCall": fc}
            if p.get("thoughtSignature"):part["thoughtSignature"] = p["thoughtSignature"]
            call["native_replay"] = bind("gemini", {"part":part})
            tool_calls.append(call)
    return "".join(text_parts), tool_calls


async def a_chat_step(
    messages: list[dict[str, Any]],
    tools: list[dict[str, Any]] | None = None,
    sampling: dict[str, Any] | None = None,
    model: str | None = None,
) -> dict[str, Any]:
    from . import llm

    stage = llm._consume_stage()
    sampling = sampling or {}
    model = model or cfg("llm_model", settings.llm_model)
    body = _build_body(messages, sampling, tools, model=model)
    client = http_client.get_client()
    r = await http_client.post(client, _url(model, "generateContent"), json=body, headers=_headers())
    # Graceful tools-drop: a model/route without function-calling rejects `tools`.
    if tools and r.status_code >= 400 and not strict():
        logger.warning("Gemini rejected tools (%s); retrying without tools", r.status_code)
        body.pop("tools", None)
        r = await http_client.post(client, _url(model, "generateContent"), json=body, headers=_headers())
    if r.status_code != 200:
        logger.warning("Gemini chat_step %s: %.500r", r.status_code, "[routed details omitted]" if strict() else r.text)
        r.raise_for_status()
    data = r.json()
    cand = (data.get("candidates") or [{}])[0]
    content, tool_calls = _parse_parts((cand.get("content") or {}).get("parts"))
    usage = _map_usage(data.get("usageMetadata"))
    llm._record_usage(stage, usage)
    return {
        "content": content,
        "tool_calls": tool_calls,
        "finish_reason": _map_finish(cand.get("finishReason")),
        "usage": usage,
    }


async def a_complete(system: str, user: str, max_tokens: int = 512) -> str:
    from . import llm

    stage = llm._consume_stage()
    msgs: list[dict[str, Any]] = []
    if system:
        msgs.append({"role": "system", "content": system})
    msgs.append({"role": "user", "content": user})
    model = cfg("llm_model", settings.llm_model)
    body = _build_body(msgs, {"max_tokens": max_tokens, "temperature": 0}, None, model=model)
    client = http_client.get_client()
    r = await http_client.post(client, _url(model, "generateContent"), json=body, headers=_headers())
    if r.status_code != 200:
        logger.warning("Gemini complete %s: %.500r", r.status_code, "[routed details omitted]" if strict() else r.text)
        r.raise_for_status()
    data = r.json()
    cand = (data.get("candidates") or [{}])[0]
    content, _ = _parse_parts((cand.get("content") or {}).get("parts"))
    llm._record_usage(stage, _map_usage(data.get("usageMetadata")))
    return content


async def a_stream_chat(
    messages: list[dict[str, Any]],
    sampling: dict[str, Any],
    model: str | None = None,
    tools: list[dict[str, Any]] | None = None,
) -> AsyncIterator[dict[str, Any]]:
    from . import llm
    from .llm import LlmError

    stage = llm._consume_stage()
    model = model or cfg("llm_model", settings.llm_model)
    body = _build_body(messages, sampling, tools, model=model)
    usage: dict[str, Any] | None = None
    finish: str | None = None
    fc_count = 0  # Gemini has no native call id — synthesise a stable one per call

    client = http_client.get_client()
    async with http_client.stream(client, "POST", _url(model, "streamGenerateContent", sse=True), json=body, headers=_headers()) as resp:
        if resp.status_code != 200:
            err = await resp.aread()
            logger.warning("Gemini stream %s: %.500r", resp.status_code, "[routed details omitted]" if strict() else err)
            raise await LlmError.from_response(resp)
        async for line in resp.aiter_lines():
            line = line.strip()
            if not line.startswith("data:"):
                continue
            payload = line[len("data:") :].strip()
            try:
                chunk = json.loads(payload)
            except json.JSONDecodeError:
                continue
            if chunk.get("usageMetadata"):
                usage = _map_usage(chunk["usageMetadata"])
            for cand in chunk.get("candidates") or []:
                for p in (cand.get("content") or {}).get("parts") or []:
                    fc = p.get("functionCall")
                    if fc and fc.get("name"):
                        # Gemini delivers a function call as one complete part (args are a
                        # full dict), so it emits directly — no fragment accumulation.
                        finish = "tool_calls"
                        args = fc.get("args")
                        yield {
                            "type": "tool_call", "id": f"call_{fc_count}", "name": fc["name"],
                            "arguments": args if isinstance(args, dict) else {},
                        }
                        fc_count += 1
                        continue
                    text = p.get("text")
                    if not text:
                        continue
                    if p.get("thought"):
                        yield {"type": "reasoning", "delta": text}
                    else:
                        yield {"type": "token", "delta": text}
                if cand.get("finishReason") and finish != "tool_calls":
                    finish = _map_finish(cand["finishReason"])

    llm._record_usage(stage, usage)
    yield {"type": "done", "finish_reason": finish, "model": model, "usage": usage or {}}
