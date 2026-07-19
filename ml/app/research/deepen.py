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

"""Per-section deepening (step 3c, before writing): a bounded, agentic loop that
judges each section's evidence sufficiency and digs for the gaps.

Today the writer sees a section's bound notes once and writes single-pass; an
under-evidenced ("hungry") section produces thin, padded prose. This stage runs
AFTER the outline is final and BEFORE the writer loop: for each hungry section it
asks a cheap scaffolding judge whether the bound evidence suffices, and if not
runs one or two targeted digs (web collect / corpus retrieve, mirroring the
primary collection paths), binding the new sources to that section only.

It never changes the section count or headings, so the already-published roadmap
stays valid. Every failure fails open — a judge that errors is treated as
"sufficient", a dig that errors is skipped, and the stage as a whole is wrapped
by the caller so a section keeps its original bindings on any error. When the
budget disables it (small context, or the admin switch off) the caller skips the
stage entirely and the pipeline is byte-identical to the single-pass path."""

import asyncio
import json
import logging
import time
from dataclasses import dataclass

from .. import guided, llm, retrieve
from ..config import settings
from ..web import loop as web_loop
from ..web.loop import _Pool, _State
from . import notes as notes_mod
from . import progress
from . import writer as writer_mod
from .bank import Bank, DocSource
from .budgets import ResearchBudgets, est_tokens
from .outline import Outline, OutlineSection

_log = logging.getLogger("pai.research.deepen")

# Slack added to the stage's own wall-clock slice for the caller's safety net. The
# per-dig timeouts below already bound each provider call to the remaining budget;
# this only catches a pathology outside them, so it stays small.
STAGE_GRACE_SECONDS = 30.0


@dataclass
class _Gap:
    query: str
    why: str


@dataclass
class _Judgement:
    sufficient: bool
    gaps: list[_Gap]


@dataclass
class _Ctx:
    """Stage-global handles shared by every section's dig. The event loop is
    single-threaded, so the bind loops (which hold no `await` between reading and
    mutating the bank) stay race-free even while sections dig concurrently.
    `seen` is shared with the primary collect so a URL already fetched is never
    re-fetched, and shared across sections so two hungry sections never fetch the
    same new URL twice."""

    question: str
    bank: Bank
    b: ResearchBudgets
    source: str
    kb_ids: list[str]
    seen: set[str]
    docs_meta: dict[str, dict]
    notes_sem: asyncio.Semaphore
    deepen_deadline: float


def _budget_left(ctx: _Ctx) -> float:
    """Seconds remaining before the stage's deadline. Every provider call inside a
    dig is bounded by this, so a hung backend cannot overrun the time-box: the
    deadline checks between rounds and gaps cannot interrupt a call already in
    flight."""
    return ctx.deepen_deadline - time.monotonic()


def _note_mass(bank: Bank, section: OutlineSection) -> int:
    """Token mass of the section's bound notes — the hunger signal. A section
    whose evidence does not even fill the writer's input window is a natural
    candidate for deepening."""
    return sum(est_tokens(rec.note.text()) for rec in bank.resolve(section.note_ids) if rec.note)


async def _judge(question: str, outline: Outline, section: OutlineSection, bank: Bank, b: ResearchBudgets) -> _Judgement:
    """One scaffolding call: does this section's bound evidence suffice, and if
    not, what self-contained queries fill the gap. Fails open to `sufficient`."""
    digest = writer_mod._notes_block(bank, section.note_ids, b.deepen_input_tokens)
    others = "\n".join(f"- {s.heading}" for s in outline.sections if s is not section)
    system = (
        "You assess whether a report section has ENOUGH evidence to be written "
        "well, and if not, what to search for next. Judge ONLY this section's "
        "remit: the other section headings are listed so you do not demand "
        "material that belongs to a neighbour. Return ONLY JSON: "
        '{"sufficient": bool, "gaps": [{"query": str, "why": str}]}. At most 3 '
        "gaps. Each query must be a self-contained search string: no pronouns, no "
        'reference to "this section".'
    )
    user = (
        f"Research question: {question}\n\n"
        f'Section: "{section.heading}" — {section.brief}\n\n'
        f"Other sections (NOT this section's job):\n{others or '(none)'}\n\n"
        f"Evidence currently bound to this section:\n{digest or '(none)'}"
    )
    try:
        llm.set_stage("research.deepen_judge")
        llm.set_guided(guided.RESEARCH_DEEPEN)
        out = await llm.complete(system, user, max_tokens=512)
        start, end = out.find("{"), out.rfind("}")
        obj = json.loads(out[start : end + 1]) if start >= 0 else {}
        gaps: list[_Gap] = []
        for g in obj.get("gaps", [])[:3]:  # hard cap at 3
            if not isinstance(g, dict):
                continue
            q = str(g.get("query", "")).strip()
            if q:
                gaps.append(_Gap(query=q, why=str(g.get("why", "")).strip()))
        return _Judgement(sufficient=bool(obj.get("sufficient", True)), gaps=gaps)
    except Exception as e:  # noqa: BLE001 — a judge that fails cannot fail the run
        _log.debug("deepen judge failed for %r (treated sufficient): %s", section.heading, e)
        return _Judgement(sufficient=True, gaps=[])


async def _note_new(ctx: _Ctx, recs: list) -> None:
    """Build notes for freshly-dug records (idempotent per record). Reuses the
    same per-source note builder as the primary notes step so dug evidence reads
    identically to collected evidence, and so a later re-judge round sees it."""
    todo = [r for r in recs if r is not None and r.note is None]
    if todo:
        await asyncio.gather(*[notes_mod._note_one(ctx.notes_sem, r, ctx.question, ctx.b) for r in todo])


async def _dig_web(ctx: _Ctx, state: _State, section: OutlineSection, gap: _Gap, room: int) -> int:
    """Web dig for one gap. Digs into a fresh local pool (so its new sources are
    unambiguously this section's) while sharing `seen`/`state` for dedup and the
    section's fetch budget. New sources fold into the bank directly (bypassing the
    primary rerank cap — deliberate headroom so a shared cap can never evict a
    neighbour's evidence) and bind to THIS section only."""
    left = _budget_left(ctx)
    if left <= 0:
        return 0
    local_pool = _Pool()
    wb = ctx.b.per_deepen_budget()
    try:
        await asyncio.wait_for(
            web_loop.collect(gap.query, "any", wb, pool=local_pool, seen=ctx.seen, state=state),
            timeout=left,
        )
    except TimeoutError:  # the dig is skipped; anything it half-gathered is dropped
        _log.warning("web deepen dig timed out for %r (%.0fs budget)", gap.query, left)
        return 0
    except Exception as e:  # noqa: BLE001 — a failed dig is skipped, not fatal
        _log.warning("web deepen dig failed for %r: %s", gap.query, e)
        return 0
    added, recs = 0, []
    for src in local_pool.sources:
        if added >= room:
            break
        if not src.chunks:
            continue
        sid = ctx.bank.add_source(src)
        if sid not in section.note_ids:
            section.note_ids.append(sid)
            recs.append(ctx.bank.get(sid))
            added += 1
    await _note_new(ctx, recs)
    return added


async def _dig_files(ctx: _Ctx, section: OutlineSection, gap: _Gap, room: int) -> int:
    """Corpus dig for one gap — ZERO egress. Retrieves against the readable KBs
    and groups the returned citations into D# sources, mirroring the sampling
    path, then binds them to THIS section only."""
    left = _budget_left(ctx)
    if left <= 0:
        return 0
    try:
        res = await asyncio.wait_for(retrieve.retrieve(gap.query, ctx.kb_ids), timeout=left)
    except TimeoutError:  # the dig is skipped; the section keeps what it had
        _log.warning("corpus deepen dig timed out for %r (%.0fs budget)", gap.query, left)
        return 0
    except Exception as e:  # noqa: BLE001
        _log.warning("corpus deepen dig failed for %r: %s", gap.query, e)
        return 0
    grouped: dict[str, dict] = {}
    for c in res.get("citations", []):
        did = c.get("doc_id")
        if not did:
            continue
        did = str(did)
        g = grouped.setdefault(did, {"chunks": [], "anchor": c})
        q = c.get("quote_text")
        if q:
            g["chunks"].append(q)
    added, recs = 0, []
    for did, g in grouped.items():
        if added >= room:
            break
        m = ctx.docs_meta.get(did, {})
        a = g["anchor"]
        sid = ctx.bank.add_doc_source(
            DocSource(
                doc_id=did,
                kb_id=m.get("kb_id") or (ctx.kb_ids[0] if ctx.kb_ids else ""),
                kb_name=m.get("kb_name", ""),
                filename=m.get("filename", f"document {did[:8]}"),
                mime=m.get("mime"),
                path=m.get("path", ""),
                chunks=g["chunks"],
                page_number=a.get("page_number"),
                chunk_index=a.get("chunk_index"),
                clause_section_ref=a.get("clause_section_ref"),
            )
        )
        if sid not in section.note_ids:
            section.note_ids.append(sid)
            recs.append(ctx.bank.get(sid))
            added += 1
    await _note_new(ctx, recs)
    return added


async def _dig(ctx: _Ctx, state: _State, section: OutlineSection, gap: _Gap, room: int) -> int:
    """Dispatch one gap by source mode. Hybrid digs corpus first, then web
    (mirroring the primary corpus-then-gap-web ordering)."""
    if room <= 0:
        return 0
    added = 0
    if ctx.source in ("files", "hybrid"):
        added += await _dig_files(ctx, section, gap, room - added)
    if ctx.source in ("web", "hybrid") and added < room:
        added += await _dig_web(ctx, state, section, gap, room - added)
    return added


async def _deepen_section(ctx: _Ctx, sem: asyncio.Semaphore, outline: Outline, section: OutlineSection) -> int:
    """Judge → dig → re-judge for one section, up to `deepen_rounds`. Returns the
    number of sources newly bound to the section (0 if it was already sufficient
    or nothing could be dug)."""
    bound = 0
    empty_digs = 0  # diagnostics only: a run of these means the search backend is starving
    async with sem:
        state = _State(
            serp_budget=max(ctx.b.deepen_max_new_sources, 3),
            fetch_budget=max(ctx.b.deepen_max_new_sources * 2, 6),
            deadline=ctx.deepen_deadline,
        )
        for _round in range(ctx.b.deepen_rounds):
            if time.monotonic() >= ctx.deepen_deadline:
                break
            judged = await _judge(ctx.question, outline, section, ctx.bank, ctx.b)
            if judged.sufficient or not judged.gaps:
                break
            added = 0
            for gap in judged.gaps:
                room = ctx.b.deepen_max_new_sources - bound
                if room <= 0 or time.monotonic() >= ctx.deepen_deadline:
                    break
                got = await _dig(ctx, state, section, gap, room)
                if got == 0:
                    empty_digs += 1
                added += got
                bound += got
            if added:
                progress.emit("deepen", section.heading, sources_read=len(ctx.bank.records))
            else:
                break  # a dry round means more rounds will not help
    if empty_digs:
        # The judge asked for evidence and the digs returned none. Usually a starved
        # or rate-limited search backend rather than a genuinely empty topic.
        _log.debug("deepen: %d empty dig(s) for section %r", empty_digs, section.heading)
    return bound


async def deepen(
    question: str,
    outline: Outline,
    bank: Bank,
    b: ResearchBudgets,
    *,
    source: str,
    kb_ids: list[str] | None,
    docs: list[dict] | None,
    seen: set[str],
    deadline: float,
) -> dict:
    """Deepen the hungriest evidence sections in place. Returns
    `{"sections": <deepened>, "new_sources": <bank growth>}`. Never raises for
    content reasons; the caller only invokes this when the budget enables it."""
    if b.deepen_rounds <= 0:
        return {"sections": 0, "new_sources": 0}

    deepen_deadline = min(deadline, time.monotonic() + b.deepen_seconds)
    if time.monotonic() >= deepen_deadline:
        return {"sections": 0, "new_sources": 0}

    # Placeholder sections (corpus-analysis, exec-summary) bypass the writer, so
    # they are exempt — exactly as the outline's unbound-note assignment exempts
    # them. Rank the rest hungriest-first and spend the budget on the top.
    candidates = [s for s in outline.sections if s.placeholder is None]
    candidates.sort(key=lambda s: _note_mass(bank, s))
    targets = candidates[: b.deepen_sections_hi]
    if not targets:
        return {"sections": 0, "new_sources": 0}

    concurrency = max(1, int(cfg_concurrency()))
    sem = asyncio.Semaphore(concurrency)
    notes_sem = asyncio.Semaphore(max(1, int(cfg_notes_concurrency())))
    ctx = _Ctx(
        question=question,
        bank=bank,
        b=b,
        source=source,
        kb_ids=kb_ids or [],
        seen=seen,
        docs_meta={d["doc_id"]: d for d in (docs or []) if d.get("doc_id")},
        notes_sem=notes_sem,
        deepen_deadline=deepen_deadline,
    )

    before = len(bank.records)
    results = await asyncio.gather(*[_deepen_section(ctx, sem, outline, s) for s in targets])
    deepened = sum(1 for r in results if r > 0)
    new_sources = len(bank.records) - before
    progress.emit("deepen", f"{deepened} sections deepened, {new_sources} new sources", sources_read=len(bank.records))
    return {"sections": deepened, "new_sources": new_sources}


def cfg_concurrency() -> int:
    from ..rag_ctx import cfg

    return cfg("research_deepen_concurrency", settings.research_deepen_concurrency)


def cfg_notes_concurrency() -> int:
    from ..rag_ctx import cfg

    return cfg("research_notes_concurrency", settings.research_notes_concurrency)
