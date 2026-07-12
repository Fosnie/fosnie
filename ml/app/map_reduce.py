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

"""Whole-document map-reduce. For a document too large to
stuff into the context window, read EVERY section (completeness is critical for
legal — a missed clause is a missed problem) and accumulate the relevant findings
in a STRUCTURED list (section_index → result), not a lossy running summary. The
section maps run under bounded concurrency so a large document cannot flood the
LLM. The reduce step concatenates the non-empty findings, tagged by section, so
the caller (and any downstream citation) keeps the section anchor."""

import asyncio

from . import chunker, llm
from .config import settings

# A section that contributes nothing answers with this sentinel — dropped at
# reduce. Matched leniently (models add punctuation/casing: "NONE", "none.").
_NONE = "NONE"


def _is_empty(result: str) -> bool:
    r = result.strip().rstrip(".").strip().upper()
    return r == "" or r == _NONE


async def _map_section(sem: asyncio.Semaphore, index: int, section: str, prompt: str) -> dict:
    async with sem:
        out = await llm.complete(
            "You are reading ONE section of a larger document. Extract EVERYTHING in "
            "this section relevant to the task, verbatim where it matters (names, "
            "figures, dates, clause text). Be exhaustive — do not summarise away "
            f"detail. If the section has nothing relevant, reply exactly {_NONE}.",
            f"Task: {prompt}\n\nSection:\n{section}",
            max_tokens=512,
        )
    return {"section_index": index, "section_ref": f"section {index + 1}", "result": out.strip()}


async def map_reduce(text: str, prompt: str) -> dict:
    """Map every section against `prompt`, then reduce to a structured digest.
    Returns `{mode, sections, text}` where `sections` is the structured
    accumulation and `text` is the reduced, section-tagged digest ready to inject."""
    sections = chunker.chunk_text(text, size=settings.map_window_chars)
    if not sections:
        return {"mode": "map_reduce", "sections": [], "text": ""}

    sem = asyncio.Semaphore(max(1, settings.map_concurrency))
    mapped = await asyncio.gather(
        *[_map_section(sem, i, s, prompt) for i, s in enumerate(sections)]
    )

    # Structured accumulation: keep only sections that contributed, in order.
    kept = [m for m in mapped if not _is_empty(m["result"])]
    digest = "\n\n".join(f"[{m['section_ref']}]\n{m['result']}" for m in kept)
    return {"mode": "map_reduce", "sections": kept, "text": digest}
