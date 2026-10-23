# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Versioned, explicitly opted-in inference targets. No model-name ranking here.

The Rust routing service supplies complete assignments. This module validates
and installs exactly one assignment while a call runs, including native adapters.
Legacy requests without a routing envelope retain their existing behaviour.
"""
from contextlib import contextmanager, aclosing
from typing import Literal
from uuid import UUID
import functools
import inspect
import time
import asyncio

from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator
from . import rag_ctx
from .inference_budget import BudgetLease
from .inference_fixed import FixedTarget, FixedLease, Role
from .variant_generation import GenerationConfig

Purpose = Literal[
    "primary", "tool_loop", "final", "router_classify", "utility_title", "utility_summary",
    "retrieval_decompose", "retrieval_grade", "web_plan", "web_grade", "web_synthesis",
    "research_plan", "research_write", "document", "voice", "moderation", "quality_judge", "verify", "embed", "rerank", "ocr", "stt", "tts",
]


class VariantRef(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)
    id: UUID
    revision: int = Field(gt=0)


class VariantBinding(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)
    variant: VariantRef
    model_config_revision: int = Field(gt=0)
    behaviour_sha256: str = Field(pattern=r"^[0-9a-f]{64}$")


class InferenceTarget(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True, hide_input_in_errors=True)
    schema_version: Literal[1, 2]
    provider_id: UUID
    provider_revision: int = Field(gt=0)
    profile_revision: int = Field(gt=0)
    adapter: Literal["openai_chat", "openai_responses", "anthropic", "gemini"]
    base_url: str = Field(min_length=1, max_length=4096, repr=False)
    model: str = Field(min_length=1, max_length=256)
    api_key: str = Field(repr=False)
    effort: str | None = Field(default=None, max_length=24)

    generation: "GenerationConfig | None" = None
    variant: VariantBinding | None = None

    @model_validator(mode="before")
    @classmethod
    def strict_v1_fields(cls, value):
        if isinstance(value, dict) and value.get("schema_version") == 1 and ("generation" in value or "variant" in value):
            raise ValueError("v1 target cannot carry v2 fields, including null")
        return value

    @model_validator(mode="after")
    def versioned_variant(self):
        if self.schema_version == 1 and (self.generation is not None or self.variant is not None):
            raise ValueError("v1 cannot carry a variant")
        if self.schema_version == 2 and (self.generation is None or self.variant is None):
            raise ValueError("v2 target requires effective generation and binding")
        return self

    @field_validator("base_url")
    @classmethod
    def valid_endpoint(cls, value):
        from urllib.parse import urlsplit
        url = urlsplit(value)
        if url.scheme not in ("http", "https") or not url.hostname or url.username or url.password or url.query or url.fragment:
            raise ValueError("endpoint must be HTTP(S) without credentials, query or fragment")
        return value.rstrip("/")


class InferenceEnvelope(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True, hide_input_in_errors=True)
    schema_version: Literal[1, 2]
    decision_id: UUID
    purpose: Purpose
    assignments: dict[Purpose, InferenceTarget] = Field(default_factory=dict, max_length=24, repr=False)
    budget: BudgetLease | None = None
    fixed_assignments: dict[Role, FixedTarget] = Field(default_factory=dict, max_length=8, repr=False)
    fixed_budget: FixedLease | None = None

    @model_validator(mode="after")
    def assignments_match(self):
        if not self.assignments and not self.fixed_assignments:
            raise ValueError("inference envelope has no assignments")
        if self.schema_version == 1 and any(t.schema_version != 1 for t in self.assignments.values()):
            raise ValueError("v2 target requires v2 envelope")
        fixed_roles = {"embed", "rerank", "ocr", "stt", "tts", "verify"}
        if set(self.assignments) & fixed_roles:
            raise ValueError("fixed roles cannot use LLM assignments")
        if any(role != target.role for role, target in self.fixed_assignments.items()):
            raise ValueError("fixed target role differs from assignment")
        if self.fixed_budget is not None and set(self.fixed_budget.allowances) - set(self.fixed_assignments):
            raise ValueError("fixed grant contains unassigned roles")
        return self


STAGES: dict[str, Purpose] = {
    "draft.decompose": "document", "draft.repair": "document",
    "moderation.classify": "moderation",
    "document.map": "document",
    "generate": "final", "tool_loop": "tool_loop",
    "rag.decompose": "retrieval_decompose", "rag.reformulate": "retrieval_decompose",
    "rag.grade": "retrieval_grade", "rag.subanswer": "retrieval_grade",
    "rag.subcheck": "retrieval_grade", "rag.gapcheck": "retrieval_grade",
    "web.plan": "web_plan", "web.reformulate": "web_plan", "web.grade": "web_grade",
    "web.detect_conflict": "web_grade", "web.synthesize": "web_synthesis",
    "ingest.metadata": "utility_summary", "ingest.context": "utility_summary",
    "research.census": "research_plan", "research.cohere": "research_write",
    "research.corpus_analysis": "research_plan", "research.deepen_judge": "research_plan",
    "research.notes": "research_plan", "research.outline": "research_plan",
    "research.plan": "research_plan", "research.gap": "research_plan",
    "research.title": "utility_title", "research.triage": "research_plan",
    "research.write_section": "research_write", "research.update_summary": "utility_summary",
}


def strict() -> bool:
    return rag_ctx.cfg("routing", None) is not None


def note_usage(usage):
    record = rag_ctx.cfg("_routing_active_call", None)
    if record is not None:
        record["usage"] = normalise_usage(usage)
        budget = rag_ctx.cfg("_routing_budget", None)
        if budget is not None:
            budget.observe(record["purpose"], record["usage"])


def records():
    return list(rag_ctx.cfg("_routing_records", []))


def with_report(result: dict, *, terminal=True):
    """Only terminal pipeline results seal a lease, never an inner helper."""
    if not strict():
        return result
    result = {**result, "routing_usage": records()}
    budget = rag_ctx.cfg("_routing_budget", None)
    if budget is not None and terminal:
        result["routing_report"] = {"lease_id": str(budget.grant.id),
                                    "complete": budget.seal(), "calls": records()}
    fixed_budget = rag_ctx.cfg("_routing_fixed_budget", None)
    if fixed_budget is not None and terminal:
        result["fixed_routing_report"] = fixed_budget.seal()
    return result


async def report_events(source):
    """Keep progress small; attach all helper usage only to the terminal event."""
    async with aclosing(source):
        async for event in source:
            if event.get("type") in ("done", "error"):
                event = with_report(event)
            yield event


def adapter_is(name, base, legacy_check):
    adapter = rag_ctx.cfg("llm_adapter", None) if strict() else None
    return adapter == name if adapter else legacy_check(base)


def routed(*, utility=False):
    """Install the assignment before any adapter reads its settings."""
    def decorate(fn):
        def call_scope(args, kwargs):
            from . import llm
            bound = inspect.signature(fn).bind(*args, **kwargs)
            bound.apply_defaults()
            return for_call(llm._stage_var.get(), utility=utility, bound=bound)

        def output_contract(target, args, kwargs, *, stream=False):
            from .output_schema import validate_schema, OutputSchemaError
            bound = inspect.signature(fn).bind(*args, **kwargs)
            schema = (bound.arguments.get("sampling") or {}).get("response_schema")
            if schema is not None:
                validate_schema(schema)
                if stream or target is None or target.adapter not in ("openai_chat", "openai_responses", "gemini") or bound.arguments.get("tools"):
                    raise OutputSchemaError("output schema requires a supported routed text-only non-streaming call")
            return schema

        def validate_model(target, args, kwargs):
            if target is None:
                return
            bound = inspect.signature(fn).bind(*args, **kwargs)
            model = bound.arguments.get("model")
            if model is not None and model != target.model:
                raise ValueError("request model conflicts with routing target")
            if target.adapter in ("anthropic", "gemini"):
                for message in bound.arguments.get("messages", []):
                    content = message.get("content")
                    if not isinstance(content, list):
                        continue
                    for part in content:
                        kind = part.get("type")
                        if kind not in ("text", "image_url"):
                            raise ValueError("selected native adapter cannot preserve this content type")
                        if kind == "image_url" and not (part.get("image_url") or {}).get("url", "").startswith("data:image/"):
                            raise ValueError("selected native adapter requires inline image data")

        if inspect.isasyncgenfunction(fn):
            @functools.wraps(fn)
            async def stream(*args, **kwargs):
                terminal_event = None
                with call_scope(args, kwargs) as target:
                    validate_model(target, args, kwargs)
                    output_contract(target, args, kwargs, stream=True)
                    async with aclosing(fn(*args, **kwargs)) as source:
                        while True:
                            budget = rag_ctx.cfg("_routing_budget", None)
                            try:
                                async with asyncio.timeout(budget.remaining() if budget else None):
                                    event = await anext(source)
                            except StopAsyncIteration:
                                break
                            if target is not None and event.get("type") == "done":
                                terminal_event = event
                                break
                            yield event
                if terminal_event is not None:
                    yield with_report(terminal_event, terminal=False)
            return stream
        @functools.wraps(fn)
        async def call(*args, **kwargs):
            # Internal utility defaults are maxima, not permission to exceed
            # the caller's purpose-specific grant (e.g. browser RAG helpers).
            # Explicit chat/generation sampling remains strictly validated.
            budget = rag_ctx.cfg("_routing_budget", None)
            envelope = rag_ctx.cfg("routing", None)
            if utility and budget is not None and envelope is not None:
                parsed = InferenceEnvelope.model_validate(envelope)
                from . import llm
                purpose = parsed.purpose if parsed.purpose in ("router_classify", "utility_title", "utility_summary") else STAGES.get(llm._stage_var.get(), parsed.purpose)
                allowance = budget.grant.allowances.get(purpose)
                bound = inspect.signature(fn).bind(*args, **kwargs)
                bound.apply_defaults()
                requested = bound.arguments.get("max_tokens")
                if purpose in ("retrieval_decompose", "retrieval_grade", "web_grade") and allowance is not None and type(requested) is int:
                    target = parsed.assignments.get(purpose)
                    limit = min(requested, allowance.output_tokens)
                    # Tiny binary-answer defaults are visible-text limits; native
                    # reasoning still needs generated-token room from its grant.
                    if requested < 16 and target and target.adapter in ("openai_responses", "gemini") and target.effort not in (None,"none"):
                        if allowance.output_tokens < 16:raise ValueError("reasoning grader needs a larger output grant")
                        limit = allowance.output_tokens
                    bound.arguments["max_tokens"] = limit
                    args, kwargs = bound.args, bound.kwargs
            with call_scope(args, kwargs) as target:
                validate_model(target, args, kwargs)
                schema = output_contract(target, args, kwargs)
                budget = rag_ctx.cfg("_routing_budget", None)
                async with asyncio.timeout(budget.remaining() if budget else None):
                    result = await fn(*args, **kwargs)
                    if schema is not None:
                        from .output_schema import validate_output
                        validate_output(schema, result)
            if target is not None and isinstance(result, dict):
                return {**result, "routing_usage": records()}
            return result
        return call
    return decorate


@contextmanager
def for_call(stage: str | None = None, *, utility: bool = False, bound=None):
    envelope = rag_ctx.cfg("routing", None)
    if envelope is None:
        # Existing utility overrides must be installed for native adapters too.
        base = rag_ctx.cfg("utility_base_url", None) if utility else None
        if base is None:
            yield None
            return
        model = rag_ctx.cfg("utility_model", None)
        if not model:
            raise ValueError("utility endpoint requires its own model")
        with rag_ctx.scoped_overrides({"llm_base_url": base, "llm_model": model,
                                      "llm_api_key": rag_ctx.cfg("utility_api_key", "")}):
            yield None
        return
    parsed = InferenceEnvelope.model_validate(envelope)
    purpose = parsed.purpose if parsed.purpose in ("router_classify", "utility_title", "utility_summary") else STAGES.get(stage, parsed.purpose)
    target = parsed.assignments.get(purpose)
    if target is None:
        raise ValueError(f"routing assignment missing for {purpose}")
    values = {"llm_base_url": target.base_url, "llm_model": target.model,
              "llm_api_key": target.api_key, "utility_base_url": target.base_url,
              "utility_model": target.model, "utility_api_key": target.api_key,
              "llm_adapter": target.adapter, "routing_purpose": purpose,
              "routing_generation": target.generation.model_dump() if target.generation else None,
              "routing_provider_id": str(target.provider_id), "routing_effort": target.effort,
              "llm_reasoning_enabled": ("false" if target.effort == "none" else "true") if target.effort else None,
              "llm_reasoning_level": target.effort,
              "llm_thinking": f"adaptive:{target.effort}" if target.effort else "off"}
    record = {"provider_id":str(target.provider_id),"provider_revision":target.provider_revision,
              "profile_revision":target.profile_revision,"decision_id":str(parsed.decision_id),
              "purpose":purpose,"status":"started","usage":normalise_usage(None)}
    started = time.monotonic()
    budget = rag_ctx.cfg("_routing_budget", None)
    if budget is not None:
        if bound is None:
            raise ValueError("budgeted inference requires call arguments")
        record["call_id"] = budget.begin(purpose, bound)
        record["lease_id"] = str(budget.grant.id)
    values["_routing_active_call"] = record
    values["_routing_started"] = started
    with rag_ctx.scoped_overrides(values):
        try:
            yield target
            record["status"] = "completed"
        except BaseException as error:
            record["status"] = "cancelled" if isinstance(error, (GeneratorExit, asyncio.CancelledError, TimeoutError)) else "failed"
            raise
        finally:
            record["latency_ms"] = round((time.monotonic() - started) * 1000)
            collector = rag_ctx.cfg("_routing_records", None)
            if collector is not None and len(collector) < 1024:
                collector.append(record)
            if budget is not None:
                budget.end()


def normalise_usage(raw: dict | None) -> dict:
    """Reasoning/cache tokens are subsets, never added twice to billed totals."""
    raw = raw or {}
    def number(value):
        return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None
    input_tokens = number(raw.get("prompt_tokens", raw.get("input_tokens")))
    output_tokens = number(raw.get("completion_tokens", raw.get("output_tokens")))
    reasoning = number(raw.get("reasoning_tokens", (raw.get("completion_tokens_details") or raw.get("output_tokens_details") or {}).get("reasoning_tokens")))
    cached = number((raw.get("prompt_tokens_details") or raw.get("input_tokens_details") or {}).get("cached_tokens"))
    return {"input_tokens": input_tokens, "output_tokens": output_tokens,
            "reasoning_tokens": reasoning, "cached_input_tokens": cached,
            "total_tokens": input_tokens + output_tokens if input_tokens is not None and output_tokens is not None else None,
            "raw_usage": raw.get("raw_provider_usage", raw)}
