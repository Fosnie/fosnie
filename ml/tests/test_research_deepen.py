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

"""Per-section deepening: the sufficiency judge (parse + fail-open + gap cap), a
targeted dig that grows only the hungry section and leaves its neighbours
untouched, the files-mode zero-egress guarantee, the deadline/disable no-ops
(pipeline stays byte-identical), and clean cancellation."""

import asyncio
import pathlib
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import llm, reranker
from app.config import settings
from app.research import deepen as dp
from app.research import pipeline as rp
from app.research import progress
from app.research.bank import Bank, DocSource, Note
from app.research.budgets import budgets
from app.research.outline import Outline, OutlineSection
from app.web import loop as web_loop
from app.web.loop import CollectResult, _Pool
from app.web.pipeline import _Source


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _web_source(tag: str) -> _Source:
    return _Source(
        url=f"https://{tag}.example/a", title=f"Source {tag}", domain=f"{tag}.example",
        published_date="2026-05-01", fetched_at="2026-06-10T00:00:00+00:00",
        snippet_only=False, chunks=[f"fresh evidence about {tag} " * 8],
    )


def _bank_with(*specs: tuple[str, int]) -> tuple[Bank, dict[str, str]]:
    """A bank of web sources; each spec is (tag, n_claims). Returns the bank and
    a {tag: sid} map so a section can bind to a chosen source."""
    bank = Bank()
    tag_sid: dict[str, str] = {}
    for tag, n in specs:
        sid = bank.add_source(_web_source(tag))
        bank.get(sid).note = Note(claims=[f"claim {i} for {tag}" for i in range(n)], quotes=[])
        tag_sid[tag] = sid
    return bank, tag_sid


# --- Judge: parse, fail-open, gap cap ---------------------------------------


def test_judge_parses_gaps(monkeypatch):
    async def fake(system, user, max_tokens=0):
        return '{"sufficient": false, "gaps": [{"query": "find X", "why": "sparse"}]}'

    monkeypatch.setattr(llm, "complete", fake)
    bank, m = _bank_with(("a", 1))
    section = OutlineSection("Topic", "brief", [m["a"]])
    outline = Outline([section])
    j = _run(dp._judge("q", outline, section, bank, budgets(65_536)))
    assert j.sufficient is False
    assert [g.query for g in j.gaps] == ["find X"]


def test_judge_fail_open_on_broken_json(monkeypatch):
    async def fake(system, user, max_tokens=0):
        return "the model rambled and produced no JSON at all"

    monkeypatch.setattr(llm, "complete", fake)
    bank, m = _bank_with(("a", 1))
    section = OutlineSection("Topic", "brief", [m["a"]])
    j = _run(dp._judge("q", Outline([section]), section, bank, budgets(65_536)))
    assert j.sufficient is True, "unparseable ⇒ treated sufficient (never fails the run)"
    assert j.gaps == []


def test_judge_caps_gaps_at_three(monkeypatch):
    async def fake(system, user, max_tokens=0):
        gaps = ",".join(f'{{"query": "q{i}", "why": "w"}}' for i in range(6))
        return f'{{"sufficient": false, "gaps": [{gaps}]}}'

    monkeypatch.setattr(llm, "complete", fake)
    bank, m = _bank_with(("a", 1))
    section = OutlineSection("Topic", "brief", [m["a"]])
    j = _run(dp._judge("q", Outline([section]), section, bank, budgets(65_536)))
    assert len(j.gaps) == 3, "a runaway gap list is capped at three"


def test_judge_reasoning_effort_not_overridden(monkeypatch):
    # Reasoning guard: the judge relies on complete()'s default (minimal → none on
    # gpt-5.x); it must NOT pass an explicit effort override.
    seen = {}

    async def fake(system, user, max_tokens=0, **kw):
        seen.update(kw)
        return '{"sufficient": true, "gaps": []}'

    monkeypatch.setattr(llm, "complete", fake)
    bank, m = _bank_with(("a", 1))
    section = OutlineSection("Topic", "brief", [m["a"]])
    _run(dp._judge("q", Outline([section]), section, bank, budgets(65_536)))
    assert "reasoning_effort" not in seen, "judge must not override reasoning effort"


# --- Dig: hungry section grows, neighbours untouched ------------------------


def _judge_and_notes(monkeypatch, *, insufficient_heading: str):
    """LLM stub: the named section is judged insufficient with one gap, every
    other section sufficient; notes calls return a normal claim/quote note."""

    async def fake(system, user, max_tokens=0, **kw):
        if "research notes from ONE web source" in system:
            return '{"claims": ["a dug claim with figure 7"], "quotes": ["seven"]}'
        if "You assess whether a report section" in system:
            if f'"{insufficient_heading}"' in user:
                return '{"sufficient": false, "gaps": [{"query": "dig more", "why": "thin"}]}'
            return '{"sufficient": true, "gaps": []}'
        return "unused"

    monkeypatch.setattr(llm, "complete", fake)


def test_dig_grows_only_hungry_section(monkeypatch):
    _judge_and_notes(monkeypatch, insufficient_heading="Thin")

    async def fake_collect(query, recency, budget, *, pool=None, seen=None, state=None):
        pool = pool if pool is not None else _Pool()
        pool.upgrade_fetched(_web_source("dug"))
        return CollectResult(pool=pool, subq_evidence=[], notes=[], beast=False)

    monkeypatch.setattr(web_loop, "collect", fake_collect)

    bank, m = _bank_with(("thin", 1), ("rich", 8))
    thin = OutlineSection("Thin", "needs more", [m["thin"]])
    rich = OutlineSection("Rich", "well covered", [m["rich"]])
    outline = Outline([thin, rich])

    stats = _run(dp.deepen("q", outline, bank, budgets(65_536),
                           source="web", kb_ids=None, docs=None, seen=set(), deadline=time.monotonic() + 60))

    assert stats["new_sources"] == 1
    assert len(thin.note_ids) == 2, "the hungry section gained the dug source"
    new_sid = thin.note_ids[-1]
    assert bank.get(new_sid).note is not None, "the dug source was noted"
    assert rich.note_ids == [m["rich"]], "the well-covered section is untouched"


def test_dig_respects_max_new_sources(monkeypatch):
    _judge_and_notes(monkeypatch, insufficient_heading="Thin")

    async def flood_collect(query, recency, budget, *, pool=None, seen=None, state=None):
        pool = pool if pool is not None else _Pool()
        for i in range(20):
            pool.upgrade_fetched(_web_source(f"f{i}"))
        return CollectResult(pool=pool, subq_evidence=[], notes=[], beast=False)

    monkeypatch.setattr(web_loop, "collect", flood_collect)
    bank, m = _bank_with(("thin", 1))
    thin = OutlineSection("Thin", "needs more", [m["thin"]])
    b = budgets(65_536)
    stats = _run(dp.deepen("q", Outline([thin]), bank, b,
                           source="web", kb_ids=None, docs=None, seen=set(), deadline=time.monotonic() + 60))
    assert stats["new_sources"] <= b.deepen_max_new_sources, "a section cannot exceed its source cap"


# --- Files mode: zero egress -------------------------------------------------


def test_files_dig_never_touches_the_web(monkeypatch):
    _judge_and_notes(monkeypatch, insufficient_heading="Thin")
    web_calls = {"n": 0}

    async def boom_collect(*a, **k):
        web_calls["n"] += 1
        raise AssertionError("a files run must perform ZERO web collection")

    monkeypatch.setattr(web_loop, "collect", boom_collect)

    import app.retrieve as retrieve_mod

    async def fake_retrieve(prompt, kb_ids, deny_doc_ids=None):
        return {"citations": [
            {"doc_id": "doc-9", "chunk_index": 1, "page_number": 4,
             "clause_section_ref": "§3", "quote_text": "a corpus passage"},
        ]}

    monkeypatch.setattr(retrieve_mod, "retrieve", fake_retrieve)

    bank = Bank()
    sid = bank.add_doc_source(DocSource(doc_id="doc-1", kb_id="kb1", kb_name="KB", filename="f1.docx"))
    bank.get(sid).note = Note(claims=["thin doc claim"], quotes=[])
    thin = OutlineSection("Thin", "needs more", [sid])
    docs = [{"doc_id": "doc-9", "kb_id": "kb1", "kb_name": "KB", "filename": "f9.docx"}]

    stats = _run(dp.deepen("q", Outline([thin]), bank, budgets(65_536),
                           source="files", kb_ids=["kb1"], docs=docs, seen=set(), deadline=time.monotonic() + 60))
    assert web_calls["n"] == 0
    assert stats["new_sources"] == 1
    assert thin.note_ids[-1].startswith("D"), "the dug corpus source is a D# document"


# --- Deadline + disable no-ops (byte-identical pipeline) ---------------------


def _capture_events():
    events: list[dict] = []
    progress.set_emitter(events.append)
    return events


def test_deadline_passed_is_silent_noop(monkeypatch):
    events = _capture_events()
    try:
        bank, m = _bank_with(("a", 1))
        section = OutlineSection("Topic", "brief", [m["a"]])
        stats = _run(dp.deepen("q", Outline([section]), bank, budgets(65_536),
                               source="web", kb_ids=None, docs=None, seen=set(),
                               deadline=time.monotonic() - 1))  # already expired
        assert stats == {"sections": 0, "new_sources": 0}
        assert not [e for e in events if e["phase"] == "deepen"], "no events past the deadline"
    finally:
        progress.set_emitter(None)


def test_rounds_zero_is_silent_noop(monkeypatch):
    events = _capture_events()
    try:
        b = budgets(65_536)
        b.deepen_rounds = 0  # the small-context / disabled state
        bank, m = _bank_with(("a", 1))
        section = OutlineSection("Topic", "brief", [m["a"]])
        stats = _run(dp.deepen("q", Outline([section]), bank, b,
                               source="web", kb_ids=None, docs=None, seen=set(), deadline=time.monotonic() + 60))
        assert stats == {"sections": 0, "new_sources": 0}
        assert not [e for e in events], "zero rounds ⇒ the stage emits nothing at all"
    finally:
        progress.set_emitter(None)


# --- Full-pipeline guard: disabling keeps the run identical + deepen-free ----


def _mock_pipeline(monkeypatch):
    """Minimal end-to-end stack (mirrors test_research_pipeline) with a judge stub
    added, so the deepening stage runs but finds every section sufficient."""
    import app.main as main_mod

    async def fake_resolve():
        return ("model", 65_536)

    monkeypatch.setattr(main_mod, "_resolve_model", fake_resolve)

    async def fake_collect(query, recency, budget, *, pool=None, seen=None, state=None):
        pool = pool if pool is not None else _Pool()
        for i in range(1, 4):
            src = _web_source(str(i))
            if src.url not in {s.url for s in pool.sources}:
                pool.upgrade_fetched(src)
        return CollectResult(pool=pool, subq_evidence=[], notes=[], beast=False)

    monkeypatch.setattr(web_loop, "collect", fake_collect)

    async def fake_rerank(query, docs):
        return [float(len(docs) - i) for i in range(len(docs))]

    monkeypatch.setattr(reranker, "rerank", fake_rerank)

    body = ("substantive finding " * 25).strip()

    async def fake_llm(system, user, max_tokens=0, **kw):
        if "Decompose the research question" in system:
            return '["the question", "an angle"]'
        if "research notes from ONE web source" in system:
            return '{"claims": ["a precise claim 42"], "quotes": ["forty-two"]}'
        if "You assess whether a report section" in system:
            return '{"sufficient": true, "gaps": []}'
        if "planning a research report outline" in system:
            return ('[{"heading": "Findings", "brief": "b", "note_ids": ["W1","W2"]},'
                    '{"heading": "Analysis", "brief": "b", "note_ids": ["W3"]}]')
        if "EDITING a finished research report" in system:
            return user.split("\n\n", 1)[1]
        if "running summary" in system:
            return "summary"
        if "report title" in system:
            return "A Title"
        return f"{body} [W1] [W3]"

    monkeypatch.setattr(llm, "complete", fake_llm)


def test_disabled_emits_no_deepen_phase_and_matches_enabled_noop(monkeypatch):
    # Enabled but every section sufficient ⇒ a pure no-op on content. Disabled ⇒
    # the stage is skipped entirely (no deepen events). The report is identical
    # either way: deepening that changes nothing changes nothing.
    _mock_pipeline(monkeypatch)
    events_on = _capture_events()
    try:
        on = _run(rp.run("the question", "freeform"))
    finally:
        progress.set_emitter(None)

    _mock_pipeline(monkeypatch)
    monkeypatch.setattr(settings, "research_deepen_enabled", False)
    events_off = _capture_events()
    try:
        off = _run(rp.run("the question", "freeform"))
    finally:
        progress.set_emitter(None)

    assert not [e for e in events_off if e["phase"] == "deepen"], "disabled ⇒ no deepen phase"
    assert on["report_md"] == off["report_md"], "a content no-op leaves the report byte-identical"


# --- Cancellation ------------------------------------------------------------


def test_hung_provider_cannot_overrun_the_time_box(monkeypatch):
    # The deadline is only consulted between rounds and between gaps, so a provider
    # call already in flight must be bounded on its own: a backend that hangs far
    # longer than the remaining budget has to be abandoned, not waited on.
    _judge_and_notes(monkeypatch, insufficient_heading="Thin")
    hang = 30.0

    async def hanging_collect(query, recency, budget, *, pool=None, seen=None, state=None):
        await asyncio.sleep(hang)
        return CollectResult(pool=pool or _Pool(), subq_evidence=[], notes=[], beast=False)

    monkeypatch.setattr(web_loop, "collect", hanging_collect)

    b = budgets(65_536)
    b.deepen_seconds = 1.0  # the whole stage gets one second of provider time

    async def drive():
        bank, m = _bank_with(("thin", 1))
        thin = OutlineSection("Thin", "needs more", [m["thin"]])
        t0 = time.monotonic()
        stats = await dp.deepen("q", Outline([thin]), bank, b,
                                source="web", kb_ids=None, docs=None, seen=set(),
                                deadline=time.monotonic() + 600)
        elapsed = time.monotonic() - t0
        leftover = [t for t in asyncio.all_tasks()
                    if t is not asyncio.current_task() and not t.done()]
        return stats, elapsed, thin, leftover

    stats, elapsed, thin, leftover = _run(drive())
    assert elapsed < hang / 2, f"the stage waited on the hung provider ({elapsed:.1f}s)"
    assert stats == {"sections": 0, "new_sources": 0}, "a timed-out dig yields nothing"
    assert thin.note_ids == [thin.note_ids[0]], "the section keeps its original binding"
    assert leftover == [], "the abandoned dig leaves no orphan task"


def test_cancel_mid_dig_propagates_without_orphans(monkeypatch):
    _judge_and_notes(monkeypatch, insufficient_heading="Thin")
    reached = {"after": False}

    async def hang_collect(query, recency, budget, *, pool=None, seen=None, state=None):
        await asyncio.sleep(30)  # a dig in flight
        reached["after"] = True
        return CollectResult(pool=pool or _Pool(), subq_evidence=[], notes=[], beast=False)

    monkeypatch.setattr(web_loop, "collect", hang_collect)

    async def drive():
        bank, m = _bank_with(("thin", 1))
        thin = OutlineSection("Thin", "needs more", [m["thin"]])
        task = asyncio.ensure_future(
            dp.deepen("q", Outline([thin]), bank, budgets(65_536),
                      source="web", kb_ids=None, docs=None, seen=set(), deadline=time.monotonic() + 60)
        )
        await asyncio.sleep(0.05)  # let it reach the hanging collect
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass
        # No orphaned dig kept running after cancellation.
        leftover = [t for t in asyncio.all_tasks() if t is not asyncio.current_task() and not t.done()]
        return task.cancelled(), reached["after"], leftover

    cancelled, ran_after, leftover = _run(drive())
    assert cancelled, "cancellation propagated to the deepen task"
    assert not ran_after, "the in-flight dig did not silently complete"
    assert leftover == [], "no orphan tasks survive cancellation"
