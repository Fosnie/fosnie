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

"""Document-level metadata extraction. Today: the EFFECTIVE DATE — the date the
document itself bears (agreement/report/letter date, or period covered), not the
ingestion time. LLM-primary (our own served model, zero-egress) with cheap
fallbacks (file metadata, a page-1 regex). Best-effort: returns an ISO date
string (YYYY-MM-DD) or None — never raises."""

from __future__ import annotations

import logging
import re
from datetime import date

from . import llm

_log = logging.getLogger("pai.metadata")

_SYSTEM = (
    "You extract the EFFECTIVE DATE of a document — the date the document itself "
    "bears (e.g. the agreement date, report date, letter date, or the period it "
    "covers), NOT today's date and NOT a future date. Reply with that date as "
    "ISO-8601 YYYY-MM-DD. If only a month and year are shown, use the first day of "
    "the month. If there is no clear document date, reply exactly NONE. Output only "
    "the date or NONE — nothing else."
)

_ISO = re.compile(r"\b(\d{4})-(\d{1,2})-(\d{1,2})\b")
_MONTHS = {
    m.lower(): i + 1
    for i, m in enumerate(
        ["January", "February", "March", "April", "May", "June", "July",
         "August", "September", "October", "November", "December"]
    )
}
_NUMERIC = re.compile(r"\b(\d{4})[-/.](\d{1,2})[-/.](\d{1,2})\b")
_DMY = re.compile(r"\b(\d{1,2})\s+([A-Za-z]+)\s+(\d{4})\b")
_MDY = re.compile(r"\b([A-Za-z]+)\s+(\d{1,2}),?\s+(\d{4})\b")


def _norm(y, m, d) -> str | None:
    try:
        return date(int(y), int(m), int(d)).isoformat()
    except Exception:
        return None


def _regex_date(text: str) -> str | None:
    """A prominent date near the top of the document (cheap fallback)."""
    head = text[:2000]
    if m := _NUMERIC.search(head):
        return _norm(m.group(1), m.group(2), m.group(3))
    if m := _DMY.search(head):
        mon = _MONTHS.get(m.group(2).lower())
        if mon:
            return _norm(m.group(3), mon, m.group(1))
    if m := _MDY.search(head):
        mon = _MONTHS.get(m.group(1).lower())
        if mon:
            return _norm(m.group(3), mon, m.group(2))
    return None


def _metadata_date(mime: str | None, path: str) -> str | None:
    """The file's own creation date (PDF /CreationDate, DOCX core props)."""
    try:
        if (mime and "pdf" in mime) or path.lower().endswith(".pdf"):
            import pypdf

            meta = pypdf.PdfReader(path).metadata
            cd = getattr(meta, "creation_date", None) if meta else None
            if cd:
                return cd.date().isoformat()
        if (mime and "word" in mime) or path.lower().endswith(".docx"):
            import docx  # python-docx, if available

            created = docx.Document(path).core_properties.created
            if created:
                return created.date().isoformat()
    except Exception:
        pass
    return None


async def extract_effective_date(
    pages: list[tuple[int, str]], mime: str | None, path: str
) -> str | None:
    """LLM-primary effective-date extraction with cheap fallbacks. Returns an ISO
    date (YYYY-MM-DD) or None. Never raises."""
    head = "\n\n".join(t for _, t in (pages[:2] if pages else [])).strip()[:4000]
    if head:
        try:
            out = (await llm.complete(_SYSTEM, head, max_tokens=12)).strip()
            if m := _ISO.search(out):
                if iso := _norm(m.group(1), m.group(2), m.group(3)):
                    return iso
        except Exception as e:
            _log.warning("effective-date LLM extraction failed: %s", e)
    # Fallbacks: file metadata, then a page-1 regex.
    return _metadata_date(mime, path) or (_regex_date(head) if head else None)
