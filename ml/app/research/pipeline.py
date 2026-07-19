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

"""The Deep Research orchestrator (run lifecycle):
plan → collect → memory bank → notes → outline → section-by-section writing →
edit-only coherence → checks + one repair → dense reference renumbering +
deterministic References. Three sources:

  • web    — the existing web loop (shared pool), W# citations only;
  • files  — a corpus census (or retrieval sampling above the cap), D# only,
             ZERO egress (air-gap-safe);
  • hybrid — corpus first, then gap-targeted web rounds; both namespaces, kept
             visibly segregated ([D#] documents vs [W#] web, two reference
             sections).

Deterministic code owns control flow; the LLM proposes within each step (house
pattern). The pipeline NEVER raises for content reasons; the wall-clock deadline
triggers beast-mode early assembly — always deliver."""

import asyncio
import json
import logging
import re
import time

from .. import guided, llm, retrieve
from ..config import settings
from ..web import loop as web_loop
from ..web.loop import _Pool, _State
from . import census as census_mod
from . import checks as checks_mod
from . import cohere as cohere_mod
from . import corpus_analysis as corpus_mod
from . import deepen as deepen_mod
from . import notes as notes_mod
from . import outline as outline_mod
from . import padding as padding_mod
from . import progress
from . import templates as templates_mod
from . import verify as verify_mod
from . import writer as writer_mod
from .bank import Bank, DocSource, from_pool_sources
from .budgets import ResearchBudgets, budgets
from .outline import OutlineSection

_log = logging.getLogger("pai.research.pipeline")

_WID_RE = re.compile(r"\[([WD]\d+)\]")


async def _plan_subquestions(question: str, b: ResearchBudgets, *, for_web: bool = True) -> list[str]:
    """One LLM call → ≤ b.subqs sub-questions (the collect targets). Failure ⇒
    the question itself."""
    target = "WEB research" if for_web else "searching the user's document corpus"
    try:
        llm.set_stage("research.plan")
        llm.set_guided(guided.RESEARCH_SUBQS)
        out = await llm.complete(
            f"Decompose the research question into at most {b.subqs} sub-questions that "
            f"together cover it for {target}. Broad before narrow. Return ONLY a JSON "
            'array of strings: ["...", ...]. The first must be the question itself.',
            question,
            max_tokens=512,
        )
        start, end = out.find("["), out.rfind("]")
        arr = json.loads(out[start : end + 1]) if start >= 0 else []
        subqs = [str(s).strip() for s in arr if isinstance(s, str) and str(s).strip()]
        if question not in subqs:
            subqs = [question, *subqs]
        return list(dict.fromkeys(subqs))[: b.subqs] or [question]
    except Exception as e:  # noqa: BLE001
        _log.warning("research plan failed (single question): %s", e)
        return [question]


async def _gap_subquestions(question: str, bank: Bank, b: ResearchBudgets) -> list[str]:
    """Hybrid: one LLM call → web sub-questions targeting what the corpus does
    NOT already cover. Failure ⇒ the question itself."""
    digest_lines: list[str] = []
    for rec in bank.doc_records():
        claims = rec.note.claims if rec.note else []
        digest_lines.extend(f"- {c}" for c in claims[:4])
    digest = "\n".join(digest_lines)[:8_000] or "(the documents yielded little)"
    try:
        llm.set_stage("research.gap")
        llm.set_guided(guided.RESEARCH_SUBQS)
        out = await llm.complete(
            f"The user's own documents already cover the points below. List at most "
            f"{b.subqs} WEB search sub-questions that fill the GAPS — what the "
            "documents do NOT establish (recent developments, external context, "
            "independent corroboration). Return ONLY a JSON array of strings.",
            f"Research question: {question}\n\nAlready covered by the documents:\n{digest}",
            max_tokens=512,
        )
        start, end = out.find("["), out.rfind("]")
        arr = json.loads(out[start : end + 1]) if start >= 0 else []
        subqs = [str(s).strip() for s in arr if isinstance(s, str) and str(s).strip()]
        return list(dict.fromkeys(subqs))[: b.subqs] or [question]
    except Exception as e:  # noqa: BLE001
        _log.warning("gap analysis failed (single question): %s", e)
        return [question]


async def _title(question: str, report_lead: str) -> str:
    try:
        llm.set_stage("research.title")
        out = await llm.complete(
            "Write a concise report title (≤12 words) for this research. Plain text, "
            "no quotes, no trailing punctuation.",
            f"Question: {question}\n\nReport opening:\n{report_lead[:1500]}",
            max_tokens=256,  # headroom: a reasoning model spends tokens thinking before the title
        )
        t = out.strip().strip('"').strip()
        if 0 < len(t) <= 120:
            return t
    except Exception as e:  # noqa: BLE001
        _log.debug("title call failed: %s", e)
    return question[:120]


# --- Collection paths --------------------------------------------------------


async def _collect_web(subqs: list[str], bank_pool: _Pool, seen: set, b: ResearchBudgets, deadline: float) -> bool:
    """Run the web loop per sub-question into a shared pool (caps enforced by the
    shared _State). Mutates `bank_pool`/`seen` in place. Returns True if the
    collection budget ran out before every sub-question was covered (beast
    mode — deliver best-effort)."""
    state = _State(
        serp_budget=b.max_serp_queries,
        fetch_budget=b.max_fetches,
        deadline=min(time.monotonic() + b.collect_seconds, deadline),
    )
    wb = b.per_subq_budget()
    for sq in subqs:
        if state.expired() or time.monotonic() >= deadline:
            return True
        progress.emit("collect", sq, sources_read=len(bank_pool.sources))
        try:
            await web_loop.collect(sq, "any", wb, pool=bank_pool, seen=seen, state=state)
        except Exception as e:  # noqa: BLE001 — a failed sub-question is skipped
            _log.warning("collect failed for %r: %s", sq, e)
    return False


async def _sample_corpus(
    question: str, kb_ids: list[str], docs: list[dict], bank: Bank, b: ResearchBudgets, deadline: float
) -> None:
    """Above the census cap: agentic-retrieval sampling. Group returned
    citations by document into D# sources (keeping real page/chunk anchors).
    Notes are built later by `build_notes` from the retrieved chunks."""
    meta = {d["doc_id"]: d for d in docs}
    subqs = await _plan_subquestions(question, b, for_web=False)
    grouped: dict[str, dict] = {}
    for sq in subqs:
        if time.monotonic() >= deadline:
            break
        progress.emit("census", f"sampling: {sq}", sources_read=len(grouped))
        try:
            res = await retrieve.retrieve(sq, kb_ids)
        except Exception as e:  # noqa: BLE001
            _log.warning("sampling retrieve failed for %r: %s", sq, e)
            continue
        for c in res.get("citations", []):
            did = c.get("doc_id")
            if not did:
                continue
            did = str(did)
            g = grouped.setdefault(did, {"chunks": [], "anchor": c})
            q = c.get("quote_text")
            if q:
                g["chunks"].append(q)
    for did, g in grouped.items():
        m = meta.get(did, {})
        a = g["anchor"]
        bank.add_doc_source(
            DocSource(
                doc_id=did,
                kb_id=m.get("kb_id") or (kb_ids[0] if kb_ids else ""),
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


# --- Renumbering, references, citations (per-namespace) ----------------------


def _renumber(report: str, bank: Bank) -> tuple[str, list, list]:
    """Dense W1..Wn / D1..Dn renumbering, each namespace in first-appearance
    order. Returns (rewritten report, cited web records, cited doc records)."""
    order: list[str] = []
    for m in _WID_RE.finditer(report):
        if m.group(1) not in order:
            order.append(m.group(1))
    w_order = [s for s in order if s.startswith("W")]
    d_order = [s for s in order if s.startswith("D")]
    mapping: dict[str, str] = {}
    for i, old in enumerate(w_order):
        mapping[old] = f"W{i + 1}"
    for i, old in enumerate(d_order):
        mapping[old] = f"D{i + 1}"
    # Two-phase rewrite via unambiguous placeholders (avoids cascade collisions).
    out = _WID_RE.sub(lambda m: f"[«{mapping.get(m.group(1), m.group(1))}»]", report)
    out = out.replace("[«", "[").replace("»]", "]")
    web = [bank.get(o) for o in w_order]
    doc = [bank.get(o) for o in d_order]
    return out, [r for r in web if r is not None], [r for r in doc if r is not None]


def _references(web_recs: list, doc_recs: list) -> str:
    """Deterministic References. Both namespaces ⇒ subheadings; one namespace ⇒
    a flat list. Entries are blank-line separated so they survive DOCX
    conversion (one paragraph each) and render one-per-line in chat markdown."""
    if not web_recs and not doc_recs:
        return ""
    both = bool(web_recs) and bool(doc_recs)
    lines = ["## References", ""]

    def _doc_block() -> None:
        if both:
            lines.extend(["### Your documents", ""])
        for i, rec in enumerate(doc_recs):
            s = rec.source
            lines.append(f"[D{i + 1}] {s.filename} — {s.kb_name}.")
            lines.append("")

    def _web_block() -> None:
        if both:
            lines.extend(["### Web sources", ""])
        for i, rec in enumerate(web_recs):
            s = rec.source
            date = f", published {s.published_date}" if s.published_date else ""
            note = " (search snippet only)" if s.snippet_only else ""
            lines.append(f"[W{i + 1}] {s.title} — {s.domain}{date}{note}. {s.url}")
            lines.append("")

    if doc_recs:
        _doc_block()
    if web_recs:
        _web_block()
    return "\n".join(lines).rstrip()


def _web_citations_list(records: list) -> list[dict]:
    """Web-citation dicts in reference order (frame order = numbering)."""
    out = []
    for rec in records:
        s = rec.source
        quote = ""
        if rec.note and rec.note.quotes:
            quote = rec.note.quotes[0]
        elif s.chunks:
            quote = " ".join(s.chunks[0].split()[:25])
        out.append({
            "url": s.url,
            "title": s.title or None,
            "domain": s.domain,
            "published_date": s.published_date,
            "fetched_at": s.fetched_at,
            "quote_text": quote,
            "snippet_only": s.snippet_only,
        })
    return out


def _doc_citations_list(records: list) -> list[dict]:
    """Document-anchored citation dicts (the unified `citations` shape) in
    reference order. Whole-document census notes carry no page anchor (None);
    retrieval-sampled sources carry real page/chunk anchors."""
    out = []
    for rec in records:
        s = rec.source
        quote = ""
        if rec.note and rec.note.quotes:
            quote = rec.note.quotes[0]
        elif rec.note and rec.note.claims:
            quote = rec.note.claims[0]
        elif s.chunks:
            quote = " ".join(s.chunks[0].split()[:25])
        out.append({
            "doc_id": s.doc_id,
            "quote_text": quote,
            "page_number": s.page_number,
            "chunk_index": s.chunk_index,
            "clause_section_ref": s.clause_section_ref,
        })
    return out


def _coverage(source: str, result, total_docs, sampling: bool) -> str:
    """Honest coverage appendix (corpus/hybrid only)."""
    if source not in ("files", "hybrid"):
        return ""
    m = total_docs if total_docs is not None else (result.reviewed if result else 0)
    if sampling:
        body = (
            f"The corpus ({m} documents) exceeds the census limit, so targeted "
            "retrieval sampling was used rather than reading every document. "
            "Individual documents were not catalogued in full."
        )
        return f"## Coverage\n\n{body}"
    reviewed = result.reviewed if result else 0
    if reviewed >= m and not (result and result.unreviewed):
        return f"## Coverage\n\nAll {m} documents in scope were reviewed."
    names = [d.get("filename", d.get("doc_id", "?")) for d in (result.unreviewed if result else [])]
    shown = names[:30]
    more = max(0, m - reviewed - len(shown))
    lines = [
        "## Coverage",
        "",
        f"Reviewed {reviewed} of {m} documents before the research budget was "
        "exhausted. Not reviewed:",
        "",
    ]
    lines.extend(f"- {n}" for n in shown)
    if more:
        lines.append(f"- …and {more} more.")
    return "\n".join(lines)


def _assemble_draft(outline: outline_mod.Outline, bodies: list[str]) -> str:
    parts = []
    for i, (section, body) in enumerate(zip(outline.sections, bodies)):
        parts.append(f"## {i + 1}. {section.heading}\n{body}")
    return "\n\n".join(parts)


def _empty_report(question: str, source: str) -> dict:
    if source == "files":
        msg = (
            "No readable documents could be catalogued for this research (the "
            "selected libraries may be empty or still indexing). Nothing was "
            "synthesised — check the scope and try again."
        )
    elif source == "hybrid":
        msg = (
            "Neither your documents nor the web yielded usable evidence for this "
            "research. Nothing was synthesised — check the scope, or try again."
        )
    else:
        msg = (
            "No usable web sources could be gathered for this research (search "
            "engines may be unavailable). Nothing was synthesised — try again, "
            "or rephrase the question."
        )
    return {"title": question[:120], "report_md": msg, "citations": [], "doc_citations": []}


async def run(
    question: str,
    template_id: str = "exploration",
    source: str = "web",
    kb_ids: list[str] | None = None,
    docs: list[dict] | None = None,
    total_docs: int | None = None,
    refinements: list[str] | None = None,
    verify: bool = False,
    template_spec: dict | None = None,
) -> dict:
    """Returns {title, report_md, citations, doc_citations, verification}. Never
    raises for content reasons. `verify` (gated by Rust on
    features.groundedness + research.verify) runs an in-pipeline citation
    verification + ground-or-cut pass; OFF ⇒ output is byte-identical to the
    unverified pass and `verification` is None. `template_spec`, when present, is
    a user-defined template the backend resolved and sent inline; it takes
    priority over `template_id`, which is used only for the built-ins."""
    source = (source or "web").lower()
    kb_ids = kb_ids or []
    docs = docs or []
    refinements = refinements or []
    template = (
        templates_mod.from_spec(template_spec)
        if template_spec
        else templates_mod.get(template_id)
    )

    # Scope refinements (from the triage chips) steer planning + writing.
    plan_question = question
    if refinements:
        plan_question = f"{question}\n\nScope refinements: {'; '.join(refinements)}"

    # Budgets from the runtime context window (adaptive scaling). Runtime
    # admin knobs (research.max_minutes / research.census_cap) arrive as rag_ctx
    # overrides; None ⇒ ML env defaults.
    from ..main import _resolve_model
    from ..rag_ctx import cfg

    model_id, max_model_len = await _resolve_model()
    run_minutes = cfg("research_max_minutes", settings.research_max_minutes)
    census_cap = cfg("research_census_cap", settings.research_census_cap)
    b = budgets(max_model_len, source, max_minutes=run_minutes)
    deadline = time.monotonic() + run_minutes * 60.0
    beast = False
    census_result = None
    sampling = False
    # Shared with the primary web collect (web/hybrid) and, later, the deepening
    # stage: a URL fetched once is never re-fetched. Empty for a files-only run.
    seen: set[str] = set()

    bank = Bank()

    # 1. Collect — corpus first (files/hybrid), then web (web/hybrid).
    if source in ("files", "hybrid"):
        above_cap = total_docs is not None and total_docs > census_cap
        census_deadline = time.monotonic() + b.census_seconds
        if above_cap:
            progress.emit("census", "sampling the corpus", sources_read=0)
            await _sample_corpus(question, kb_ids, docs, bank, b, census_deadline)
            sampling = True
        else:
            progress.emit("census", f"reading {len(docs)} documents", sources_read=0)
            census_result = await census_mod.run_census(docs, bank, b, census_deadline, model_id)

    if source in ("web", "hybrid"):
        progress.emit("plan", "planning the research")
        if source == "hybrid" and bank.doc_records():
            subqs = await _gap_subquestions(question, bank, b)
        else:
            subqs = await _plan_subquestions(plan_question, b)
        progress.emit("plan", f"{len(subqs)} web question{'s' if len(subqs) != 1 else ''}")
        pool = _Pool()
        if await _collect_web(subqs, pool, seen, b, deadline):
            beast = True
        progress.emit("collect", f"{len(pool.sources)} web sources", sources_read=len(pool.sources))
        # Web sources (capped) join the bank, after any documents already in it.
        web_bank = await from_pool_sources(question, pool.sources, b.max_sources)
        for rec in web_bank.records:
            bank.add_source(rec.source)

    if not bank.records:
        return _empty_report(question, source)

    # 2. Notes — every record lacking one (web sources + sampled docs). Census
    #    documents already carry their note.
    progress.emit("notes", f"reading {len(bank.records)} sources", sources_read=0)
    await notes_mod.build_notes(question, bank, b)

    # 3. Outline.
    progress.emit("outline", "building the outline")
    outline = await outline_mod.build(question, template, bank, b)

    # 3b. Consensus/contradictions/gaps (corpus modes). Deterministic body from
    #     the notes; inserted as a placeholder section (writer-bypassing). If the
    #     template skeleton already has the heading (literature), fill it instead.
    if source in ("files", "hybrid"):
        analysis_body = await corpus_mod.analyse(bank)
        if analysis_body:
            target = corpus_mod.SECTION_HEADING.strip().lower()
            existing = next(
                (s for s in outline.sections if s.heading.strip().lower() == target), None
            )
            if existing is not None:
                existing.placeholder = analysis_body
            else:
                pos = max(0, len(outline.sections) - 1)
                outline.sections.insert(
                    pos,
                    OutlineSection(
                        heading=corpus_mod.SECTION_HEADING,
                        brief="Where the documents agree, conflict, and fall silent.",
                        note_ids=[],
                        placeholder=analysis_body,
                    ),
                )
    total = len(outline.sections)

    # Publish the full, ordered section roadmap now that the outline is final
    # (post corpus-analysis insert) — the client renders it and ticks each section
    # off as the `write` events arrive. Emitted once; `write` events carry the
    # running `sections_done`.
    progress.emit(
        "outline",
        f"{total} sections",
        sections=[s.heading for s in outline.sections],
        sections_total=total,
    )

    # 3c. Deepen the hungriest sections before writing: judge each section's
    #     bound evidence, dig for the gaps, rebind. Additive and fail-open — on any
    #     failure the run continues and each section keeps whatever bindings it has
    #     (deepening only ever appends, so a partial pass is safe to keep). Disabled
    #     by the budget (small context) or the admin switch ⇒ skipped entirely, so
    #     the event sequence and result stay identical to the single-pass path.
    #     The stage bounds each of its own provider calls; this outer timeout is the
    #     backstop that keeps a pathological backend from eating the writer's budget.
    deepen_on = cfg("research_deepen_enabled", settings.research_deepen_enabled)
    if deepen_on and b.deepen_rounds > 0 and time.monotonic() < deadline:
        try:
            await asyncio.wait_for(
                deepen_mod.deepen(
                    question,
                    outline,
                    bank,
                    b,
                    source=source,
                    kb_ids=kb_ids,
                    docs=docs,
                    seen=seen,
                    deadline=deadline,
                ),
                timeout=b.deepen_seconds + deepen_mod.STAGE_GRACE_SECONDS,
            )
        except TimeoutError:
            _log.warning(
                "deepen stage exceeded its time budget; continuing with the bindings it had made"
            )
        except Exception as e:  # noqa: BLE001 — deepening never fails the run
            _log.warning("deepen stage failed (sections keep the bindings they had): %s", e)

    # 4. Write, section by section.
    bodies: list[str] = []
    rolling = ""
    register: list[str] = []
    for k, section in enumerate(outline.sections):
        progress.emit("write", section.heading, sections_done=k, sections_total=total)
        # Stream the pipeline-owned heading, then the section body, so the report
        # types into the chat as it is written. The streamed text is a live draft;
        # the terminal `report_md` (post-coherence/renumber) is authoritative and
        # the client reconciles to it on finalise.
        await progress.emit_token(f"## {k + 1}. {section.heading}\n\n")
        if section.placeholder is not None:
            await progress.emit_token(f"{section.placeholder}\n\n")
            bodies.append(section.placeholder)
            continue
        if time.monotonic() >= deadline:
            beast = True
        body = await writer_mod.write_section(
            k, outline, bank, rolling, register, template.writing_instructions, b, stream=True
        )
        await progress.emit_token("\n\n")
        bodies.append(body)
        if not beast:
            rolling = await writer_mod.update_rolling(rolling, section.heading, body)
        writer_mod.extend_register(register, section.heading, body)
    draft = _assemble_draft(outline, bodies)

    # 5. Cohere (skipped under beast-mode deadline pressure).
    if time.monotonic() < deadline:
        progress.emit("cohere", "global coherence pass")
        draft = await cohere_mod.run(draft, question, b)
    else:
        beast = True
        draft = draft.replace(templates_mod.EXEC_SUMMARY_PLACEHOLDER, "").strip()

    # 6. Checks + one repair round.
    progress.emit("check", "structure checks")
    draft, stripped = checks_mod.strip_unresolved(draft, bank)
    if stripped:
        _log.info("stripped %d unresolved citation markers", len(stripped))
    violations = checks_mod.run_checks(draft, outline, bank, b)
    structural = [v for v in violations if v.kind in ("word_band", "no_citations") and v.section]
    if structural and time.monotonic() < deadline:
        v = structural[0]
        idx = next(
            (i for i, s in enumerate(outline.sections) if f"{i + 1}. {s.heading}" == v.section), None
        )
        if idx is not None and outline.sections[idx].placeholder is None:
            progress.emit("check", f"rewriting section {idx + 1}")
            new_body = await writer_mod.write_section(
                idx, outline, bank, rolling, register, template.writing_instructions, b
            )
            head = f"## {idx + 1}. {outline.sections[idx].heading}"
            parts = cohere_mod.split_sections(draft)
            for i, part in enumerate(parts):
                if part.splitlines()[0] == head:
                    parts[i] = f"{head}\n{new_body}"
                    break
            draft = "\n\n".join(parts)
            draft, _ = checks_mod.strip_unresolved(draft, bank)
            violations = checks_mod.run_checks(draft, outline, bank, b)

    # 6b. Verify + ground-or-cut, then padding detection (gated, best-effort,
    # deadline-aware). OFF / disabled / past-deadline ⇒ draft unchanged.
    verification = None
    pad_violations: list = []
    if verify and not beast and time.monotonic() < deadline:
        progress.emit("verify", "checking citations")
        draft, verification = await verify_mod.verify_and_prune(draft, outline, bank, b, deadline)
        pad_violations = await padding_mod.detect_padding(draft, outline, bank, b, deadline)

    if violations or pad_violations:
        draft += "\n\n*Some sections did not meet structural targets; the evidence is delivered as gathered.*"
    if beast:
        draft += "\n\n*The research budget was exhausted; this report is best-effort from the evidence gathered.*"

    # 7. Title, per-namespace renumbering, references, coverage, citations.
    progress.emit("deliver", "finalising the report")
    report, web_cited, doc_cited = _renumber(draft, bank)
    title = await _title(question, report)
    refs = _references(web_cited, doc_cited)
    if refs:
        report = f"{report}\n\n{refs}"
    coverage = _coverage(source, census_result, total_docs, sampling)
    if coverage:
        report = f"{report}\n\n{coverage}"

    # Resolve verification span offsets against the FINAL delivered report (not
    # the pre-renumber/pre-references draft the prune ran on).
    if verification is not None:
        spans = []
        for f in verification.pop("flagged", []):
            idx = report.find(f["text"])
            if idx >= 0:
                spans.append({
                    "start": idx, "end": idx + len(f["text"]),
                    "text": f["text"], "label": f["label"], "score": f["score"],
                })
        verification["spans"] = spans

    return {
        "title": title,
        "report_md": report,
        "citations": _web_citations_list(web_cited),
        "doc_citations": _doc_citations_list(doc_cited),
        "verification": verification,
    }
