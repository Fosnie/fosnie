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

"""Progress events from inside the research pipeline — same ContextVar shape
as web/progress.py: the streaming endpoint installs an emitter for its request
context; everywhere else `emit()` is a no-op. Fire-and-forget; the pipeline
never blocks or fails on a slow/gone consumer."""

import logging
from contextvars import ContextVar
from typing import Awaitable, Callable

_log = logging.getLogger("pai.research.progress")

_emitter: ContextVar[Callable[[dict], None] | None] = ContextVar("research_progress_emitter", default=None)
# Report-writing tokens are RELIABLE (a dropped token corrupts the streamed
# report), so they ride a separate async emitter that awaits the queue put —
# unlike the best-effort, drop-on-full progress emitter.
_token_emitter: ContextVar[Callable[[str], Awaitable[None]] | None] = ContextVar(
    "research_token_emitter", default=None
)


def set_emitter(fn: Callable[[dict], None] | None) -> None:
    _emitter.set(fn)


def set_token_emitter(fn: Callable[[str], Awaitable[None]] | None) -> None:
    """Install the per-request RELIABLE token emitter (async, takes the delta).
    No-op everywhere except the streaming research run."""
    _token_emitter.set(fn)


def token_emitter_installed() -> bool:
    """True when a token consumer is listening — i.e. the streaming research run.
    The writer gates on this so non-streaming callers (and tests, which stub
    `llm.complete` not `llm.stream_chat`) keep using `complete()` and never make a
    streaming LLM call."""
    return _token_emitter.get() is not None


async def emit_token(delta: str) -> None:
    """Emit one report token delta reliably (awaited). No-op without an installed
    emitter or for an empty delta."""
    if not delta:
        return
    fn = _token_emitter.get()
    if fn is None:
        return
    await fn(delta)


def emit(
    phase: str,
    detail: str | None = None,
    *,
    sources_read: int | None = None,
    sections_done: int | None = None,
    sections_total: int | None = None,
    sections: list[str] | None = None,
) -> None:
    """Fire a research progress event. Never raises; no-op without an emitter.

    `sections` carries the full ordered list of section headings (emitted once the
    outline is known) so the client can render the report roadmap before writing
    begins."""
    fn = _emitter.get()
    if fn is None:
        return
    event: dict = {"type": "progress", "phase": phase}
    if detail:
        event["detail"] = detail
    if sources_read is not None:
        event["sources_read"] = sources_read
    if sections_done is not None:
        event["sections_done"] = sections_done
    if sections_total is not None:
        event["sections_total"] = sections_total
    if sections is not None:
        event["sections"] = sections
    try:
        fn(event)
    except Exception as e:  # noqa: BLE001 — progress is best-effort by contract
        _log.debug("research progress emit failed: %s", e)
