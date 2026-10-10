# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""A Core-issued grant is shared by all tasks in one ML request.

Reservations are synchronous and locked before the first await. Failed and
cancelled attempts consume their slot. No model can extend or replenish a grant.
"""
import json
import threading
import time
from uuid import UUID, uuid4
from pydantic import BaseModel, ConfigDict, Field, model_validator


class Allowance(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)
    max_calls: int = Field(ge=1, le=1024, strict=True)
    input_tokens: int = Field(ge=0, le=2**32-1, strict=True)
    output_tokens: int = Field(ge=1, le=2**32-1, strict=True)


class BudgetLease(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)
    id: UUID
    timeout_ms: int = Field(gt=0, le=86_400_000, strict=True)
    allowances: dict[str, Allowance] = Field(min_length=1, max_length=24)

    @model_validator(mode="after")
    def bounded(self):
        if sum(a.max_calls for a in self.allowances.values()) > 1024:
            raise ValueError("lease exceeds 1024 calls")
        return self


class BudgetExhausted(ValueError):
    pass


class RequestBudget:
    def __init__(self, grant: BudgetLease):
        self.grant = grant
        self.deadline = time.monotonic() + grant.timeout_ms / 1000
        self.counts = {}
        self.active = 0
        self.closed = False
        self.lock = threading.Lock()

    def remaining(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise BudgetExhausted("routing request deadline exhausted")
        return remaining

    def begin(self, purpose, bound):
        """UTF-8 bytes plus template margin; no chars/4 under-reservation.

        Multimodal tokenization needs adapter-specific evidence. Until that is
        available, a bounded grant rejects image/audio content before dispatch.
        """
        arguments = bound.arguments
        if "messages" in arguments:
            messages = arguments["messages"]
            for message in messages:
                content = message.get("content")
                if isinstance(content, list) and any(p.get("type") != "text" for p in content):
                    raise BudgetExhausted("bounded multimodal inference requires token accounting")
            payload = {"messages": messages, "tools": arguments.get("tools")}
            from .config import settings
            output = (arguments.get("sampling") or {}).get("max_tokens") or settings.llm_default_max_tokens
        else:
            payload = {"system": arguments.get("system", ""), "user": arguments.get("user", "")}
            output = arguments["max_tokens"]
        from .output_schema import schema_overhead
        extra = schema_overhead((arguments.get("sampling") or {}).get("response_schema"))
        input_tokens = len(json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")) + 512 + extra
        with self.lock:
            self.remaining()
            allowance = self.grant.allowances.get(purpose)
            if self.closed or allowance is None or self.counts.get(purpose, 0) >= allowance.max_calls:
                raise BudgetExhausted("routing call allowance exhausted")
            if isinstance(output, bool) or not isinstance(output, int) or output <= 0 or output > allowance.output_tokens or input_tokens > allowance.input_tokens:
                raise BudgetExhausted("routing token allowance exceeded")
            self.counts[purpose] = self.counts.get(purpose, 0) + 1
            self.active += 1
        return str(uuid4())

    def end(self):
        with self.lock:
            self.active -= 1

    def observe(self, purpose, usage):
        allowance = self.grant.allowances[purpose]
        if ((usage.get("input_tokens") or 0) > allowance.input_tokens
                or (usage.get("output_tokens") or 0) > allowance.output_tokens):
            with self.lock:
                self.closed = True
            raise BudgetExhausted("provider exceeded routing token allowance")

    def seal(self):
        """A terminal report prevents background stragglers spending refunded slots."""
        with self.lock:
            self.closed = True
            return self.active == 0
