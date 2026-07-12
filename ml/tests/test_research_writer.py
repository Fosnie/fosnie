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

"""Writer: section calls see only their bound notes; rolling summary and the
no-repeat register grow; [W#] markers survive; model-echoed headings are
stripped; failures degrade to evidence-quoting bodies."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio

from app import llm
from app.research import writer as writer_mod
from app.research.bank import Bank, Note
from app.research.budgets import budgets
from app.research.outline import Outline, OutlineSection
from app.web.pipeline import _Source


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _bank() -> Bank:
    b = Bank()
    for i in (1, 2):
        sid = b.add_source(_Source(
            url=f"https://s{i}.example/x", title=f"Source {i}", domain=f"s{i}.example",
            published_date="2026-01-0" + str(i), fetched_at="2026-06-10T00:00:00+00:00",
            snippet_only=False, chunks=[f"chunk {i}"],
        ))
        b.get(sid).note = Note(claims=[f"unique claim {i}"], quotes=[f"verbatim {i}"])
    return b


_OUTLINE = Outline(sections=[
    OutlineSection("First", "the first", ["W1"]),
    OutlineSection("Second", "the second", ["W2"]),
])


def test_writer_sees_only_bound_notes(monkeypatch):
    captured = {}

    async def fake_llm(system, user, max_tokens=0):
        captured["system"], captured["user"] = system, user
        return "Body text citing [W1] properly."

    monkeypatch.setattr(llm, "complete", fake_llm)
    out = _run(writer_mod.write_section(0, _OUTLINE, _bank(), "", [], "instr", budgets(65_536)))
    assert "[W1]" in out
    assert "unique claim 1" in captured["user"]
    assert "unique claim 2" not in captured["user"], "section 1 must not see section 2's notes"
    assert "cite ONLY" in captured["system"].lower() or "Cite ONLY" in captured["system"]


def test_register_and_summary_passed(monkeypatch):
    captured = {}

    async def fake_llm(system, user, max_tokens=0):
        captured["user"] = user
        return "ok [W2]"

    monkeypatch.setattr(llm, "complete", fake_llm)
    _run(writer_mod.write_section(
        1, _OUTLINE, _bank(), "summary so far text", ["already covered point"], "instr", budgets(65_536),
    ))
    assert "summary so far text" in captured["user"]
    assert "already covered point" in captured["user"]


def test_model_headings_stripped(monkeypatch):
    async def fake_llm(system, user, max_tokens=0):
        return "## My Own Heading\nActual body [W1].\n### Sub\nMore body."

    monkeypatch.setattr(llm, "complete", fake_llm)
    out = _run(writer_mod.write_section(0, _OUTLINE, _bank(), "", [], "i", budgets(32_768)))
    assert "##" not in out
    assert "Actual body [W1]." in out


def test_streaming_emits_prose_tokens_and_filters_think(monkeypatch):
    """With a token emitter installed and stream=True, the body's prose tokens are
    emitted live (reasoning filtered out) and the returned body matches them."""
    from app.research import progress

    async def fake_stream(messages, sampling, model=None):
        for delta in ["<think>", "reasoning ", "noise", "</think>", "Body ", "[W1]."]:
            yield {"type": "token", "delta": delta}
        yield {"type": "done", "finish_reason": "stop", "model": "x", "usage": {}}

    monkeypatch.setattr(llm, "stream_chat", fake_stream)

    emitted: list[str] = []

    async def scenario():
        async def emit_token(delta: str) -> None:
            emitted.append(delta)

        progress.set_token_emitter(emit_token)
        try:
            return await writer_mod.write_section(
                0, _OUTLINE, _bank(), "", [], "instr", budgets(65_536), stream=True
            )
        finally:
            progress.set_token_emitter(None)

    out = _run(scenario())
    assert out == "Body [W1]."
    assert "".join(emitted) == "Body [W1].", "only prose streams; <think> is filtered"
    assert "reasoning" not in "".join(emitted)


def test_stream_flag_without_emitter_uses_complete(monkeypatch):
    """stream=True but NO emitter installed → falls back to complete() (the path
    tests and non-streaming callers rely on); never calls stream_chat."""
    async def fake_complete(system, user, max_tokens=0):
        return "Plain body [W1]."

    async def boom(*a, **k):
        raise AssertionError("stream_chat must not be called without an emitter")
        yield  # pragma: no cover — make it an async generator

    monkeypatch.setattr(llm, "complete", fake_complete)
    monkeypatch.setattr(llm, "stream_chat", boom)
    out = _run(writer_mod.write_section(0, _OUTLINE, _bank(), "", [], "i", budgets(32_768), stream=True))
    assert out == "Plain body [W1]."


def test_writer_failure_degrades_to_evidence(monkeypatch):
    async def dead(system, user, max_tokens=0):
        raise RuntimeError("down")

    monkeypatch.setattr(llm, "complete", dead)
    out = _run(writer_mod.write_section(0, _OUTLINE, _bank(), "", [], "i", budgets(32_768)))
    assert "unique claim 1" in out and "[W1]" in out, "fallback quotes the evidence"


def test_rolling_summary_fallback_and_register():
    async def scenario():
        async def dead(system, user, max_tokens=0):
            raise RuntimeError("down")

        import app.llm as l

        orig = l.complete
        l.complete = dead
        try:
            s = await writer_mod.update_rolling("old summary", "Heading", "New section words here. More.")
            assert "old summary" in s and "Heading" in s, "deterministic concat fallback"
        finally:
            l.complete = orig

    _run(scenario())
    reg: list[str] = []
    writer_mod.extend_register(reg, "Heading", "This is the first sentence of paragraph one. Rest.\n\nSecond paragraph opener sentence here. Tail.")
    assert "Heading" in reg
    assert any("first sentence" in r for r in reg)
    assert any("Second paragraph opener" in r for r in reg)
