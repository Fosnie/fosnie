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

"""Evidence → structured notes, once (step 1):
claim bullets + verbatim quotes/figures per source. The notes are the ONLY
thing the writer ever sees; quotes are kept verbatim so numbers and names
survive compression. Every failure degrades to a stub note — never raises."""

import asyncio
import json
import logging

from .. import guided, llm
from ..config import settings
from . import progress
from .bank import Bank, Note
from .budgets import ResearchBudgets

_log = logging.getLogger("pai.research.notes")

_SYSTEM = (
    "You are building research notes from ONE web source. Extract everything "
    "relevant to the research question: factual claims (one bullet each, specific, "
    "with figures/dates/names) and short verbatim quotes worth citing. Return ONLY "
    'JSON: {"claims": ["...", ...], "quotes": ["...", ...]}. No commentary.'
)


def _stub(rec_source) -> Note:
    lead = rec_source.chunks[0] if rec_source.chunks else ""
    claim = f"{rec_source.title}: {' '.join(lead.split()[:40])}".strip(": ")
    return Note(claims=[claim] if claim else [], quotes=[])


async def _note_one(sem: asyncio.Semaphore, rec, question: str, b: ResearchBudgets) -> None:
    async with sem:
        src = rec.source
        body = "\n\n".join(src.chunks)
        body = body[: b.note_input_tokens * 4]  # chars/4 budgeting heuristic
        meta = f"{src.title} — {src.domain}" + (f", published {src.published_date}" if src.published_date else "")
        try:
            llm.set_stage("research.notes")
            llm.set_guided(guided.RESEARCH_NOTES)
            out = await llm.complete(
                _SYSTEM,
                f"Research question: {question}\n\nSource [{rec.sid}] {meta}\n\n{body}",
                max_tokens=b.note_tokens,
            )
            start, end = out.find("{"), out.rfind("}")
            obj = json.loads(out[start : end + 1]) if start >= 0 else {}
            claims = [str(c).strip() for c in obj.get("claims", []) if str(c).strip()]
            quotes = [str(q).strip() for q in obj.get("quotes", []) if str(q).strip()]
            rec.note = Note(claims=claims, quotes=quotes) if (claims or quotes) else _stub(src)
        except Exception as e:  # noqa: BLE001 — a stub note beats a dead source
            _log.debug("notes call failed for %s (stub): %s", rec.sid, e)
            rec.note = _stub(src)


async def build_notes(question: str, bank: Bank, b: ResearchBudgets) -> None:
    """Fill `rec.note` for every record that lacks one (web sources). Corpus
    documents are noted by the census and already carry a note, so they are
    skipped here. Bounded concurrency, with sources-read progress."""
    from ..rag_ctx import cfg

    todo = [rec for rec in bank.records if rec.note is None]
    sem = asyncio.Semaphore(max(1, cfg("research_notes_concurrency", settings.research_notes_concurrency)))
    total = len(todo)
    done = 0

    async def one(rec):
        nonlocal done
        await _note_one(sem, rec, question, b)
        done += 1
        if done % 5 == 0 or done == total:
            progress.emit("notes", f"{done}/{total} sources read", sources_read=done)

    await asyncio.gather(*[one(rec) for rec in todo])
