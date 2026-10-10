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

"""Per-request RAG/ingest overrides (the super-admin runtime knobs).

The backend sends them in the `/retrieve` and `/ingest` bodies; we stash them in a
`ContextVar` so the existing `settings.*` read-sites can prefer an override without
threading params through every function. asyncio copies the current context into
child tasks (`gather`/`create_task`), so the concurrent search fan-out sees them
too — and it's per-request safe (no global mutation)."""

from contextvars import ContextVar
from contextlib import contextmanager

_overrides: ContextVar[dict] = ContextVar("rag_overrides", default={})


def set_overrides(d: dict | None) -> None:
    """Set the overrides for the current request context (None values dropped)."""
    if d is not None and not isinstance(d, dict):
        raise ValueError("inference overrides must be an object")
    values = {k: v for k, v in (d or {}).items() if v is not None}
    if values.get("routing") is not None:
        values["_routing_records"] = []
        from .inference_target import InferenceEnvelope
        from .inference_budget import RequestBudget
        envelope = InferenceEnvelope.model_validate(values["routing"])
        if envelope.budget is not None:
            if set(envelope.budget.allowances) - set(envelope.assignments):
                raise ValueError("lease contains unassigned inference purposes")
            values["_routing_budget"] = RequestBudget(envelope.budget)
        if envelope.fixed_budget is not None:
            from .inference_fixed import FixedBudget
            values["_routing_fixed_budget"] = FixedBudget(envelope.fixed_budget)
    _overrides.set(values)


def cfg(key: str, default):
    """Override value for `key`, else the boot/settings `default`."""
    return _overrides.get().get(key, default)


@contextmanager
def scoped_overrides(values: dict):
    """Temporary per-call values, restored on success, failure and cancellation."""
    token = _overrides.set({**_overrides.get(), **values})
    try:
        yield
    finally:
        _overrides.reset(token)
