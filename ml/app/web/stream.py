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

"""NDJSON event stream for `/web_search?stream=true`: progress events while the loop runs, then a terminal
`{"type": "done", digest, citations}` (or `{"type": "error", message}`).

Back-pressure policy: progress events `put_nowait` onto a bounded queue and are
DROPPED when it is full — the loop never blocks on a slow consumer. The
terminal event is enqueued by the runner task itself with an awaited put, so it
is never dropped. Client disconnect cancels the loop task (the `finally`)."""

import asyncio
import logging
from typing import AsyncIterator

from . import loop, progress

_log = logging.getLogger("pai.web.stream")

_QUEUE_MAX = 256
_DONE = object()  # queue sentinel: runner finished, drain and stop


async def stream_events(query: str, recency: str = "any", depth: str = "standard") -> AsyncIterator[dict]:
    queue: asyncio.Queue = asyncio.Queue(maxsize=_QUEUE_MAX)

    def emit(event: dict) -> None:
        try:
            queue.put_nowait(event)
        except asyncio.QueueFull:
            pass  # drop — progress is best-effort, the loop must not block

    async def emit_token(delta: str) -> None:
        # RELIABLE (awaited) — synthesis tokens must not be dropped. A full queue
        # back-pressures the synthesis until the consumer drains; if the client is
        # gone the runner task is cancelled (the `finally` below), unblocking it.
        await queue.put({"type": "token", "delta": delta})

    # Install the emitters BEFORE create_task so the task's copied context
    # carries them (asyncio copies the current context into child tasks).
    progress.set_emitter(emit)
    progress.set_token_emitter(emit_token)

    async def runner() -> None:
        try:
            result = await loop.run(query, recency=recency, depth=depth)
            await queue.put({"type": "done", **result})
        except Exception as e:  # noqa: BLE001 — terminal error event, never a broken pipe
            _log.warning("web search stream errored: %s", e)
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
