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

"""Progress events from inside the web-search loop (agent-activity streaming). Same ContextVar shape as rag_ctx:
the streaming endpoint installs an emitter for its request context; everywhere
else `emit()` is a no-op, so the non-streaming path costs nothing. Emission is
fire-and-forget — the loop must NEVER block or fail on a slow/gone consumer."""

import logging
from contextvars import ContextVar
from typing import Awaitable, Callable

_log = logging.getLogger("pai.web.progress")

_emitter: ContextVar[Callable[[dict], None] | None] = ContextVar("web_progress_emitter", default=None)
# Synthesis tokens (deep path) are RELIABLE, unlike best-effort progress: a dropped
# token would corrupt the streamed answer. So they ride a separate ASYNC emitter
# that awaits the queue put (natural back-pressure) rather than the drop-on-full
# `put_nowait` of the progress emitter.
_token_emitter: ContextVar[Callable[[str], Awaitable[None]] | None] = ContextVar(
    "web_token_emitter", default=None
)


def set_emitter(fn: Callable[[dict], None] | None) -> None:
    """Install the per-request emitter (a sync callable taking the event dict)."""
    _emitter.set(fn)


def set_token_emitter(fn: Callable[[str], Awaitable[None]] | None) -> None:
    """Install the per-request RELIABLE token emitter (an async callable taking the
    delta). No-op everywhere except the streaming deep path."""
    _token_emitter.set(fn)


def token_emitter_installed() -> bool:
    """True when a token consumer is listening — i.e. the streaming deep path. The
    synthesis step gates on this so non-streaming callers (and tests, which stub
    `llm.complete` not `llm.stream_chat`) keep the original assembled-digest
    behaviour and never make a streaming LLM call."""
    return _token_emitter.get() is not None


async def emit_token(delta: str) -> None:
    """Emit one synthesis token delta reliably (awaited). No-op without an
    installed emitter or for an empty delta."""
    if not delta:
        return
    fn = _token_emitter.get()
    if fn is None:
        return
    await fn(delta)


def emit(stage: str, detail: str | None = None, *, round: int | None = None, subq: str | None = None) -> None:
    """Fire a progress event. Never raises; no-op without an installed emitter."""
    fn = _emitter.get()
    if fn is None:
        return
    event: dict = {"type": "progress", "stage": stage}
    if detail:
        event["detail"] = detail
    if round is not None:
        event["round"] = round
    if subq:
        event["subq"] = subq
    try:
        fn(event)
    except Exception as e:  # noqa: BLE001 — progress is best-effort by contract
        _log.debug("progress emit failed: %s", e)
