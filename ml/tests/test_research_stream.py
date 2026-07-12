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

"""Research NDJSON stream: progress events then a terminal done; error path;
queue-full drop policy; web-loop events bridged as collect-phase events."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app.research import pipeline as rp
from app.research import progress, stream
from app.web import progress as web_progress


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


async def _collect_events(question="q", template="freeform"):
    events = []
    async for e in stream.stream_events(question, template):
        events.append(e)
    return events


def test_progress_then_done(monkeypatch):
    async def fake_run(question, template_id="exploration", **_kw):
        progress.emit("plan", "planning")
        progress.emit("write", "Findings", sections_done=1, sections_total=4)
        return {"title": "T", "report_md": "## 1. X\nbody", "citations": []}

    monkeypatch.setattr(rp, "run", fake_run)
    events = _run(_collect_events())
    types = [e["type"] for e in events]
    assert types[-1] == "done"
    phases = [e["phase"] for e in events if e["type"] == "progress"]
    assert "plan" in phases and "write" in phases
    done = events[-1]
    assert done["title"] == "T" and done["report_md"].startswith("## 1.")


def test_web_events_bridged_to_collect_phase(monkeypatch):
    async def fake_run(question, template_id="exploration", **_kw):
        web_progress.emit("serp", "some query", round=1)
        return {"title": "T", "report_md": "r", "citations": []}

    monkeypatch.setattr(rp, "run", fake_run)
    events = _run(_collect_events())
    collected = [e for e in events if e.get("phase") == "collect"]
    assert collected and "serp" in collected[0]["detail"]


def test_error_terminal(monkeypatch):
    async def boom(question, template_id="exploration", **_kw):
        raise RuntimeError("pipeline exploded")

    monkeypatch.setattr(rp, "run", boom)
    events = _run(_collect_events())
    assert events[-1]["type"] == "error"
    assert "pipeline exploded" in events[-1]["message"]


def test_queue_full_drops_progress_done_survives(monkeypatch):
    async def flood(question, template_id="exploration", **_kw):
        for i in range(2000):
            progress.emit("collect", f"flood {i}")
        return {"title": "T", "report_md": "r", "citations": []}

    monkeypatch.setattr(rp, "run", flood)
    events = _run(_collect_events())
    n_progress = sum(1 for e in events if e["type"] == "progress")
    assert n_progress < 2000, "excess progress dropped"
    assert events[-1]["type"] == "done", "terminal never dropped"


def test_verify_threads_through_and_done_carries_verification(monkeypatch):
    seen = {}

    async def fake_run(question, template_id="exploration", **kw):
        seen.update(kw)
        return {"title": "T", "report_md": "r", "citations": [],
                "verification": {"score": 0.9, "total": 3, "supported": 3,
                                 "contradicted": 0, "not_mentioned": 0, "model": "factcg", "spans": []}}

    monkeypatch.setattr(rp, "run", fake_run)
    events = []

    async def go():
        async for e in stream.stream_events("q", "freeform", verify=True):
            events.append(e)

    _run(go())
    assert seen.get("verify") is True, "verify flag threads to pipeline.run"
    done = events[-1]
    assert done["type"] == "done" and done["verification"]["score"] == 0.9
