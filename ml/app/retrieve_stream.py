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

"""NDJSON event stream for `/retrieve?stream=true`:
progress events while the agentic retrieval loop runs, then a terminal
`{"type": "done", context, citations}` (or `{"type": "error", message}`). It lets
the Rust chat hot path surface live "Searching your library…" activity before the
first generated token — retrieval already finishes before generation, so this is a
perceived-latency win, not a change to real TTFT.

Mirrors web/stream.py: progress events `put_nowait` onto a bounded queue and are
DROPPED when it is full (retrieval never blocks on a slow consumer); the terminal
event is enqueued by the runner with an awaited put, so it is never dropped.
Client disconnect cancels the runner task (the `finally`)."""

import asyncio
import logging
from typing import AsyncIterator

from . import retrieve

_log = logging.getLogger("pai-ml.retrieve.stream")

_QUEUE_MAX = 256
_DONE = object()  # queue sentinel: runner finished, drain and stop


async def stream_events(
    prompt: str, kb_ids: list[str], deny_doc_ids: list[str] | None = None
) -> AsyncIterator[dict]:
    queue: asyncio.Queue = asyncio.Queue(maxsize=_QUEUE_MAX)

    def emit(event: dict) -> None:
        try:
            queue.put_nowait(event)
        except asyncio.QueueFull:
            pass  # drop — progress is best-effort, retrieval must not block

    # Install the emitter BEFORE create_task so the task's copied context carries
    # it (asyncio copies the current context into child tasks).
    retrieve.set_emitter(emit)

    async def runner() -> None:
        try:
            result = await retrieve.retrieve(prompt, kb_ids, deny_doc_ids)
            await queue.put({"type": "done", **result})
        except Exception as e:  # noqa: BLE001 — terminal error event, never a broken pipe
            _log.warning("retrieve stream errored: %s", e)
            await queue.put({"type": "error", "message": str(e)})
        finally:
            await queue.put(_DONE)

    task = asyncio.create_task(runner())
    try:
        while True:
            event = await queue.get()
            if event is _DONE:
                break
            yield event
    finally:
        # Client gone or terminal reached — stop the loop either way.
        task.cancel()
        try:
            await task
        except (asyncio.CancelledError, Exception):  # noqa: BLE001
            pass
