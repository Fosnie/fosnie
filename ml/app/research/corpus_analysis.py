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

"""Consensus / contradictions / gaps across the corpus: the differentiator nothing mainstream offers explicitly. One LLM call
over the per-document claim digest → structured JSON; the section body is then
rendered DETERMINISTICALLY from that JSON (so the writer can't smuggle in new
claims, and every [D#] marker is validated against the bank). Failure ⇒ the
section is omitted, never an error."""

import json
import logging

from .. import llm
from .bank import Bank

_log = logging.getLogger("pai.research.corpus_analysis")

SECTION_HEADING = "Consensus, contradictions and gaps"

_SYSTEM = (
    "You are analysing a corpus of the user's own documents. From the per-document "
    "claim notes, identify: where sources AGREE, where they CONTRADICT each other, "
    "and what is MISSING (questions the corpus does not answer). Cite documents by "
    "their [D#] IDs exactly as shown. Return ONLY JSON: "
    '{"consensus": [{"point": "...", "sids": ["D1","D3"]}], '
    '"contradictions": [{"point": "...", "sids": ["D2","D4"]}], '
    '"gaps": ["..."]}. Omit a list if there is nothing to report. No commentary.'
)


def _digest(bank: Bank, budget_chars: int = 16_000) -> str:
    lines: list[str] = []
    for rec in bank.doc_records():
        claims = rec.note.claims if rec.note else []
        lines.append(rec.meta_line())
        lines.extend(f"  - {c}" for c in claims[:6])
    return "\n".join(lines)[:budget_chars]


def _render(obj: dict, known: set[str]) -> str:
    """JSON → markdown body, keeping only [D#] markers that resolve."""

    def _sids(raw) -> str:
        ids = [s for s in raw if isinstance(s, str) and s in known]
        return " ".join(f"[{s}]" for s in ids)

    blocks: list[str] = []
    consensus = [c for c in obj.get("consensus", []) if isinstance(c, dict) and c.get("point")]
    contradictions = [
        c for c in obj.get("contradictions", []) if isinstance(c, dict) and c.get("point")
    ]
    gaps = [str(g).strip() for g in obj.get("gaps", []) if str(g).strip()]

    if consensus:
        blocks.append("**Where the documents agree**")
        for c in consensus:
            tail = _sids(c.get("sids", []))
            blocks.append(f"- {str(c['point']).strip()}{(' ' + tail) if tail else ''}")
    if contradictions:
        blocks.append("**Where they disagree**")
        for c in contradictions:
            tail = _sids(c.get("sids", []))
            blocks.append(f"- {str(c['point']).strip()}{(' ' + tail) if tail else ''}")
    if gaps:
        blocks.append("**What the corpus does not settle**")
        blocks.extend(f"- {g}" for g in gaps)
    return "\n".join(blocks).strip()


async def analyse(bank: Bank) -> str | None:
    """Return the rendered section body, or None if there is nothing to say /
    the call failed. Caller inserts it as a placeholder (writer-bypassing)
    section."""
    if not bank.doc_records():
        return None
    known = set(bank.sids())
    try:
        llm.set_stage("research.corpus_analysis")
        out = await llm.complete(_SYSTEM, _digest(bank), max_tokens=900)
        start, end = out.find("{"), out.rfind("}")
        obj = json.loads(out[start : end + 1]) if start >= 0 else {}
        body = _render(obj, known) if isinstance(obj, dict) else ""
        return body or None
    except Exception as e:  # noqa: BLE001 — omit the section, never raise
        _log.warning("corpus analysis failed (section omitted): %s", e)
        return None
