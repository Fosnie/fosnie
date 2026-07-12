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

"""In-pipeline citation verification + ground-or-cut. Marker resolution
already guarantees [W#]/[D#] markers only *resolve* against the bank;
this checks each claim is actually *supported* by the raw evidence the bank
holds (no re-retrieval — the writer already saw it). Reuses the groundedness
verifier (`verify.verify_claims`, FactCG, fail-open), `decompose_claims`
(Claimify/SAFE) and `locate` (the sentence-binding keystone).

Ground-or-cut, conservatively ("if it can't be cited, it can't be stated"):
  • supported     → keep;
  • contradicted  → CUT the sentence (the source disagrees — a hard fail);
  • not_mentioned + had-markers → STRIP the markers (the claim survives uncited);
  • not_mentioned + uncited     → leave (already flagged by checks.no_citations).

Never raises, never empties a section (reverts that section if pruning would),
honours a claim cap, is deadline-aware, and — critically — when the verifier is
disabled/absent returns the report byte-identical so a verifier outage can never
strip citations (`verify_claims` itself fails open to all-not_mentioned)."""

import logging
import re
import time

from .. import decompose, locate
from .. import verify as verify_svc
from ..config import settings
from . import cohere as cohere_mod
from .bank import Bank
from .budgets import ResearchBudgets
from .outline import Outline

_log = logging.getLogger("pai.research.verify")

_WID_RE = re.compile(r"\[[WD]\d+\]")
_WID_CAPTURE = re.compile(r"\[([WD]\d+)\]")
_EVIDENCE_CAP = 6_000


def _exempt(outline: Outline) -> set[str]:
    """Section headings (as they appear after '## ') that are NOT verified:
    placeholder/exec-summary sections plus the appended References/Coverage."""
    ex = {"References", "Coverage"}
    for i, s in enumerate(outline.sections):
        if s.placeholder is not None:
            ex.add(f"{i + 1}. {s.heading}")
    return ex


def _evidence(bank: Bank, sids: list[str]) -> str:
    """Concatenated raw evidence for the cited sources — chunks for web, the note
    text (full_text / claims+quotes) for census documents. Bounded."""
    parts: list[str] = []
    for sid in sids:
        rec = bank.get(sid)
        if rec is None:
            continue
        if rec.source.chunks:
            parts.extend(rec.source.chunks)
        elif rec.note is not None:
            parts.append(rec.note.text())
    return "\n\n".join(p for p in parts if p)[:_EVIDENCE_CAP]


def _strip_markers(sentence: str, sids: list[str]) -> str:
    """Remove the given [sid] markers from a sentence; tidy doubled spaces and a
    space before terminal punctuation."""
    out = sentence
    for sid in sids:
        out = out.replace(f"[{sid}]", "")
    out = re.sub(r"[ \t]{2,}", " ", out)
    out = re.sub(r"\s+([.!?,;:])", r"\1", out)
    return out.strip()


async def verify_and_prune(
    report: str, outline: Outline, bank: Bank, b: ResearchBudgets, deadline: float
) -> tuple[str, dict | None]:
    """Returns (pruned_report, summary) or (report, None) when skipped. `None`
    guarantees the report is byte-identical (the OFF/outage path)."""
    from ..rag_ctx import cfg

    # Up-front gate: never prune on a disabled/cooling verifier (verify_claims
    # fails open to all-not_mentioned — stripping on that would gut citations).
    if not cfg("verify_enabled", settings.verify_enabled):
        return report, None
    if time.monotonic() >= deadline:
        return report, None
    try:
        return await _run(report, outline, bank, b, cfg)
    except Exception as e:  # noqa: BLE001 — deliver unverified, never raise
        _log.warning("verify_and_prune failed (delivering unverified): %s", e)
        return report, None


async def _run(report, outline, bank, b, cfg) -> tuple[str, dict | None]:
    exempt = _exempt(outline)
    parts = cohere_mod.split_sections(report)
    max_claims = max(1, settings.verify_draft_max_claims)

    # --- Pass 1: decompose each evidence section → locate → bind markers -------
    # A "group" is one located sentence (its claims share a verdict decision).
    groups: list[dict] = []  # {part_idx, start, end, sids, evidence, claims:[str]}
    uncited = 0  # claims with no resolvable cited sentence (counted, never acted on)
    for pidx, part in enumerate(parts):
        if not part.startswith("## "):
            continue
        head = part.splitlines()[0][3:].strip()
        if head in exempt:
            continue
        body = "\n".join(part.splitlines()[1:])
        if not body.strip():
            continue
        claims = await decompose.decompose_claims(body)
        for claim in claims:
            loc = locate.locate(claim, body)
            if loc is None:
                uncited += 1
                continue
            sids = list(dict.fromkeys(_WID_CAPTURE.findall(loc["text"])))
            # Merge claims that bind to the same sentence span.
            g = next(
                (x for x in groups if x["part_idx"] == pidx and x["start"] == loc["start"] and x["end"] == loc["end"]),
                None,
            )
            if g is None:
                g = {"part_idx": pidx, "start": loc["start"], "end": loc["end"],
                     "text": loc["text"], "sids": list(sids), "claims": []}
                groups.append(g)
            else:
                for s in sids:
                    if s not in g["sids"]:
                        g["sids"].append(s)
            g["claims"].append(claim)

    if not groups:
        return report, None  # nothing verifiable located → byte-identical

    # --- Claim cap: prioritise cited groups, then truncate --------------------
    flat: list[tuple[dict, str]] = [(g, c) for g in groups for c in g["claims"]]
    flat.sort(key=lambda gc: 0 if gc[0]["sids"] else 1)  # cited first
    flat = flat[:max_claims]

    pairs = [{"text": c, "evidence": _evidence(bank, g["sids"])} for g, c in flat]
    verdicts = await verify_svc.verify_claims(pairs, hhem_filter=bool(cfg("verify_hhem_filter", False)))

    # Outage guard: `verify_claims` fails open to all-not_mentioned/score-0 when
    # the sidecar is down. Stripping on THAT would gut every citation, so if no
    # verdict shows a real signal (a supported/contradicted, or any positive
    # score) treat it as an outage and deliver byte-identical (no prune, no pill).
    live = any(
        v.get("verdict") in ("supported", "contradicted") or float(v.get("score", 0.0)) > 0.0
        for v in verdicts
    )
    if not live:
        return report, None

    # --- Aggregate verdicts per group -----------------------------------------
    per_group: dict[int, dict] = {}  # id(group) → {contradicted, not_mentioned, supported}
    supported = contradicted = not_mentioned = 0
    for (g, _claim), v in zip(flat, verdicts):
        verd = v.get("verdict", "not_mentioned")
        score = float(v.get("score", 0.0))
        agg = per_group.setdefault(id(g), {"group": g, "verdict": "supported", "score": 0.0})
        # Worst verdict wins: contradicted > not_mentioned > supported.
        if verd == "contradicted":
            agg["verdict"] = "contradicted"
            agg["score"] = max(agg["score"], score)
        elif verd == "not_mentioned" and agg["verdict"] != "contradicted":
            agg["verdict"] = "not_mentioned"
            agg["score"] = max(agg["score"], score)
        if verd == "supported":
            supported += 1
        elif verd == "contradicted":
            contradicted += 1
        else:
            not_mentioned += 1
    not_mentioned += uncited  # uncited claims are unsupported-by-definition
    total = len(flat) + uncited

    # --- Pass 2: apply ground-or-cut per section (offset-stable) --------------
    flagged: list[dict] = []  # surviving spans for the UI: {text, label, score}
    edits_by_part: dict[int, list[dict]] = {}
    for agg in per_group.values():
        g = agg["group"]
        if agg["verdict"] == "supported":
            continue
        edits_by_part.setdefault(g["part_idx"], []).append({**g, "verdict": agg["verdict"], "score": agg["score"]})

    new_parts = list(parts)
    for pidx, edits in edits_by_part.items():
        part = parts[pidx]
        nl = part.index("\n") if "\n" in part else len(part)
        head_line, body = part[:nl], part[nl + 1 :] if "\n" in part else ""
        original_body = body
        # Apply highest-offset first so earlier offsets stay valid.
        for e in sorted(edits, key=lambda x: x["start"], reverse=True):
            seg = body[e["start"] : e["end"]]
            if e["verdict"] == "contradicted":
                body = body[: e["start"]] + body[e["end"] :]  # cut the sentence
            elif e["sids"]:
                stripped = _strip_markers(seg, e["sids"])
                body = body[: e["start"]] + stripped + body[e["end"] :]
                if stripped:
                    flagged.append({"text": stripped, "label": "not_mentioned", "score": e["score"]})
        body = re.sub(r"\n{3,}", "\n\n", body).strip()
        if not body:
            body = original_body  # never empty a section
            # nothing flagged survives from a reverted section
            flagged = [f for f in flagged if f["text"] in body]
        new_parts[pidx] = f"{head_line}\n{body}"

    pruned = "\n\n".join(new_parts)
    score = supported / total if total else 1.0
    summary = {
        "score": score,
        "total": total,
        "supported": supported,
        "contradicted": contradicted,
        "not_mentioned": not_mentioned,
        "model": settings.verify_factcg_model or settings.verify_model,
        "flagged": flagged,  # pipeline resolves final offsets against the delivered report
    }
    return pruned, summary
