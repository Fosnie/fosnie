# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Apply a pinned effective variant at the shared native HTTP boundary.

The call's grant remains an upper bound. No hidden retry or parameter removal.
Unsupported mappings are rejected by Core and checked again here.
"""
from pydantic import BaseModel, ConfigDict, Field, model_validator


class GenerationConfig(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)
    max_output_tokens: int | None = Field(default=None, gt=0, le=4294967295)
    max_reasoning_tokens: int | None = Field(default=None, gt=0, le=4294967295)
    temperature_milli: int | None = Field(default=None, ge=0, le=2000)
    top_p_milli: int | None = Field(default=None, ge=1, le=1000)
    seed: int | None = Field(default=None, ge=-(2**63), lt=2**63)


def apply(payload, adapter, generation, *, model=None):
    if generation is None:
        return payload
    g = GenerationConfig.model_validate(generation)
    result = dict(payload)
    settings = dict(result.get("generationConfig", {})) if adapter == "gemini" else result
    output = "maxOutputTokens" if adapter == "gemini" else "max_output_tokens" if adapter == "openai_responses" else "max_completion_tokens" if "max_completion_tokens" in result else "max_tokens"
    if g.max_output_tokens is not None:
        current = settings.get(output)
        if not isinstance(current, int) or current < 1:
            raise ValueError("variant requires bounded output")
        settings[output] = min(current, g.max_output_tokens)
    for source, wire in (("temperature_milli", "temperature"), ("top_p_milli", "topP" if adapter == "gemini" else "top_p")):
        value = getattr(g, source)
        if value is not None:
            settings[wire] = value / 1000
    if g.seed is not None:
        if adapter not in ("openai_chat", "gemini"):
            raise ValueError("adapter has no seed mapping")
        settings["seed"] = g.seed
    if g.max_reasoning_tokens is not None:
        if adapter == "gemini":
            from .gemini_adapter import _uses_thinking_level
            if model is not None and _uses_thinking_level(model):
                raise ValueError("selected model uses thinking levels, not a numeric reasoning budget")
            thinking = dict(settings.get("thinkingConfig", {}))
            if "thinkingLevel" in thinking:
                raise ValueError("budget cannot replace a pinned thinking level")
            thinking["thinkingBudget"] = g.max_reasoning_tokens
            settings["thinkingConfig"] = thinking
        elif adapter == "anthropic":
            thinking = dict(settings.get("thinking", {}))
            if thinking.get("type") == "adaptive" or g.max_reasoning_tokens < 1024 or g.max_reasoning_tokens >= settings[output]:
                raise ValueError("invalid pinned Anthropic thinking budget")
            settings["thinking"] = {"type": "enabled", "budget_tokens": g.max_reasoning_tokens}
        else:
            raise ValueError("adapter has no reasoning token budget mapping")
    if adapter == "gemini":
        result["generationConfig"] = settings
    return result


def request(url, kwargs):
    from .rag_ctx import cfg
    generation = cfg("routing_generation", None)
    if generation is None:
        return kwargs
    adapter = cfg("llm_adapter", None)
    # Fixed services use their own target and contract, even inside a mixed call.
    suffix = str(url).split("?", 1)[0]
    matching = (adapter == "openai_chat" and suffix.endswith("/chat/completions")
                or adapter == "openai_responses" and suffix.endswith("/responses")
                or adapter == "anthropic" and suffix.endswith("/messages")
                or adapter == "gemini" and suffix.endswith((":generateContent", ":streamGenerateContent")))
    if not matching:
        return kwargs
    if "json" not in kwargs:
        raise ValueError("variant requires a structured native request")
    return {**kwargs, "json": apply(kwargs["json"], adapter, generation, model=cfg("llm_model", None))}
