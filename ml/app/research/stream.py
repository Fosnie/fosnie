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

"""NDJSON event stream for `POST /deep_research` — the web/stream.py mechanics:
progress events on a bounded queue (drop-on-full; the pipeline never blocks on
a slow consumer), an awaited terminal `{"type": "done", title, report_md,
citations}` (or error), and client-disconnect cancellation. Also bridges the
web loop's own progress events into research `phase="collect"` events so the
user sees SERP/fetch activity during collection."""

import asyncio
import logging
from typing import AsyncIterator

from ..web import progress as web_progress
from . import pipeline, progress

_log = logging.getLogger("pai.research.stream")

_QUEUE_MAX = 256
_DONE = object()


async def stream_events(
    question: str,
    template: str = "exploration",
    source: str = "web",
    kb_ids: list[str] | None = None,
    docs: list[dict] | None = None,
    total_docs: int | None = None,
    refinements: list[str] | None = None,
    verify: bool = False,
) -> AsyncIterator[dict]:
    queue: asyncio.Queue = asyncio.Queue(maxsize=_QUEUE_MAX)

    def emit(event: dict) -> None:
        try:
            queue.put_nowait(event)
        except asyncio.QueueFull:
            pass  # drop — progress is best-effort

    def emit_web(event: dict) -> None:
        # Bridge web-loop progress (serp/fetch/grade…) into collect-phase
        # research events; detail keeps the inner stage for colour.
        stage = event.get("stage", "")
        detail = event.get("detail", "")
        emit({"type": "progress", "phase": "collect", "detail": f"{stage}: {detail}".strip(": ")})

    async def emit_token(delta: str) -> None:
        # Report-writing tokens are RELIABLE (awaited) — never dropped.
        await queue.put({"type": "token", "delta": delta})

    # Install BEFORE create_task so the task's copied context carries them.
    progress.set_emitter(emit)
    progress.set_token_emitter(emit_token)
    web_progress.set_emitter(emit_web)

    async def runner() -> None:
        try:
            result = await pipeline.run(
                question,
                template_id=template,
                source=source,
                kb_ids=kb_ids or [],
                docs=docs or [],
                total_docs=total_docs,
                refinements=refinements or [],
                verify=verify,
            )
            await queue.put({"type": "done", **result})
        except Exception as e:  # noqa: BLE001 — terminal error event
            _log.warning("deep research stream errored: %s", e)
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
        task.cancel()
        try:
            await task
        except (asyncio.CancelledError, Exception):  # noqa: BLE001
            pass
