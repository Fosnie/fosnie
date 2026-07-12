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

"""Corpus census: read every document in
scope ONCE and emit a per-document structured note (type, themes, claims,
entities, dates, open questions, quotable passages). Notes are
QUESTION-INDEPENDENT — they describe the document, so they cache in Qdrant
(`pai_doc_notes`) and are reused across runs. Synthesis then works from the
notes, never from raw retrieval samples (census beats retrieval on bounded
corpora).

Three honest behaviours, all deterministic:
  • cache hit  → the stored note is used as-is (no extraction, no LLM);
  • stuff-whole-corpus fast path → if the *uncached* documents together fit the
    stuff fraction, the writer reads their full text directly (no note LLM call);
  • per-document note → otherwise one note call per doc (the whole doc if it
    fits the census window, else a map-reduce digest first).

The wall-clock deadline is honoured: documents not reached are returned as
`unreviewed` for the coverage appendix. Every per-document failure degrades to a
stub note from the filename + lead text — the census NEVER raises."""

import asyncio
import json
import logging
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone

from .. import extract, guided, llm, map_reduce
from ..config import settings
from . import progress
from .bank import Bank, DocSource, Note
from .budgets import ResearchBudgets, est_tokens

_log = logging.getLogger("pai.research.census")

SCHEMA_VERSION = 1

_SYSTEM = (
    "You are cataloguing ONE document for a research library. Describe the "
    "document itself (not any particular question). Return ONLY JSON: "
    '{"doc_type": "...", "themes": ["..."], "claims": ["specific factual claim '
    'with figures/dates/names", ...], "entities": ["..."], "dates": ["..."], '
    '"open_questions": ["..."], "quotes": ["short verbatim passage worth citing", '
    '...]}. Keep every list concise; omit what is absent. No commentary.'
)

_MAP_PROMPT = (
    "Extract everything that characterises this document: its purpose and type, "
    "key claims and findings (with figures, dates and names), the entities "
    "involved, important dates, and any quotable passages."
)


@dataclass
class CensusResult:
    reviewed: int = 0
    unreviewed: list[dict] = field(default_factory=list)  # doc dicts not read
    stuffed_corpus: bool = False  # the fast path was taken


def _note_from_struct(n: dict) -> Note:
    """Flatten a structured catalogue note into the claims/quotes the writer
    sees. doc_type/themes/entities/dates/open-questions ride as claim lines so
    they survive into the writer's evidence block."""
    claims: list[str] = []
    if n.get("doc_type"):
        claims.append(f"Document type: {n['doc_type']}")
    if n.get("themes"):
        claims.append("Themes: " + "; ".join(str(t) for t in n["themes"]))
    claims += [str(c).strip() for c in n.get("claims", []) if str(c).strip()]
    if n.get("entities"):
        claims.append("Entities: " + "; ".join(str(e) for e in n["entities"]))
    if n.get("dates"):
        claims.append("Dates: " + "; ".join(str(d) for d in n["dates"]))
    if n.get("open_questions"):
        claims.append("Open questions: " + "; ".join(str(q) for q in n["open_questions"]))
    quotes = [str(q).strip() for q in n.get("quotes", []) if str(q).strip()]
    return Note(claims=[c for c in claims if c], quotes=quotes)


def _stub_note(filename: str, text: str) -> Note:
    lead = " ".join(text.split()[:60])
    claim = f"{filename}: {lead}".strip(": ")
    return Note(claims=[claim] if claim else [filename], quotes=[])


def _valid_cached(payload: dict | None) -> bool:
    return bool(payload) and payload.get("schema_version") == SCHEMA_VERSION


async def _extract_text(d: dict) -> str:
    """Whole-document text off the event loop (extraction is sync/CPU-bound)."""
    return await asyncio.to_thread(extract.extract, d["path"], d.get("mime"))


async def _build_struct_note(text: str, b: ResearchBudgets) -> dict:
    """One catalogue note for a document. If the document fits the census
    window it is read whole; otherwise a map-reduce digest is read instead."""
    budget_chars = b.census_input_tokens * 4
    if est_tokens(text) > b.census_input_tokens:
        mr = await map_reduce.map_reduce(text, _MAP_PROMPT)
        body = mr.get("text", "")[:budget_chars]
    else:
        body = text[:budget_chars]
    llm.set_stage("research.census")
    llm.set_guided(guided.RESEARCH_CENSUS)
    out = await llm.complete(_SYSTEM, body, max_tokens=b.census_note_tokens)
    start, end = out.find("{"), out.rfind("}")
    obj = json.loads(out[start : end + 1]) if start >= 0 else {}
    return obj if isinstance(obj, dict) else {}


async def run_census(
    docs: list[dict],
    bank: Bank,
    b: ResearchBudgets,
    deadline: float,
    model_id: str,
) -> CensusResult:
    """Read `docs` (already capped by the caller) into the bank as D# sources.

    `docs[i]` = {doc_id, kb_id, kb_name, path, mime, filename}. `deadline` is a
    `time.monotonic()` value; documents not reached before it become
    `unreviewed`. Notes are cached in Qdrant; fresh ones are upserted
    best-effort."""
    result = CensusResult()
    if not docs:
        return result

    cached = await _safe_get_notes([d["doc_id"] for d in docs])
    misses = [d for d in docs if not _valid_cached(cached.get(d["doc_id"]))]

    # --- Stuff-whole-corpus probe (uncached docs only) -----------------------
    # Extract the misses up front, summing tokens; if they fit the stuff
    # fraction the writer reads them whole (zero note LLM calls). Stop early the
    # moment the budget is exceeded — texts already pulled are reused below, so
    # nothing is extracted twice and RAM stays bounded by the stuff budget.
    stuff_budget = int(b.max_model_len * settings.stuff_fraction)
    pre_text: dict[str, str] = {}
    fast_path = bool(misses)
    running = 0
    if misses:
        for d in misses:
            if time.monotonic() >= deadline:
                fast_path = False
                break
            try:
                txt = await _extract_text(d)
            except Exception as e:  # noqa: BLE001 — handled per-doc later
                _log.debug("census extract failed for %s: %s", d.get("filename"), e)
                pre_text[d["doc_id"]] = ""
                continue
            pre_text[d["doc_id"]] = txt
            running += est_tokens(txt)
            if running > stuff_budget:
                fast_path = False
                break

    from ..rag_ctx import cfg

    fresh: list[dict] = []
    fresh_lock = asyncio.Lock()
    sem = asyncio.Semaphore(max(1, cfg("research_notes_concurrency", settings.research_notes_concurrency)))
    done = 0
    total = len(docs)
    reviewed_ids: set[str] = set()

    def _register(d: dict) -> object:
        sid = bank.add_doc_source(
            DocSource(
                doc_id=d["doc_id"],
                kb_id=d["kb_id"],
                kb_name=d.get("kb_name", ""),
                filename=d.get("filename", d["doc_id"]),
                mime=d.get("mime"),
                path=d.get("path", ""),
            )
        )
        return bank.get(sid)

    async def _worker(d: dict) -> None:
        nonlocal done
        async with sem:
            rec = _register(d)
            payload = cached.get(d["doc_id"])
            if _valid_cached(payload):
                rec.note = _note_from_struct(payload.get("note", {}))
            elif fast_path:
                text = pre_text.get(d["doc_id"], "")
                rec.note = Note(claims=[], quotes=[], full_text=text) if text else _stub_note(
                    d.get("filename", d["doc_id"]), ""
                )
            else:
                text = pre_text.get(d["doc_id"])
                if text is None:
                    try:
                        text = await _extract_text(d)
                    except Exception as e:  # noqa: BLE001
                        _log.debug("census extract failed for %s: %s", d.get("filename"), e)
                        text = ""
                if not text:
                    rec.note = _stub_note(d.get("filename", d["doc_id"]), "")
                else:
                    try:
                        struct = await _build_struct_note(text, b)
                        rec.note = _note_from_struct(struct) if struct else _stub_note(
                            d.get("filename", d["doc_id"]), text
                        )
                        if struct:
                            async with fresh_lock:
                                fresh.append(
                                    {
                                        "doc_id": d["doc_id"],
                                        "knowledge_base_id": d["kb_id"],
                                        "schema_version": SCHEMA_VERSION,
                                        "model_id": model_id,
                                        "created_at": datetime.now(timezone.utc).isoformat(),
                                        "note": struct,
                                    }
                                )
                    except Exception as e:  # noqa: BLE001 — stub beats a dead doc
                        _log.debug("census note failed for %s (stub): %s", d.get("filename"), e)
                        rec.note = _stub_note(d.get("filename", d["doc_id"]), text)
            reviewed_ids.add(d["doc_id"])
            done += 1
            if done % 3 == 0 or done == total:
                progress.emit(
                    "census", d.get("filename", ""), sources_read=done
                )

    # Dispatch in order, but stop scheduling once the deadline passes so the run
    # always reaches synthesis. Cache hits are cheap, so we let those through.
    pending: list[dict] = []
    for d in docs:
        if time.monotonic() >= deadline and not _valid_cached(cached.get(d["doc_id"])):
            result.unreviewed.append(d)
            continue
        pending.append(d)
    await asyncio.gather(*[_worker(d) for d in pending])

    result.reviewed = len(reviewed_ids)
    result.stuffed_corpus = fast_path and not any(
        _valid_cached(cached.get(d["doc_id"])) for d in docs
    )
    if fresh:
        await _safe_upsert_notes(fresh)
    return result


async def _safe_get_notes(doc_ids: list[str]) -> dict[str, dict]:
    from .. import qdrant_store

    try:
        return await qdrant_store.get_notes(doc_ids)
    except Exception as e:  # noqa: BLE001 — a cache miss is harmless
        _log.warning("notes cache read failed (treating as miss): %s", e)
        return {}


async def _safe_upsert_notes(notes: list[dict]) -> None:
    from .. import qdrant_store

    try:
        await qdrant_store.upsert_notes(notes)
    except Exception as e:  # noqa: BLE001 — a failed cache write must not sink the run
        _log.warning("notes cache write failed (ignored): %s", e)
