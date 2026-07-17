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

"""Ground-or-cut repair. For each
flagged claim from a verify-draft run: re-retrieve a supporting node, regenerate
the claim constrained to that node, and **re-verify the regenerated text against
that NEW citation** (never trust a freshly-generated citation). A claim
that cannot be grounded is **cut, not rewritten** ("if it can't be cited, it can't
be stated"). The backend turns each result into a tracked-change proposal.

Reuses the existing primitives: `retrieve._search_one` (re-retrieval),
`llm.complete` (regeneration), `verify.verify_claims` (re-verification)."""

import asyncio
import logging

from . import llm
from . import retrieve as retrieve_mod
from . import verify as verify_mod
from .config import settings

_log = logging.getLogger("pai.ml.repair")

_SYSTEM = (
    "You repair a single STATEMENT so that it is fully supported by the SOURCE.\n"
    "Rules:\n"
    "1. Change ONLY what the SOURCE does not support; keep the rest of the wording.\n"
    "2. Add nothing the SOURCE does not state; do not speculate.\n"
    "3. If the SOURCE cannot support the statement at all, reply with exactly CUT.\n"
    "Return ONLY the rewritten statement (or the single word CUT). No preamble, no quotes."
)


def _node_text(hits: list[dict], k: int = 1) -> str:
    return "\n\n".join(h["payload"]["chunk_text"] for h in hits[:k])


def _is_cut(reply: str) -> bool:
    return reply.strip().strip(" .\"'").upper() == "CUT"


async def _repair_one(claim: dict, kb_ids: list[str], sem: asyncio.Semaphore) -> dict:
    span = (claim.get("source_text") or claim.get("text") or "").strip()
    verdict = claim.get("verdict", "not_mentioned")
    out = {
        "source_text": claim.get("source_text"),
        "claim_text": claim.get("text"),
        "action": "kept",
        "replacement": None,
        "evidence": "",
        "citation_ref": None,
        "reverify_verdict": verdict,
        "reverify_score": claim.get("score") or 0.0,
    }
    # A flagged claim we cannot even attempt to ground is cut.
    if not span or not kb_ids:
        out["action"] = "cut"
        return out

    # 1) Re-retrieve the best supporting node for the claim (one attempt).
    hits = await retrieve_mod._search_one(claim.get("text", span), kb_ids, sem)
    node = _node_text(hits, k=1)
    if not node:
        out["action"] = "cut"
        return out

    # 2) Regenerate the span constrained to that node.
    try:
        rewritten = (await llm.complete(_SYSTEM, f"STATEMENT: {span}\n\nSOURCE: {node}", max_tokens=400)).strip()
    except Exception as e:  # degrade to a cut rather than abort the batch
        _log.warning("repair regenerate failed: %s", e)
        out["action"] = "cut"
        out["evidence"] = node[:600]
        return out
    if not rewritten or _is_cut(rewritten):
        out["action"] = "cut"
        out["evidence"] = node[:600]
        return out

    # 3) Re-verify the regenerated text against the NEW citation. Only a
    #    `supported` verdict promotes it to a proposal; anything else → cut.
    rv = await verify_mod.verify_claims([{"text": rewritten, "evidence": node}])
    rverdict = rv[0]["verdict"] if rv else "not_mentioned"
    rscore = rv[0]["score"] if rv else 0.0
    out["evidence"] = node[:600]
    out["reverify_verdict"] = rverdict
    out["reverify_score"] = rscore
    if rverdict != "supported":
        out["action"] = "cut"
        return out

    payload = hits[0]["payload"] if hits else {}
    out["citation_ref"] = payload.get("clause_section_ref") or payload.get("doc_id")
    if rewritten.strip() == span:
        out["action"] = "kept"  # source actually supports it; nothing to change
    else:
        out["action"] = "regenerated"
        out["replacement"] = rewritten
    return out


async def repair_claims(claims: list[dict], kb_ids: list[str], strictness: str = "strict") -> list[dict]:
    """Repair each flagged claim concurrently; returns one result per input claim.
    `strictness` is accepted for parity — repair always requires a `supported`
    re-verification before proposing a rewrite, regardless of the dial."""
    sem = asyncio.Semaphore(max(1, settings.retrieve_concurrency))
    results = await asyncio.gather(*[_repair_one(c, kb_ids, sem) for c in claims])
    return list(results)
