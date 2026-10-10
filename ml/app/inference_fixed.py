# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Fixed services share a bounded request grant; bytes are never called tokens.

One record describes one HTTP attempt, including each verification batch. No
credentials, document contents, audio or endpoint URLs enter the report.
"""
import asyncio
import functools
import threading
import time
from typing import Literal
from uuid import UUID, uuid4

import httpx
from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator
from . import rag_ctx
from .inference_budget import BudgetExhausted

Role = Literal["embed", "embed_desired", "rerank", "ocr", "stt", "tts", "verify", "verify_claims"]


class FixedTarget(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True, hide_input_in_errors=True)
    schema_version: Literal[1]
    provider_id: UUID
    provider_revision: int = Field(gt=0)
    profile_revision: int = Field(gt=0)
    role: Role
    model: str = Field(min_length=1, max_length=256)
    active_index_revision: int | None = Field(default=None, gt=0)
    desired_index_revision: int | None = Field(default=None, gt=0)
    base_url: str = Field(min_length=1, max_length=4096, repr=False)
    api_key: str = Field(repr=False)
    embedding_dimension: int | None = Field(default=None, gt=0)
    embedding_collection: str | None = Field(default=None, min_length=1, repr=False)
    ocr_pdf_mode: Literal["document", "page_images"] | None = None

    @field_validator("base_url")
    @classmethod
    def endpoint(cls, value):
        from .inference_target import InferenceTarget
        return InferenceTarget.valid_endpoint(value)

    @model_validator(mode="after")
    def index_identity(self):
        if self.ocr_pdf_mode is not None and self.role != "ocr":
            raise ValueError("PDF input mode applies only to OCR")
        revision = self.desired_index_revision if self.role == "embed_desired" else self.active_index_revision
        fields = (revision, self.embedding_dimension, self.embedding_collection)
        if (self.active_index_revision is not None and self.role != "embed"
                or self.desired_index_revision is not None and self.role != "embed_desired"
                or self.role == "embed_desired" and revision is None
                or any(v is not None for v in fields) and (self.role not in ("embed", "embed_desired") or any(v is None for v in fields))):
            raise ValueError("embedding source identity must be complete and match its stage")
        return self


class FixedAllowance(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)
    max_calls: int = Field(ge=1, le=1024, strict=True)
    input_bytes: int = Field(ge=1, le=2**32-1, strict=True)
    output_bytes: int = Field(ge=1, le=2**32-1, strict=True)


class FixedLease(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)
    id: UUID
    timeout_ms: int = Field(gt=0, le=86_400_000, strict=True)
    allowances: dict[Role, FixedAllowance] = Field(min_length=1, max_length=8)

    @model_validator(mode="after")
    def bounded(self):
        if sum(a.max_calls for a in self.allowances.values()) > 1024:
            raise ValueError("fixed lease exceeds 1024 calls")
        return self


class FixedBudget:
    def __init__(self, grant):
        self.grant = grant
        self.deadline = time.monotonic() + grant.timeout_ms / 1000
        self.counts = {}
        self.active = 0
        self.closed = False
        self.records = []
        self.lock = threading.Lock()

    def remaining(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise BudgetExhausted("fixed request deadline exhausted")
        return remaining

    def begin(self, role, size):
        with self.lock:
            self.remaining()
            allowance = self.grant.allowances.get(role)
            if self.closed or allowance is None or self.counts.get(role, 0) >= allowance.max_calls:
                raise BudgetExhausted("fixed call allowance exhausted")
            if size > allowance.input_bytes:
                raise BudgetExhausted("fixed input byte allowance exceeded")
            self.counts[role] = self.counts.get(role, 0) + 1
            self.active += 1
            return str(uuid4())

    def seal(self):
        with self.lock:
            self.closed = True
            return {"lease_id": str(self.grant.id), "complete": self.active == 0,
                    "calls": list(self.records)}


def routed_fixed(role):
    """Install all endpoint fields before adapter defaults can be consulted."""
    def decorate(fn):
        @functools.wraps(fn)
        async def call(*args, **kwargs):
            from .inference_target import InferenceEnvelope, strict
            if not strict():
                return await fn(*args, **kwargs)
            envelope = InferenceEnvelope.model_validate(rag_ctx.cfg("routing", None))
            target = envelope.fixed_assignments.get(role)
            budget = rag_ctx.cfg("_routing_fixed_budget", None)
            if target is None or budget is None:
                raise ValueError(f"fixed assignment and grant required for {role}")
            prefix = {"embed_desired":"embed", "verify_claims":"verify"}.get(role, role)
            values = {f"{prefix}_base_url": target.base_url, f"{prefix}_model": target.model,
                      f"{prefix}_api_key": target.api_key, f"{prefix}_enabled": True,
                      "_routing_fixed_target": target}
            with rag_ctx.scoped_overrides(values):
                async with asyncio.timeout(budget.remaining()):
                    return await fn(*args, **kwargs)
        return call
    return decorate


async def post(role, client, url, **kwargs):
    """Count the serialized request body and decoded response bytes, not tokens.

    Strict traffic uses a single streamed HTTP attempt, with redirects disabled.
    Output overrun closes the stream and poisons the remaining fixed grant.
    """
    from .inference_target import InferenceEnvelope, strict
    if not strict():
        return await client.post(url, **kwargs)
    envelope = InferenceEnvelope.model_validate(rag_ctx.cfg("routing", None))
    target = envelope.fixed_assignments.get(role)
    budget = rag_ctx.cfg("_routing_fixed_budget", None)
    if target is None or budget is None or rag_ctx.cfg("_routing_fixed_target", None) != target:
        raise ValueError("unassigned fixed inference call")
    # Build with a fresh Request: a shared client's default Authorization must
    # never replace the target's explicitly empty key.
    expected = target.base_url + "/"
    if not str(url).startswith(expected):
        raise ValueError("fixed endpoint conflicts with target")
    payload = kwargs.get("json") or kwargs.get("data") or {}
    if payload.get("model") != target.model:
        raise ValueError("fixed model conflicts with target")
    from .http_client import bearer_headers
    if kwargs.get("headers", {}).get("Authorization") != bearer_headers(target.api_key).get("Authorization"):
        raise ValueError("fixed credential conflicts with target")
    if role in ("verify", "verify_claims"):
        # Older verifier sidecars ignored model fields. Require explicit
        # acknowledgement so a routed request cannot silently use another one.
        kwargs["json"] = {**payload, "routing_model": target.model}
    timeout = kwargs.pop("timeout", None)
    request = httpx.Request("POST", url, **kwargs)
    body = await request.aread()
    call_id = budget.begin(role, len(body))
    started = time.monotonic()
    record = {"call_id": call_id, "lease_id": str(budget.grant.id),
              "decision_id": str(envelope.decision_id), "role": role,
              "provider_id": str(target.provider_id), "provider_revision": target.provider_revision,
              "profile_revision": target.profile_revision, "active_index_revision": target.active_index_revision,
              "desired_index_revision": target.desired_index_revision,
              "input_bytes": len(body), "output_bytes": None, "status": "failed"}
    try:
        seconds = min(budget.remaining(), float(timeout)) if isinstance(timeout, (int, float)) else budget.remaining()
        async with asyncio.timeout(seconds):
            response = await client.send(request, stream=True, follow_redirects=False, auth=None)
            try:
                response.raise_for_status()
                chunks, size = [], 0
                async for chunk in response.aiter_bytes():
                    size += len(chunk)
                    record["output_bytes"] = size
                    if size > budget.grant.allowances[role].output_bytes:
                        with budget.lock:
                            budget.closed = True
                        raise BudgetExhausted("fixed output byte allowance exceeded")
                    chunks.append(chunk)
                record["output_bytes"] = size
                headers = {k: v for k, v in response.headers.items() if k.lower() not in ("content-encoding", "content-length")}
                result = httpx.Response(response.status_code, headers=headers,
                                        content=b"".join(chunks), request=request)
                if role in ("verify", "verify_claims") and result.json().get("model") != target.model:
                    raise ValueError("verifier did not acknowledge the routed model")
            finally:
                await response.aclose()
        record["status"] = "completed"
        return result
    except (asyncio.CancelledError, TimeoutError):
        record["status"] = "cancelled"
        raise
    finally:
        record["latency_ms"] = round((time.monotonic() - started) * 1000)
        with budget.lock:
            budget.records.append(record)
            budget.active -= 1
